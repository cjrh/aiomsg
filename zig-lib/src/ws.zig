//! Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg **bind**
//! sockets (see `../../PROTOCOL.md` §10).
//!
//! This is a pure, transport-agnostic layer: it turns the payloads of inbound
//! binary WebSocket messages into the concatenated aiomsg byte stream of §2
//! (pushed to the existing `protocol.Decoder`), and frames each outbound aiomsg
//! write as one unmasked binary WebSocket message. WebSocket message boundaries
//! carry no meaning. It never touches a socket itself — the caller (`aiomsg.zig`)
//! feeds it raw bytes and flushes the control-frame bytes it queues — so only the
//! server half of RFC 6455 lives here, with no subprotocol or extension (notably
//! no `permessage-deflate`) ever negotiated.

const std = @import("std");
const Allocator = std.mem.Allocator;
const proto = @import("protocol.zig");

/// The magic GUID from RFC 6455 §1.3, appended to the client key before SHA-1.
const ws_guid = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// Opcodes (RFC 6455 §5.2).
const op_cont: u8 = 0x0;
const op_text: u8 = 0x1;
const op_bin: u8 = 0x2;
const op_close: u8 = 0x8;
const op_ping: u8 = 0x9;
const op_pong: u8 = 0xA;

pub const max_request_bytes: usize = 8192;

/// The outcome of feeding inbound bytes: keep serving, or tear the connection
/// down (peer close or protocol error — the close frame, if any, is already
/// queued in `out`).
pub const FeedResult = enum { open, close };

/// Per-connection WebSocket parser state. `in` accumulates partial inbound
/// frames; `out` holds control-frame bytes (pong / close) the caller must write
/// to the peer after each feed.
pub const Ws = struct {
    in: std.ArrayList(u8) = .empty,
    out: std.ArrayList(u8) = .empty,
    close_sent: bool = false,

    pub fn deinit(self: *Ws, gpa: Allocator) void {
        self.in.deinit(gpa);
        self.out.deinit(gpa);
    }
};

// --- handshake -------------------------------------------------------------

/// Write `Sec-WebSocket-Accept` for `key` — `base64(SHA1(key ++ GUID))`,
/// exactly 28 characters (RFC 6455 §4.2.2) — into `out`.
pub fn computeAccept(key: []const u8, out: *[28]u8) void {
    var digest: [std.crypto.hash.Sha1.digest_length]u8 = undefined;
    var h = std.crypto.hash.Sha1.init(.{});
    h.update(key);
    h.update(ws_guid);
    h.final(&digest);
    _ = std.base64.standard.Encoder.encode(out, &digest);
}

/// Case-insensitive lookup of a header value, trimmed of surrounding whitespace.
/// Scans CRLF-separated lines after the request line; returns null if absent.
fn headerGet(req: []const u8, name: []const u8) ?[]const u8 {
    var it = std.mem.splitSequence(u8, req, "\r\n");
    _ = it.next(); // skip the request line
    while (it.next()) |line| {
        if (line.len == 0) break; // blank line: end of headers
        const colon = std.mem.indexOfScalar(u8, line, ':') orelse continue;
        if (std.ascii.eqlIgnoreCase(std.mem.trim(u8, line[0..colon], " \t"), name)) {
            return std.mem.trim(u8, line[colon + 1 ..], " \t");
        }
    }
    return null;
}

/// Does the comma-separated token list contain `token` (case-insensitive)?
fn listHasToken(list: []const u8, token: []const u8) bool {
    var it = std.mem.tokenizeAny(u8, list, ", \t");
    while (it.next()) |tok| {
        if (std.ascii.eqlIgnoreCase(tok, token)) return true;
    }
    return false;
}

/// Number of bytes `key` base64-decodes to, or null if it is not valid base64.
fn base64DecodedLen(key: []const u8) ?usize {
    return std.base64.standard.Decoder.calcSizeForSlice(key) catch null;
}

