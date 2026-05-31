/* aiomsg — native C smart sockets.
 *
 * A single opaque `aiomsg_socket` multiplexes many TCP connections behind one
 * handle, with ZMQ-like distribution patterns (publish / round-robin /
 * by-identity), automatic reconnection, send buffering, heartbeating, an
 * optional at-least-once delivery guarantee, and TLS (OpenSSL). It speaks the
 * language-independent aiomsg wire protocol (see ../PROTOCOL.md) and
 * interoperates with the Python reference and every other port.
 *
 * Synchronous / blocking, backed by background threads (pthreads). The handle is
 * safe to use from multiple threads: send from one, recv on another.
 */
#ifndef AIOMSG_H
#define AIOMSG_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct aiomsg_socket aiomsg_socket;

typedef enum {
    AIOMSG_ROUNDROBIN = 0, /* each message to one peer, cycling */
    AIOMSG_PUBLISH = 1,    /* a copy to every connected peer */
} aiomsg_send_mode;

typedef enum {
    AIOMSG_AT_MOST_ONCE = 0, /* buffer when no peers, never ack/retry */
    AIOMSG_AT_LEAST_ONCE = 1, /* ack + retry (round-robin / by-identity only) */
} aiomsg_delivery;

/* Create a socket. `identity` is 16 bytes, or NULL to generate a random one. */
aiomsg_socket *aiomsg_new(aiomsg_send_mode mode, aiomsg_delivery delivery,
                          const uint8_t *identity);

/* Listen on host:port and accept many peers. Returns 0 on success, -1 on error.
 * May be combined with aiomsg_connect. */
int aiomsg_bind(aiomsg_socket *s, const char *host, uint16_t port);
/* Like aiomsg_bind, but every accepted connection is wrapped in a TLS server
 * handshake using the PEM certificate chain and private key at the given paths. */
int aiomsg_bind_tls(aiomsg_socket *s, const char *host, uint16_t port,
                    const char *cert_pem_path, const char *key_pem_path);

/* Connect to host:port, reconnecting for the life of the socket. Returns
 * immediately (0 on success). May be called multiple times. */
int aiomsg_connect(aiomsg_socket *s, const char *host, uint16_t port);
/* Like aiomsg_connect, but performs a TLS client handshake, trusting the CA PEM
 * at `ca_pem_path` and verifying the peer certificate against `server_name`
 * (NULL means use `host`). */
int aiomsg_connect_tls(aiomsg_socket *s, const char *host, uint16_t port,
                       const char *ca_pem_path, const char *server_name);

/* Send `data` (copied) to peers per the send mode. Buffered if no peers yet. */
int aiomsg_send(aiomsg_socket *s, const void *data, size_t len);
/* Send `data` directly to the peer with the given 16-byte identity. */
int aiomsg_send_to(aiomsg_socket *s, const uint8_t *identity, const void *data, size_t len);

/* Block for the next message. On a message: returns 1, sets *out_data to a
 * freshly malloc'd buffer (caller frees) of *out_len bytes, and copies the
 * 16-byte sender identity into identity_out if non-NULL. Returns 0 once the
 * socket is closed, -1 on error. */
int aiomsg_recv(aiomsg_socket *s, void **out_data, size_t *out_len, uint8_t *identity_out);
/* Like aiomsg_recv, but returns 0 if no message arrives within timeout_ms. */
int aiomsg_recv_timeout(aiomsg_socket *s, void **out_data, size_t *out_len,
                        uint8_t *identity_out, int timeout_ms);

/* This socket's 16-byte identity. */
const uint8_t *aiomsg_identity(const aiomsg_socket *s);

/* Gracefully shut down: stop accepting/connecting, close connections, stop the
 * broker. Idempotent. After this, aiomsg_recv returns 0. */
void aiomsg_close(aiomsg_socket *s);
/* Free the socket (call once, after aiomsg_close). */
void aiomsg_free(aiomsg_socket *s);

#ifdef __cplusplus
}
#endif

#endif /* AIOMSG_H */
