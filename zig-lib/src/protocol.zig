//! Wire protocol: framing and typed envelopes (the Zig counterpart of PROTOCOL.md).
//!
//! Pure data — no sockets, no threads — so it can be unit-tested in isolation.
//! Frames on the wire are [u32 big-endian length][envelope]; an envelope is
//! [u8 type][body]. The `frame*` helpers return an allocator-owned buffer
//! (length prefix + envelope) ready to write to the wire; `Decoder` reassembles
//! frames from a byte stream that may arrive in arbitrary chunks.

const std = @import("std");
const Allocator = std.mem.Allocator;

pub const protocol_version: u8 = 1;
pub const identity_size: usize = 16;
pub const msg_id_size: usize = 16;

pub const Identity = [identity_size]u8;
pub const MsgId = [msg_id_size]u8;

pub const MsgType = enum(u8) {
    hello = 0x01,
    heartbeat = 0x02,
    data = 0x03,
    data_req = 0x04,
    ack = 0x05,
    _,
};

/// A decoded envelope. The slices (identity/msg_id/payload) point into the
/// buffer passed to `parseEnvelope`, valid only as long as it is.
pub const Envelope = struct {
    type: MsgType,
    version: u8 = 0, // hello only
    identity: ?[]const u8 = null, // hello only, 16 bytes
    msg_id: ?[]const u8 = null, // data_req / ack only, 16 bytes
    payload: []const u8 = &.{}, // data / data_req
};

/// Build [u32 length][type][body] in a freshly allocated buffer.
fn frame(gpa: Allocator, t: MsgType, body: []const u8) Allocator.Error![]u8 {
    const env_len: u32 = @intCast(1 + body.len);
    var out = try gpa.alloc(u8, 4 + env_len);
    std.mem.writeInt(u32, out[0..4], env_len, .big);
    out[4] = @intFromEnum(t);
    @memcpy(out[5..][0..body.len], body);
    return out;
}

pub fn frameHello(gpa: Allocator, identity: *const Identity) Allocator.Error![]u8 {
    var body: [1 + identity_size]u8 = undefined;
    body[0] = protocol_version;
    @memcpy(body[1..], identity);
    return frame(gpa, .hello, &body);
}

pub fn frameHeartbeat(gpa: Allocator) Allocator.Error![]u8 {
    return frame(gpa, .heartbeat, &.{});
}

pub fn frameData(gpa: Allocator, payload: []const u8) Allocator.Error![]u8 {
    return frame(gpa, .data, payload);
}

pub fn frameDataReq(gpa: Allocator, msg_id: *const MsgId, payload: []const u8) Allocator.Error![]u8 {
    const body = try gpa.alloc(u8, msg_id_size + payload.len);
    defer gpa.free(body);
    @memcpy(body[0..msg_id_size], msg_id);
    @memcpy(body[msg_id_size..], payload);
    return frame(gpa, .data_req, body);
}

pub fn frameAck(gpa: Allocator, msg_id: *const MsgId) Allocator.Error![]u8 {
    return frame(gpa, .ack, msg_id);
}

/// Parse one envelope (no length prefix). Returns null if empty, truncated, or
/// an unknown type.
pub fn parseEnvelope(env: []const u8) ?Envelope {
    if (env.len < 1) return null;
    const t: MsgType = @enumFromInt(env[0]);
    const body = env[1..];
    switch (t) {
        .hello => {
            if (body.len < 1 + identity_size) return null;
            return .{ .type = t, .version = body[0], .identity = body[1 .. 1 + identity_size] };
        },
        .heartbeat => return .{ .type = t },
        .data => return .{ .type = t, .payload = body },
        .data_req => {
            if (body.len < msg_id_size) return null;
            return .{ .type = t, .msg_id = body[0..msg_id_size], .payload = body[msg_id_size..] };
        },
        .ack => {
            if (body.len < msg_id_size) return null;
            return .{ .type = t, .msg_id = body[0..msg_id_size] };
        },
        _ => return null,
    }
}

