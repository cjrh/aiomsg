/* Integration tests: two sockets over real loopback (plain TCP and TLS). */
#include "aiomsg.h"

#include <arpa/inet.h>
#include <assert.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

/* Grab a likely-free ephemeral port (small TOCTOU race, fine for a local test). */
static uint16_t free_port(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = 0;
    bind(fd, (struct sockaddr *)&a, sizeof(a));
    socklen_t len = sizeof(a);
    getsockname(fd, (struct sockaddr *)&a, &len);
    uint16_t port = ntohs(a.sin_port);
    close(fd);
    return port;
}

static void expect_message(aiomsg_socket *sink, const char *want) {
    void *data = NULL;
    size_t len = 0;
    int r = aiomsg_recv_timeout(sink, &data, &len, NULL, 5000);
    assert(r == 1);
    assert(len == strlen(want) && memcmp(data, want, len) == 0);
    free(data);
}

static void test_tcp(void) {
    uint16_t port = free_port();
    aiomsg_socket *server = aiomsg_new(AIOMSG_ROUNDROBIN, AIOMSG_AT_MOST_ONCE, NULL);
    assert(aiomsg_bind(server, "127.0.0.1", port) == 0);
    aiomsg_socket *client = aiomsg_new(AIOMSG_ROUNDROBIN, AIOMSG_AT_MOST_ONCE, NULL);
    assert(aiomsg_connect(client, "127.0.0.1", port) == 0);

    /* Sent before the client necessarily connects: exercises buffering. */
    aiomsg_send(server, "hello", 5);
    expect_message(client, "hello");

    aiomsg_close(server);
    aiomsg_close(client);
    aiomsg_free(server);
    aiomsg_free(client);
    printf("tcp OK\n");
}

static void test_tls(void) {
    uint16_t port = free_port();
    aiomsg_socket *server = aiomsg_new(AIOMSG_ROUNDROBIN, AIOMSG_AT_MOST_ONCE, NULL);
    assert(aiomsg_bind_tls(server, "127.0.0.1", port,
                           CERT_DIR "/cert.pem", CERT_DIR "/key.pem") == 0);
    aiomsg_socket *client = aiomsg_new(AIOMSG_ROUNDROBIN, AIOMSG_AT_MOST_ONCE, NULL);
    /* server_name NULL -> verify against host "127.0.0.1" (matches the IP SAN). */
    assert(aiomsg_connect_tls(client, "127.0.0.1", port,
                              CERT_DIR "/cert.pem", NULL) == 0);

    aiomsg_send(server, "secure hello", 12);
    expect_message(client, "secure hello");

    aiomsg_close(server);
    aiomsg_close(client);
    aiomsg_free(server);
    aiomsg_free(client);
    printf("tls OK\n");
}

int main(void) {
    test_tcp();
    test_tls();
    printf("integration tests OK\n");
    return 0;
}