/// Validate an HTTP upgrade request (bytes through the terminating blank line)
/// and build the response. On success writes a 101 response to `resp` and
/// returns `true`; on any violation writes a 400/426 error response and returns
/// `false`. The caller writes `resp` to the peer in both cases and frees it.
pub fn handshakeResponse(gpa: Allocator, req: []const u8, resp: *[]u8) Allocator.Error!bool {
    const bad_request = "HTTP/1.1 400 Bad Request\r\n\r\n";
    const bad_version = "HTTP/1.1 426 Upgrade Required\r\nSec-WebSocket-Version: 13\r\n\r\n";

    if (req.len > max_request_bytes or req.len < 4 or !std.mem.eql(u8, req[0..4], "GET ")) {
        resp.* = try gpa.dupe(u8, bad_request);
        return false;
    }
    const upgrade = headerGet(req, "Upgrade");
    if (upgrade == null or !std.ascii.eqlIgnoreCase(upgrade.?, "websocket")) {
        resp.* = try gpa.dupe(u8, bad_request);
        return false;
    }
    const connection = headerGet(req, "Connection");
    if (connection == null or !listHasToken(connection.?, "upgrade")) {
        resp.* = try gpa.dupe(u8, bad_request);
        return false;
    }
    const version = headerGet(req, "Sec-WebSocket-Version");
    if (version == null or !std.mem.eql(u8, version.?, "13")) {
        resp.* = try gpa.dupe(u8, bad_version);
        return false;
    }
    const key = headerGet(req, "Sec-WebSocket-Key");
    if (key == null or base64DecodedLen(key.?) != proto.identity_size) {
        resp.* = try gpa.dupe(u8, bad_request);
        return false;
    }

    var accept: [28]u8 = undefined;
    computeAccept(key.?, &accept);
    resp.* = try std.fmt.allocPrint(gpa, "HTTP/1.1 101 Switching Protocols\r\n" ++
        "Upgrade: websocket\r\n" ++
        "Connection: Upgrade\r\n" ++
        "Sec-WebSocket-Accept: {s}\r\n" ++
        "\r\n", .{accept});
    return true;
}

// --- frame layer -----------------------------------------------------------

/// Encode one server->client frame (FIN=1, unmasked, given opcode) into a
/// freshly allocated buffer. opcode 0x2 = binary.
pub fn encode(gpa: Allocator, opcode: u8, payload: []const u8) Allocator.Error![]u8 {
    const len = payload.len;
    // TODO(frame-size): enforce a configurable maximum frame length
    const header: usize = if (len >= 65536) 10 else if (len >= 126) 4 else 2;
    var f = try gpa.alloc(u8, header + len);
    f[0] = 0x80 | opcode; // FIN + opcode, RSV=0
    if (header == 2) {
        f[1] = @intCast(len);
    } else if (header == 4) {
        f[1] = 126;
        f[2] = @intCast((len >> 8) & 0xff);
        f[3] = @intCast(len & 0xff);
    } else {
        f[1] = 127;
        var i: u6 = 0;
        while (i < 8) : (i += 1) {
            f[2 + i] = @intCast((len >> (56 - 8 * @as(u6, i))) & 0xff);
        }
    }
    if (len != 0) @memcpy(f[header..], payload);
    return f;
}

/// Queue a server frame (payload <= 125, so a fixed 2-byte header) into `out`.
fn queueControl(self: *Ws, gpa: Allocator, opcode: u8, payload: []const u8) Allocator.Error!void {
    try self.out.append(gpa, 0x80 | opcode);
    try self.out.append(gpa, @intCast(payload.len));
    try self.out.appendSlice(gpa, payload);
}

/// Queue a close frame with `code` (once) and signal the connection must close.
fn fail(self: *Ws, gpa: Allocator, code: u16) Allocator.Error!FeedResult {
    if (!self.close_sent) {
        self.close_sent = true;
        try queueControl(self, gpa, op_close, &.{ @intCast(code >> 8), @intCast(code & 0xff) });
    }
    return .close;
}

