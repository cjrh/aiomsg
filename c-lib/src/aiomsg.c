/* aiomsg — native C implementation.
 *
 * Threading model (mirrors rust-lib-sync's broker design):
 *   - one broker thread owns the connection map, round-robin cursor, send
 *     buffer, and in-flight ack table; everything talks to it over a command
 *     queue, so that shared state is touched from one thread only;
 *   - one thread per connection runs a poll loop: each iteration it drains the
 *     broker's outbound queue for that connection, sends a heartbeat if idle,
 *     then reads (with a short socket timeout) and forwards frames to the broker.
 *
 * The short read timeout (POLL_MS) lets a single per-connection thread interleave
 * reading and writing — necessary because an OpenSSL `SSL`, like a rustls
 * connection, must not be driven from two threads at once. The cost is up to
 * POLL_MS of latency on a message sent to an otherwise-idle connection; see
 * rust-lib-sync/docs/threading-and-tls.md for the trade-offs and alternatives.
 *
 * Resends (at-least-once) are driven by the broker's own timed wait against the
 * nearest pending deadline — no per-message timer threads.
 */
#include "aiomsg.h"
#include "protocol.h"
#include "ws.h"

#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/random.h>
#include <sys/socket.h>

#include <openssl/err.h>
#include <openssl/ssl.h>

#define POLL_MS 50
#define HEARTBEAT_MS 5000
#define TIMEOUT_MS 15000
#define HANDSHAKE_MS 15000
#define CONNECT_TIMEOUT_MS 1000
#define RECONNECT_MS 100
#define ACCEPT_POLL_MS 200
#define RESEND_MS 5000
#define MAX_RETRIES 5

static long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void random_bytes(uint8_t *buf, size_t n) {
    if (getrandom(buf, n, 0) == (ssize_t)n) {
        return;
    }
    for (size_t i = 0; i < n; i++) {
        buf[i] = (uint8_t)(rand() & 0xff);
    }
}

/* ----- intrusive list nodes ----------------------------------------------- */

/* Outbound (framed bytes) and received (payload + sender) messages. */
typedef struct bnode {
    struct bnode *next;
    uint8_t identity[AIOMSG_IDENTITY_SIZE];
    uint8_t *data;
    size_t len;
} bnode;

/* A send buffered while no peers were connected. */
typedef struct buffered {
    struct buffered *next;
    int has_identity;
    uint8_t identity[AIOMSG_IDENTITY_SIZE];
    uint8_t *data;
    size_t len;
} buffered;

/* ----- broker commands ---------------------------------------------------- */

typedef enum {
    CMD_SEND,
    CMD_CONN_UP,
    CMD_CONN_DOWN,
    CMD_RECEIVED,
    CMD_CLOSE,
} cmd_kind;

struct conn;

typedef struct cmd {
    struct cmd *next;
    cmd_kind kind;
    int has_identity;
    uint8_t identity[AIOMSG_IDENTITY_SIZE];
    uint8_t *data; /* SEND / RECEIVED payload (owned) */
    size_t len;
    struct conn *conn;                  /* CONN_UP / CONN_DOWN */
    aiomsg_msgtype recv_type;           /* RECEIVED */
    uint8_t msg_id[AIOMSG_MSG_ID_SIZE]; /* RECEIVED(DATA_REQ/ACK) */
} cmd;

/* ----- pending (at-least-once) -------------------------------------------- */

typedef struct pending {
    struct pending *next;
    uint8_t msg_id[AIOMSG_MSG_ID_SIZE];
    int has_identity;
    uint8_t identity[AIOMSG_IDENTITY_SIZE];
    uint8_t *data;
    size_t len;
    uint32_t retries;
    long deadline_ms;
} pending;

/* ----- connection --------------------------------------------------------- */

typedef struct conn {
    int fd;
    SSL *ssl; /* NULL for plain TCP */
    aiomsg_ws *ws; /* non-NULL once a WebSocket upgrade has completed (§10) */
    uint8_t peer[AIOMSG_IDENTITY_SIZE];

    pthread_mutex_t out_mtx;
    bnode *out_head, *out_tail; /* framed bytes queued by the broker */

    /* The broker reports its CONN_UP decision here. Once a connection has been
     * registered (CONN_UP sent), the broker owns this object and frees it when
     * it processes the matching CONN_DOWN; conn_session must not touch it after
     * sending CONN_DOWN. Pre-registration failures are freed by conn_session. */
    pthread_mutex_t mtx;
    pthread_cond_t cond;
    int acc_done, acc_ok;

    aiomsg_socket *sock;
} conn;

/* ----- socket ------------------------------------------------------------- */

typedef struct {
    uint8_t identity[AIOMSG_IDENTITY_SIZE];
    conn *c;
} conn_entry;

struct aiomsg_socket {
    aiomsg_send_mode mode;
    aiomsg_delivery delivery;
    uint8_t identity[AIOMSG_IDENTITY_SIZE];

    pthread_mutex_t cmd_mtx;
    pthread_cond_t cmd_cond;
    cmd *cmd_head, *cmd_tail;

    pthread_mutex_t recv_mtx;
    pthread_cond_t recv_cond;
    bnode *recv_head, *recv_tail;
    int recv_closed;

    pthread_mutex_t threads_mtx;
    pthread_t *threads;
    size_t nthreads, threads_cap;
    pthread_t broker_thread;

    pthread_mutex_t fds_mtx;
    int *fds;
    size_t nfds, fds_cap;

    SSL_CTX *server_ctx; /* set by aiomsg_bind_tls */

    volatile int shutdown;
    int closed;
};

/* ----- fd registry & thread tracking -------------------------------------- */

static void fds_add(aiomsg_socket *s, int fd) {
    pthread_mutex_lock(&s->fds_mtx);
    if (s->nfds == s->fds_cap) {
        s->fds_cap = s->fds_cap ? s->fds_cap * 2 : 8;
        s->fds = realloc(s->fds, s->fds_cap * sizeof(int));
    }
    s->fds[s->nfds++] = fd;
    pthread_mutex_unlock(&s->fds_mtx);
}

