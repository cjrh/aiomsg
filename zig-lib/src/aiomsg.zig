//! aiomsg — native Zig smart sockets.
//!
//! A single `Socket` multiplexes many TCP connections behind one object, with
//! ZMQ-like distribution patterns (publish / round-robin / by-identity),
//! automatic reconnection, send buffering, heartbeating, an optional
//! at-least-once delivery guarantee, and TLS (OpenSSL). It speaks the
//! language-independent aiomsg wire protocol (see ../PROTOCOL.md) and
//! interoperates with the Python reference and every other port.
//!
//! Concurrency model. `std.Io` is the backbone: the caller supplies an `Io`
//! (e.g. a `std.Io.Threaded`), and the Socket runs its accept loop, connect
//! loops, per-connection workers, and a resend ticker as `io.concurrent` tasks
//! (real threads under the threaded backend). `std.Io.Mutex`/`Condition` guard
//! the shared broker state, the inbox, and each connection's outbound queue.
//!
//! Why C interop for the byte stream. TLS uses OpenSSL (a project-wide choice
//! for the C-family ports), and OpenSSL drives the socket directly via its fd.
//! `std.Io.net` hands us that fd (`stream.socket.handle`), so `std.Io` owns
//! everything it is good at — addressing, listen/accept/connect, scheduling,
//! synchronization — while the encrypted bytes flow through OpenSSL/libc. Each
//! connection is serviced by ONE worker running a short-timeout poll loop, so a
//! single thread interleaves reads and writes; this is required because an
//! OpenSSL `SSL` must not be driven from two threads at once. The cost is up to
//! `poll_ms` of latency on a message to an otherwise-idle peer.
//!
//! Resends (at-least-once) are swept by a periodic `io.sleep` ticker task, since
//! `std.Io.Condition` offers no timed wait.

const std = @import("std");
const Allocator = std.mem.Allocator;
const Io = std.Io;
const net = std.Io.net;
const proto = @import("protocol.zig");

const c = @cImport({
    @cInclude("errno.h");
    @cInclude("signal.h");
    @cInclude("sys/socket.h");
    @cInclude("netinet/in.h");
    @cInclude("netinet/tcp.h");
    @cInclude("arpa/inet.h");
    @cInclude("sys/random.h");
    @cInclude("time.h");
    @cInclude("fcntl.h");
    @cInclude("unistd.h");
    @cInclude("openssl/ssl.h");
    @cInclude("openssl/err.h");
});

pub const Identity = proto.Identity;
pub const identity_size = proto.identity_size;

const poll_ms: i64 = 50;
const heartbeat_ms: i64 = 5000;
const timeout_ms: i64 = 15000;
const handshake_ms: i64 = 15000;
const reconnect_ms: i64 = 100;
const resend_ms: i64 = 5000;
const sweep_ms: i64 = 200;
const max_retries: u32 = 5;

pub const SendMode = enum { round_robin, publish };
pub const Delivery = enum { at_most_once, at_least_once };

pub const Options = struct {
    mode: SendMode = .round_robin,
    delivery: Delivery = .at_most_once,
    identity: ?Identity = null,
};

/// A received message: payload plus the identity of the peer it came from.
pub const Message = struct {
    data: []u8,
    sender: Identity,
};

/// PEM paths for a TLS server (bind) handshake.
pub const ServerTls = struct { cert_path: []const u8, key_path: []const u8 };

/// Trust configuration for a TLS client (connect) handshake. `server_name`
/// empty means verify against the connect host.
pub const ClientTls = struct { ca_path: []const u8, server_name: []const u8 = "" };

fn nowMs() i64 {
    var ts: c.struct_timespec = undefined;
    _ = c.clock_gettime(c.CLOCK_MONOTONIC, &ts);
    return @as(i64, ts.tv_sec) * 1000 + @divTrunc(@as(i64, ts.tv_nsec), 1_000_000);
}

