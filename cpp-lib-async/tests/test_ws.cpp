// Unit tests for the pure WebSocket layer (ws.hpp): the RFC 6455 handshake and
// the client-frame parser / server-frame encoder. No sockets.
#include "ws.hpp"

#include <cassert>
#include <cstdio>
#include <cstring>
#include <string>

using namespace aiomsg::ws;

// Build a masked client->server frame (all client frames must be masked).
static Bytes client_frame(uint8_t opcode, const std::string& payload, bool fin = true) {
    const uint8_t mask[4] = {0xa1, 0xb2, 0xc3, 0xd4};
    Bytes f;
    f.push_back(static_cast<uint8_t>((fin ? 0x80 : 0) | opcode));
    size_t n = payload.size();
    if (n < 126) {
        f.push_back(static_cast<uint8_t>(0x80 | n));
    } else if (n < 65536) {
        f.push_back(0x80 | 126);
        f.push_back(static_cast<uint8_t>((n >> 8) & 0xff));
        f.push_back(static_cast<uint8_t>(n & 0xff));
    } else {
        f.push_back(0x80 | 127);
        for (int s = 56; s >= 0; s -= 8) f.push_back(static_cast<uint8_t>((n >> s) & 0xff));
    }
    for (int i = 0; i < 4; i++) f.push_back(mask[i]);
    for (size_t i = 0; i < n; i++) f.push_back(static_cast<uint8_t>(payload[i]) ^ mask[i & 3]);
    return f;
}

static std::string as_string(const Bytes& b) {
    return std::string(reinterpret_cast<const char*>(b.data()), b.size());
}

static void test_accept_vector() {
    assert(compute_accept("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

static void test_parse_upgrade_valid() {
    std::string req =
        "GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n"
        "Connection: keep-alive, Upgrade\r\n"
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    auto r = parse_upgrade(req);
    assert(r.ok && r.key == "dGhlIHNhbXBsZSBub25jZQ==");
}

static void test_parse_upgrade_rejects() {
    auto post = parse_upgrade(
        "POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n");
    assert(!post.ok && post.status.rfind("400", 0) == 0);

    auto ver = parse_upgrade(
        "GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n");
    assert(!ver.ok && ver.status.rfind("426", 0) == 0);

    auto nokey = parse_upgrade(
        "GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
        "Sec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n");
    assert(!nokey.ok && nokey.status.rfind("400", 0) == 0);
}

static void test_masked_binary_unmasked() {
    FrameParser p;
    auto f = client_frame(kOpBin, "aiomsg-payload");
    p.feed(f.data(), f.size());
    auto r = p.next();
    assert(r.event == Event::Binary && as_string(r.payload) == "aiomsg-payload");
    assert(p.next().event == Event::Need);
}

static void test_fragmented_binary() {
    FrameParser p;
    auto a = client_frame(kOpBin, "abc", false);
    auto b = client_frame(kOpCont, "def", false);
    auto c = client_frame(kOpCont, "ghi", true);
    p.feed(a.data(), a.size());
    p.feed(b.data(), b.size());
    p.feed(c.data(), c.size());
    std::string all;
    for (int i = 0; i < 3; i++) {
        auto r = p.next();
        assert(r.event == Event::Binary);
        all += as_string(r.payload);
    }
    assert(all == "abcdefghi");
}

static void test_ping_pong_then_data() {
    FrameParser p;
    auto ping = client_frame(kOpPing, "pingdata");
    auto data = client_frame(kOpBin, "XY");
    p.feed(ping.data(), ping.size());
    p.feed(data.data(), data.size());
    auto r1 = p.next();
    assert(r1.event == Event::Ping && as_string(r1.payload) == "pingdata");
    auto r2 = p.next();
    assert(r2.event == Event::Binary && as_string(r2.payload) == "XY");
}

static void test_close_reports_code() {
    FrameParser p;
    std::string body;
    body.push_back(0x03);
    body.push_back(static_cast<char>(0xe8));  // 1000
    auto f = client_frame(kOpClose, body);
    p.feed(f.data(), f.size());
    auto r = p.next();
    assert(r.event == Event::Close && r.code == 1000);
}

static void test_text_rejected_1003() {
    FrameParser p;
    auto f = client_frame(kOpText, "hi");
    p.feed(f.data(), f.size());
    auto r = p.next();
    assert(r.event == Event::ProtocolError && r.code == 1003);
}

static void test_unmasked_rejected_1002() {
    FrameParser p;
    // Unmasked binary frame (MASK bit clear) — illegal from a client.
    Bytes f = {0x82, 0x04, 'n', 'o', 'p', 'e'};
    p.feed(f.data(), f.size());
    auto r = p.next();
    assert(r.event == Event::ProtocolError && r.code == 1002);
}

static void test_server_frame_roundtrip() {
    std::string payload = "\x00\x00\x00\x03""abc";
    auto f = server_frame(kOpBin, reinterpret_cast<const uint8_t*>(payload.data()),
                          payload.size());
    assert(f[0] == static_cast<uint8_t>(0x80 | kOpBin));
    assert(f[1] == payload.size());  // unmasked, short length
    assert(std::memcmp(f.data() + 2, payload.data(), payload.size()) == 0);
}

int main() {
    test_accept_vector();
    test_parse_upgrade_valid();
    test_parse_upgrade_rejects();
    test_masked_binary_unmasked();
    test_fragmented_binary();
    test_ping_pong_then_data();
    test_close_reports_code();
    test_text_rejected_1003();
    test_unmasked_rejected_1002();
    test_server_frame_roundtrip();
    std::printf("ws: all tests passed\n");
    return 0;
}
