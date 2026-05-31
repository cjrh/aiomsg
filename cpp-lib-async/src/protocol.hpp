// Wire protocol: framing and typed envelopes (the C++ counterpart of PROTOCOL.md).
//
// Pure data — no sockets or threads — so it can be unit-tested in isolation.
// Frames on the wire are [u32 big-endian length][envelope]; an envelope is
// [u8 type][body]. The frame_* helpers return a ready-to-write byte buffer
// (length prefix + envelope); the Decoder reassembles frames from a byte stream
// that may arrive in arbitrary chunks.
#ifndef AIOMSG_PROTOCOL_HPP
#define AIOMSG_PROTOCOL_HPP

#include <array>
#include <cstdint>
#include <optional>
#include <vector>

namespace aiomsg::proto {

inline constexpr uint8_t kProtocolVersion = 1;
inline constexpr size_t kIdentitySize = 16;
inline constexpr size_t kMsgIdSize = 16;

using Bytes = std::vector<uint8_t>;
using Identity = std::array<uint8_t, kIdentitySize>;
using MsgId = std::array<uint8_t, kMsgIdSize>;

enum class MsgType : uint8_t {
    Hello = 0x01,
    Heartbeat = 0x02,
    Data = 0x03,
    DataReq = 0x04,
    Ack = 0x05,
};

// A decoded envelope. The byte views (payload/identity/msg_id) point into the
// buffer handed to parse_envelope and are valid only as long as it is.
struct Envelope {
    MsgType type{};
    uint8_t version = 0;            // Hello only
    const uint8_t* identity = nullptr;  // Hello only, 16 bytes
    const uint8_t* msg_id = nullptr;    // DataReq / Ack only, 16 bytes
    const uint8_t* payload = nullptr;   // Data / DataReq
    size_t payload_len = 0;
};

// Framed-message constructors: each returns a buffer of [length prefix][envelope].
Bytes frame_hello(const Identity& identity);
Bytes frame_heartbeat();
Bytes frame_data(const uint8_t* payload, size_t len);
Bytes frame_data_req(const MsgId& msg_id, const uint8_t* payload, size_t len);
Bytes frame_ack(const MsgId& msg_id);

// Parse one envelope (no length prefix). Returns nullopt if empty, truncated,
// or an unknown type.
std::optional<Envelope> parse_envelope(const uint8_t* env, size_t len);

// Incremental frame reassembler: push arbitrary bytes, pop complete envelopes.
// Tolerates a frame split across several push() calls. A popped envelope view
// is valid until the next push().
class Decoder {
public:
    void push(const uint8_t* bytes, size_t n);

    // If a complete frame is buffered, set out/out_len to its envelope and
    // return true; otherwise return false. The view is valid until next push().
    bool pop(const uint8_t** out, size_t* out_len);

private:
    Bytes buf_;
    size_t head_ = 0;  // read position; bytes before head_ are consumed
};

}  // namespace aiomsg::proto

#endif  // AIOMSG_PROTOCOL_HPP
