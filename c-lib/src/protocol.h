/* Wire protocol: framing and typed envelopes (the C counterpart of PROTOCOL.md).
 *
 * Pure data: no sockets or threads, so it can be unit-tested in isolation.
 * Frames on the wire are [u32 big-endian length][envelope]; an envelope is
 * [u8 type][body]. The encoders below return a malloc'd, already-framed buffer
 * ready to write to a socket; the decoder reassembles frames from a byte stream.
 */
#ifndef AIOMSG_PROTOCOL_H
#define AIOMSG_PROTOCOL_H

#include <stddef.h>
#include <stdint.h>

#define AIOMSG_PROTOCOL_VERSION 1
#define AIOMSG_IDENTITY_SIZE 16
#define AIOMSG_MSG_ID_SIZE 16

typedef enum {
    AIOMSG_T_HELLO = 0x01,
    AIOMSG_T_HEARTBEAT = 0x02,
    AIOMSG_T_DATA = 0x03,
    AIOMSG_T_DATA_REQ = 0x04,
    AIOMSG_T_ACK = 0x05,
} aiomsg_msgtype;

/* A decoded envelope. `payload`/`identity`/`msg_id` point into the caller-owned
 * envelope buffer passed to aiomsg_parse_envelope (no copies). */
typedef struct {
    aiomsg_msgtype type;
    uint8_t version;                       /* HELLO only */
    const uint8_t *identity;               /* HELLO only, 16 bytes */
    const uint8_t *msg_id;                 /* DATA_REQ / ACK only, 16 bytes */
    const uint8_t *payload;                /* DATA / DATA_REQ */
    size_t payload_len;
} aiomsg_envelope;

/* Framed-message constructors. Each returns a malloc'd buffer of *out_len bytes
 * (length prefix + envelope), ready to write to the wire, or NULL on OOM. */
uint8_t *aiomsg_frame_hello(const uint8_t identity[AIOMSG_IDENTITY_SIZE], size_t *out_len);
uint8_t *aiomsg_frame_heartbeat(size_t *out_len);
uint8_t *aiomsg_frame_data(const uint8_t *payload, size_t len, size_t *out_len);
uint8_t *aiomsg_frame_data_req(const uint8_t msg_id[AIOMSG_MSG_ID_SIZE],
                               const uint8_t *payload, size_t len, size_t *out_len);
uint8_t *aiomsg_frame_ack(const uint8_t msg_id[AIOMSG_MSG_ID_SIZE], size_t *out_len);

/* Parse one envelope (no length prefix). Returns 0 on success, -1 if empty,
 * truncated, or an unknown type. */
int aiomsg_parse_envelope(const uint8_t *env, size_t len, aiomsg_envelope *out);

/* Incremental frame reassembler: push arbitrary bytes, pop complete envelopes.
 * Tolerates partial reads (a frame split across several push calls). */
typedef struct {
    uint8_t *buf;
    size_t head; /* read position */
    size_t tail; /* write position */
    size_t cap;
} aiomsg_decoder;

void aiomsg_decoder_init(aiomsg_decoder *d);
void aiomsg_decoder_free(aiomsg_decoder *d);
/* Append raw bytes read from the wire. Returns 0 on success, -1 on OOM. */
int aiomsg_decoder_push(aiomsg_decoder *d, const uint8_t *bytes, size_t n);
/* If a complete frame is buffered, point `*env` (length `*env_len`) at its
 * envelope (valid until the next push) and return 1; otherwise return 0. */
int aiomsg_decoder_pop(aiomsg_decoder *d, const uint8_t **env, size_t *env_len);

#endif /* AIOMSG_PROTOCOL_H */
