/* Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg bind sockets
 * (see ../../PROTOCOL.md §10).
 *
 * This is a pure, transport-agnostic layer: it turns the payloads of inbound
 * binary WebSocket messages into the concatenated aiomsg byte stream of §2
 * (pushed to the existing decoder), and frames each outbound aiomsg write as one
 * unmasked binary WebSocket message. WebSocket message boundaries carry no
 * meaning. It never touches a socket itself — the caller (aiomsg.c) feeds it raw
 * bytes and flushes the control-frame bytes it queues — so only the server half
 * of RFC 6455 lives here, with no subprotocol or extension ever negotiated.
 */
#ifndef AIOMSG_WS_H
#define AIOMSG_WS_H

#include <stddef.h>
#include <stdint.h>

#include "protocol.h"

/* Per-connection WebSocket parser state. `in` accumulates partial inbound
 * frames; `out` holds control-frame bytes (pong / close) the caller must write
 * to the peer after each feed. */
typedef struct {
    uint8_t *in;
    size_t in_len, in_cap;
    uint8_t *out;
    size_t out_len, out_cap;
    int close_sent;
} aiomsg_ws;

void aiomsg_ws_init(aiomsg_ws *w);
void aiomsg_ws_free(aiomsg_ws *w);

/* Write Sec-WebSocket-Accept for `key` (base64(SHA1(key + GUID))) into `out`,
 * which must hold at least 29 bytes. RFC 6455 §4.2.2. */
void aiomsg_ws_compute_accept(const char *key, size_t key_len, char *out);

/* Validate an HTTP upgrade request (bytes through the terminating blank line).
 * On success writes a malloc'd 101 response to *resp/*resp_len and returns 0; on
 * any violation writes a malloc'd 400/426 error response and returns -1. The
 * caller frees *resp in both cases. */
int aiomsg_ws_handshake_response(const uint8_t *req, size_t req_len,
                                 uint8_t **resp, size_t *resp_len);

/* Encode one server->client frame (FIN=1, unmasked, given opcode). Returns a
 * malloc'd buffer of *out_len bytes, or NULL on OOM. opcode 0x2 = binary. */
uint8_t *aiomsg_ws_encode(int opcode, const uint8_t *payload, size_t len, size_t *out_len);

/* Feed raw inbound bytes. Appends decoded binary payload to `dec`, and queues
 * pong / close frames into w->out for the caller to flush. Returns 0 to keep the
 * connection open, or -1 when it must close (peer close, or a protocol error —
 * a close frame with the right status code is queued in w->out first). */
int aiomsg_ws_feed(aiomsg_ws *w, const uint8_t *data, size_t len, aiomsg_decoder *dec);

#endif /* AIOMSG_WS_H */
