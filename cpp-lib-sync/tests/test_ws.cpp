// Unit tests for the server-side WebSocket adapter (ws.cpp, PROTOCOL.md §10).
// Driven over in-memory RawRead/RawWrite callbacks — no sockets.
#include "ws.hpp"

#include <cassert>
#include <cstdio>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

using namespace aiomsg;

namespace {

// A RawRead that serves a fixed byte string, then reports timeout (0).
ws::RawRead reader_over(std::shared_ptr<std::vector<uint8_t>> data,
                        std::shared_ptr<size_t> pos) {
    return [data, pos](uint8_t* buf, size_t cap) -> int {
        size_t avail = data->size() - *pos;
        if (avail == 0) return 0;
        size_t n = std::min(avail, cap);
        std::memcpy(buf, data->data() + *pos, n);
        *pos += n;
        return static_cast<int>(n);
    };
}

// A RawWrite that appends everything to a capture buffer.
ws::RawWrite writer_into(std::shared_ptr<std::vector<uint8_t>> out) {
    return [out](const uint8_t* buf, size_t len) -> int {
        out->insert(out->end(), buf, buf + len);
        return 0;
    };
}

// Encode a masked client->server frame.
std::vector<uint8_t> client_frame(uint8_t opcode, const std::string& payload, bool fin = true) {
    const uint8_t mask[4] = {0xa1, 0xb2, 0xc3, 0xd4};
    std::vector<uint8_t> f;
    f.push_back((fin ? 0x80 : 0x00) | opcode);
    size_t n = payload.size();
    if (n < 126) {
        f.push_back(0x80 | static_cast<uint8_t>(n));
    } else if (n < 65536) {
        f.push_back(0x80 | 126);
        f.push_back((n >> 8) & 0xff);
        f.push_back(n & 0xff);
    } else {
        f.push_back(0x80 | 127);
        for (int i = 7; i >= 0; --i) f.push_back((static_cast<uint64_t>(n) >> (8 * i)) & 0xff);
    }
    f.insert(f.end(), mask, mask + 4);
    for (size_t i = 0; i < n; ++i) f.push_back(payload[i] ^ mask[i & 3]);
    return f;
}

void append(std::vector<uint8_t>& v, const std::vector<uint8_t>& w) {
    v.insert(v.end(), w.begin(), w.end());
}

std::string str(const std::vector<uint8_t>& v) { return std::string(v.begin(), v.end()); }

// --- tests ---

void test_accept_key_vector() {
    assert(ws::accept_key("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

// Drive a full upgrade over `frames` bytes trailing the request; return the
// adapter state, the collected stream bytes, and everything written.
struct Driven {
    ws::State st;
    std::vector<uint8_t> out;      // decoded byte stream
    std::vector<uint8_t> written;  // server->client bytes
    bool closed = false;
};

const char* kRequest =
    "GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n"
    "Connection: keep-alive, Upgrade\r\n"
    "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";

Driven drive(const std::vector<uint8_t>& frames) {
    Driven d;
    auto data = std::make_shared<std::vector<uint8_t>>();
    data->insert(data->end(), kRequest, kRequest + std::strlen(kRequest));
    data->insert(data->end(), frames.begin(), frames.end());
    auto pos = std::make_shared<size_t>(0);
    auto out_written = std::make_shared<std::vector<uint8_t>>();
    auto rr = reader_over(data, pos);
    auto rw = writer_into(out_written);

    std::vector<uint8_t> prefix;
    ws::Sniff s = ws::sniff_and_upgrade(rr, rw, d.st, prefix, std::chrono::milliseconds(1000));
    assert(s == ws::Sniff::WebSocket);
    // Drain frames.
    for (;;) {
        int r = ws::read(d.st, rr, rw, d.out);
        if (r <= 0) {
            d.closed = (r < 0);
            break;
        }
    }
    d.written = *out_written;
    return d;
}

void test_upgrade_response() {
    auto d = drive(client_frame(0x2, "hello"));
    std::string resp = str(d.written).substr(0, str(d.written).find("\r\n\r\n") + 4);
    assert(resp.rfind("HTTP/1.1 101 Switching Protocols\r\n", 0) == 0);
    assert(resp.find("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n") != std::string::npos);
    assert(str(d.out) == "hello");
}

void test_masked_unmask() {
    auto d = drive(client_frame(0x2, "aiomsg-payload"));
    assert(str(d.out) == "aiomsg-payload");
}

void test_fragmented_reassembly() {
    std::vector<uint8_t> frames;
    append(frames, client_frame(0x2, "abc", false));
    append(frames, client_frame(0x0, "def", false));
    append(frames, client_frame(0x0, "ghi", true));
    auto d = drive(frames);
    assert(str(d.out) == "abcdefghi");
}

void test_ping_pong() {
    std::vector<uint8_t> frames;
    append(frames, client_frame(0x9, "pingdata"));
    append(frames, client_frame(0x2, "XY"));
    auto d = drive(frames);
    assert(str(d.out) == "XY");
    // A pong (opcode 0xA, unmasked) echoing the payload was written.
    auto pong = ws::encode_server_frame(0xA, reinterpret_cast<const uint8_t*>("pingdata"), 8);
    std::string w = str(d.written), p = str(pong);
    assert(w.find(p) != std::string::npos);
}

void test_close_echo() {
    uint8_t code[2] = {0x03, 0xe8};  // 1000
    std::vector<uint8_t> frames;
    // Build a masked close frame carrying the 1000 code.
    frames = client_frame(0x8, std::string(reinterpret_cast<char*>(code), 2));
    auto d = drive(frames);
    assert(d.closed);
    auto close = ws::encode_server_frame(0x8, code, 2);
    assert(str(d.written).find(str(close)) != std::string::npos);
}

void test_reject_text() {
    auto d = drive(client_frame(0x1, "hi"));
    assert(d.closed);
    uint8_t code[2] = {0x03, 0xeb};  // 1003
    auto close = ws::encode_server_frame(0x8, code, 2);
    assert(str(d.written).find(str(close)) != std::string::npos);
}

void test_reject_unmasked() {
    // Unmasked binary frame (MASK bit clear) — illegal from a client.
    std::vector<uint8_t> frame = {0x82, 0x04, 'n', 'o', 'p', 'e'};
    auto d = drive(frame);
    assert(d.closed);
    uint8_t code[2] = {0x03, 0xea};  // 1002
    auto close = ws::encode_server_frame(0x8, code, 2);
    assert(str(d.written).find(str(close)) != std::string::npos);
}

void test_reject_bad_upgrade() {
    ws::State st;
    std::vector<uint8_t> prefix;
    auto data = std::make_shared<std::vector<uint8_t>>();
    const char* bad = "GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n";  // no key/version
    data->insert(data->end(), bad, bad + std::strlen(bad));
    auto pos = std::make_shared<size_t>(0);
    auto out = std::make_shared<std::vector<uint8_t>>();
    ws::Sniff s = ws::sniff_and_upgrade(reader_over(data, pos), writer_into(out), st, prefix,
                                        std::chrono::milliseconds(1000));
    assert(s == ws::Sniff::Reject);
    assert(str(*out).rfind("HTTP/1.1 400 Bad Request\r\n", 0) == 0);
}

void test_sniff_raw() {
    ws::State st;
    std::vector<uint8_t> prefix;
    auto data = std::make_shared<std::vector<uint8_t>>(
        std::vector<uint8_t>{0x00, 0x00, 0x00, 0x12, 0x99});
    auto pos = std::make_shared<size_t>(0);
    auto out = std::make_shared<std::vector<uint8_t>>();
    ws::Sniff s = ws::sniff_and_upgrade(reader_over(data, pos), writer_into(out), st, prefix,
                                        std::chrono::milliseconds(1000));
    assert(s == ws::Sniff::Raw);
    assert((prefix == std::vector<uint8_t>{0x00, 0x00, 0x00, 0x12, 0x99}));
}

}  // namespace

int main() {
    test_accept_key_vector();
    test_upgrade_response();
    test_masked_unmask();
    test_fragmented_reassembly();
    test_ping_pong();
    test_close_echo();
    test_reject_text();
    test_reject_unmasked();
    test_reject_bad_upgrade();
    test_sniff_raw();
    std::printf("test_ws: all passed\n");
    return 0;
}