fn fillRandom(buf: []u8) void {
    if (c.getrandom(buf.ptr, buf.len, 0) == @as(isize, @intCast(buf.len))) return;
    // Fallback: seed a PRNG from the clock (identities need not be crypto-grade).
    var ts: c.struct_timespec = undefined;
    _ = c.clock_gettime(c.CLOCK_MONOTONIC, &ts);
    var prng = std.Random.DefaultPrng.init(@intCast(ts.tv_nsec));
    prng.random().bytes(buf);
}

// ---------------------------------------------------------------------------
// Internal value types
// ---------------------------------------------------------------------------

const Buffered = struct {
    identity: ?Identity,
    data: []u8, // owned
};

const Pending = struct {
    msg_id: proto.MsgId,
    identity: ?Identity,
    data: []u8, // owned
    retries: u32,
    deadline: i64,
};

const ConnEntry = struct { identity: Identity, conn: *Conn };

// One live transport, serviced by one worker. The worker owns the `Conn`; the
// broker holds a borrowed pointer between register and unregister (both under
// the broker mutex), so it never sees a freed `Conn`.
const Conn = struct {
    sock: *Socket,
    fd: c_int,
    ssl: ?*c.SSL, // null for plain TCP
    peer: Identity = std.mem.zeroes(Identity),

    out_mtx: Io.Mutex = .init,
    out: std.ArrayList([]u8) = .empty, // framed bytes queued by the broker, owned

    fn queue(self: *Conn, framed: []u8) void {
        self.out_mtx.lockUncancelable(self.sock.io);
        defer self.out_mtx.unlock(self.sock.io);
        self.out.append(self.sock.gpa, framed) catch self.sock.gpa.free(framed);
    }

    fn drainInto(self: *Conn, list: *std.ArrayList([]u8)) void {
        self.out_mtx.lockUncancelable(self.sock.io);
        defer self.out_mtx.unlock(self.sock.io);
        std.mem.swap(std.ArrayList([]u8), list, &self.out);
    }

    fn deinit(self: *Conn) void {
        for (self.out.items) |b| self.sock.gpa.free(b);
        self.out.deinit(self.sock.gpa);
    }
};

// ---------------------------------------------------------------------------
// Socket
// ---------------------------------------------------------------------------

