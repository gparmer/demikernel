#include <dmtr/annot.h>
#include <dmtr/libos.h>
#include <dmtr/mem.h>
#include <dmtr/wait.h>
#include <libos/common/latency.h>

#include <netinet/in.h>
#include <assert.h>
#include <arpa/inet.h>

#include <cassert>
#include <cstring>
#include <unistd.h>
#include "common.hh"

int main(int argc, char **argv)
{

    parse_args(argc, argv);
    DMTR_OK(dmtr_init(dmtr_argc, dmtr_argv));

    int lqd = 0;
    DMTR_OK(dmtr_socket(&lqd, AF_INET, SOCK_STREAM, 0));
    //printf("listen qd:\t%d\n", lqd);

    struct sockaddr_in saddr = {};
    saddr.sin_family = AF_INET;
    if (inet_pton(AF_INET, ip.c_str(), &saddr.sin_addr) != 1) {
        printf("Address not supported!\n");
        return -1;
    }
    // todo: this should be done from within the libos.
    saddr.sin_port = htons(port);
    DMTR_OK(dmtr_bind(lqd, reinterpret_cast<struct sockaddr *>(&saddr), sizeof(saddr)));

    //printf("listening for connections\n");
    DMTR_OK(dmtr_listen(lqd, 3));

    int qd = 0;
    struct sockaddr_in paddr = {};
    socklen_t paddrsz = sizeof(struct sockaddr_in);
    int err = EAGAIN;
    while (err == EAGAIN) {
        sleep(1);
        err = dmtr_accept(&qd, reinterpret_cast<struct sockaddr *>(&paddr), &paddrsz, lqd);
    }
    DMTR_OK(err);
    //printf("accepted connection from: %x:%d\n", paddr.sin_addr.s_addr, paddr.sin_port);

    // process ITERATION_COUNT packets from client
    for (size_t i = 0; i < iterations; i++) {
        dmtr_sgarray_t sga = {};
        dmtr_qtoken_t qt = 0;
        DMTR_OK(dmtr_pop(&qt, qd));
        DMTR_OK(dmtr_wait(&sga, qt));
        //DMTR_OK(dmtr_drop(qt));
        DMTR_TRUE(EPERM, sga.sga_numsegs == 1);
        DMTR_TRUE(EPERM, sga.sga_segs[0].sgaseg_len == packet_size);
        //fprintf(stderr, "[%lu] server: rcvd\t%s\tbuf size:\t%d\n", i, reinterpret_cast<char *>(sga.sga_segs[0].sgaseg_buf), sga.sga_segs[0].sgaseg_len);
        DMTR_OK(dmtr_push(&qt, qd, &sga));
        DMTR_OK(dmtr_wait(NULL, qt));
        //DMTR_OK(dmtr_drop(qt));

        //fprintf(stderr, "send complete.\n");
        free(sga.sga_buf);
    }

    Latency_DumpAll();
    DMTR_OK(dmtr_close(qd));
    DMTR_OK(dmtr_close(lqd));

    return 0;
}
