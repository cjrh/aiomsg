//! The bind end of the two-process demo: publishes a counter once a second.
//!
//!     zig build run-server     # this program
//!     zig build run-client     # in another terminal
//!
//! std.process.Init hands us the io backend, allocator, and args.

const std = @import("std");
const aiomsg = @import("aiomsg");

pub fn main(init: std.process.Init) !void {
    const sock = try aiomsg.Socket.init(init.gpa, init.io, .{ .mode = .publish });
    defer sock.deinit();

    try sock.bind("127.0.0.1", 25000);

    var i: usize = 0;
    while (true) : (i += 1) {
        var buf: [64]u8 = undefined;
        // send() copies the bytes, so the stack buffer is safe to reuse.
        sock.send(std.fmt.bufPrint(&buf, "tick {d}", .{i}) catch unreachable);
        init.io.sleep(std.Io.Duration.fromMilliseconds(1000), .boot) catch {};
    }
}