pub const Socket = struct {
    gpa: Allocator,
    io: Io,
    mode: SendMode,
    delivery: Delivery,
    id: Identity,

    // Broker state, guarded by broker_mtx.
    broker_mtx: Io.Mutex = .init,
    conns: std.ArrayList(ConnEntry) = .empty,
    rr_cursor: usize = 0,
    buffer: std.ArrayList(Buffered) = .empty,
    pending: std.ArrayList(Pending) = .empty,

    // Inbox feeding recv().
    recv_mtx: Io.Mutex = .init,
    recv_cond: Io.Condition = .init,
    inbox: std.ArrayList(Message) = .empty,
    recv_closed: bool = false,

    // Open fds we shut down to interrupt blocking I/O on close.
    fds_mtx: Io.Mutex = .init,
    fds: std.ArrayList(c_int) = .empty,

    group: Io.Group = .init,
    server_ctx: ?*c.SSL_CTX = null,
    shutting_down: std.atomic.Value(bool) = .init(false),
    closed: bool = false,

    pub fn init(gpa: Allocator, io: Io, opts: Options) !*Socket {
        _ = c.signal(c.SIGPIPE, c.SIG_IGN);
        const self = try gpa.create(Socket);
        self.* = .{
            .gpa = gpa,
            .io = io,
            .mode = opts.mode,
            .delivery = opts.delivery,
            .id = undefined,
        };
        if (opts.identity) |given| {
            self.id = given;
        } else {
            fillRandom(&self.id);
        }
        // The resend ticker runs for the socket's whole life.
        self.group.concurrent(io, tickerTask, .{self}) catch {};
        return self;
    }

    pub fn deinit(self: *Socket) void {
        self.close();
        // Drain anything still in the inbox.
        for (self.inbox.items) |m| self.gpa.free(m.data);
        self.inbox.deinit(self.gpa);
        for (self.buffer.items) |b| self.gpa.free(b.data);
        self.buffer.deinit(self.gpa);
        for (self.pending.items) |p| self.gpa.free(p.data);
        self.pending.deinit(self.gpa);
        self.conns.deinit(self.gpa);
        self.fds.deinit(self.gpa);
        if (self.server_ctx) |ctx| c.SSL_CTX_free(ctx);
        self.gpa.destroy(self);
    }

    pub fn identity(self: *const Socket) *const Identity {
        return &self.id;
    }

    // --- fd registry ------------------------------------------------------
    fn addFd(self: *Socket, fd: c_int) void {
        self.fds_mtx.lockUncancelable(self.io);
        defer self.fds_mtx.unlock(self.io);
        self.fds.append(self.gpa, fd) catch {};
    }
    fn removeFd(self: *Socket, fd: c_int) void {
        self.fds_mtx.lockUncancelable(self.io);
        defer self.fds_mtx.unlock(self.io);
        for (self.fds.items, 0..) |f, i| {
            if (f == fd) {
                _ = self.fds.swapRemove(i);
                break;
            }
        }
    }

    // --- public bind / connect / send / recv ------------------------------
    pub fn bind(self: *Socket, host: []const u8, port: u16) !void {
        try self.startBind(host, port, null);
    }

    pub fn bindTls(self: *Socket, host: []const u8, port: u16, tls: ServerTls) !void {
        const ctx = c.SSL_CTX_new(c.TLS_server_method()) orelse return error.TlsContext;
        errdefer c.SSL_CTX_free(ctx);
        const cert_z = try self.gpa.dupeZ(u8, tls.cert_path);
        defer self.gpa.free(cert_z);
        const key_z = try self.gpa.dupeZ(u8, tls.key_path);
        defer self.gpa.free(key_z);
        if (c.SSL_CTX_use_certificate_chain_file(ctx, cert_z) != 1 or
            c.SSL_CTX_use_PrivateKey_file(ctx, key_z, c.SSL_FILETYPE_PEM) != 1)
        {
            return error.TlsCertLoad;
        }
        self.server_ctx = ctx;
        try self.startBind(host, port, ctx);
    }

    fn startBind(self: *Socket, host: []const u8, port: u16, ctx: ?*c.SSL_CTX) !void {
        const addr = try resolveOne(self.io, host, port);
        const server = addr.listen(self.io, .{ .reuse_address = true }) catch return error.BindFailed;
        const listen_fd: c_int = server.socket.handle;
        self.addFd(listen_fd);
        self.group.concurrent(self.io, acceptTask, .{ self, server, ctx }) catch return error.SpawnFailed;
    }

    pub fn connect(self: *Socket, host: []const u8, port: u16) !void {
        try self.startConnect(host, port, null, "");
    }

    pub fn connectTls(self: *Socket, host: []const u8, port: u16, tls: ClientTls) !void {
        const ctx = c.SSL_CTX_new(c.TLS_client_method()) orelse return error.TlsContext;
        errdefer c.SSL_CTX_free(ctx);
        const ca_z = try self.gpa.dupeZ(u8, tls.ca_path);
        defer self.gpa.free(ca_z);
        if (c.SSL_CTX_load_verify_locations(ctx, ca_z, null) != 1) return error.TlsCaLoad;
        c.SSL_CTX_set_verify(ctx, c.SSL_VERIFY_PEER, null);
        try self.startConnect(host, port, ctx, tls.server_name);
    }

    fn startConnect(self: *Socket, host: []const u8, port: u16, ctx: ?*c.SSL_CTX, server_name: []const u8) !void {
        const cfg = try self.gpa.create(ConnectConfig);
        cfg.* = .{
            .host = try self.gpa.dupe(u8, host),
            .port = port,
            .ctx = ctx,
            .server_name = try self.gpa.dupe(u8, if (server_name.len > 0) server_name else host),
        };
        self.group.concurrent(self.io, connectTask, .{ self, cfg }) catch return error.SpawnFailed;
    }

    pub fn send(self: *Socket, data: []const u8) void {
        self.dispatch(null, data);
    }
    pub fn sendTo(self: *Socket, ident: Identity, data: []const u8) void {
        self.dispatch(ident, data);
    }

    fn dispatch(self: *Socket, ident: ?Identity, data: []const u8) void {
        if (self.shutting_down.load(.acquire)) return;
        const copy = self.gpa.dupe(u8, data) catch return;
        self.broker_mtx.lockUncancelable(self.io);
        defer self.broker_mtx.unlock(self.io);
        self.brokerSend(ident, copy);
    }

    /// Block for the next message. Returns null once the socket is closed and
    /// drained. Caller owns `Message.data` and must free it with the same
    /// allocator passed to `init`.
    pub fn recv(self: *Socket) ?Message {
        self.recv_mtx.lockUncancelable(self.io);
        defer self.recv_mtx.unlock(self.io);
        while (self.inbox.items.len == 0 and !self.recv_closed) {
            self.recv_cond.waitUncancelable(self.io, &self.recv_mtx);
        }
        if (self.inbox.items.len == 0) return null;
        return self.inbox.orderedRemove(0);
    }

    pub fn close(self: *Socket) void {
        if (self.closed) return;
        self.closed = true;
        self.shutting_down.store(true, .release);

        // Interrupt blocking accept/handshake/read by shutting every fd.
        {
            self.fds_mtx.lockUncancelable(self.io);
            for (self.fds.items) |fd| _ = c.shutdown(fd, c.SHUT_RDWR);
            self.fds_mtx.unlock(self.io);
        }
        // Cancel Io-blocked tasks (accept/connect/sleep) at their cancelation
        // points and join everything. C-blocked poll loops exit via the flag.
        self.group.cancel(self.io);

        self.recv_mtx.lockUncancelable(self.io);
        self.recv_closed = true;
        self.recv_cond.broadcast(self.io);
        self.recv_mtx.unlock(self.io);
    }

    // --- broker operations (caller holds broker_mtx unless noted) ---------
    fn findConn(self: *Socket, ident: *const Identity) ?*Conn {
        for (self.conns.items) |e| {
            if (std.mem.eql(u8, &e.identity, ident)) return e.conn;
        }
        return null;
    }

    fn brokerRoute(self: *Socket, target: ?Identity, framed: []u8) void {
        if (self.shutting_down.load(.acquire)) {
            self.gpa.free(framed);
            return;
        }
        if (target) |t| {
            if (self.findConn(&t)) |conn| conn.queue(framed) else self.gpa.free(framed);
            return;
        }
        if (self.conns.items.len == 0) {
            self.gpa.free(framed);
            return;
        }
        if (self.mode == .publish) {
            for (self.conns.items) |e| {
                const cp = self.gpa.dupe(u8, framed) catch continue;
                e.conn.queue(cp);
            }
            self.gpa.free(framed);
        } else {
            const conn = self.conns.items[self.rr_cursor % self.conns.items.len].conn;
            self.rr_cursor +%= 1;
            conn.queue(framed);
        }
    }

    fn brokerTransmit(self: *Socket, target: ?Identity, data: []const u8, retries: u32) void {
        const at_least_once = self.delivery == .at_least_once and
            (target != null or self.mode == .round_robin);
        if (!at_least_once) {
            const f = proto.frameData(self.gpa, data) catch return;
            self.brokerRoute(target, f);
            return;
        }
        var p: Pending = .{
            .msg_id = undefined,
            .identity = target,
            .data = self.gpa.dupe(u8, data) catch return,
            .retries = retries,
            .deadline = nowMs() + resend_ms,
        };
        fillRandom(&p.msg_id);
        const f = proto.frameDataReq(self.gpa, &p.msg_id, data) catch {
            self.gpa.free(p.data);
            return;
        };
        self.pending.append(self.gpa, p) catch {
            self.gpa.free(p.data);
            self.gpa.free(f);
            return;
        };
        self.brokerRoute(target, f);
    }

    fn brokerSend(self: *Socket, target: ?Identity, data: []u8) void {
        if (self.conns.items.len == 0) {
            self.buffer.append(self.gpa, .{ .identity = target, .data = data }) catch self.gpa.free(data);
        } else {
            defer self.gpa.free(data);
            self.brokerTransmit(target, data, max_retries);
        }
    }

    fn brokerSweep(self: *Socket) void {
        const now = nowMs();
        var i: usize = 0;
        // Collect expired entries, resend the ones with retries left.
        while (i < self.pending.items.len) {
            if (self.pending.items[i].deadline <= now) {
                const p = self.pending.swapRemove(i);
                if (p.retries > 0) {
                    if (self.conns.items.len == 0) {
                        self.buffer.append(self.gpa, .{ .identity = p.identity, .data = p.data }) catch self.gpa.free(p.data);
                    } else {
                        defer self.gpa.free(p.data);
                        self.brokerTransmit(p.identity, p.data, p.retries - 1);
                    }
                } else {
                    self.gpa.free(p.data);
                }
            } else {
                i += 1;
            }
        }
    }

    /// Register a connection. Returns false on a duplicate identity. Caller
    /// holds broker_mtx.
    fn brokerRegister(self: *Socket, conn: *Conn) bool {
        if (self.findConn(&conn.peer) != null) return false;
        self.conns.append(self.gpa, .{ .identity = conn.peer, .conn = conn }) catch return false;
        // Flush the send buffer now that a peer exists.
        var pend = self.buffer;
        self.buffer = .empty;
        defer pend.deinit(self.gpa);
        for (pend.items) |b| self.brokerSend(b.identity, b.data);
        return true;
    }

    fn brokerUnregister(self: *Socket, conn: *Conn) void {
        for (self.conns.items, 0..) |e, i| {
            if (e.conn == conn) {
                _ = self.conns.swapRemove(i);
                break;
            }
        }
    }

    fn brokerReceived(self: *Socket, sender: *const Identity, e: proto.Envelope) void {
        switch (e.type) {
            .data => self.deliver(sender, e.payload),
            .data_req => {
                self.deliver(sender, e.payload);
                var mid: proto.MsgId = undefined;
                @memcpy(&mid, e.msg_id.?[0..proto.msg_id_size]);
                const ack = proto.frameAck(self.gpa, &mid) catch return;
                self.brokerRoute(sender.*, ack);
            },
            .ack => {
                for (self.pending.items, 0..) |p, i| {
                    if (std.mem.eql(u8, &p.msg_id, e.msg_id.?[0..proto.msg_id_size])) {
                        self.gpa.free(p.data);
                        _ = self.pending.swapRemove(i);
                        break;
                    }
                }
            },
            else => {},
        }
    }

    /// Hand a received payload to recv(). Caller holds broker_mtx (a different
    /// mutex than recv_mtx, so no reentrancy).
    fn deliver(self: *Socket, sender: *const Identity, payload: []const u8) void {
        const copy = self.gpa.dupe(u8, payload) catch return;
        self.recv_mtx.lockUncancelable(self.io);
        defer self.recv_mtx.unlock(self.io);
        self.inbox.append(self.gpa, .{ .data = copy, .sender = sender.* }) catch {
            self.gpa.free(copy);
            return;
        };
        self.recv_cond.signal(self.io);
    }
};

