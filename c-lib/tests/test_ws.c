/* Unit tests for the server-side WebSocket adapter (ws.c, PROTOCOL.md §10). */
#include "ws.h"
#include "protocol.h"

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Build a masked client->server frame into `out` (caller-sized big enough);
 * returns its length. All client frames must be masked. */
static size_t client_frame(uint8_t *out, int opcode, int fin, const uint8_t *payload, size_t n) {
    const uint8_t mask[4] = {0xa1, 0xb2, 0xc3, 0xd4};
    size_t i = 0;
    out[i++] = (uint8_t)((fin ? 0x80 : 0) | opcode);
    if (n < 126) {
        out[i++] = (uint8_t)(0x80 | n);
    } else if (n < 65536) {
        out[i++] = 0x80 | 126;
        out[i++] = (uint8_t)(n >> 8);
        out[i++] = (uint8_t)n;
    } else {
        out[i++] = 0x80 | 127;
        for (int k = 0; k < 8; k++) {
            out[i++] = (uint8_t)(n >> (56 - 8 * k));
        }
    }
    memcpy(out + i, mask, 4);
    i += 4;
    for (size_t k = 0; k < n; k++) {
        out[i + k] = payload[k] ^ mask[k & 3];
    }
    return i + n;
}

/* Pop one reassembled envelope from `d` and assert it is a DATA frame carrying
 * exactly `payload` (length `n`). The WS layer feeds decoded binary payload into
 * the frame decoder as the raw §2 byte stream, so a test payload must itself be
 * a real aiomsg frame for the decoder to yield it back. */
static void expect_data(aiomsg_decoder *d, const char *payload, size_t n) {
    const uint8_t *env;
    size_t env_len;
    assert(aiomsg_decoder_pop(d, &env, &env_len) == 1);
    aiomsg_envelope e;
    assert(aiomsg_parse_envelope(env, env_len, &e) == 0);
    assert(e.type == AIOMSG_T_DATA && e.payload_len == n && memcmp(e.payload, payload, n) == 0);
}

