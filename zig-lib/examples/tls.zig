//! Self-contained TLS demo: a bind socket and a connect socket talking over
//! OpenSSL in one process.
//!
//!     zig build run-tls            # cert dir defaults to ../conformance/certs
//!     zig build run-tls -- /path/to/certs
//!
//! It loads the repository's shared self-signed certificate (EC P-256, with
//! SANs for "localhost" and 127.0.0.1). A real deployment points cert/key at
//! its own PEM files and trusts the issuing CA via the ca path. The TCP target
//! and the verified name are independent: we dial 127.0.0.1 but verify
//! "localhost".

const std = @import("std");
const aiomsg = @import("aiomsg");
const c = @cImport(@cInclude("unistd.h"));

pub fn main(init: std.process.Init) !void {
    const gpa = init.gpa;
    const io = init.io;
    const args = try init.minimal.args.toSlice(init.arena.allocator());
    const dir = if (args.len > 1) args[1] else "../conformance/certs";

    var cert_buf: [512]u8 = undefined;
    var key_buf: [512]u8 = undefined;
    const cert = std.fmt.bufPrint(&cert_buf, "{s}/cert.pem", .{dir}) catch unreachable;
    const key = std.fmt.bufPrint(&key_buf, "{s}/key.pem", .{dir}) catch unreachable;

    const server = try aiomsg.Socket.init(gpa, io, .{ .mode = .publish });
    defer server.deinit();
    try server.bindTls("127.0.0.1", 25001, .{ .cert_path = cert, .key_path = key });

    const client = try aiomsg.Socket.init(gpa, io, .{});
    defer client.deinit();
    try client.connectTls("127.0.0.1", 25001, .{ .ca_path = cert, .server_name = "localhost" });

    server.send("hello over TLS");

    if (client.recv()) |m| {
        defer gpa.free(m.data);
        var line: [4096]u8 = undefined;
        const out = std.fmt.bufPrint(&line, "client received: {s}\n", .{m.data}) catch return;
        _ = c.write(1, out.ptr, out.len);
    }
}