static void fds_remove(aiomsg_socket *s, int fd) {
    pthread_mutex_lock(&s->fds_mtx);
    for (size_t i = 0; i < s->nfds; i++) {
        if (s->fds[i] == fd) {
            s->fds[i] = s->fds[--s->nfds];
            break;
        }
    }
    pthread_mutex_unlock(&s->fds_mtx);
}

static void track_thread(aiomsg_socket *s, pthread_t t) {
    pthread_mutex_lock(&s->threads_mtx);
    if (s->nthreads == s->threads_cap) {
        s->threads_cap = s->threads_cap ? s->threads_cap * 2 : 8;
        s->threads = realloc(s->threads, s->threads_cap * sizeof(pthread_t));
    }
    s->threads[s->nthreads++] = t;
    pthread_mutex_unlock(&s->threads_mtx);
}

/* ----- queues ------------------------------------------------------------- */

static cmd *cmd_new(cmd_kind kind) {
    cmd *c = calloc(1, sizeof(cmd));
    if (c) {
        c->kind = kind;
    }
    return c;
}

static void cmd_push(aiomsg_socket *s, cmd *c) {
    c->next = NULL;
    pthread_mutex_lock(&s->cmd_mtx);
    if (s->cmd_tail) {
        s->cmd_tail->next = c;
    } else {
        s->cmd_head = c;
    }
    s->cmd_tail = c;
    pthread_cond_signal(&s->cmd_cond);
    pthread_mutex_unlock(&s->cmd_mtx);
}

static void recv_push(aiomsg_socket *s, const uint8_t *identity,
                      const uint8_t *data, size_t len) {
    bnode *n = calloc(1, sizeof(bnode));
    if (!n) {
        return;
    }
    memcpy(n->identity, identity, AIOMSG_IDENTITY_SIZE);
    n->data = malloc(len ? len : 1);
    memcpy(n->data, data, len);
    n->len = len;
    pthread_mutex_lock(&s->recv_mtx);
    if (s->recv_tail) {
        s->recv_tail->next = n;
    } else {
        s->recv_head = n;
    }
    s->recv_tail = n;
    pthread_cond_signal(&s->recv_cond);
    pthread_mutex_unlock(&s->recv_mtx);
}

static void out_push(conn *c, uint8_t *framed, size_t len) {
    bnode *n = calloc(1, sizeof(bnode));
    if (!n) {
        free(framed);
        return;
    }
    n->data = framed;
    n->len = len;
    pthread_mutex_lock(&c->out_mtx);
    if (c->out_tail) {
        c->out_tail->next = n;
    } else {
        c->out_head = n;
    }
    c->out_tail = n;
    pthread_mutex_unlock(&c->out_mtx);
}

static bnode *out_drain(conn *c) {
    pthread_mutex_lock(&c->out_mtx);
    bnode *head = c->out_head;
    c->out_head = c->out_tail = NULL;
    pthread_mutex_unlock(&c->out_mtx);
    return head;
}

/* ======================================================================== */
/* Broker                                                                    */
/* ======================================================================== */

typedef struct {
    aiomsg_socket *s;
    conn_entry *conns;
    size_t nconns, conns_cap;
    size_t rr_cursor;
    buffered *buf_head, *buf_tail;
    pending *pending_head;
} broker;

static conn *broker_find(broker *b, const uint8_t *identity) {
    for (size_t i = 0; i < b->nconns; i++) {
        if (memcmp(b->conns[i].identity, identity, AIOMSG_IDENTITY_SIZE) == 0) {
            return b->conns[i].c;
        }
    }
    return NULL;
}

static void broker_route(broker *b, int has_identity, const uint8_t *identity,
                         uint8_t *framed, size_t len) {
    if (b->s->shutdown) {
        free(framed); /* tearing down: stop queueing to connections */
        return;
    }
    if (has_identity) {
        conn *c = broker_find(b, identity);
        if (c) {
            out_push(c, framed, len);
        } else {
            free(framed);
        }
        return;
    }
    if (b->nconns == 0) {
        free(framed);
        return;
    }
    if (b->s->mode == AIOMSG_PUBLISH) {
        for (size_t i = 0; i < b->nconns; i++) {
            uint8_t *cp = malloc(len);
            if (cp) {
                memcpy(cp, framed, len);
                out_push(b->conns[i].c, cp, len);
            }
        }
        free(framed);
    } else {
        conn *c = b->conns[b->rr_cursor % b->nconns].c;
        b->rr_cursor++;
        out_push(c, framed, len);
    }
}

static void broker_buffer(broker *b, int has_id, const uint8_t *id,
                          const uint8_t *data, size_t len) {
    buffered *n = calloc(1, sizeof(buffered));
    if (!n) {
        return;
    }
    n->has_identity = has_id;
    if (has_id) {
        memcpy(n->identity, id, AIOMSG_IDENTITY_SIZE);
    }
    n->data = malloc(len ? len : 1);
    memcpy(n->data, data, len);
    n->len = len;
    if (b->buf_tail) {
        b->buf_tail->next = n;
    } else {
        b->buf_head = n;
    }
    b->buf_tail = n;
}

static void broker_transmit(broker *b, int has_id, const uint8_t *id,
                            const uint8_t *data, size_t len, uint32_t retries) {
    int at_least_once = b->s->delivery == AIOMSG_AT_LEAST_ONCE &&
                        (has_id || b->s->mode == AIOMSG_ROUNDROBIN);
    if (!at_least_once) {
        size_t flen;
        uint8_t *f = aiomsg_frame_data(data, len, &flen);
        if (f) {
            broker_route(b, has_id, id, f, flen);
        }
        return;
    }
    uint8_t msg_id[AIOMSG_MSG_ID_SIZE];
    random_bytes(msg_id, AIOMSG_MSG_ID_SIZE);

    pending *p = calloc(1, sizeof(pending));
    if (!p) {
        return;
    }
    memcpy(p->msg_id, msg_id, AIOMSG_MSG_ID_SIZE);
    p->has_identity = has_id;
    if (has_id) {
        memcpy(p->identity, id, AIOMSG_IDENTITY_SIZE);
    }
    p->data = malloc(len ? len : 1);
    memcpy(p->data, data, len);
    p->len = len;
    p->retries = retries;
    p->deadline_ms = now_ms() + RESEND_MS;
    p->next = b->pending_head;
    b->pending_head = p;

    size_t flen;
    uint8_t *f = aiomsg_frame_data_req(msg_id, data, len, &flen);
    if (f) {
        broker_route(b, has_id, id, f, flen);
    }
}

