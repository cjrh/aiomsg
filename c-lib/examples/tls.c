/* Self-contained TLS demo: a bind socket and a connect socket talking over
 * OpenSSL in one process.
 *
 *     ./tls [cert-dir]     # cert-dir defaults to ../conformance/certs
 *
 * It loads the repository's shared self-signed certificate (EC P-256, with
 * SANs for "localhost" and 127.0.0.1). A real deployment points cert/key at its
 * own PEM files and trusts the issuing CA via the ca path. The TCP target and
 * the verified name are independent: we dial 127.0.0.1 but verify "localhost". */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "aiomsg.h"

int main(int argc, char **argv) {
    const char *dir = argc > 1 ? argv[1] : "../conformance/certs";
    char cert[512], key[512];
    snprintf(cert, sizeof cert, "%s/cert.pem", dir);
    snprintf(key, sizeof key, "%s/key.pem", dir);

    aiomsg_socket *server = aiomsg_new(AIOMSG_PUBLISH, AIOMSG_AT_MOST_ONCE, NULL);
    if (aiomsg_bind_tls(server, "127.0.0.1", 25001, cert, key) != 0) {
        fprintf(stderr, "bind_tls failed (is %s readable?)\n", cert);
        return 1;
    }
    printf("server bound (TLS) on 127.0.0.1:25001\n");

    aiomsg_socket *client = aiomsg_new(AIOMSG_ROUNDROBIN, AIOMSG_AT_MOST_ONCE, NULL);
    if (aiomsg_connect_tls(client, "127.0.0.1", 25001, cert, "localhost") != 0) {
        fprintf(stderr, "connect_tls failed\n");
        return 1;
    }

    const char *msg = "hello over TLS";
    aiomsg_send(server, msg, strlen(msg));

    void *data;
    size_t len;
    if (aiomsg_recv_timeout(client, &data, &len, NULL, 5000) == 1) {
        printf("client received: %.*s\n", (int)len, (char *)data);
        free(data);
    } else {
        printf("timed out\n");
    }

    aiomsg_close(client);
    aiomsg_free(client);
    aiomsg_close(server);
    aiomsg_free(server);
    return 0;
}