/// Feed raw inbound bytes. Appends decoded binary payload to `dec`, and queues
/// pong / close frames into `out` for the caller to flush. Returns `.open` to
/// keep the connection open, or `.close` when it must be torn down (peer close,
/// or a protocol error — a close frame is queued in `out` first).
pub fn feed(self: *Ws, gpa: Allocator, data: []const u8, dec: *proto.Decoder) Allocator.Error!FeedResult {
    try self.in.appendSlice(gpa, data);
    const buf = self.in.items;
    var off: usize = 0;
    const result: FeedResult = outer: while (true) {
        if (buf.len - off < 2) break :outer .open;
        const p = buf[off..];
        const b0 = p[0];
        const b1 = p[1];
        if (b0 & 0x70 != 0) break :outer try fail(self, gpa, 1002); // RSV bits set
        const opcode = b0 & 0x0f;
        const fin = b0 & 0x80 != 0;
        if (b1 & 0x80 == 0) break :outer try fail(self, gpa, 1002); // client frames MUST be masked
        var plen: u64 = b1 & 0x7f;
        var hdr: usize = 2;
        if (plen == 126) {
            if (buf.len - off < 4) break :outer .open;
            plen = (@as(u64, p[2]) << 8) | p[3];
            hdr = 4;
        } else if (plen == 127) {
            if (buf.len - off < 10) break :outer .open;
            if (p[2] & 0x80 != 0) break :outer try fail(self, gpa, 1002); // 64-bit length MSB must be 0
            // TODO(frame-size): enforce a configurable maximum frame length
            plen = 0;
            var i: usize = 0;
            while (i < 8) : (i += 1) plen = (plen << 8) | p[2 + i];
            hdr = 10;
        }
        if (opcode >= 0x8 and (!fin or plen > 125)) {
            break :outer try fail(self, gpa, 1002); // control frames: FIN=1, <=125 bytes
        }
        if (buf.len - off < hdr + 4 + plen) break :outer .open; // need mask (4) + payload

        const mask = p[hdr .. hdr + 4];
        const masked = p[hdr + 4 .. hdr + 4 + @as(usize, @intCast(plen))];
        for (masked, 0..) |*b, i| b.* ^= mask[i & 3];
        off += hdr + 4 + @as(usize, @intCast(plen));

        switch (opcode) {
            op_bin, op_cont => try dec.push(gpa, masked),
            op_ping => try queueControl(self, gpa, op_pong, masked),
            op_pong => {}, // ignore
            op_text => break :outer try fail(self, gpa, 1003), // text is not valid for aiomsg
            op_close => {
                const code: u16 = if (plen >= 2)
                    (@as(u16, masked[0]) << 8) | masked[1]
                else
                    1000;
                if (!self.close_sent) {
                    self.close_sent = true;
                    try queueControl(self, gpa, op_close, &.{ @intCast(code >> 8), @intCast(code & 0xff) });
                }
                break :outer .close;
            },
            else => break :outer try fail(self, gpa, 1002), // unknown opcode
        }
    };

    if (off != 0) {
        const rem = self.in.items.len - off;
        std.mem.copyForwards(u8, self.in.items[0..rem], self.in.items[off..]);
        self.in.shrinkRetainingCapacity(rem);
    }
    return result;
}

// --- tests -----------------------------------------------------------------

/// Build a masked client frame (as a browser would send). Test-only.
fn maskedFrame(gpa: Allocator, opcode: u8, payload: []const u8) Allocator.Error![]u8 {
    std.debug.assert(payload.len < 126); // tests use short payloads only
    var f = try gpa.alloc(u8, 2 + 4 + payload.len);
    f[0] = 0x80 | opcode;
    f[1] = 0x80 | @as(u8, @intCast(payload.len));
    const mask = [4]u8{ 0x12, 0x34, 0x56, 0x78 };
    @memcpy(f[2..6], &mask);
    for (payload, 0..) |b, i| f[6 + i] = b ^ mask[i & 3];
    return f;
}

