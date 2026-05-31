/* Conformance test agent for the cross-language interop suite (C).
 * Same CLI as the other agents; a sink prints each received message on its own
 * line and exits after --count messages. */
#include "aiomsg.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static const char *arg(int argc, char **argv, const char *key, const char *def) {
    for (int i = 1; i + 1 < argc; i++) {
        if (strncmp(argv[i], "--", 2) == 0 && strcmp(argv[i] + 2, key) == 0) {
            return argv[i + 1];
        }
    }
    return def;
}

static void parse_identity(const char *hex, uint8_t out[16]) {
    memset(out, 0, 16);
    for (int i = 0; i < 16 && hex[i * 2] && hex[i * 2 + 1]; i++) {
        char byte[3] = {hex[i * 2], hex[i * 2 + 1], 0};
        out[i] = (uint8_t)strtol(byte, NULL, 16);
    }
}

int main(int argc, char **argv) {
    const char *role = arg(argc, argv, "role", "connect");
    const char *host = arg(argc, argv, "host", "127.0.0.1");
    uint16_t port = (uint16_t)atoi(arg(argc, argv, "port", "25000"));
    const char *send_mode = arg(argc, argv, "send-mode", "roundrobin");
    const char *behavior = arg(argc, argv, "behavior", "sink");
    int count = atoi(arg(argc, argv, "count", "10"));
    const char *prefix = arg(argc, argv, "prefix", "m");
    const char *delivery = arg(argc, argv, "delivery", "at-most-once");
    const char *identity_hex = arg(argc, argv, "identity", NULL);
    double linger = atof(arg(argc, argv, "linger", "1.0"));
    int tls = strcmp(arg(argc, argv, "tls", "false"), "true") == 0;
    const char *tls_cert = arg(argc, argv, "tls-cert", "");
    const char *tls_key = arg(argc, argv, "tls-key", "");
    const char *tls_ca = arg(argc, argv, "tls-ca", "");
    const char *server_name = arg(argc, argv, "tls-server-name", NULL);

    aiomsg_send_mode mode = strcmp(send_mode, "publish") == 0 ? AIOMSG_PUBLISH
                                                              : AIOMSG_ROUNDROBIN;
    aiomsg_delivery del = strcmp(delivery, "at-least-once") == 0 ? AIOMSG_AT_LEAST_ONCE
                                                                 : AIOMSG_AT_MOST_ONCE;
    uint8_t id[16];
    const uint8_t *idp = NULL;
    if (identity_hex) {
        parse_identity(identity_hex, id);
        idp = id;
    }

    aiomsg_socket *s = aiomsg_new(mode, del, idp);
    if (!s) {
        fprintf(stderr, "alloc failed\n");
        return 1;
    }

    int rc;
    if (strcmp(role, "bind") == 0) {
        rc = tls ? aiomsg_bind_tls(s, host, port, tls_cert, tls_key)
                 : aiomsg_bind(s, host, port);
    } else {
        rc = tls ? aiomsg_connect_tls(s, host, port, tls_ca, server_name)
                 : aiomsg_connect(s, host, port);
    }
    if (rc != 0) {
        fprintf(stderr, "%s failed\n", role);
        return 1;
    }

    if (strcmp(behavior, "source") == 0) {
        for (int i = 0; i < count; i++) {
            char msg[64];
            int n = snprintf(msg, sizeof(msg), "%s%d", prefix, i);
            aiomsg_send(s, msg, (size_t)n);
        }
        struct timespec ts = {(time_t)linger, (long)((linger - (long)linger) * 1e9)};
        nanosleep(&ts, NULL);
    } else if (strcmp(behavior, "echo") == 0) {
        for (int i = 0; i < count; i++) {
            void *data;
            size_t len;
            uint8_t sender[16];
            if (aiomsg_recv(s, &data, &len, sender) != 1) {
                break;
            }
            aiomsg_send_to(s, sender, data, len);
            free(data);
        }
        struct timespec ts = {(time_t)linger, (long)((linger - (long)linger) * 1e9)};
        nanosleep(&ts, NULL);
    } else { /* sink */
        for (int i = 0; i < count; i++) {
            void *data;
            size_t len;
            if (aiomsg_recv(s, &data, &len, NULL) != 1) {
                break;
            }
            fwrite(data, 1, len, stdout);
            fputc('\n', stdout);
            fflush(stdout);
            free(data);
        }
    }

    aiomsg_close(s);
    aiomsg_free(s);
    return 0;
}
