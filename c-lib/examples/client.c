/* The connect end of the two-process demo: prints whatever the server sends,
 * reconnecting automatically if the server restarts.
 *
 *     ./server     # in another terminal
 *     ./client     # this program */
#include <stdio.h>
#include <stdlib.h>

#include "aiomsg.h"

int main(void) {
    aiomsg_socket *sock = aiomsg_new(AIOMSG_ROUNDROBIN, AIOMSG_AT_MOST_ONCE, NULL);
    if (aiomsg_connect(sock, "127.0.0.1", 25000) != 0) {
        fprintf(stderr, "connect failed\n");
        return 1;
    }

    void *data;
    size_t len;
    /* aiomsg_recv returns 1 per message and 0 once the socket is closed. */
    while (aiomsg_recv(sock, &data, &len, NULL) == 1) {
        printf("%.*s\n", (int)len, (char *)data);
        free(data); /* recv hands ownership of the buffer to the caller */
    }

    aiomsg_close(sock);
    aiomsg_free(sock);
    return 0;
}
