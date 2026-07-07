/* Server-side WebSocket adapter — see ws.h and PROTOCOL.md §10. */
#include "ws.h"

#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include <openssl/evp.h>
#include <openssl/sha.h>

#define WS_GUID "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

/* Opcodes (RFC 6455 §5.2). */
#define OP_CONT 0x0
#define OP_TEXT 0x1
#define OP_BIN 0x2
#define OP_CLOSE 0x8
#define OP_PING 0x9
#define OP_PONG 0xA

#define MAX_REQUEST_BYTES 8192

void aiomsg_ws_init(aiomsg_ws *w) { memset(w, 0, sizeof(*w)); }

void aiomsg_ws_free(aiomsg_ws *w) {
    free(w->in);
    free(w->out);
    memset(w, 0, sizeof(*w));
}

/* --- growable byte buffers ------------------------------------------------ */

static int buf_append(uint8_t **buf, size_t *len, size_t *cap, const uint8_t *data, size_t n) {
    if (*len + n > *cap) {
        size_t nc = *cap ? *cap : 256;
        while (nc < *len + n) {
            nc *= 2;
        }
        uint8_t *nb = realloc(*buf, nc);
        if (!nb) {
            return -1;
        }
        *buf = nb;
        *cap = nc;
    }
    memcpy(*buf + *len, data, n);
    *len += n;
    return 0;
}

/* --- handshake ------------------------------------------------------------ */

void aiomsg_ws_compute_accept(const char *key, size_t key_len, char *out) {
    unsigned char cat[512];
    size_t n = key_len < sizeof(cat) - sizeof(WS_GUID) ? key_len : 0;
    memcpy(cat, key, n);
    memcpy(cat + n, WS_GUID, sizeof(WS_GUID) - 1);
    unsigned char digest[SHA_DIGEST_LENGTH];
    SHA1(cat, n + sizeof(WS_GUID) - 1, digest);
    EVP_EncodeBlock((unsigned char *)out, digest, SHA_DIGEST_LENGTH); /* writes 28 chars + NUL */
}

/* Case-insensitive lookup of a header value into `val` (NUL-terminated, capped).
 * Returns 1 if found. `req` is the request text (NUL not required); scans lines
 * separated by CRLF. */
static int header_get(const char *req, size_t req_len, const char *name,
                      char *val, size_t val_cap) {
    size_t name_len = strlen(name);
    size_t i = 0;
    /* Skip the request line. */
    while (i < req_len && !(req[i] == '\r' && i + 1 < req_len && req[i + 1] == '\n')) {
        i++;
    }
    i += 2;
    while (i < req_len) {
        size_t start = i;
        while (i < req_len && !(req[i] == '\r' && i + 1 < req_len && req[i + 1] == '\n')) {
            i++;
        }
        size_t line_len = i - start;
        i += 2;
        if (line_len == 0) {
            break; /* blank line: end of headers */
        }
        const char *colon = memchr(req + start, ':', line_len);
        if (!colon) {
            continue;
        }
        size_t hn = (size_t)(colon - (req + start));
        if (hn == name_len && strncasecmp(req + start, name, name_len) == 0) {
            const char *v = colon + 1;
            size_t vlen = line_len - hn - 1;
            while (vlen && (*v == ' ' || *v == '\t')) {
                v++;
                vlen--;
            }
            while (vlen && (v[vlen - 1] == ' ' || v[vlen - 1] == '\t')) {
                vlen--;
            }
            if (vlen >= val_cap) {
                vlen = val_cap - 1;
            }
            memcpy(val, v, vlen);
            val[vlen] = '\0';
            return 1;
        }
    }
    return 0;
}

/* Does the comma-separated token list contain `token` (case-insensitive)? */
static int list_has_token(const char *list, const char *token) {
    size_t tl = strlen(token);
    const char *p = list;
    while (*p) {
        while (*p == ' ' || *p == ',' || *p == '\t') {
            p++;
        }
        const char *start = p;
        while (*p && *p != ',') {
            p++;
        }
        size_t len = (size_t)(p - start);
        while (len && (start[len - 1] == ' ' || start[len - 1] == '\t')) {
            len--;
        }
        if (len == tl && strncasecmp(start, token, tl) == 0) {
            return 1;
        }
    }
    return 0;
}