static void broker_send(broker *b, int has_id, const uint8_t *id,
                        const uint8_t *data, size_t len) {
    if (b->nconns == 0) {
        broker_buffer(b, has_id, id, data, len);
    } else {
        broker_transmit(b, has_id, id, data, len, MAX_RETRIES);
    }
}

static void pending_free(pending *p) {
    free(p->data);
    free(p);
}

/* Fire any expired resends. Returns the soonest remaining deadline (ms from
 * now) or -1 if none. */
static long broker_sweep(broker *b) {
    long now = now_ms();
    pending **pp = &b->pending_head;
    while (*pp) {
        pending *p = *pp;
        if (p->deadline_ms <= now) {
            *pp = p->next;
            if (p->retries > 0) {
                if (b->nconns == 0) {
                    broker_buffer(b, p->has_identity, p->identity, p->data, p->len);
                } else {
                    broker_transmit(b, p->has_identity, p->identity, p->data,
                                    p->len, p->retries - 1);
                }
            }
            pending_free(p);
        } else {
            pp = &p->next;
        }
    }
    long soonest = -1;
    for (pending *p = b->pending_head; p; p = p->next) {
        long d = p->deadline_ms - now_ms();
        if (d < 0) {
            d = 0;
        }
        if (soonest < 0 || d < soonest) {
            soonest = d;
        }
    }
    return soonest;
}

static void broker_up(broker *b, conn *c) {
    int dup = broker_find(b, c->peer) != NULL;
    pthread_mutex_lock(&c->mtx);
    c->acc_done = 1;
    c->acc_ok = !dup;
    pthread_cond_signal(&c->cond);
    pthread_mutex_unlock(&c->mtx);
    if (dup) {
        return;
    }
    if (b->nconns == b->conns_cap) {
        b->conns_cap = b->conns_cap ? b->conns_cap * 2 : 8;
        b->conns = realloc(b->conns, b->conns_cap * sizeof(conn_entry));
    }
    memcpy(b->conns[b->nconns].identity, c->peer, AIOMSG_IDENTITY_SIZE);
    b->conns[b->nconns].c = c;
    b->nconns++;

    /* Flush the send buffer now that a peer exists. */
    buffered *q = b->buf_head;
    b->buf_head = b->buf_tail = NULL;
    while (q) {
        buffered *n = q->next;
        broker_send(b, q->has_identity, q->identity, q->data, q->len);
        free(q->data);
        free(q);
        q = n;
    }
}

static void conn_free(conn *c) {
    bnode *n = c->out_head;
    while (n) {
        bnode *next = n->next;
        free(n->data);
        free(n);
        n = next;
    }
    pthread_mutex_destroy(&c->out_mtx);
    pthread_mutex_destroy(&c->mtx);
    pthread_cond_destroy(&c->cond);
    free(c);
}

static void broker_down(broker *b, conn *c) {
    for (size_t i = 0; i < b->nconns; i++) {
        if (b->conns[i].c == c) {
            b->conns[i] = b->conns[--b->nconns];
            break;
        }
    }
    conn_free(c); /* the broker owns a registered connection's storage */
}

static void broker_received(broker *b, const cmd *c) {
    switch (c->recv_type) {
        case AIOMSG_T_DATA:
            recv_push(b->s, c->identity, c->data, c->len);
            break;
        case AIOMSG_T_DATA_REQ: {
            recv_push(b->s, c->identity, c->data, c->len);
            size_t flen;
            uint8_t *ack = aiomsg_frame_ack(c->msg_id, &flen);
            if (ack) {
                broker_route(b, 1, c->identity, ack, flen);
            }
            break;
        }
        case AIOMSG_T_ACK: {
            pending **pp = &b->pending_head;
            while (*pp) {
                if (memcmp((*pp)->msg_id, c->msg_id, AIOMSG_MSG_ID_SIZE) == 0) {
                    pending *found = *pp;
                    *pp = found->next;
                    pending_free(found);
                    break;
                }
                pp = &(*pp)->next;
            }
            break;
        }
        default:
            break;
    }
}

static void abstime_in(struct timespec *ts, long ms) {
    clock_gettime(CLOCK_REALTIME, ts);
    ts->tv_sec += ms / 1000;
    ts->tv_nsec += (ms % 1000) * 1000000L;
    if (ts->tv_nsec >= 1000000000L) {
        ts->tv_sec++;
        ts->tv_nsec -= 1000000000L;
    }
}