const ConnectConfig = struct {
    host: []u8,
    port: u16,
    ctx: ?*c.SSL_CTX,
    server_name: []u8,
};

// ===========================================================================
// Tasks (run via io.concurrent)
// ===========================================================================

fn tickerTask(self: *Socket) void {
    while (!self.shutting_down.load(.acquire)) {
        self.io.sleep(Io.Duration.fromMilliseconds(sweep_ms), .boot) catch break;
        if (self.shutting_down.load(.acquire)) break;
        self.broker_mtx.lockUncancelable(self.io);
        self.brokerSweep();
        self.broker_mtx.unlock(self.io);
    }
}

fn acceptTask(self: *Socket, server_in: net.Server, ctx: ?*c.SSL_CTX) void {
    var server = server_in;
    const listen_fd: c_int = server.socket.handle;
    while (!self.shutting_down.load(.acquire)) {
        const stream = server.accept(self.io) catch break; // closed / canceled
        const fd: c_int = stream.socket.handle;
        // Each connection is serviced by its own worker (blocking C I/O).
        self.group.concurrent(self.io, runConnection, .{ self, fd, ctx, true, @as([]const u8, "") }) catch {
            _ = c.close(fd);
        };
    }
    self.removeFd(listen_fd);
    _ = c.close(listen_fd);
}

