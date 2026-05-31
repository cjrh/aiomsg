// Unit tests for the framing / envelope / decoder layer (no sockets).
#include "protocol.hpp"

#include <cassert>
#include <cstdio>
#include <cstring>
#include <string>

using namespace aiomsg::proto;

static void test_data_roundtrip() {
    std::string payload = "hello world";
    Bytes framed = frame_data(reinterpret_cast<const uint8_t*>(payload.data()),
                              payload.size());
    Decoder dec;
    dec.push(framed.data(), framed.size());
    const uint8_t* env;
    size_t env_len;
    assert(dec.pop(&env, &env_len));
    auto e = parse_envelope(env, env_len);
    assert(e && e->type == MsgType::Data);
    assert(std::string(reinterpret_cast<const char*>(e->payload), e->payload_len) == payload);
    assert(!dec.pop(&env, &env_len));  // nothing left
}

static void test_hello_and_data_req() {
    Identity wantid{};
    for (size_t i = 0; i < wantid.size(); i++) {
        wantid[i] = static_cast<uint8_t>(i + 1);
    }
    Bytes hello = frame_hello(wantid);
    Decoder dec;
    dec.push(hello.data(), hello.size());
    const uint8_t* env;
    size_t env_len;
    assert(dec.pop(&env, &env_len));
    auto e = parse_envelope(env, env_len);
    assert(e && e->type == MsgType::Hello && e->version == kProtocolVersion);
    assert(std::memcmp(e->identity, wantid.data(), kIdentitySize) == 0);

    MsgId mid{};
    for (size_t i = 0; i < mid.size(); i++) {
        mid[i] = static_cast<uint8_t>(0xA0 + i);
    }
    std::string p = "body";
    Bytes req = frame_data_req(mid, reinterpret_cast<const uint8_t*>(p.data()), p.size());
    Bytes ack = frame_ack(mid);
    Decoder d2;
    d2.push(req.data(), req.size());
    d2.push(ack.data(), ack.size());
    assert(d2.pop(&env, &env_len));
    auto er = parse_envelope(env, env_len);
    assert(er && er->type == MsgType::DataReq);
    assert(std::memcmp(er->msg_id, mid.data(), kMsgIdSize) == 0);
    assert(std::string(reinterpret_cast<const char*>(er->payload), er->payload_len) == p);
    assert(d2.pop(&env, &env_len));
    auto ea = parse_envelope(env, env_len);
    assert(ea && ea->type == MsgType::Ack);
    assert(std::memcmp(ea->msg_id, mid.data(), kMsgIdSize) == 0);
}

static void test_partial_reads() {
    std::string payload = "fragmented";
    Bytes framed = frame_data(reinterpret_cast<const uint8_t*>(payload.data()),
                              payload.size());
    Decoder dec;
    const uint8_t* env;
    size_t env_len;
    // Feed one byte at a time; only the final byte completes the frame.
    for (size_t i = 0; i < framed.size(); i++) {
        dec.push(&framed[i], 1);
        if (i + 1 < framed.size()) {
            assert(!dec.pop(&env, &env_len));
        }
    }
    assert(dec.pop(&env, &env_len));
    auto e = parse_envelope(env, env_len);
    assert(e && e->type == MsgType::Data);
    assert(std::string(reinterpret_cast<const char*>(e->payload), e->payload_len) == payload);
}

static void test_bad_envelopes() {
    assert(!parse_envelope(nullptr, 0));  // empty
    uint8_t hello_short[2] = {0x01, 0x01};  // HELLO with no identity
    assert(!parse_envelope(hello_short, sizeof(hello_short)));
    uint8_t unknown[1] = {0x7f};
    assert(!parse_envelope(unknown, sizeof(unknown)));
    uint8_t ack_short[3] = {0x05, 0x00, 0x00};  // ACK with truncated msg_id
    assert(!parse_envelope(ack_short, sizeof(ack_short)));
}

int main() {
    test_data_roundtrip();
    test_hello_and_data_req();
    test_partial_reads();
    test_bad_envelopes();
    std::printf("protocol tests passed\n");
    return 0;
}
