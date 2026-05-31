#include "protocol.h"

#include <string.h>
#include <stdlib.h>

/* Allocate a framed buffer: [u32 BE length][type][body...]. The body is `body`
 * (which may be NULL when body_len is 0) optionally preceded by `lead` bytes
 * (e.g. version, or a msg_id). Returns NULL on OOM. */
static uint8_t *frame(uint8_t type,
                      const uint8_t *lead, size_t lead_len,
                      const uint8_t *body, size_t body_len,
                      size_t *out_len) {
    size_t env_len = 1 + lead_len + body_len;
    size_t total = 4 + env_len;
    uint8_t *buf = malloc(total);
    if (!buf) {
        return NULL;
    }
    buf[0] = (uint8_t)(env_len >> 24);
    buf[1] = (uint8_t)(env_len >> 16);
    buf[2] = (uint8_t)(env_len >> 8);
    buf[3] = (uint8_t)(env_len);
    buf[4] = type;
    if (lead_len) {
        memcpy(buf + 5, lead, lead_len);
    }
    if (body_len) {
        memcpy(buf + 5 + lead_len, body, body_len);
    }
    *out_len = total;
    return buf;
}

uint8_t *aiomsg_frame_hello(const uint8_t identity[AIOMSG_IDENTITY_SIZE], size_t *out_len) {
    uint8_t version = AIOMSG_PROTOCOL_VERSION;
    return frame(AIOMSG_T_HELLO, &version, 1, identity, AIOMSG_IDENTITY_SIZE, out_len);
}

uint8_t *aiomsg_frame_heartbeat(size_t *out_len) {
    return frame(AIOMSG_T_HEARTBEAT, NULL, 0, NULL, 0, out_len);
}

uint8_t *aiomsg_frame_data(const uint8_t *payload, size_t len, size_t *out_len) {
    return frame(AIOMSG_T_DATA, NULL, 0, payload, len, out_len);
}

uint8_t *aiomsg_frame_data_req(const uint8_t msg_id[AIOMSG_MSG_ID_SIZE],
                               const uint8_t *payload, size_t len, size_t *out_len) {
    return frame(AIOMSG_T_DATA_REQ, msg_id, AIOMSG_MSG_ID_SIZE, payload, len, out_len);
}

uint8_t *aiomsg_frame_ack(const uint8_t msg_id[AIOMSG_MSG_ID_SIZE], size_t *out_len) {
    return frame(AIOMSG_T_ACK, msg_id, AIOMSG_MSG_ID_SIZE, NULL, 0, out_len);
}

int aiomsg_parse_envelope(const uint8_t *env, size_t len, aiomsg_envelope *out) {
    if (len < 1) {
        return -1;
    }
    memset(out, 0, sizeof(*out));
    out->type = (aiomsg_msgtype)env[0];
    const uint8_t *body = env + 1;
    size_t body_len = len - 1;
    switch (out->type) {
        case AIOMSG_T_HELLO:
            if (body_len < 1 + AIOMSG_IDENTITY_SIZE) {
                return -1;
            }
            out->version = body[0];
            out->identity = body + 1;
            return 0;
        case AIOMSG_T_HEARTBEAT:
            return 0;
        case AIOMSG_T_DATA:
            out->payload = body;
            out->payload_len = body_len;
            return 0;
        case AIOMSG_T_DATA_REQ:
            if (body_len < AIOMSG_MSG_ID_SIZE) {
                return -1;
            }
            out->msg_id = body;
            out->payload = body + AIOMSG_MSG_ID_SIZE;
            out->payload_len = body_len - AIOMSG_MSG_ID_SIZE;
            return 0;
        case AIOMSG_T_ACK:
            if (body_len < AIOMSG_MSG_ID_SIZE) {
                return -1;
            }
            out->msg_id = body;
            return 0;
        default:
            return -1;
    }
}

void aiomsg_decoder_init(aiomsg_decoder *d) {
    d->buf = NULL;
    d->head = d->tail = d->cap = 0;
}

void aiomsg_decoder_free(aiomsg_decoder *d) {
    free(d->buf);
    d->buf = NULL;
    d->head = d->tail = d->cap = 0;
}

int aiomsg_decoder_push(aiomsg_decoder *d, const uint8_t *bytes, size_t n) {
    /* Make room at the tail, compacting consumed bytes first, then growing. */
    if (d->cap - d->tail < n) {
        size_t used = d->tail - d->head;
        if (d->head > 0) {
            memmove(d->buf, d->buf + d->head, used);
            d->head = 0;
            d->tail = used;
        }
        if (d->cap - d->tail < n) {
            size_t cap = d->cap ? d->cap : 4096;
            while (cap - d->tail < n) {
                cap *= 2;
            }
            uint8_t *grown = realloc(d->buf, cap);
            if (!grown) {
                return -1;
            }
            d->buf = grown;
            d->cap = cap;
        }
    }
    memcpy(d->buf + d->tail, bytes, n);
    d->tail += n;
    return 0;
}

int aiomsg_decoder_pop(aiomsg_decoder *d, const uint8_t **env, size_t *env_len) {
    size_t avail = d->tail - d->head;
    if (avail < 4) {
        return 0;
    }
    const uint8_t *p = d->buf + d->head;
    size_t flen = ((size_t)p[0] << 24) | ((size_t)p[1] << 16) |
                  ((size_t)p[2] << 8) | (size_t)p[3];
    if (avail < 4 + flen) {
        return 0;
    }
    /* Point at the envelope and advance the read cursor. The bytes are not moved
     * here, so *env stays valid until the next push (which may compact). */
    *env = p + 4;
    *env_len = flen;
    d->head += 4 + flen;
    return 1;
}
