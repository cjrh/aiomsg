// Server-side WebSocket (RFC 6455) support for aiomsg bind sockets — the pure,
// transport-agnostic half (see PROTOCOL.md §10 and WEBSOCKET-PLAN.md §2).
//
// This header has no Asio and no sockets: it is just the handshake computation
// and a stateful client-frame parser plus a server-frame encoder, so it can be
// unit-tested in isolation. socket.cpp layers the async glue (a WsTransport and
// the accept-time first-byte sniff) on top of these primitives.
//
// WebSocket message boundaries carry no meaning: the parser yields the payloads
// of binary messages, which the caller concatenates into the aiomsg byte stream
// (PROTOCOL.md §2). Only the server half of RFC 6455 is implemented; no
// subprotocol or extension is ever negotiated.
#ifndef AIOMSG_WS_HPP
#define AIOMSG_WS_HPP

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace aiomsg::ws {

using Bytes = std::vector<uint8_t>;

// Opcodes (RFC 6455 §5.2).
inline constexpr uint8_t kOpCont = 0x0;
inline constexpr uint8_t kOpText = 0x1;
inline constexpr uint8_t kOpBin = 0x2;
inline constexpr uint8_t kOpClose = 0x8;
inline constexpr uint8_t kOpPing = 0x9;
inline constexpr uint8_t kOpPong = 0xA;

inline constexpr size_t kMaxRequestBytes = 8192;  // pre-upgrade HTTP request cap

// base64(SHA1(key + GUID)) per RFC 6455 §4.2.2. Test vector: key
// "dGhlIHNhbXBsZSBub25jZQ==" -> "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".
std::string compute_accept(const std::string& key);

// Result of validating an HTTP upgrade request.
struct UpgradeResult {
    bool ok = false;
    std::string key;      // Sec-WebSocket-Key, when ok
    std::string status;   // HTTP status line to return when !ok ("400 Bad Request", "426 ...")
};

// Validate the request bytes (through the blank line). Host/Origin/subprotocol/
// extension headers are ignored — none is ever negotiated, and auth is out of
// scope (WEBSOCKET-PLAN.md §1.5).
UpgradeResult parse_upgrade(std::string_view request);

std::string success_response(const std::string& key);  // 101 Switching Protocols
std::string error_response(const std::string& status);  // 400 / 426

// Encode one server->client frame: FIN=1, RSV=0, unmasked, given opcode.
Bytes server_frame(uint8_t opcode, const uint8_t* payload, size_t len);

// What a single parsed client frame means to the transport layer.
enum class Event {
    Need,           // incomplete: feed more bytes and retry
    Binary,         // data payload available in `payload` (append to the stream)
    Ping,           // control ping: reply with a pong carrying `payload`
    Pong,           // control pong: ignore
    Close,          // peer close: echo close (with `code`) then EOF
    ProtocolError,  // fatal: send close(`code`) then drop the connection
};

// Stateful parser for masked client->server frames arriving as a byte stream.
// feed() appends raw bytes; next() consumes one complete frame and reports what
// it was. Fragmentation and frames split across feeds are handled transparently
// (Binary covers both 0x2 and 0x0 continuation).
class FrameParser {
public:
    void feed(const uint8_t* data, size_t len);

    struct Result {
        Event event = Event::Need;
        Bytes payload;      // unmasked payload for Binary/Ping/Pong
        uint16_t code = 0;  // close/error status code for Close/ProtocolError
    };
    Result next();

private:
    Bytes buf_;
    size_t pos_ = 0;  // consumed prefix of buf_ (compacted lazily)
};

}  // namespace aiomsg::ws

#endif  // AIOMSG_WS_HPP