test "RFC 6455 accept-key vector" {
    var out: [28]u8 = undefined;
    computeAccept("dGhlIHNhbXBsZSBub25jZQ==", &out);
    try std.testing.expectEqualStrings("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", &out);
}

test "handshake accepts a valid upgrade" {
    const gpa = std.testing.allocator;
    const req = "GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n" ++
        "Connection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n" ++
        "Sec-WebSocket-Version: 13\r\n\r\n";
    var resp: []u8 = undefined;
    const ok = try handshakeResponse(gpa, req, &resp);
    defer gpa.free(resp);
    try std.testing.expect(ok);
    try std.testing.expect(std.mem.indexOf(u8, resp, "101 Switching Protocols") != null);
    try std.testing.expect(std.mem.indexOf(u8, resp, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=") != null);
}

test "handshake rejects a wrong version with 426" {
    const gpa = std.testing.allocator;
    const req = "GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n" ++
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n";
    var resp: []u8 = undefined;
    const ok = try handshakeResponse(gpa, req, &resp);
    defer gpa.free(resp);
    try std.testing.expect(!ok);
    try std.testing.expect(std.mem.indexOf(u8, resp, "426") != null);
}

test "feed unmasks a binary message into the decoder" {
    const gpa = std.testing.allocator;
    var w = Ws{};
    defer w.deinit(gpa);
    var dec: proto.Decoder = .{};
    defer dec.deinit(gpa);

    const beat = try proto.frameHeartbeat(gpa);
    defer gpa.free(beat);
    const frame = try maskedFrame(gpa, op_bin, beat);
    defer gpa.free(frame);

    try std.testing.expectEqual(FeedResult.open, try feed(&w, gpa, frame, &dec));
    const e = proto.parseEnvelope(dec.pop().?).?;
    try std.testing.expect(e.type == .heartbeat);
}

test "feed answers a ping with a pong" {
    const gpa = std.testing.allocator;
    var w = Ws{};
    defer w.deinit(gpa);
    var dec: proto.Decoder = .{};
    defer dec.deinit(gpa);

    const frame = try maskedFrame(gpa, op_ping, "hi");
    defer gpa.free(frame);
    try std.testing.expectEqual(FeedResult.open, try feed(&w, gpa, frame, &dec));
    try std.testing.expectEqual(@as(u8, 0x80 | op_pong), w.out.items[0]);
    try std.testing.expectEqualStrings("hi", w.out.items[2..4]);
}

test "feed rejects an unmasked frame with close 1002" {
    const gpa = std.testing.allocator;
    var w = Ws{};
    defer w.deinit(gpa);
    var dec: proto.Decoder = .{};
    defer dec.deinit(gpa);

    var frame = [_]u8{ 0x82, 0x01, 0x00 }; // binary, mask bit clear
    try std.testing.expectEqual(FeedResult.close, try feed(&w, gpa, &frame, &dec));
    try std.testing.expectEqual(@as(u8, 0x88), w.out.items[0]); // close frame
    try std.testing.expectEqual(@as(u8, 0x03), w.out.items[2]); // 1002 = 0x03EA
    try std.testing.expectEqual(@as(u8, 0xEA), w.out.items[3]);
}

test "feed rejects a text frame with close 1003" {
    const gpa = std.testing.allocator;
    var w = Ws{};
    defer w.deinit(gpa);
    var dec: proto.Decoder = .{};
    defer dec.deinit(gpa);

    const frame = try maskedFrame(gpa, op_text, "x");
    defer gpa.free(frame);
    try std.testing.expectEqual(FeedResult.close, try feed(&w, gpa, frame, &dec));
    try std.testing.expectEqual(@as(u8, 0x88), w.out.items[0]);
    try std.testing.expectEqual(@as(u8, 0xEB), w.out.items[3]); // 1003 = 0x03EB
}
