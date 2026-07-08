// Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg bind sockets
// (see ../../PROTOCOL.md §10).
//
// A pure transport adapter, exactly parallel to TLS: it sits between the raw
// fd/SSL transport and the unchanged aiomsg frame codec. The concatenation of
// inbound binary-message payloads is the byte stream of §2, so the decoder is
// used unchanged; each aiomsg write leaves as one unmasked binary frame. WS
// message boundaries carry no meaning. Only the server half of RFC 6455 is
// implemented and no subprotocol or extension is ever negotiated.
//
// This module knows nothing about the socket internals: raw byte movement is
// injected as RawRead / RawWrite callbacks so the same code serves plain and
// TLS transports and can be unit-tested over in-memory buffers.
#ifndef AIOMSG_WS_HPP
#define AIOMSG_WS_HPP

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <functional>
#include <string>
#include <vector>

namespace aiomsg::ws {

// Move raw bytes to/from the underlying transport (fd or SSL).
//   RawRead:  fill up to `cap` bytes; returns >0 bytes read, 0 on timeout/no
//             data yet, -1 on close/fatal error.
//   RawWrite: write all `len` bytes; returns 0 on success, -1 on error.
using RawRead = std::function<int(uint8_t* buf, size_t cap)>;
using RawWrite = std::function<int(const uint8_t* buf, size_t len)>;

// Per-connection WebSocket adapter state (only meaningful once upgraded).
struct State {
    std::vector<uint8_t> inbuf;  // raw bytes awaiting WS-frame parsing
    bool close_sent = false;
};

// Result of the first-byte sniff on an accepted connection.
enum class Sniff {
    Raw = 0,        // raw aiomsg; `prefix` holds the already-read bytes to replay
    WebSocket = 1,  // upgraded; `st.inbuf` holds any bytes read past the request
    Reject = -1,    // unknown first byte / bad upgrade / EOF — close the connection
};

// Peek the first byte and, for an HTTP 'G', perform the RFC 6455 upgrade,
// writing the 101 (or an error) response via `rw`. `timeout` bounds the whole
// exchange. PROTOCOL.md §10.
Sniff sniff_and_upgrade(const RawRead& rr, const RawWrite& rw, State& st,
                        std::vector<uint8_t>& prefix,
                        std::chrono::milliseconds timeout);

// Read from the transport, decode WS frames, dispose of control frames
// (ping->pong, pong ignored, text/close/protocol-error -> teardown), and append
// binary-message payload to `out`. Returns >0 if any raw bytes were read
// (connection activity), 0 if nothing arrived, -1 if the connection closed.
int read(State& st, const RawRead& rr, const RawWrite& rw, std::vector<uint8_t>& out);

// Encode `data` as one unmasked binary frame (FIN=1) and write it. Returns 0 on
// success, -1 on error.
int write(State& st, const RawWrite& rw, const uint8_t* data, size_t len);

// The Sec-WebSocket-Accept value for a given key (base64(SHA1(key + GUID))).
// Exposed for unit tests.
std::string accept_key(const std::string& key);

// Encode a server->client frame (FIN=1, unmasked). Exposed for unit tests.
std::vector<uint8_t> encode_server_frame(uint8_t opcode, const uint8_t* data, size_t len);

}  // namespace aiomsg::ws

#endif  // AIOMSG_WS_HPP