/// Incremental frame reassembler: push arbitrary bytes, pop complete envelopes.
/// Tolerates a frame split across several `push` calls. A popped envelope slice
/// is valid until the next `push`.
pub const Decoder = struct {
    buf: std.ArrayList(u8) = .empty,
    head: usize = 0, // consumed bytes before this offset

    pub fn deinit(self: *Decoder, gpa: Allocator) void {
        self.buf.deinit(gpa);
    }

    pub fn push(self: *Decoder, gpa: Allocator, bytes: []const u8) Allocator.Error!void {
        // Reclaim consumed bytes lazily so the buffer doesn't grow unbounded.
        if (self.head > 0 and self.head == self.buf.items.len) {
            self.buf.clearRetainingCapacity();
            self.head = 0;
        } else if (self.head > 4096 and self.head * 2 >= self.buf.items.len) {
            const remaining = self.buf.items.len - self.head;
            std.mem.copyForwards(u8, self.buf.items[0..remaining], self.buf.items[self.head..]);
            self.buf.shrinkRetainingCapacity(remaining);
            self.head = 0;
        }
        try self.buf.appendSlice(gpa, bytes);
    }

    /// If a complete frame is buffered, return its envelope slice (valid until
    /// the next `push`); otherwise null.
    pub fn pop(self: *Decoder) ?[]const u8 {
        const avail = self.buf.items[self.head..];
        if (avail.len < 4) return null;
        const env_len = std.mem.readInt(u32, avail[0..4], .big);
        if (avail.len < 4 + env_len) return null;
        const env = avail[4 .. 4 + env_len];
        self.head += 4 + env_len;
        return env;
    }
};

// --- tests -----------------------------------------------------------------

test "data frame roundtrip" {
    const gpa = std.testing.allocator;
    const framed = try frameData(gpa, "hello world");
    defer gpa.free(framed);
    var dec: Decoder = .{};
    defer dec.deinit(gpa);
    try dec.push(gpa, framed);
    const env = dec.pop().?;
    const e = parseEnvelope(env).?;
    try std.testing.expectEqual(MsgType.data, e.type);
    try std.testing.expectEqualStrings("hello world", e.payload);
    try std.testing.expect(dec.pop() == null);
}

test "hello and data_req/ack" {
    const gpa = std.testing.allocator;
    var id: Identity = undefined;
    for (&id, 0..) |*b, i| b.* = @intCast(i + 1);
    const hello = try frameHello(gpa, &id);
    defer gpa.free(hello);
    var dec: Decoder = .{};
    defer dec.deinit(gpa);
    try dec.push(gpa, hello);
    const e = parseEnvelope(dec.pop().?).?;
    try std.testing.expectEqual(MsgType.hello, e.type);
    try std.testing.expectEqual(protocol_version, e.version);
    try std.testing.expectEqualSlices(u8, &id, e.identity.?);

    var mid: MsgId = undefined;
    for (&mid, 0..) |*b, i| b.* = @intCast(0xA0 + i);
    const req = try frameDataReq(gpa, &mid, "body");
    defer gpa.free(req);
    const ack = try frameAck(gpa, &mid);
    defer gpa.free(ack);
    var d2: Decoder = .{};
    defer d2.deinit(gpa);
    try d2.push(gpa, req);
    try d2.push(gpa, ack);
    const er = parseEnvelope(d2.pop().?).?;
    try std.testing.expectEqual(MsgType.data_req, er.type);
    try std.testing.expectEqualSlices(u8, &mid, er.msg_id.?);
    try std.testing.expectEqualStrings("body", er.payload);
    const ea = parseEnvelope(d2.pop().?).?;
    try std.testing.expectEqual(MsgType.ack, ea.type);
    try std.testing.expectEqualSlices(u8, &mid, ea.msg_id.?);
}

test "partial reads (byte at a time)" {
    const gpa = std.testing.allocator;
    const framed = try frameData(gpa, "fragmented");
    defer gpa.free(framed);
    var dec: Decoder = .{};
    defer dec.deinit(gpa);
    for (framed, 0..) |b, i| {
        try dec.push(gpa, &.{b});
        if (i + 1 < framed.len) try std.testing.expect(dec.pop() == null);
    }
    const e = parseEnvelope(dec.pop().?).?;
    try std.testing.expectEqualStrings("fragmented", e.payload);
}

test "bad envelopes" {
    try std.testing.expect(parseEnvelope(&.{}) == null);
    try std.testing.expect(parseEnvelope(&.{ 0x01, 0x01 }) == null); // hello, no identity
    try std.testing.expect(parseEnvelope(&.{0x7f}) == null); // unknown type
    try std.testing.expect(parseEnvelope(&.{ 0x05, 0x00, 0x00 }) == null); // short ack
}
