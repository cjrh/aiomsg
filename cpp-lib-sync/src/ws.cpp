// Server-side WebSocket (RFC 6455) byte-stream adapter — see ws.hpp.

#include "ws.hpp"

#include <openssl/evp.h>

#include <algorithm>
#include <cctype>
#include <cstring>

namespace aiomsg::ws {
namespace {

using Clock = std::chrono::steady_clock;

constexpr char kGuid[] = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// Opcodes (RFC 6455 §5.2).
constexpr uint8_t kOpCont = 0x0;
constexpr uint8_t kOpText = 0x1;
constexpr uint8_t kOpBin = 0x2;
constexpr uint8_t kOpClose = 0x8;
constexpr uint8_t kOpPing = 0x9;
constexpr uint8_t kOpPong = 0xA;

constexpr size_t kMaxRequestBytes = 8192;

std::string base64(const uint8_t* data, size_t len) {
    std::string out(4 * ((len + 2) / 3), '\0');
    int n = EVP_EncodeBlock(reinterpret_cast<uint8_t*>(&out[0]), data, static_cast<int>(len));
    out.resize(n < 0 ? 0 : static_cast<size_t>(n));
    return out;
}

// Decode standard base64; returns false on invalid input. Used only to check
// that Sec-WebSocket-Key decodes to 16 bytes.
bool base64_decode_len(const std::string& s, size_t& out_len) {
    std::string buf(3 * (s.size() / 4) + 3, '\0');
    int n = EVP_DecodeBlock(reinterpret_cast<uint8_t*>(&buf[0]),
                            reinterpret_cast<const uint8_t*>(s.data()),
                            static_cast<int>(s.size()));
    if (n < 0) {
        return false;
    }
    // EVP_DecodeBlock ignores '=' padding in its count; subtract the padding.
    size_t pad = 0;
    for (auto it = s.rbegin(); it != s.rend() && *it == '='; ++it) {
        ++pad;
    }
    out_len = static_cast<size_t>(n) - std::min<size_t>(pad, static_cast<size_t>(n));
    return true;
}

std::string to_lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return s;
}

std::string trim(const std::string& s) {
    size_t b = s.find_first_not_of(" \t");
    size_t e = s.find_last_not_of(" \t");
    return b == std::string::npos ? "" : s.substr(b, e - b + 1);
}

std::string success_response(const std::string& key) {
    return "HTTP/1.1 101 Switching Protocols\r\n"
           "Upgrade: websocket\r\n"
           "Connection: Upgrade\r\n"
           "Sec-WebSocket-Accept: " +
           accept_key(key) + "\r\n\r\n";
}

std::string error_response(const std::string& status) {
    if (status.rfind("426", 0) == 0) {
        return "HTTP/1.1 " + status + "\r\nSec-WebSocket-Version: 13\r\n\r\n";
    }
    return "HTTP/1.1 " + status + "\r\n\r\n";
}

void write_str(const RawWrite& rw, const std::string& s) {
    rw(reinterpret_cast<const uint8_t*>(s.data()), s.size());
}

// Validate the upgrade request; on success set `key`, else set `status` (the
// HTTP status line to return). Returns true on success.
bool parse_upgrade(const std::string& request, std::string& key, std::string& status) {
    size_t line_end = request.find("\r\n");
    std::string request_line = request.substr(0, line_end);
    // Request line: GET <path> HTTP/1.1
    size_t s1 = request_line.find(' ');
    size_t s2 = request_line.rfind(' ');
    if (s1 == std::string::npos || s1 == s2 || request_line.substr(0, s1) != "GET" ||
        request_line.substr(s2 + 1).rfind("HTTP/1.", 0) != 0) {
        status = "400 Bad Request";
        return false;
    }

    // Case-insensitive header map (last value wins; adequate for this subset).
    std::string upgrade, connection, version;
    key.clear();
    size_t pos = line_end + 2;
    while (pos < request.size()) {
        size_t eol = request.find("\r\n", pos);
        if (eol == std::string::npos || eol == pos) {
            break;
        }
        std::string line = request.substr(pos, eol - pos);
        pos = eol + 2;
        size_t colon = line.find(':');
        if (colon == std::string::npos) {
            status = "400 Bad Request";
            return false;
        }
        std::string name = to_lower(trim(line.substr(0, colon)));
        std::string value = trim(line.substr(colon + 1));
        if (name == "upgrade") upgrade = to_lower(value);
        else if (name == "connection") connection = to_lower(value);
        else if (name == "sec-websocket-version") version = value;
        else if (name == "sec-websocket-key") key = value;
        // Host / Origin / Sec-WebSocket-Protocol / -Extensions: ignored.
    }

    if (upgrade != "websocket") {
        status = "400 Bad Request";
        return false;
    }
    bool has_upgrade_token = false;  // Connection may be a comma-separated list
    for (size_t start = 0; start <= connection.size();) {
        size_t comma = connection.find(',', start);
        std::string tok = trim(connection.substr(start, comma - start));
        if (tok == "upgrade") has_upgrade_token = true;
        if (comma == std::string::npos) break;
        start = comma + 1;
    }
    if (!has_upgrade_token) {
        status = "400 Bad Request";
        return false;
    }
    if (version != "13") {
        status = "426 Upgrade Required";
        return false;
    }
    size_t klen = 0;
    if (key.empty() || !base64_decode_len(key, klen) || klen != 16) {
        status = "400 Bad Request";
        return false;
    }
    return true;
}

}  // namespace