fn connectTask(self: *Socket, cfg: *ConnectConfig) void {
    defer {
        if (cfg.ctx) |ctx| c.SSL_CTX_free(ctx);
        self.gpa.free(cfg.host);
        self.gpa.free(cfg.server_name);
        self.gpa.destroy(cfg);
    }
    while (!self.shutting_down.load(.acquire)) {
        if (dialConnect(self.io, cfg.host, cfg.port)) |fd| {
            runConnection(self, fd, cfg.ctx, false, cfg.server_name);
        } else |_| {}
        if (self.shutting_down.load(.acquire)) break;
        self.io.sleep(Io.Duration.fromMilliseconds(reconnect_ms), .boot) catch break;
    }
}

// ===========================================================================
// Connection establishment helpers
// ===========================================================================

fn resolveOne(io: Io, host: []const u8, port: u16) !net.IpAddress {
    if (net.IpAddress.parse(host, port)) |addr| return addr else |_| {}
    return net.IpAddress.resolve(io, host, port) catch error.ResolveFailed;
}

fn dialConnect(io: Io, host: []const u8, port: u16) !c_int {
    const addr = try resolveOne(io, host, port);
    const stream = addr.connect(io, .{ .mode = .stream }) catch return error.ConnectFailed;
    return stream.socket.handle;
}