/* base64-decoded length of `s` (no decode), or -1 if not valid base64 length. */
static int b64_decoded_len(const char *s) {
    size_t n = strlen(s);
    if (n == 0 || n % 4 != 0) {
        return -1;
    }
    int pad = 0;
    if (s[n - 1] == '=') {
        pad++;
    }
    if (s[n - 2] == '=') {
        pad++;
    }
    return (int)(n / 4 * 3) - pad;
}

static uint8_t *make_response(const char *text, size_t *out_len) {
    size_t n = strlen(text);
    uint8_t *r = malloc(n);
    if (r) {
        memcpy(r, text, n);
        *out_len = n;
    }
    return r;
}

int aiomsg_ws_handshake_response(const uint8_t *req, size_t req_len,
                                 uint8_t **resp, size_t *resp_len) {
    const char *r = (const char *)req;
    char val[256];

    /* Request line: GET <path> HTTP/1.1 */
    if (req_len > MAX_REQUEST_BYTES || req_len < 4 || memcmp(r, "GET ", 4) != 0) {
        *resp = make_response("HTTP/1.1 400 Bad Request\r\n\r\n", resp_len);
        return -1;
    }
    if (!header_get(r, req_len, "Upgrade", val, sizeof(val)) || strcasecmp(val, "websocket") != 0) {
        *resp = make_response("HTTP/1.1 400 Bad Request\r\n\r\n", resp_len);
        return -1;
    }
    if (!header_get(r, req_len, "Connection", val, sizeof(val)) || !list_has_token(val, "upgrade")) {
        *resp = make_response("HTTP/1.1 400 Bad Request\r\n\r\n", resp_len);
        return -1;
    }
    if (!header_get(r, req_len, "Sec-WebSocket-Version", val, sizeof(val)) || strcmp(val, "13") != 0) {
        *resp = make_response("HTTP/1.1 426 Upgrade Required\r\nSec-WebSocket-Version: 13\r\n\r\n",
                              resp_len);
        return -1;
    }
    if (!header_get(r, req_len, "Sec-WebSocket-Key", val, sizeof(val)) || b64_decoded_len(val) != 16) {
        *resp = make_response("HTTP/1.1 400 Bad Request\r\n\r\n", resp_len);
        return -1;
    }

    char accept[32];
    aiomsg_ws_compute_accept(val, strlen(val), accept);
    char out[256];
    int n = snprintf(out, sizeof(out),
                     "HTTP/1.1 101 Switching Protocols\r\n"
                     "Upgrade: websocket\r\n"
                     "Connection: Upgrade\r\n"
                     "Sec-WebSocket-Accept: %s\r\n"
                     "\r\n",
                     accept);
    *resp = malloc((size_t)n);
    if (!*resp) {
        *resp_len = 0;
        return -1;
    }
    memcpy(*resp, out, (size_t)n);
    *resp_len = (size_t)n;
    return 0;
}

/* --- frame layer ---------------------------------------------------------- */

uint8_t *aiomsg_ws_encode(int opcode, const uint8_t *payload, size_t len, size_t *out_len) {
    size_t header = 2;
    /* TODO(frame-size): enforce a configurable maximum frame length */
    if (len >= 65536) {
        header = 10;
    } else if (len >= 126) {
        header = 4;
    }
    uint8_t *f = malloc(header + len);
    if (!f) {
        return NULL;
    }
    f[0] = (uint8_t)(0x80 | opcode); /* FIN + opcode, RSV=0 */
    if (header == 2) {
        f[1] = (uint8_t)len;
    } else if (header == 4) {
        f[1] = 126;
        f[2] = (uint8_t)(len >> 8);
        f[3] = (uint8_t)len;
    } else {
        f[1] = 127;
        for (int i = 0; i < 8; i++) {
            f[2 + i] = (uint8_t)(len >> (56 - 8 * i));
        }
    }
    if (len) {
        memcpy(f + header, payload, len);
    }
    *out_len = header + len;
    return f;
}