std::string accept_key(const std::string& key) {
    std::string concat = key + kGuid;
    uint8_t md[EVP_MAX_MD_SIZE];
    unsigned int md_len = 0;
    EVP_Digest(concat.data(), concat.size(), md, &md_len, EVP_sha1(), nullptr);
    return base64(md, md_len);
}

std::vector<uint8_t> encode_server_frame(uint8_t opcode, const uint8_t* data, size_t len) {
    std::vector<uint8_t> f;
    f.push_back(0x80 | opcode);  // FIN=1, RSV=0
    // TODO(frame-size): enforce a configurable maximum frame length
    if (len < 126) {
        f.push_back(static_cast<uint8_t>(len));
    } else if (len < 65536) {
        f.push_back(126);
        f.push_back(static_cast<uint8_t>((len >> 8) & 0xff));
        f.push_back(static_cast<uint8_t>(len & 0xff));
    } else {
        f.push_back(127);
        for (int i = 7; i >= 0; --i) {
            f.push_back(static_cast<uint8_t>((static_cast<uint64_t>(len) >> (8 * i)) & 0xff));
        }
    }
    f.insert(f.end(), data, data + len);
    return f;
}

namespace {

void send_close(State& st, const RawWrite& rw, uint16_t code) {
    if (st.close_sent) {
        return;
    }
    st.close_sent = true;
    uint8_t payload[2] = {static_cast<uint8_t>(code >> 8), static_cast<uint8_t>(code & 0xff)};
    auto f = encode_server_frame(kOpClose, payload, 2);
    rw(f.data(), f.size());
}

// Parse one client frame from st.inbuf. Returns 1 if a frame was consumed, 0 if
// more bytes are needed, -1 if the connection must close (a close frame was
// already sent where appropriate). Binary/continuation payload is appended to
// `out`.
int parse_one(State& st, const RawWrite& rw, std::vector<uint8_t>& out) {
    auto& buf = st.inbuf;
    if (buf.size() < 2) {
        return 0;
    }
    uint8_t b0 = buf[0];
    uint8_t b1 = buf[1];
    if (b0 & 0x70) {  // RSV bits set
        send_close(st, rw, 1002);
        return -1;
    }
    uint8_t opcode = b0 & 0x0f;
    bool fin = (b0 & 0x80) != 0;
    if (!(b1 & 0x80)) {  // every client frame MUST be masked
        send_close(st, rw, 1002);
        return -1;
    }
    uint64_t len = b1 & 0x7f;
    size_t offset = 2;
    if (len == 126) {
        if (buf.size() < 4) return 0;
        len = (static_cast<uint64_t>(buf[2]) << 8) | buf[3];
        offset = 4;
    } else if (len == 127) {
        if (buf.size() < 10) return 0;
        if (buf[2] & 0x80) {  // 64-bit length MSB must be 0
            send_close(st, rw, 1002);
            return -1;
        }
        // TODO(frame-size): enforce a configurable maximum frame length
        len = 0;
        for (int i = 0; i < 8; ++i) {
            len = (len << 8) | buf[2 + i];
        }
        offset = 10;
    }
    if (opcode >= 0x8 && (!fin || len > 125)) {  // control-frame constraints
        send_close(st, rw, 1002);
        return -1;
    }
    if (buf.size() < offset + 4 + len) {
        return 0;  // need mask (4) + payload
    }
    const uint8_t* mask = &buf[offset];
    const uint8_t* masked = &buf[offset + 4];
    std::vector<uint8_t> payload(len);
    for (uint64_t i = 0; i < len; ++i) {
        payload[i] = masked[i] ^ mask[i & 3];
    }
    buf.erase(buf.begin(), buf.begin() + offset + 4 + len);

    switch (opcode) {
        case kOpBin:
        case kOpCont:
            out.insert(out.end(), payload.begin(), payload.end());
            return 1;
        case kOpPing: {
            auto pong = encode_server_frame(kOpPong, payload.data(), payload.size());
            rw(pong.data(), pong.size());
            return 1;
        }
        case kOpPong:
            return 1;  // ignore
        case kOpText:
            send_close(st, rw, 1003);  // text is not valid for aiomsg
            return -1;
        case kOpClose: {
            uint16_t code = payload.size() >= 2
                                ? static_cast<uint16_t>((payload[0] << 8) | payload[1])
                                : 1000;
            send_close(st, rw, code);
            return -1;
        }
        default:
            send_close(st, rw, 1002);  // unknown opcode
            return -1;
    }
}

size_t find_header_end(const std::vector<uint8_t>& buf) {
    static const uint8_t needle[4] = {'\r', '\n', '\r', '\n'};
    if (buf.size() < 4) {
        return std::string::npos;
    }
    for (size_t i = 0; i + 4 <= buf.size(); ++i) {
        if (std::memcmp(&buf[i], needle, 4) == 0) {
            return i;
        }
    }
    return std::string::npos;
}

}  // namespace

