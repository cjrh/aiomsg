/* The bind end of the two-process demo: publishes the time once a second.
 *
 *     ./server     # this program
 *     ./client     # in another terminal
 *
 * Build the examples with `just examples` (or enable -DAIOMSG_EXAMPLES=ON). */
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "aiomsg.h"

int main(void) {
    aiomsg_socket *sock = aiomsg_new(AIOMSG_PUBLISH, AIOMSG_AT_MOST_ONCE, NULL);
    if (aiomsg_bind(sock, "127.0.0.1", 25000) != 0) {
        fprintf(stderr, "bind failed\n");
        return 1;
    }
    printf("bound to 127.0.0.1:25000, publishing the time...\n");

    for (;;) {
        char buf[64];
        time_t now = time(NULL);
        size_t n = strftime(buf, sizeof buf, "%Y-%m-%dT%H:%M:%S", localtime(&now));
        aiomsg_send(sock, buf, n);
        sleep(1);
    }

    aiomsg_close(sock);
    aiomsg_free(sock);
    return 0;
}
