#include "protocol.hpp"

#include <cstring>

namespace aiomsg::proto {

namespace {

void put_u32_be(Bytes& b, uint32_t v) {
    b.push_back(static_cast<uint8_t>(v >> 24));
    b.push_back(static_cast<uint8_t>(v >> 16));
    b.push_back(static_cast<uint8_t>(v >> 8));
    b.push_back(static_cast<uint8_t>(v));
}

uint32_t read_u32_be(const uint8_t* p) {
    return (static_cast<uint32_t>(p[0]) << 24) | (static_cast<uint32_t>(p[1]) << 16) |
           (static_cast<uint32_t>(p[2]) << 8) | static_cast<uint32_t>(p[3]);
}

// Build [u32 length][type][body...] from already-assembled body bytes.
Bytes frame(MsgType type, const uint8_t* body, size_t body_len) {
    Bytes out;
    uint32_t env_len = static_cast<uint32_t>(1 + body_len);
    out.reserve(4 + env_len);
    put_u32_be(out, env_len);
    out.push_back(static_cast<uint8_t>(type));
    if (body_len) {
        out.insert(out.end(), body, body + body_len);
    }
    return out;
}

}  // namespace

Bytes frame_hello(const Identity& identity) {
    Bytes body;
    body.push_back(kProtocolVersion);
    body.insert(body.end(), identity.begin(), identity.end());
    return frame(MsgType::Hello, body.data(), body.size());
}

Bytes frame_heartbeat() { return frame(MsgType::Heartbeat, nullptr, 0); }

Bytes frame_data(const uint8_t* payload, size_t len) {
    return frame(MsgType::Data, payload, len);
}

Bytes frame_data_req(const MsgId& msg_id, const uint8_t* payload, size_t len) {
    Bytes body;
    body.insert(body.end(), msg_id.begin(), msg_id.end());
    if (len) {
        body.insert(body.end(), payload, payload + len);
    }
    return frame(MsgType::DataReq, body.data(), body.size());
}

Bytes frame_ack(const MsgId& msg_id) {
    return frame(MsgType::Ack, msg_id.data(), msg_id.size());
}

std::optional<Envelope> parse_envelope(const uint8_t* env, size_t len) {
    if (len < 1) {
        return std::nullopt;
    }
    Envelope e;
    e.type = static_cast<MsgType>(env[0]);
    const uint8_t* body = env + 1;
    size_t body_len = len - 1;
    switch (e.type) {
        case MsgType::Hello:
            if (body_len < 1 + kIdentitySize) {
                return std::nullopt;
            }
            e.version = body[0];
            e.identity = body + 1;
            return e;
        case MsgType::Heartbeat:
            return e;
        case MsgType::Data:
            e.payload = body;
            e.payload_len = body_len;
            return e;
        case MsgType::DataReq:
            if (body_len < kMsgIdSize) {
                return std::nullopt;
            }
            e.msg_id = body;
            e.payload = body + kMsgIdSize;
            e.payload_len = body_len - kMsgIdSize;
            return e;
        case MsgType::Ack:
            if (body_len < kMsgIdSize) {
                return std::nullopt;
            }
            e.msg_id = body;
            return e;
        default:
            return std::nullopt;
    }
}

void Decoder::push(const uint8_t* bytes, size_t n) {
    // Reclaim already-consumed bytes lazily so the buffer doesn't grow without
    // bound on a long-lived connection.
    if (head_ > 0 && head_ == buf_.size()) {
        buf_.clear();
        head_ = 0;
    } else if (head_ > 4096 && head_ * 2 >= buf_.size()) {
        buf_.erase(buf_.begin(), buf_.begin() + static_cast<long>(head_));
        head_ = 0;
    }
    buf_.insert(buf_.end(), bytes, bytes + n);
}

bool Decoder::pop(const uint8_t** out, size_t* out_len) {
    size_t avail = buf_.size() - head_;
    if (avail < 4) {
        return false;
    }
    const uint8_t* p = buf_.data() + head_;
    uint32_t env_len = read_u32_be(p);
    if (avail < 4 + static_cast<size_t>(env_len)) {
        return false;
    }
    *out = p + 4;
    *out_len = env_len;
    head_ += 4 + env_len;
    return true;
}

}  // namespace aiomsg::proto
