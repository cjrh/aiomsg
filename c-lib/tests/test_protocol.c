/* Unit tests for the framing + envelope layer (no sockets). */
#include "protocol.h"

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void test_data_roundtrip(void) {
    size_t flen;
    uint8_t *f = aiomsg_frame_data((const uint8_t *)"hello", 5, &flen);
    assert(f);
    /* frame = [u32 len=6][type=DATA][hello] */
    assert(flen == 4 + 1 + 5);

    aiomsg_decoder d;
    aiomsg_decoder_init(&d);
    assert(aiomsg_decoder_push(&d, f, flen) == 0);
    const uint8_t *env;
    size_t env_len;
    assert(aiomsg_decoder_pop(&d, &env, &env_len) == 1);
    aiomsg_envelope e;
    assert(aiomsg_parse_envelope(env, env_len, &e) == 0);
    assert(e.type == AIOMSG_T_DATA);
    assert(e.payload_len == 5 && memcmp(e.payload, "hello", 5) == 0);
    assert(aiomsg_decoder_pop(&d, &env, &env_len) == 0);
    aiomsg_decoder_free(&d);
    free(f);
}

static void test_hello_and_datareq(void) {
    uint8_t id[16];
    memset(id, 0xab, 16);
    size_t flen;
    uint8_t *f = aiomsg_frame_hello(id, &flen);
    aiomsg_envelope e;
    assert(aiomsg_parse_envelope(f + 4, flen - 4, &e) == 0);
    assert(e.type == AIOMSG_T_HELLO && e.version == AIOMSG_PROTOCOL_VERSION);
    assert(memcmp(e.identity, id, 16) == 0);
    free(f);

    uint8_t mid[16];
    memset(mid, 0x7c, 16);
    f = aiomsg_frame_data_req(mid, (const uint8_t *)"p", 1, &flen);
    assert(aiomsg_parse_envelope(f + 4, flen - 4, &e) == 0);
    assert(e.type == AIOMSG_T_DATA_REQ && memcmp(e.msg_id, mid, 16) == 0);
    assert(e.payload_len == 1 && e.payload[0] == 'p');
    free(f);
}

/* A frame split across several pushes must still decode once complete. */
static void test_partial_reads(void) {
    size_t f1len, f2len;
    uint8_t *f1 = aiomsg_frame_data((const uint8_t *)"abc", 3, &f1len);
    uint8_t *f2 = aiomsg_frame_data((const uint8_t *)"de", 2, &f2len);

    aiomsg_decoder d;
    aiomsg_decoder_init(&d);
    const uint8_t *env;
    size_t env_len;
    /* Push one byte at a time across both frames. */
    for (size_t i = 0; i < f1len; i++) {
        aiomsg_decoder_push(&d, f1 + i, 1);
    }
    for (size_t i = 0; i < f2len; i++) {
        aiomsg_decoder_push(&d, f2 + i, 1);
    }
    aiomsg_envelope e;
    assert(aiomsg_decoder_pop(&d, &env, &env_len) == 1);
    assert(aiomsg_parse_envelope(env, env_len, &e) == 0 && e.payload_len == 3);
    assert(aiomsg_decoder_pop(&d, &env, &env_len) == 1);
    assert(aiomsg_parse_envelope(env, env_len, &e) == 0 && e.payload_len == 2);
    assert(aiomsg_decoder_pop(&d, &env, &env_len) == 0);
    aiomsg_decoder_free(&d);
    free(f1);
    free(f2);
}

static void test_bad_envelopes(void) {
    aiomsg_envelope e;
    assert(aiomsg_parse_envelope((const uint8_t *)"", 0, &e) == -1);
    uint8_t unknown[2] = {0xff, 0x00};
    assert(aiomsg_parse_envelope(unknown, 2, &e) == -1);
}

int main(void) {
    test_data_roundtrip();
    test_hello_and_datareq();
    test_partial_reads();
    test_bad_envelopes();
    printf("protocol tests OK\n");
    return 0;
}