static void test_accept_vector(void) {
    char accept[32];
    const char *key = "dGhlIHNhbXBsZSBub25jZQ==";
    aiomsg_ws_compute_accept(key, strlen(key), accept);
    assert(strcmp(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=") == 0);
}

static void test_handshake_ok(void) {
    const char *req =
        "GET /chat HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n"
        "Connection: keep-alive, Upgrade\r\n"
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    uint8_t *resp = NULL;
    size_t rl = 0;
    assert(aiomsg_ws_handshake_response((const uint8_t *)req, strlen(req), &resp, &rl) == 0);
    assert(memcmp(resp, "HTTP/1.1 101", 12) == 0);
    assert(strstr((char *)resp, "Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
    free(resp);
}

static void test_handshake_rejects(void) {
    const char *cases[] = {
        "POST / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        "GET / HTTP/1.1\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        "GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n",
    };
    for (size_t i = 0; i < 3; i++) {
        uint8_t *resp = NULL;
        size_t rl = 0;
        assert(aiomsg_ws_handshake_response((const uint8_t *)cases[i], strlen(cases[i]), &resp, &rl) == -1);
        assert(memcmp(resp, "HTTP/1.1 400", 12) == 0);
        free(resp);
    }
    const char *badver =
        "GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n";
    uint8_t *resp = NULL;
    size_t rl = 0;
    assert(aiomsg_ws_handshake_response((const uint8_t *)badver, strlen(badver), &resp, &rl) == -1);
    assert(memcmp(resp, "HTTP/1.1 426", 12) == 0);
    free(resp);
}

static void test_masked_binary(void) {
    aiomsg_ws w;
    aiomsg_ws_init(&w);
    aiomsg_decoder d;
    aiomsg_decoder_init(&d);
    /* One masked binary frame carrying a whole aiomsg DATA frame. */
    size_t msg_len;
    uint8_t *msg = aiomsg_frame_data((const uint8_t *)"aiomsg-payload", 14, &msg_len);
    assert(msg);
    uint8_t frame[128];
    size_t n = client_frame(frame, 0x2, 1, msg, msg_len);
    assert(aiomsg_ws_feed(&w, frame, n, &d) == 0);
    expect_data(&d, "aiomsg-payload", 14);
    free(msg);
    aiomsg_decoder_free(&d);
    aiomsg_ws_free(&w);
}

static void test_fragmented(void) {
    aiomsg_ws w;
    aiomsg_ws_init(&w);
    aiomsg_decoder d;
    aiomsg_decoder_init(&d);
    /* Split one aiomsg DATA frame across three WS fragments (bin + 2 cont),
     * fed a byte at a time to prove both WS and aiomsg reassembly. */
    size_t msg_len;
    uint8_t *msg = aiomsg_frame_data((const uint8_t *)"abcdefghi", 9, &msg_len);
    assert(msg);
    size_t a = msg_len / 3, b = msg_len / 3;
    uint8_t buf[256];
    size_t n = 0;
    n += client_frame(buf + n, 0x2, 0, msg, a);
    n += client_frame(buf + n, 0x0, 0, msg + a, b);
    n += client_frame(buf + n, 0x0, 1, msg + a + b, msg_len - a - b);
    for (size_t i = 0; i < n; i++) {
        assert(aiomsg_ws_feed(&w, buf + i, 1, &d) == 0);
    }
    expect_data(&d, "abcdefghi", 9);
    free(msg);
    aiomsg_decoder_free(&d);
    aiomsg_ws_free(&w);
}

static void test_ping_pong(void) {
    aiomsg_ws w;
    aiomsg_ws_init(&w);
    aiomsg_decoder d;
    aiomsg_decoder_init(&d);
    uint8_t frame[64];
    size_t n = client_frame(frame, 0x9, 1, (const uint8_t *)"ping!", 5);
    assert(aiomsg_ws_feed(&w, frame, n, &d) == 0);
    /* A pong (opcode 0xA, FIN, unmasked, same payload) was queued. */
    assert(w.out_len == 2 + 5);
    assert(w.out[0] == (0x80 | 0xA) && w.out[1] == 5 && memcmp(w.out + 2, "ping!", 5) == 0);
    aiomsg_decoder_free(&d);
    aiomsg_ws_free(&w);
}

static void test_close_echo(void) {
    aiomsg_ws w;
    aiomsg_ws_init(&w);
    aiomsg_decoder d;
    aiomsg_decoder_init(&d);
    uint8_t body[2] = {0x03, 0xe8}; /* 1000 */
    uint8_t frame[16];
    size_t n = client_frame(frame, 0x8, 1, body, 2);
    assert(aiomsg_ws_feed(&w, frame, n, &d) == -1); /* signals close */
    assert(w.out_len == 4 && w.out[0] == (0x80 | 0x8) && w.out[2] == 0x03 && w.out[3] == 0xe8);
    aiomsg_decoder_free(&d);
    aiomsg_ws_free(&w);
}

static void test_reject_text(void) {
    aiomsg_ws w;
    aiomsg_ws_init(&w);
    aiomsg_decoder d;
    aiomsg_decoder_init(&d);
    uint8_t frame[16];
    size_t n = client_frame(frame, 0x1, 1, (const uint8_t *)"hi", 2);
    assert(aiomsg_ws_feed(&w, frame, n, &d) == -1);
    assert(w.out[0] == (0x80 | 0x8) && w.out[2] == 0x03 && w.out[3] == 0xeb); /* 1003 */
    aiomsg_decoder_free(&d);
    aiomsg_ws_free(&w);
}

static void test_reject_unmasked(void) {
    aiomsg_ws w;
    aiomsg_ws_init(&w);
    aiomsg_decoder d;
    aiomsg_decoder_init(&d);
    /* Unmasked binary frame (MASK bit clear) — illegal from a client. */
    uint8_t frame[6] = {0x82, 0x04, 'n', 'o', 'p', 'e'};
    assert(aiomsg_ws_feed(&w, frame, sizeof(frame), &d) == -1);
    assert(w.out[0] == (0x80 | 0x8) && w.out[2] == 0x03 && w.out[3] == 0xea); /* 1002 */
    aiomsg_decoder_free(&d);
    aiomsg_ws_free(&w);
}

int main(void) {
    test_accept_vector();
    test_handshake_ok();
    test_handshake_rejects();
    test_masked_binary();
    test_fragmented();
    test_ping_pong();
    test_close_echo();
    test_reject_text();
    test_reject_unmasked();
    printf("ws tests OK\n");
    return 0;
}