/* Queue a server frame into w->out. */
static int queue_frame(aiomsg_ws *w, int opcode, const uint8_t *payload, size_t len) {
    size_t flen;
    uint8_t *f = aiomsg_ws_encode(opcode, payload, len, &flen);
    if (!f) {
        return -1;
    }
    int rc = buf_append(&w->out, &w->out_len, &w->out_cap, f, flen);
    free(f);
    return rc;
}

/* Queue a close frame with `code` (once) and signal connection close. */
static int ws_fail(aiomsg_ws *w, uint16_t code) {
    if (!w->close_sent) {
        w->close_sent = 1;
        uint8_t body[2] = {(uint8_t)(code >> 8), (uint8_t)code};
        queue_frame(w, OP_CLOSE, body, 2);
    }
    return -1;
}

int aiomsg_ws_feed(aiomsg_ws *w, const uint8_t *data, size_t len, aiomsg_decoder *dec) {
    if (buf_append(&w->in, &w->in_len, &w->in_cap, data, len) != 0) {
        return -1;
    }
    size_t off = 0;
    for (;;) {
        if (w->in_len - off < 2) {
            break;
        }
        uint8_t *p = w->in + off;
        uint8_t b0 = p[0], b1 = p[1];
        if (b0 & 0x70) {
            return ws_fail(w, 1002); /* RSV bits set */
        }
        int opcode = b0 & 0x0f;
        int fin = b0 & 0x80;
        if (!(b1 & 0x80)) {
            return ws_fail(w, 1002); /* client frames MUST be masked */
        }
        uint64_t plen = b1 & 0x7f;
        size_t hdr = 2;
        if (plen == 126) {
            if (w->in_len - off < 4) {
                break;
            }
            plen = ((uint64_t)p[2] << 8) | p[3];
            hdr = 4;
        } else if (plen == 127) {
            if (w->in_len - off < 10) {
                break;
            }
            if (p[2] & 0x80) {
                return ws_fail(w, 1002); /* 64-bit length MSB must be 0 */
            }
            /* TODO(frame-size): enforce a configurable maximum frame length */
            plen = 0;
            for (int i = 0; i < 8; i++) {
                plen = (plen << 8) | p[2 + i];
            }
            hdr = 10;
        }
        if (opcode >= 0x8 && (!fin || plen > 125)) {
            return ws_fail(w, 1002); /* control frames: FIN=1, <=125 bytes */
        }
        if (w->in_len - off < hdr + 4 + plen) {
            break; /* need mask (4) + payload */
        }
        uint8_t *mask = p + hdr;
        uint8_t *masked = p + hdr + 4;
        for (uint64_t i = 0; i < plen; i++) {
            masked[i] ^= mask[i & 3];
        }
        off += hdr + 4 + (size_t)plen;

        if (opcode == OP_BIN || opcode == OP_CONT) {
            if (aiomsg_decoder_push(dec, masked, (size_t)plen) != 0) {
                return -1;
            }
        } else if (opcode == OP_PING) {
            if (queue_frame(w, OP_PONG, masked, (size_t)plen) != 0) {
                return -1;
            }
        } else if (opcode == OP_PONG) {
            /* ignore */
        } else if (opcode == OP_TEXT) {
            return ws_fail(w, 1003); /* text is not valid for aiomsg */
        } else if (opcode == OP_CLOSE) {
            uint16_t code = plen >= 2 ? (uint16_t)((masked[0] << 8) | masked[1]) : 1000;
            if (!w->close_sent) {
                w->close_sent = 1;
                uint8_t body[2] = {(uint8_t)(code >> 8), (uint8_t)code};
                queue_frame(w, OP_CLOSE, body, 2);
            }
            /* Drop consumed bytes for tidiness, then signal close. */
            memmove(w->in, w->in + off, w->in_len - off);
            w->in_len -= off;
            return -1;
        } else {
            return ws_fail(w, 1002); /* unknown opcode */
        }
    }
    if (off) {
        memmove(w->in, w->in + off, w->in_len - off);
        w->in_len -= off;
    }
    return 0;
}