static void *broker_main(void *arg) {
    aiomsg_socket *s = arg;
    broker b;
    memset(&b, 0, sizeof(b));
    b.s = s;

    /* `closing` is set when CMD_CLOSE arrives, but we do NOT exit immediately:
     * connection threads are still running and own no storage — the broker frees
     * each connection only when it processes that connection's CONN_DOWN. So we
     * keep draining until every connection has reported down (nconns == 0). This
     * is bounded because close() shuts every socket, so each connection's loop
     * breaks and sends CONN_DOWN promptly. */
    pthread_mutex_lock(&s->cmd_mtx);
    int closing = 0;
    for (;;) {
        while (s->cmd_head == NULL) {
            if (closing && b.nconns == 0) {
                pthread_mutex_unlock(&s->cmd_mtx);
                goto teardown;
            }
            long soonest = broker_sweep(&b);
            if (s->cmd_head != NULL) {
                break;
            }
            if (soonest < 0) {
                pthread_cond_wait(&s->cmd_cond, &s->cmd_mtx);
            } else {
                struct timespec ts;
                abstime_in(&ts, soonest);
                pthread_cond_timedwait(&s->cmd_cond, &s->cmd_mtx, &ts);
                if (s->cmd_head == NULL) {
                    break; /* timed out: go sweep */
                }
            }
        }
        cmd *list = s->cmd_head;
        s->cmd_head = s->cmd_tail = NULL;
        pthread_mutex_unlock(&s->cmd_mtx);

        while (list) {
            cmd *next = list->next;
            switch (list->kind) {
                case CMD_SEND:
                    broker_send(&b, list->has_identity, list->identity,
                                list->data, list->len);
                    break;
                case CMD_CONN_UP:
                    broker_up(&b, list->conn);
                    break;
                case CMD_CONN_DOWN:
                    broker_down(&b, list->conn);
                    break;
                case CMD_RECEIVED:
                    broker_received(&b, list);
                    break;
                case CMD_CLOSE:
                    closing = 1;
                    break;
            }
            free(list->data);
            free(list);
            list = next;
        }
        broker_sweep(&b);
        pthread_mutex_lock(&s->cmd_mtx);
        /* Loop back; the empty-queue wait above exits once closing && nconns==0. */
    }

teardown:
    /* Every connection has been freed via its CONN_DOWN; only the broker's own
     * state remains. */
    for (pending *p = b.pending_head; p;) {
        pending *n = p->next;
        pending_free(p);
        p = n;
    }
    for (buffered *q = b.buf_head; q;) {
        buffered *n = q->next;
        free(q->data);
        free(q);
        q = n;
    }
    free(b.conns);
    /* Drop any unprocessed commands (their conn pointers, if any, belong to
     * connections we just freed or never registered — do not dereference). */
    pthread_mutex_lock(&s->cmd_mtx);
    cmd *leftover = s->cmd_head;
    s->cmd_head = s->cmd_tail = NULL;
    pthread_mutex_unlock(&s->cmd_mtx);
    while (leftover) {
        cmd *n = leftover->next;
        free(leftover->data);
        free(leftover);
        leftover = n;
    }

    /* Wake any blocked receiver. */
    pthread_mutex_lock(&s->recv_mtx);
    s->recv_closed = 1;
    pthread_cond_broadcast(&s->recv_cond);
    pthread_mutex_unlock(&s->recv_mtx);
    return NULL;
}

/* ======================================================================== */
/* Connection I/O                                                            */
/* ======================================================================== */

static void set_read_timeout(int fd, long ms) {
    struct timeval tv = {ms / 1000, (ms % 1000) * 1000};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
}

/* Read once from the transport (TLS or plain TCP) into buf. Returns >0 bytes
 * read, 0 on timeout/no data, -1 on close or error. */
static int raw_read(conn *c, uint8_t *buf, size_t cap) {
    if (c->ssl) {
        int r = SSL_read(c->ssl, buf, (int)cap);
        if (r > 0) {
            return r;
        }
        int err = SSL_get_error(c->ssl, r);
        if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE) {
            return 0;
        }
        if (err == SSL_ERROR_SYSCALL && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            return 0;
        }
        return -1; /* SSL_ERROR_ZERO_RETURN (clean close) or fatal */
    }
    ssize_t n = recv(c->fd, buf, cap, 0);
    if (n > 0) {
        return (int)n;
    }
    if (n == 0) {
        return -1;
    }
    if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR) {
        return 0;
    }
    return -1;
}

static int raw_write(conn *c, const uint8_t *buf, size_t len);

/* Flush any pending WebSocket control-frame bytes (pong / close) to the peer. */
static int ws_flush_out(conn *c) {
    if (c->ws->out_len == 0) {
        return 0;
    }
    int rc = raw_write(c, c->ws->out, c->ws->out_len);
    c->ws->out_len = 0;
    return rc;
}

/* Read once. Returns >0 if bytes were read (and any decoded frames pushed to the
 * decoder), 0 on timeout/no data, -1 on close or error. Over a WebSocket the raw
 * bytes are run through the frame parser (§10) before reaching the decoder. */
static int conn_read(conn *c, aiomsg_decoder *dec) {
    uint8_t buf[16384];
    int n = raw_read(c, buf, sizeof(buf));
    if (n <= 0) {
        return n;
    }
    if (c->ws) {
        int fr = aiomsg_ws_feed(c->ws, buf, (size_t)n, dec);
        if (ws_flush_out(c) != 0) {
            return -1;
        }
        return fr == 0 ? n : -1; /* fr<0: peer close or protocol error */
    }
    return aiomsg_decoder_push(dec, buf, (size_t)n) == 0 ? n : -1;
}

/* Write one framed aiomsg message. Over a WebSocket it becomes exactly one
 * unmasked binary frame (§2.2). */
static int conn_write_all(conn *c, const uint8_t *buf, size_t len) {
    if (c->ws) {
        size_t flen;
        uint8_t *f = aiomsg_ws_encode(0x2, buf, len, &flen);
        if (!f) {
            return -1;
        }
        int rc = raw_write(c, f, flen);
        free(f);
        return rc;
    }
    return raw_write(c, buf, len);
}

static int raw_write(conn *c, const uint8_t *buf, size_t len) {
    size_t off = 0;
    while (off < len) {
        if (c->ssl) {
            int r = SSL_write(c->ssl, buf + off, (int)(len - off));
            if (r > 0) {
                off += (size_t)r;
                continue;
            }
            int err = SSL_get_error(c->ssl, r);
            if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE) {
                continue;
            }
            return -1;
        }
        ssize_t n = send(c->fd, buf + off, len - off, MSG_NOSIGNAL);
        if (n > 0) {
            off += (size_t)n;
            continue;
        }
        if (n < 0 && errno == EINTR) {
            continue;
        }
        return -1;
    }
    return 0;
}

/* Block (within the handshake timeout) for the peer's HELLO. Returns 0 on
 * success (peer identity copied into c->peer), -1 otherwise. */
