//! The connect end of the two-process demo: prints whatever the server sends,
//! reconnecting automatically if the server restarts.
//!
//!     zig build run-server     # in another terminal
//!     zig build run-client     # this program

const std = @import("std");
const aiomsg = @import("aiomsg");
// std.fs.File stdout is gone in 0.16; write(2) directly, as the agent does.
const c = @cImport(@cInclude("unistd.h"));

pub fn main(init: std.process.Init) !void {
    const sock = try aiomsg.Socket.init(init.gpa, init.io, .{});
    defer sock.deinit();

    try sock.connect("127.0.0.1", 25000);

    // recv() returns null only once the socket is closed and drained.
    while (sock.recv()) |m| {
        defer init.gpa.free(m.data); // caller owns the payload
        var line: [4096]u8 = undefined;
        const out = std.fmt.bufPrint(&line, "{s}\n", .{m.data}) catch continue;
        _ = c.write(1, out.ptr, out.len);
    }
}
