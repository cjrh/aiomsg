// Pure server-side WebSocket (RFC 6455) primitives: the handshake computation
// and a stateful client-frame parser / server-frame encoder. See ws.hpp.
//
// SHA-1 and base64 come from OpenSSL, already linked for TLS.
#include "ws.hpp"

#include <algorithm>
#include <cctype>
#include <cstring>

#include <openssl/evp.h>
#include <openssl/sha.h>

namespace aiomsg::ws {

namespace {

constexpr char kGuid[] = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

std::string base64(const uint8_t* data, size_t len) {
    // EVP_EncodeBlock writes 4*ceil(len/3) chars plus a NUL.
    std::string out(4 * ((len + 2) / 3), '\0');
    int n = EVP_EncodeBlock(reinterpret_cast<uint8_t*>(out.data()), data, static_cast<int>(len));
    out.resize(n);
    return out;
}

std::optional<Bytes> base64_decode(std::string_view in) {
    Bytes out(3 * (in.size() / 4) + 3);
    int n = EVP_DecodeBlock(out.data(), reinterpret_cast<const uint8_t*>(in.data()),
                            static_cast<int>(in.size()));
    if (n < 0) {
        return std::nullopt;
    }
    // EVP_DecodeBlock counts '=' padding as zero bytes; correct the length.
    size_t pad = 0;
    if (!in.empty() && in.back() == '=') pad++;
    if (in.size() >= 2 && in[in.size() - 2] == '=') pad++;
    out.resize(static_cast<size_t>(n) - pad);
    return out;
}

std::string to_lower(std::string_view s) {
    std::string r(s);
    std::transform(r.begin(), r.end(), r.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return r;
}

std::string trim(std::string_view s) {
    size_t b = 0, e = s.size();
    while (b < e && std::isspace(static_cast<unsigned char>(s[b]))) b++;
    while (e > b && std::isspace(static_cast<unsigned char>(s[e - 1]))) e--;
    return std::string(s.substr(b, e - b));
}

}  // namespace

std::string compute_accept(const std::string& key) {
    std::string input = key + kGuid;
    uint8_t digest[SHA_DIGEST_LENGTH];
    SHA1(reinterpret_cast<const uint8_t*>(input.data()), input.size(), digest);
    return base64(digest, SHA_DIGEST_LENGTH);
}

UpgradeResult parse_upgrade(std::string_view request) {
    UpgradeResult bad{false, "", "400 Bad Request"};

    size_t line_end = request.find("\r\n");
    if (line_end == std::string_view::npos) {
        return bad;
    }
    std::string_view request_line = request.substr(0, line_end);
    // "GET <path> HTTP/1.x"
    size_t sp1 = request_line.find(' ');
    size_t sp2 = request_line.rfind(' ');
    if (sp1 == std::string_view::npos || sp1 == sp2) {
        return bad;
    }
    if (request_line.substr(0, sp1) != "GET" ||
        request_line.substr(sp2 + 1).substr(0, 7) != "HTTP/1.") {
        return bad;
    }

    // Header field parsing (case-insensitive names).
    std::string upgrade, connection, version, key;
    size_t pos = line_end + 2;
    while (pos < request.size()) {
        size_t eol = request.find("\r\n", pos);
        if (eol == std::string_view::npos) break;
        if (eol == pos) break;  // blank line: end of headers
        std::string_view line = request.substr(pos, eol - pos);
        size_t colon = line.find(':');
        if (colon == std::string_view::npos) {
            return bad;
        }
        std::string name = to_lower(trim(line.substr(0, colon)));
        std::string value = trim(line.substr(colon + 1));
        if (name == "upgrade") upgrade = to_lower(value);
        else if (name == "connection") connection = to_lower(value);
        else if (name == "sec-websocket-version") version = value;
        else if (name == "sec-websocket-key") key = value;
        pos = eol + 2;
    }

    if (upgrade != "websocket") {
        return bad;
    }
    // Connection may be a comma-separated token list containing "upgrade".
    bool has_upgrade_token = false;
    size_t start = 0;
    while (start <= connection.size()) {
        size_t comma = connection.find(',', start);
        std::string tok = trim(connection.substr(start, comma == std::string::npos
                                                            ? std::string::npos
                                                            : comma - start));
        if (tok == "upgrade") has_upgrade_token = true;
        if (comma == std::string::npos) break;
        start = comma + 1;
    }
    if (!has_upgrade_token) {
        return bad;
    }
    if (version != "13") {
        return {false, "", "426 Upgrade Required"};
    }
    auto decoded = base64_decode(key);
    if (!decoded || decoded->size() != 16) {
        return bad;
    }
    return {true, key, ""};
}

std::string success_response(const std::string& key) {
    return "HTTP/1.1 101 Switching Protocols\r\n"
           "Upgrade: websocket\r\n"
           "Connection: Upgrade\r\n"
           "Sec-WebSocket-Accept: " + compute_accept(key) + "\r\n"
           "\r\n";
}

std::string error_response(const std::string& status) {
    if (status.rfind("426", 0) == 0) {
        return "HTTP/1.1 " + status + "\r\nSec-WebSocket-Version: 13\r\n\r\n";
    }
    return "HTTP/1.1 " + status + "\r\n\r\n";
}

Bytes server_frame(uint8_t opcode, const uint8_t* payload, size_t len) {
    Bytes out;
    out.push_back(static_cast<uint8_t>(0x80 | opcode));  // FIN=1, RSV=0
    // TODO(frame-size): enforce a configurable maximum frame length
    if (len < 126) {
        out.push_back(static_cast<uint8_t>(len));
    } else if (len < 65536) {
        out.push_back(126);
        out.push_back(static_cast<uint8_t>((len >> 8) & 0xff));
        out.push_back(static_cast<uint8_t>(len & 0xff));
    } else {
        out.push_back(127);
        for (int shift = 56; shift >= 0; shift -= 8) {
            out.push_back(static_cast<uint8_t>((static_cast<uint64_t>(len) >> shift) & 0xff));
        }
    }
    out.insert(out.end(), payload, payload + len);
    return out;
}

void FrameParser::feed(const uint8_t* data, size_t len) {
    // Compact consumed prefix occasionally so buf_ does not grow without bound.
    if (pos_ > 0 && pos_ == buf_.size()) {
        buf_.clear();
        pos_ = 0;
    } else if (pos_ > 4096) {
        buf_.erase(buf_.begin(), buf_.begin() + pos_);
        pos_ = 0;
    }
    buf_.insert(buf_.end(), data, data + len);
}

FrameParser::Result FrameParser::next() {
    size_t avail = buf_.size() - pos_;
    if (avail < 2) {
        return {Event::Need, {}, 0};
    }
    const uint8_t* p = buf_.data() + pos_;
    uint8_t b0 = p[0];
    uint8_t b1 = p[1];
    if (b0 & 0x70) {  // any RSV bit set
        return {Event::ProtocolError, {}, 1002};
    }
    uint8_t opcode = b0 & 0x0f;
    bool fin = (b0 & 0x80) != 0;
    if (!(b1 & 0x80)) {  // every client frame MUST be masked
        return {Event::ProtocolError, {}, 1002};
    }
    uint64_t len = b1 & 0x7f;
    size_t header = 2;
    if (len == 126) {
        if (avail < 4) return {Event::Need, {}, 0};
        len = (static_cast<uint64_t>(p[2]) << 8) | p[3];
        header = 4;
    } else if (len == 127) {
        if (avail < 10) return {Event::Need, {}, 0};
        if (p[2] & 0x80) {  // 64-bit length MSB MUST be 0
            return {Event::ProtocolError, {}, 1002};
        }
        // TODO(frame-size): enforce a configurable maximum frame length
        len = 0;
        for (int i = 0; i < 8; i++) len = (len << 8) | p[2 + i];
        header = 10;
    }
    bool control = opcode >= 0x8;
    if (control && (!fin || len > 125)) {
        return {Event::ProtocolError, {}, 1002};
    }
    if (avail < header + 4 + len) {  // mask (4) + payload
        return {Event::Need, {}, 0};
    }
    const uint8_t* mask = p + header;
    const uint8_t* masked = p + header + 4;
    Bytes payload(len);
    for (uint64_t i = 0; i < len; i++) {
        payload[i] = masked[i] ^ mask[i & 3];
    }
    pos_ += header + 4 + len;

    switch (opcode) {
        case kOpBin:
        case kOpCont:
            return {Event::Binary, std::move(payload), 0};
        case kOpPing:
            return {Event::Ping, std::move(payload), 0};
        case kOpPong:
            return {Event::Pong, std::move(payload), 0};
        case kOpText:
            return {Event::ProtocolError, {}, 1003};  // text is not valid for aiomsg
        case kOpClose: {
            uint16_t code = payload.size() >= 2
                                ? static_cast<uint16_t>((payload[0] << 8) | payload[1])
                                : 1000;
            return {Event::Close, {}, code};
        }
        default:
            return {Event::ProtocolError, {}, 1002};  // unknown opcode
    }
}

}  // namespace aiomsg::ws