int read(State& st, const RawRead& rr, const RawWrite& rw, std::vector<uint8_t>& out) {
    uint8_t buf[16384];
    int r = rr(buf, sizeof(buf));
    if (r < 0) {
        return -1;
    }
    if (r > 0) {
        st.inbuf.insert(st.inbuf.end(), buf, buf + r);
    }
    for (;;) {
        int pr = parse_one(st, rw, out);
        if (pr == 0) {
            break;  // need more bytes
        }
        if (pr < 0) {
            return -1;  // close / protocol error
        }
    }
    return r > 0 ? r : 0;
}

int write(State& st, const RawWrite& rw, const uint8_t* data, size_t len) {
    (void)st;
    auto f = encode_server_frame(kOpBin, data, len);
    return rw(f.data(), f.size());
}

Sniff sniff_and_upgrade(const RawRead& rr, const RawWrite& rw, State& st,
                        std::vector<uint8_t>& prefix, std::chrono::milliseconds timeout) {
    auto deadline = Clock::now() + timeout;
    std::vector<uint8_t> buf;
    uint8_t chunk[2048];

    // Read at least the first byte.
    while (buf.empty()) {
        int r = rr(chunk, sizeof(chunk));
        if (r < 0) return Sniff::Reject;
        if (r == 0) {
            if (Clock::now() >= deadline) return Sniff::Reject;
            continue;
        }
        buf.insert(buf.end(), chunk, chunk + r);
    }

    if (buf[0] == 0x00) {  // raw aiomsg (HELLO length prefix begins 0x00...)
        prefix = std::move(buf);
        return Sniff::Raw;
    }
    if (buf[0] != 'G') {
        return Sniff::Reject;
    }

    // Read the rest of the HTTP request up to the blank line.
    size_t hdr_end;
    while ((hdr_end = find_header_end(buf)) == std::string::npos) {
        if (buf.size() > kMaxRequestBytes) {
            write_str(rw, error_response("400 Bad Request"));
            return Sniff::Reject;
        }
        int r = rr(chunk, sizeof(chunk));
        if (r < 0) return Sniff::Reject;
        if (r == 0) {
            if (Clock::now() >= deadline) return Sniff::Reject;
            continue;
        }
        buf.insert(buf.end(), chunk, chunk + r);
    }
    if (buf.size() > kMaxRequestBytes) {
        write_str(rw, error_response("400 Bad Request"));
        return Sniff::Reject;
    }

    std::string request(buf.begin(), buf.begin() + hdr_end + 4);
    std::string key, status;
    if (!parse_upgrade(request, key, status)) {
        write_str(rw, error_response(status));
        return Sniff::Reject;
    }
    write_str(rw, success_response(key));
    // Bytes past the request are the first WS frames.
    st.inbuf.assign(buf.begin() + hdr_end + 4, buf.end());
    return Sniff::WebSocket;
}

}  // namespace aiomsg::ws