static int read_hello(conn *c, aiomsg_decoder *dec) {
    long deadline = now_ms() + HANDSHAKE_MS;
    for (;;) {
        const uint8_t *env;
        size_t env_len;
        if (aiomsg_decoder_pop(dec, &env, &env_len)) {
            aiomsg_envelope e;
            if (aiomsg_parse_envelope(env, env_len, &e) == 0 &&
                e.type == AIOMSG_T_HELLO && e.version == AIOMSG_PROTOCOL_VERSION) {
                memcpy(c->peer, e.identity, AIOMSG_IDENTITY_SIZE);
                return 0;
            }
            return -1;
        }
        int r = conn_read(c, dec);
        if (r < 0) {
            return -1;
        }
        if (r == 0 && now_ms() >= deadline) {
            return -1;
        }
    }
}

/* Forward every buffered frame to the broker. Returns -1 if a connection-fatal
 * envelope arrives (none currently), 0 otherwise. */
static void forward_frames(conn *c, aiomsg_decoder *dec) {
    const uint8_t *env;
    size_t env_len;
    while (aiomsg_decoder_pop(dec, &env, &env_len)) {
        aiomsg_envelope e;
        if (aiomsg_parse_envelope(env, env_len, &e) != 0) {
            continue; /* unknown / truncated: ignore */
        }
        if (e.type == AIOMSG_T_HEARTBEAT || e.type == AIOMSG_T_HELLO) {
            continue;
        }
        cmd *cm = cmd_new(CMD_RECEIVED);
        if (!cm) {
            continue;
        }
        memcpy(cm->identity, c->peer, AIOMSG_IDENTITY_SIZE);
        cm->recv_type = e.type;
        if (e.msg_id) {
            memcpy(cm->msg_id, e.msg_id, AIOMSG_MSG_ID_SIZE);
        }
        if (e.payload_len) {
            cm->data = malloc(e.payload_len);
            memcpy(cm->data, e.payload, e.payload_len);
            cm->len = e.payload_len;
        }
        cmd_push(c->sock, cm);
    }
}

/* TLS handshake setup passed to a connection session. */
typedef struct {
    SSL_CTX *ctx; /* NULL = plain TCP */
    int is_server;
    char server_name[256]; /* client verify/SNI target */
} tls_setup;

static void apply_client_verify(SSL *ssl, const char *name) {
    unsigned char ipbuf[16];
    if (inet_pton(AF_INET, name, ipbuf) == 1 || inet_pton(AF_INET6, name, ipbuf) == 1) {
        X509_VERIFY_PARAM_set1_ip_asc(SSL_get0_param(ssl), name);
    } else {
        SSL_set1_host(ssl, name);
        SSL_set_tlsext_host_name(ssl, name); /* SNI */
    }
}

/* Run one established connection: TLS handshake (if any), HELLO exchange, broker
 * registration, then the poll loop. Owns `fd` (and any SSL). The connection
 * object is heap-allocated: until it is registered with the broker (CONN_UP),
 * this function owns and frees it; once registered, the broker owns it and frees
 * it when it processes the matching CONN_DOWN, so after sending CONN_DOWN we must
 * not touch it again. */
/* Bind-side detection of raw aiomsg vs a WebSocket upgrade (PROTOCOL.md §10),
 * run after TLS termination. Returns 0 (raw — the sniffed byte is replayed into
 * the decoder), 1 (WebSocket upgraded — c->ws is set), or -1 (drop). Only the
 * accept path calls this; the connect end never sniffs. */
static int server_sniff(conn *c, aiomsg_decoder *dec) {
    long deadline = now_ms() + HANDSHAKE_MS;
    uint8_t first;
    for (;;) {
        int r = raw_read(c, &first, 1);
        if (r > 0) {
            break;
        }
        if (r < 0 || now_ms() >= deadline) {
            return -1;
        }
    }
    if (first == 0x00) { /* raw aiomsg: HELLO length prefix begins 0x00 */
        return aiomsg_decoder_push(dec, &first, 1) == 0 ? 0 : -1;
    }
    if (first != 'G') {
        return -1;
    }

    /* Accumulate the HTTP request through the blank line (cap ~8 KiB). */
    uint8_t req[8192];
    size_t rlen = 0;
    req[rlen++] = 'G';
    ssize_t term_end = -1;
    while (term_end < 0 && rlen < sizeof(req)) {
        uint8_t chunk[2048];
        int r = raw_read(c, chunk, sizeof(chunk));
        if (r < 0 || now_ms() >= deadline) {
            return -1;
        }
        if (r == 0) {
            continue;
        }
        for (int i = 0; i < r && rlen < sizeof(req); i++) {
            req[rlen++] = chunk[i];
        }
        for (size_t k = 3; k < rlen; k++) {
            if (req[k - 3] == '\r' && req[k - 2] == '\n' && req[k - 1] == '\r' && req[k] == '\n') {
                term_end = (ssize_t)k + 1;
                break;
            }
        }
    }
    if (term_end < 0) { /* no terminator within the cap */
        const char *e = "HTTP/1.1 400 Bad Request\r\n\r\n";
        raw_write(c, (const uint8_t *)e, strlen(e));
        return -1;
    }

    uint8_t *resp = NULL;
    size_t resp_len = 0;
    int ok = aiomsg_ws_handshake_response(req, (size_t)term_end, &resp, &resp_len);
    if (resp) {
        raw_write(c, resp, resp_len);
        free(resp);
    }
    if (ok != 0) {
        return -1;
    }

    c->ws = malloc(sizeof(aiomsg_ws));
    if (!c->ws) {
        return -1;
    }
    aiomsg_ws_init(c->ws);

    /* Bytes read past the request are the first WS frames; feed them now. */
    size_t leftover = rlen - (size_t)term_end;
    if (leftover) {
        if (aiomsg_ws_feed(c->ws, req + term_end, leftover, dec) != 0) {
            return -1;
        }
        if (ws_flush_out(c) != 0) {
            return -1;
        }
    }
    return 1;
}