// ===========================================================================
// Connection worker: TLS handshake, HELLO exchange, registration, poll loop.
// Owns `fd` (and any SSL). One thread interleaves reads and writes.
// ===========================================================================

fn setReadTimeout(fd: c_int, ms: i64) void {
    var tv: c.struct_timeval = .{
        .tv_sec = @intCast(@divTrunc(ms, 1000)),
        .tv_usec = @intCast(@mod(ms, 1000) * 1000),
    };
    _ = c.setsockopt(fd, c.SOL_SOCKET, c.SO_RCVTIMEO, &tv, @sizeOf(c.struct_timeval));
}

fn setBlocking(fd: c_int) void {
    const flags = c.fcntl(fd, c.F_GETFL, @as(c_int, 0));
    if (flags >= 0) _ = c.fcntl(fd, c.F_SETFL, flags & ~@as(c_int, c.O_NONBLOCK));
}

const ReadResult = enum { data, idle, closed };

fn connRead(conn: *Conn, dec: *proto.Decoder, gpa: Allocator) ReadResult {
    var buf: [16384]u8 = undefined;
    if (conn.ssl) |ssl| {
        const r = c.SSL_read(ssl, &buf, buf.len);
        if (r > 0) {
            dec.push(gpa, buf[0..@intCast(r)]) catch return .closed;
            return .data;
        }
        const err = c.SSL_get_error(ssl, r);
        if (err == c.SSL_ERROR_WANT_READ or err == c.SSL_ERROR_WANT_WRITE) return .idle;
        if (err == c.SSL_ERROR_SYSCALL) {
            const e = std.c._errno().*;
            if (e == c.EAGAIN or e == c.EWOULDBLOCK or e == c.EINTR) return .idle;
        }
        return .closed;
    }
    const n = c.recv(conn.fd, &buf, buf.len, 0);
    if (n > 0) {
        dec.push(gpa, buf[0..@intCast(n)]) catch return .closed;
        return .data;
    }
    if (n == 0) return .closed;
    const e = std.c._errno().*;
    if (e == c.EAGAIN or e == c.EWOULDBLOCK or e == c.EINTR) return .idle;
    return .closed;
}

fn connWriteAll(conn: *Conn, buf: []const u8) bool {
    var off: usize = 0;
    while (off < buf.len) {
        if (conn.ssl) |ssl| {
            const r = c.SSL_write(ssl, buf.ptr + off, @intCast(buf.len - off));
            if (r > 0) {
                off += @intCast(r);
                continue;
            }
            const err = c.SSL_get_error(ssl, r);
            if (err == c.SSL_ERROR_WANT_READ or err == c.SSL_ERROR_WANT_WRITE) continue;
            return false;
        }
        const n = c.send(conn.fd, buf.ptr + off, buf.len - off, c.MSG_NOSIGNAL);
        if (n > 0) {
            off += @intCast(n);
            continue;
        }
        if (n < 0 and std.c._errno().* == c.EINTR) continue;
        return false;
    }
    return true;
}

