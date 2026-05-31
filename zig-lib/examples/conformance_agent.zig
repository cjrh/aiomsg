//! Conformance test agent for the cross-language interop suite (Zig).
//! Same CLI as every other agent. Creates a std.Io.Threaded backend, drives a
//! blocking Socket; a sink prints each received message on its own line and
//! exits after --count messages.

const std = @import("std");
const aiomsg = @import("aiomsg");
const c = @cImport(@cInclude("unistd.h"));

fn arg(args: []const [:0]const u8, key: []const u8, default: []const u8) []const u8 {
    var i: usize = 1;
    while (i + 1 < args.len) : (i += 1) {
        if (std.mem.startsWith(u8, args[i], "--") and std.mem.eql(u8, args[i][2..], key)) {
            return args[i + 1];
        }
    }
    return default;
}

fn parseIdentity(hex: []const u8) aiomsg.Identity {
    var id: aiomsg.Identity = std.mem.zeroes(aiomsg.Identity);
    var i: usize = 0;
    while (i < id.len and i * 2 + 1 < hex.len) : (i += 1) {
        id[i] = std.fmt.parseInt(u8, hex[i * 2 .. i * 2 + 2], 16) catch 0;
    }
    return id;
}

pub fn main(init: std.process.Init) !void {
    const gpa = init.gpa;
    const io = init.io;
    const args = try init.minimal.args.toSlice(init.arena.allocator());

    const role = arg(args, "role", "connect");
    const host = arg(args, "host", "127.0.0.1");
    const port = std.fmt.parseInt(u16, arg(args, "port", "25000"), 10) catch 25000;
    const send_mode = arg(args, "send-mode", "roundrobin");
    const behavior = arg(args, "behavior", "sink");
    const count = std.fmt.parseInt(usize, arg(args, "count", "10"), 10) catch 10;
    const prefix = arg(args, "prefix", "m");
    const delivery = arg(args, "delivery", "at-most-once");
    const identity_hex = arg(args, "identity", "");
    const linger = std.fmt.parseFloat(f64, arg(args, "linger", "1.0")) catch 1.0;
    const tls = std.mem.eql(u8, arg(args, "tls", "false"), "true");
    const tls_cert = arg(args, "tls-cert", "");
    const tls_key = arg(args, "tls-key", "");
    const tls_ca = arg(args, "tls-ca", "");
    const server_name = arg(args, "tls-server-name", "");

    var opts: aiomsg.Options = .{
        .mode = if (std.mem.eql(u8, send_mode, "publish")) .publish else .round_robin,
        .delivery = if (std.mem.eql(u8, delivery, "at-least-once")) .at_least_once else .at_most_once,
    };
    if (identity_hex.len > 0) opts.identity = parseIdentity(identity_hex);

    const sock = try aiomsg.Socket.init(gpa, io, opts);
    defer sock.deinit();

    if (std.mem.eql(u8, role, "bind")) {
        if (tls) {
            try sock.bindTls(host, port, .{ .cert_path = tls_cert, .key_path = tls_key });
        } else {
            try sock.bind(host, port);
        }
    } else {
        if (tls) {
            try sock.connectTls(host, port, .{ .ca_path = tls_ca, .server_name = server_name });
        } else {
            try sock.connect(host, port);
        }
    }

    if (std.mem.eql(u8, behavior, "source")) {
        var i: usize = 0;
        while (i < count) : (i += 1) {
            var buf: [64]u8 = undefined;
            sock.send(std.fmt.bufPrint(&buf, "{s}{d}", .{ prefix, i }) catch unreachable);
        }
        io.sleep(std.Io.Duration.fromMilliseconds(@intFromFloat(linger * 1000.0)), .boot) catch {};
    } else if (std.mem.eql(u8, behavior, "echo")) {
        var i: usize = 0;
        while (i < count) : (i += 1) {
            const m = sock.recv() orelse break;
            defer gpa.free(m.data);
            sock.sendTo(m.sender, m.data);
        }
        io.sleep(std.Io.Duration.fromMilliseconds(@intFromFloat(linger * 1000.0)), .boot) catch {};
    } else { // sink
        var i: usize = 0;
        while (i < count) : (i += 1) {
            const m = sock.recv() orelse break;
            defer gpa.free(m.data);
            var line: [4096]u8 = undefined;
            const out = std.fmt.bufPrint(&line, "{s}\n", .{m.data}) catch continue;
            _ = c.write(1, out.ptr, out.len);
        }
    }
}