static void conn_session(aiomsg_socket *s, int fd, const tls_setup *tls) {
    conn *c = calloc(1, sizeof(conn));
    if (!c) {
        close(fd);
        return;
    }
    c->fd = fd;
    c->sock = s;
    pthread_mutex_init(&c->out_mtx, NULL);
    pthread_mutex_init(&c->mtx, NULL);
    pthread_cond_init(&c->cond, NULL);

    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    set_read_timeout(fd, HANDSHAKE_MS);
    fds_add(s, fd);

    aiomsg_decoder dec;
    aiomsg_decoder_init(&dec);

    if (tls->ctx) {
        c->ssl = SSL_new(tls->ctx);
        if (!c->ssl) {
            goto fail;
        }
        SSL_set_fd(c->ssl, fd);
        if (tls->is_server) {
            if (SSL_accept(c->ssl) != 1) {
                goto fail;
            }
        } else {
            apply_client_verify(c->ssl, tls->server_name);
            if (SSL_connect(c->ssl) != 1) {
                goto fail;
            }
        }
    }

    /* Bind side only: sniff raw-vs-WebSocket on the (decrypted) stream before
     * any aiomsg bytes (PROTOCOL.md §10). The connect end never sniffs. */
    if (tls->is_server && server_sniff(c, &dec) < 0) {
        goto fail;
    }

    /* Handshake: send our HELLO, read and validate the peer's. */
    size_t hlen;
    uint8_t *hello = aiomsg_frame_hello(s->identity, &hlen);
    if (!hello || conn_write_all(c, hello, hlen) != 0) {
        free(hello);
        goto fail;
    }
    free(hello);
    if (read_hello(c, &dec) != 0) {
        goto fail;
    }

    /* Register. From here the broker owns `c`; every exit goes through `teardown`
     * (which hands `c` back to the broker via CONN_DOWN), never `fail`. */
    cmd *up = cmd_new(CMD_CONN_UP);
    if (!up) {
        goto fail;
    }
    up->conn = c;
    cmd_push(s, up);
    pthread_mutex_lock(&c->mtx);
    while (!c->acc_done && !s->shutdown) {
        struct timespec ts;
        abstime_in(&ts, 50);
        pthread_cond_timedwait(&c->cond, &c->mtx, &ts);
    }
    int accepted = c->acc_done && c->acc_ok;
    pthread_mutex_unlock(&c->mtx);
    if (!accepted) {
        goto teardown; /* duplicate identity, or shutting down */
    }

    /* Steady state: short read timeout so one thread can interleave I/O. */
    set_read_timeout(fd, POLL_MS);
    long last_in = now_ms();
    long last_out = now_ms();
    forward_frames(c, &dec); /* anything buffered during the handshake */
    while (!s->shutdown) {
        /* 1. Send everything the broker queued for us. */
        bnode *out = out_drain(c);
        int werr = 0;
        while (out) {
            bnode *next = out->next;
            if (!werr && conn_write_all(c, out->data, out->len) != 0) {
                werr = 1;
            }
            free(out->data);
            free(out);
            out = next;
            last_out = now_ms();
        }
        if (werr) {
            break;
        }
        /* 2. Heartbeat if idle. */
        if (now_ms() - last_out >= HEARTBEAT_MS) {
            size_t blen;
            uint8_t *beat = aiomsg_frame_heartbeat(&blen);
            if (beat) {
                int rc = conn_write_all(c, beat, blen);
                free(beat);
                if (rc != 0) {
                    break;
                }
            }
            last_out = now_ms();
        }
        /* 3. Read (blocks up to POLL_MS). */
        int r = conn_read(c, &dec);
        if (r < 0) {
            break;
        }
        if (r > 0) {
            last_in = now_ms();
            forward_frames(c, &dec);
        }
        /* 4. Dead peer? */
        if (now_ms() - last_in >= TIMEOUT_MS) {
            break;
        }
    }

teardown:
    /* Finished using `c` for I/O. Close the transport, then hand `c` to the
     * broker to free via CONN_DOWN. Do not touch `c` after pushing CONN_DOWN. */
    if (c->ws) {
        /* Best-effort WebSocket close (1000); do not wait for the echo. */
        if (!c->ws->close_sent) {
            uint8_t body[2] = {0x03, 0xE8};
            size_t clen;
            uint8_t *cf = aiomsg_ws_encode(0x8, body, 2, &clen);
            if (cf) {
                raw_write(c, cf, clen);
                free(cf);
            }
        }
        aiomsg_ws_free(c->ws);
        free(c->ws);
        c->ws = NULL;
    }
    aiomsg_decoder_free(&dec);
    if (c->ssl) {
        SSL_free(c->ssl);
    }
    fds_remove(s, fd);
    close(fd);
    cmd *down = cmd_new(CMD_CONN_DOWN);
    if (down) {
        down->conn = c;
        cmd_push(s, down);
    }
    /* If the CONN_DOWN allocation failed we intentionally leak `c` rather than
     * free it here: the broker may still hold it in its map (OOM, shutdown). */
    return;

fail:
    /* Failure before registration: the broker never knew about `c`. */
    if (c->ws) {
        aiomsg_ws_free(c->ws);
        free(c->ws);
        c->ws = NULL;
    }
    aiomsg_decoder_free(&dec);
    if (c->ssl) {
        SSL_free(c->ssl);
    }
    fds_remove(s, fd);
    close(fd);
    conn_free(c);
}

/* ======================================================================== */
/* Accept / connect loops                                                    */
/* ======================================================================== */

typedef struct {
    aiomsg_socket *s;
    int fd;
    tls_setup tls;
} conn_arg;

static void *conn_thread(void *arg) {
    conn_arg *a = arg;
    conn_session(a->s, a->fd, &a->tls);
    free(a);
    return NULL;
}

typedef struct {
    aiomsg_socket *s;
    int listen_fd;
    tls_setup tls;
} accept_arg;