fn applyClientVerify(ssl: *c.SSL, name_z: [:0]const u8) void {
    var ipbuf: [16]u8 = undefined;
    if (c.inet_pton(c.AF_INET, name_z, &ipbuf) == 1 or c.inet_pton(c.AF_INET6, name_z, &ipbuf) == 1) {
        _ = c.X509_VERIFY_PARAM_set1_ip_asc(c.SSL_get0_param(ssl), name_z);
    } else {
        _ = c.SSL_set1_host(ssl, name_z);
        // SSL_set_tlsext_host_name is a macro; call the underlying ctrl for SNI.
        _ = c.SSL_ctrl(ssl, c.SSL_CTRL_SET_TLSEXT_HOSTNAME, c.TLSEXT_NAMETYPE_host_name, @constCast(@ptrCast(name_z.ptr)));
    }
}

// Read the peer's HELLO within the handshake timeout. Returns true on success.
fn readHello(conn: *Conn, dec: *proto.Decoder, gpa: Allocator) bool {
    const deadline = nowMs() + handshake_ms;
    while (true) {
        if (dec.pop()) |env| {
            const e = proto.parseEnvelope(env) orelse return false;
            if (e.type == .hello and e.version == proto.protocol_version) {
                @memcpy(&conn.peer, e.identity.?[0..proto.identity_size]);
                return true;
            }
            return false;
        }
        switch (connRead(conn, dec, gpa)) {
            .closed => return false,
            .idle => if (nowMs() >= deadline) return false,
            .data => {},
        }
    }
}

fn forwardFrames(self: *Socket, conn: *Conn, dec: *proto.Decoder) void {
    self.broker_mtx.lockUncancelable(self.io);
    defer self.broker_mtx.unlock(self.io);
    while (dec.pop()) |env| {
        const e = proto.parseEnvelope(env) orelse continue;
        if (e.type == .heartbeat or e.type == .hello) continue;
        self.brokerReceived(&conn.peer, e);
    }
}

fn runConnection(self: *Socket, fd: c_int, ctx: ?*c.SSL_CTX, is_server: bool, server_name: []const u8) void {
    const gpa = self.gpa;
    var conn = Conn{ .sock = self, .fd = fd, .ssl = null };
    var registered = false;
    var dec: proto.Decoder = .{};

    const one: c_int = 1;
    _ = c.setsockopt(fd, c.IPPROTO_TCP, c.TCP_NODELAY, &one, @sizeOf(c_int));
    setBlocking(fd);
    setReadTimeout(fd, handshake_ms);
    self.addFd(fd);

    // Teardown: free transport, unregister (under lock) before freeing Conn.
    defer {
        dec.deinit(gpa);
        if (conn.ssl) |ssl| c.SSL_free(ssl);
        self.removeFd(fd);
        _ = c.close(fd);
        if (registered) {
            self.broker_mtx.lockUncancelable(self.io);
            self.brokerUnregister(&conn);
            self.broker_mtx.unlock(self.io);
        }
        conn.deinit();
    }

    if (ctx) |ssl_ctx| {
        const ssl = c.SSL_new(ssl_ctx) orelse return;
        conn.ssl = ssl;
        _ = c.SSL_set_fd(ssl, fd);
        if (is_server) {
            if (c.SSL_accept(ssl) != 1) return;
        } else {
            const name_z = gpa.dupeZ(u8, server_name) catch return;
            defer gpa.free(name_z);
            applyClientVerify(ssl, name_z);
            if (c.SSL_connect(ssl) != 1) return;
        }
    }

    // App handshake: send our HELLO, read and validate the peer's.
    const hello = proto.frameHello(gpa, &self.id) catch return;
    const wrote = connWriteAll(&conn, hello);
    gpa.free(hello);
    if (!wrote) return;
    if (!readHello(&conn, &dec, gpa)) return;

    // Register; the broker now knows this connection.
    {
        self.broker_mtx.lockUncancelable(self.io);
        const ok = self.brokerRegister(&conn);
        self.broker_mtx.unlock(self.io);
        if (!ok) return; // duplicate identity
        registered = true;
    }

    setReadTimeout(fd, poll_ms);
    var last_in = nowMs();
    var last_out = nowMs();
    forwardFrames(self, &conn, &dec); // anything buffered during the handshake

    while (!self.shutting_down.load(.acquire)) {
        // 1. Send everything the broker queued for us.
        var outgoing: std.ArrayList([]u8) = .empty;
        conn.drainInto(&outgoing);
        var werr = false;
        for (outgoing.items) |frame| {
            if (!werr and !connWriteAll(&conn, frame)) werr = true;
            gpa.free(frame);
            last_out = nowMs();
        }
        outgoing.deinit(gpa);
        if (werr) break;

        // 2. Heartbeat if idle.
        if (nowMs() - last_out >= heartbeat_ms) {
            const beat = proto.frameHeartbeat(gpa) catch break;
            const ok = connWriteAll(&conn, beat);
            gpa.free(beat);
            if (!ok) break;
            last_out = nowMs();
        }

        // 3. Read (blocks up to poll_ms).
        switch (connRead(&conn, &dec, gpa)) {
            .closed => break,
            .idle => {},
            .data => {
                last_in = nowMs();
                forwardFrames(self, &conn, &dec);
            },
        }

        // 4. Dead peer?
        if (nowMs() - last_in >= timeout_ms) break;
    }
}

// ===========================================================================
// Integration tests (TCP + TLS), gated on the shared conformance certs.
// ===========================================================================

const build_options = @import("build_options");

fn freePort() u16 {
    const fd = c.socket(c.AF_INET, c.SOCK_STREAM, 0);
    defer _ = c.close(fd);
    var addr: c.struct_sockaddr_in = std.mem.zeroes(c.struct_sockaddr_in);
    addr.sin_family = c.AF_INET;
    addr.sin_addr.s_addr = c.htonl(c.INADDR_LOOPBACK);
    addr.sin_port = 0;
    _ = c.bind(fd, @ptrCast(&addr), @sizeOf(c.struct_sockaddr_in));
    var len: c.socklen_t = @sizeOf(c.struct_sockaddr_in);
    _ = c.getsockname(fd, @ptrCast(&addr), &len);
    return c.ntohs(addr.sin_port);
}

fn roundtripTest(tls: bool) !void {
    const gpa = std.heap.c_allocator;
    var threaded = Io.Threaded.init(gpa, .{});
    defer threaded.deinit();
    const io = threaded.io();

    const port = freePort();
    const server = try Socket.init(gpa, io, .{});
    defer server.deinit();
    const client = try Socket.init(gpa, io, .{});
    defer client.deinit();

    if (tls) {
        const cert = build_options.cert_dir ++ "/cert.pem";
        const key = build_options.cert_dir ++ "/key.pem";
        try server.bindTls("127.0.0.1", port, .{ .cert_path = cert, .key_path = key });
        try client.connectTls("127.0.0.1", port, .{ .ca_path = cert });
    } else {
        try server.bind("127.0.0.1", port);
        try client.connect("127.0.0.1", port);
    }

    // The bind side buffers until the client connects, so send eagerly.
    var i: usize = 0;
    while (i < 10) : (i += 1) {
        var msgbuf: [16]u8 = undefined;
        server.send(std.fmt.bufPrint(&msgbuf, "m{d}", .{i}) catch unreachable);
    }
    i = 0;
    while (i < 10) : (i += 1) {
        const m = client.recv() orelse return error.RecvClosed;
        defer gpa.free(m.data);
        var expbuf: [16]u8 = undefined;
        const exp = std.fmt.bufPrint(&expbuf, "m{d}", .{i}) catch unreachable;
        try std.testing.expectEqualStrings(exp, m.data);
    }
}

test "tcp roundtrip" {
    try roundtripTest(false);
}

test "tls roundtrip" {
    try roundtripTest(true);
}