static void *accept_loop(void *arg) {
    accept_arg *a = arg;
    aiomsg_socket *s = a->s;
    pthread_t *kids = NULL;
    size_t nkids = 0, kids_cap = 0;

    while (!s->shutdown) {
        struct pollfd pfd = {a->listen_fd, POLLIN, 0};
        int pr = poll(&pfd, 1, ACCEPT_POLL_MS);
        if (pr <= 0) {
            continue;
        }
        int fd = accept(a->listen_fd, NULL, NULL);
        if (fd < 0) {
            continue;
        }
        conn_arg *ca = calloc(1, sizeof(conn_arg));
        ca->s = s;
        ca->fd = fd;
        ca->tls = a->tls;
        pthread_t t;
        if (pthread_create(&t, NULL, conn_thread, ca) != 0) {
            free(ca);
            close(fd);
            continue;
        }
        if (nkids == kids_cap) {
            kids_cap = kids_cap ? kids_cap * 2 : 8;
            kids = realloc(kids, kids_cap * sizeof(pthread_t));
        }
        kids[nkids++] = t;
    }
    for (size_t i = 0; i < nkids; i++) {
        pthread_join(kids[i], NULL);
    }
    free(kids);
    fds_remove(s, a->listen_fd);
    close(a->listen_fd);
    free(a);
    return NULL;
}

typedef struct {
    aiomsg_socket *s;
    char host[256];
    uint16_t port;
    SSL_CTX *client_ctx; /* NULL = plain */
    char server_name[256];
} connect_arg;

/* Resolve + connect with a timeout. Returns a connected fd or -1. */
static int dial(const char *host, uint16_t port) {
    char portstr[8];
    snprintf(portstr, sizeof(portstr), "%u", port);
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(host, portstr, &hints, &res) != 0) {
        return -1;
    }
    int fd = -1;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) {
            continue;
        }
        /* non-blocking connect with timeout */
        int flags = fcntl(fd, F_GETFL, 0);
        fcntl(fd, F_SETFL, flags | O_NONBLOCK);
        int rc = connect(fd, ai->ai_addr, ai->ai_addrlen);
        if (rc == 0) {
            fcntl(fd, F_SETFL, flags);
            break;
        }
        if (errno != EINPROGRESS) {
            close(fd);
            fd = -1;
            continue;
        }
        struct pollfd pfd = {fd, POLLOUT, 0};
        if (poll(&pfd, 1, CONNECT_TIMEOUT_MS) == 1 && (pfd.revents & POLLOUT)) {
            int err = 0;
            socklen_t elen = sizeof(err);
            getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &elen);
            if (err == 0) {
                fcntl(fd, F_SETFL, flags);
                break;
            }
        }
        close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    return fd;
}

static void sleep_ms_until_shutdown(aiomsg_socket *s, long ms) {
    long start = now_ms();
    while (now_ms() - start < ms && !s->shutdown) {
        struct timespec ts = {0, 20 * 1000000L};
        nanosleep(&ts, NULL);
    }
}

static void *connect_loop(void *arg) {
    connect_arg *a = arg;
    aiomsg_socket *s = a->s;
    while (!s->shutdown) {
        int fd = dial(a->host, a->port);
        if (fd >= 0) {
            tls_setup tls;
            memset(&tls, 0, sizeof(tls));
            tls.ctx = a->client_ctx;
            tls.is_server = 0;
            snprintf(tls.server_name, sizeof(tls.server_name), "%s", a->server_name);
            conn_session(s, fd, &tls);
        }
        if (s->shutdown) {
            break;
        }
        sleep_ms_until_shutdown(s, RECONNECT_MS);
    }
    if (a->client_ctx) {
        SSL_CTX_free(a->client_ctx);
    }
    free(a);
    return NULL;
}

/* ======================================================================== */
/* Public API                                                                */
/* ======================================================================== */

static int listen_on(const char *host, uint16_t port) {
    char portstr[8];
    snprintf(portstr, sizeof(portstr), "%u", port);
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_PASSIVE;
    if (getaddrinfo(host, portstr, &hints, &res) != 0) {
        return -1;
    }
    int fd = -1;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) {
            continue;
        }
        int one = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
        if (bind(fd, ai->ai_addr, ai->ai_addrlen) == 0 && listen(fd, 128) == 0) {
            break;
        }
        close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    return fd;
}

aiomsg_socket *aiomsg_new(aiomsg_send_mode mode, aiomsg_delivery delivery,
                          const uint8_t *identity) {
    signal(SIGPIPE, SIG_IGN);
    aiomsg_socket *s = calloc(1, sizeof(*s));
    if (!s) {
        return NULL;
    }
    s->mode = mode;
    s->delivery = delivery;
    if (identity) {
        memcpy(s->identity, identity, AIOMSG_IDENTITY_SIZE);
    } else {
        random_bytes(s->identity, AIOMSG_IDENTITY_SIZE);
    }
    pthread_mutex_init(&s->cmd_mtx, NULL);
    pthread_cond_init(&s->cmd_cond, NULL);
    pthread_mutex_init(&s->recv_mtx, NULL);
    pthread_cond_init(&s->recv_cond, NULL);
    pthread_mutex_init(&s->threads_mtx, NULL);
    pthread_mutex_init(&s->fds_mtx, NULL);
    if (pthread_create(&s->broker_thread, NULL, broker_main, s) != 0) {
        free(s);
        return NULL;
    }
    return s;
}

static int do_bind(aiomsg_socket *s, const char *host, uint16_t port, SSL_CTX *ctx) {
    int fd = listen_on(host, port);
    if (fd < 0) {
        return -1;
    }
    fds_add(s, fd);
    accept_arg *a = calloc(1, sizeof(accept_arg));
    a->s = s;
    a->listen_fd = fd;
    a->tls.ctx = ctx;
    a->tls.is_server = 1;
    pthread_t t;
    if (pthread_create(&t, NULL, accept_loop, a) != 0) {
        free(a);
        fds_remove(s, fd);
        close(fd);
        return -1;
    }
    track_thread(s, t);
    return 0;
}

int aiomsg_bind(aiomsg_socket *s, const char *host, uint16_t port) {
    return do_bind(s, host, port, NULL);
}

int aiomsg_bind_tls(aiomsg_socket *s, const char *host, uint16_t port,
                    const char *cert_pem_path, const char *key_pem_path) {
    SSL_CTX *ctx = SSL_CTX_new(TLS_server_method());
    if (!ctx) {
        return -1;
    }
    SSL_CTX_set_min_proto_version(ctx, TLS1_2_VERSION);
    if (SSL_CTX_use_certificate_chain_file(ctx, cert_pem_path) != 1 ||
        SSL_CTX_use_PrivateKey_file(ctx, key_pem_path, SSL_FILETYPE_PEM) != 1) {
        SSL_CTX_free(ctx);
        return -1;
    }
    s->server_ctx = ctx; /* freed in aiomsg_free */
    return do_bind(s, host, port, ctx);
}

static int do_connect(aiomsg_socket *s, const char *host, uint16_t port,
                      SSL_CTX *ctx, const char *server_name) {
    if (s->shutdown) {
        return -1;
    }
    connect_arg *a = calloc(1, sizeof(connect_arg));
    a->s = s;
    snprintf(a->host, sizeof(a->host), "%s", host);
    a->port = port;
    a->client_ctx = ctx;
    snprintf(a->server_name, sizeof(a->server_name), "%s",
             server_name ? server_name : host);
    pthread_t t;
    if (pthread_create(&t, NULL, connect_loop, a) != 0) {
        if (ctx) {
            SSL_CTX_free(ctx);
        }
        free(a);
        return -1;
    }
    track_thread(s, t);
    return 0;
}

int aiomsg_connect(aiomsg_socket *s, const char *host, uint16_t port) {
    return do_connect(s, host, port, NULL, NULL);
}

int aiomsg_connect_tls(aiomsg_socket *s, const char *host, uint16_t port,
                       const char *ca_pem_path, const char *server_name) {
    SSL_CTX *ctx = SSL_CTX_new(TLS_client_method());
    if (!ctx) {
        return -1;
    }
    SSL_CTX_set_min_proto_version(ctx, TLS1_2_VERSION);
    if (SSL_CTX_load_verify_locations(ctx, ca_pem_path, NULL) != 1) {
        SSL_CTX_free(ctx);
        return -1;
    }
    SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER, NULL);
    return do_connect(s, host, port, ctx, server_name);
}

static int dispatch(aiomsg_socket *s, int has_id, const uint8_t *id,
                    const void *data, size_t len) {
    if (s->shutdown) {
        return -1;
    }
    cmd *c = cmd_new(CMD_SEND);
    if (!c) {
        return -1;
    }
    c->has_identity = has_id;
    if (has_id) {
        memcpy(c->identity, id, AIOMSG_IDENTITY_SIZE);
    }
    c->data = malloc(len ? len : 1);
    memcpy(c->data, data, len);
    c->len = len;
    cmd_push(s, c);
    return 0;
}

int aiomsg_send(aiomsg_socket *s, const void *data, size_t len) {
    return dispatch(s, 0, NULL, data, len);
}

int aiomsg_send_to(aiomsg_socket *s, const uint8_t *identity, const void *data, size_t len) {
    return dispatch(s, 1, identity, data, len);
}

int aiomsg_recv(aiomsg_socket *s, void **out_data, size_t *out_len, uint8_t *identity_out) {
    return aiomsg_recv_timeout(s, out_data, out_len, identity_out, -1);
}

int aiomsg_recv_timeout(aiomsg_socket *s, void **out_data, size_t *out_len,
                        uint8_t *identity_out, int timeout_ms) {
    pthread_mutex_lock(&s->recv_mtx);
    long deadline = timeout_ms >= 0 ? now_ms() + timeout_ms : -1;
    while (s->recv_head == NULL && !s->recv_closed) {
        if (deadline < 0) {
            pthread_cond_wait(&s->recv_cond, &s->recv_mtx);
        } else {
            long remain = deadline - now_ms();
            if (remain <= 0) {
                pthread_mutex_unlock(&s->recv_mtx);
                return 0;
            }
            struct timespec ts;
            abstime_in(&ts, remain);
            pthread_cond_timedwait(&s->recv_cond, &s->recv_mtx, &ts);
        }
    }
    if (s->recv_head == NULL) {
        pthread_mutex_unlock(&s->recv_mtx);
        return 0; /* closed */
    }
    bnode *n = s->recv_head;
    s->recv_head = n->next;
    if (!s->recv_head) {
        s->recv_tail = NULL;
    }
    pthread_mutex_unlock(&s->recv_mtx);

    *out_data = n->data;
    *out_len = n->len;
    if (identity_out) {
        memcpy(identity_out, n->identity, AIOMSG_IDENTITY_SIZE);
    }
    free(n);
    return 1;
}

const uint8_t *aiomsg_identity(const aiomsg_socket *s) {
    return s->identity;
}

void aiomsg_close(aiomsg_socket *s) {
    if (s->closed) {
        return;
    }
    s->closed = 1;
    s->shutdown = 1;

    /* Interrupt any blocking accept/handshake/read by shutting every fd. */
    pthread_mutex_lock(&s->fds_mtx);
    for (size_t i = 0; i < s->nfds; i++) {
        shutdown(s->fds[i], SHUT_RDWR);
    }
    pthread_mutex_unlock(&s->fds_mtx);

    cmd *c = cmd_new(CMD_CLOSE);
    if (c) {
        cmd_push(s, c);
    }

    pthread_mutex_lock(&s->threads_mtx);
    size_t n = s->nthreads;
    pthread_mutex_unlock(&s->threads_mtx);
    for (size_t i = 0; i < n; i++) {
        pthread_join(s->threads[i], NULL);
    }
    pthread_join(s->broker_thread, NULL);
}

void aiomsg_free(aiomsg_socket *s) {
    if (!s) {
        return;
    }
    if (!s->closed) {
        aiomsg_close(s);
    }
    for (bnode *n = s->recv_head; n;) {
        bnode *next = n->next;
        free(n->data);
        free(n);
        n = next;
    }
    if (s->server_ctx) {
        SSL_CTX_free(s->server_ctx);
    }
    free(s->threads);
    free(s->fds);
    pthread_mutex_destroy(&s->cmd_mtx);
    pthread_cond_destroy(&s->cmd_cond);
    pthread_mutex_destroy(&s->recv_mtx);
    pthread_cond_destroy(&s->recv_cond);
    pthread_mutex_destroy(&s->threads_mtx);
    pthread_mutex_destroy(&s->fds_mtx);
    free(s);
}
