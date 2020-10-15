// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
#define BOOST_CHRONO_VERSION 2

#include "common.hh"
#include "capnproto.hh"
#include "flatbuffers.hh"
#include "protobuf.hh"
#include "protobytes.hh"
#include "extra_malloc.hh"
#include "extra_malloc_no_str.hh"
#include "extra_malloc_no_malloc.hh"
#include "extra_malloc_single_memcpy.hh"
#include "message.hh"
#include <arpa/inet.h>
#include <boost/chrono.hpp>
#include <boost/optional.hpp>
#include <boost/date_time/posix_time/posix_time.hpp>
#include <boost/program_options/options_description.hpp>
#include <boost/program_options/parsers.hpp>
#include <boost/program_options/variables_map.hpp>
#include <cstring>
#include <dmtr/annot.h>
#include <dmtr/latency.h>
#include <dmtr/libos.h>
#include <dmtr/libos/mem.h>
#include <dmtr/sga.h>
#include <dmtr/wait.h>
#include <iostream>
#include <netinet/in.h>
#include <unistd.h>
#include <yaml-cpp/yaml.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <boost/chrono/chrono_io.hpp>
#define DMTR_PROFILE
#include <rte_common.h>
#include <rte_mbuf.h>

namespace po = boost::program_options;
uint64_t sent = 0, recved = 0;
dmtr_latency_t *latency = NULL;
int qd;
dmtr_sgarray_t sga = {};
echo_message *echo = NULL;
bool free_buf = false;
boost::chrono::time_point<boost::chrono::system_clock> exp_start;
boost::chrono::time_point<boost::chrono::system_clock> exp_end;


//#define TRAILING_REQUESTS 
//#define WAIT_FOR_ALL
void finish() {
    exp_end = boost::chrono::system_clock::now();
    auto dt = exp_end - exp_start;
    using namespace boost::chrono;
    std::cout << time_fmt(boost::chrono::timezone::local, "%H:%M:%S") <<
    system_clock::now() << '\n';
    std::cerr << "Start: " << time_fmt(boost::chrono::timezone::local) << exp_start << "; End: " <<  time_fmt(boost::chrono::timezone::local) << exp_end << "; Total time taken: " << dt.count() << std::endl;
    std::cerr << "Sent: " << sent << "  Recved: " << recved << std::endl;
    if (free_buf) {
        dmtr_sgafree(&sga);
    }
    dmtr_close(qd);
    dmtr_dump_latency(stderr, latency);
}

void sig_handler(int signo)
{
    finish();
    exit(0);
}


int main(int argc, char *argv[]) {
    parse_args(argc, argv, false);
    // SGAs will be allocated with this many segments
    int num_send_segments = sga_size;
    int num_recv_segments = 1;
    DMTR_OK(dmtr_init(argc, argv));

    DMTR_OK(dmtr_new_latency(&latency, "end-to-end"));

    DMTR_OK(dmtr_socket(&qd, AF_INET, SOCK_STREAM, 0));
    printf("client qd:\t%d\n", qd);

        
    struct sockaddr_in saddr = {};
    saddr.sin_family = AF_INET;
    const char *server_ip = boost::get(server_ip_addr).c_str();
    if (inet_pton(AF_INET, server_ip, &saddr.sin_addr) != 1) {
        std::cerr << "Unable to parse IP address." << std::endl;
        return -1;
    }
    saddr.sin_port = htons(port);

    std::cerr << "Attempting to connect to `" << boost::get(server_ip_addr) << ":" << port << "`..." << std::endl;
    dmtr_qtoken_t q;
    DMTR_OK(dmtr_connect(&q, qd, reinterpret_cast<struct sockaddr *>(&saddr), sizeof(saddr)));

    dmtr_qresult_t qr = {};
    DMTR_OK(dmtr_wait(&qr, q));
    std::cerr << "Connected." << std::endl;


    // init serialization messages so variables aren't freed prematurely
    protobuf_echo protobuf_data(packet_size, message);
    protobuf_bytes_echo proto_bytes_data(packet_size, message);
    capnproto_echo capnproto_data(packet_size, message);
    flatbuffers_echo flatbuffers_data(packet_size, message);
    malloc_baseline malloc_baseline_echo(packet_size, message);
    malloc_baseline_no_str malloc_baseline_no_str(packet_size, message);
    malloc_baseline_no_malloc malloc_baseline_no_malloc(packet_size, message);
    malloc_baseline_single_memcpy malloc_baseline_single_memcpy(packet_size, message);

    if (zero_copy) {
        dmtr_set_zero_copy();
        sga.sga_numsegs = num_send_segments; // so it doesn't error out
        DMTR_OK(dmtr_init_mempools(packet_size, num_send_segments));
        printf("Done initializing mempools\n");
        free_buf = true;
    } else {
        // if not running serialization test, send normal "aaaaa";
        if (!run_protobuf_test) {
            fill_in_sga(sga, num_send_segments);
            free_buf = true;
        } else if (!std::strcmp(cereal_system.c_str(), "malloc_baseline")) {
            fill_in_sga(sga, num_send_segments);
            echo = &malloc_baseline_echo;
            echo->serialize_message(sga);
            free_buf = true;
        } else if (!std::strcmp(cereal_system.c_str(), "malloc_no_str")) {
            fill_in_sga(sga, num_send_segments);
            echo = &malloc_baseline_no_str;
            echo->serialize_message(sga);
            free_buf = true;
        } else if (!std::strcmp(cereal_system.c_str(), "memcpy")) {
            fill_in_sga(sga, num_send_segments);
            echo = &malloc_baseline_no_malloc;
            echo->serialize_message(sga);
            free_buf = true;
        } else if (!std::strcmp(cereal_system.c_str(), "single_memcpy")) {
            fill_in_sga(sga, num_send_segments);
            echo = &malloc_baseline_single_memcpy;
            echo->serialize_message(sga);
            free_buf = true;
        } else if (!std::strcmp(cereal_system.c_str(), "protobuf")) {
            allocate_segments(&sga, num_send_segments);
            echo = &protobuf_data;
            echo->serialize_message(sga);
            free_buf = true;
        } else if (!std::strcmp(cereal_system.c_str(), "protobytes")) {
            allocate_segments(&sga, num_send_segments);
            echo = &proto_bytes_data;
            echo->serialize_message(sga);
            free_buf = true;
        } else if (!std::strcmp(cereal_system.c_str(), "capnproto")) {
            allocate_segments(&sga, num_send_segments);
            echo = &capnproto_data;
            echo->serialize_message(sga);
        } else if (!std::strcmp(cereal_system.c_str(), "flatbuffers")) {
            allocate_segments(&sga, num_send_segments);
            echo = &flatbuffers_data;
            echo->serialize_message(sga);
        } else {
            std::cerr << "Serialization cereal_system " << cereal_system << " unknown." << std::endl;
            exit(1);
        }
    }
    uint32_t last_sent = 0;
    exp_start = boost::chrono::system_clock::now();
    // run a simpler test with retries turned on
    if (retries) {

        // one more iteration: don't count first
        iterations += 1;
        // set up timers for each client
        int timer_qds[clients];

        for (uint32_t c = 0; c < clients; c++) {
            DMTR_OK(dmtr_new_timer(&timer_qds[c]));
        }

        int ret;
        int num_retries = 0;
        dmtr_qresult_t wait_out;
        allocate_segments(&wait_out.qr_value.sga, num_recv_segments);

        dmtr_qtoken_t push_tokens[clients];
        dmtr_qtoken_t pop_tokens[clients*2];
        dmtr_qtoken_t timer_q_push[clients];
        // timeout is 100 us
        int timeout = 500000;

        boost::chrono::time_point<boost::chrono::steady_clock> start_times[clients];
        boost::chrono::time_point<boost::chrono::steady_clock> timer_times[clients];
        int current_packet[clients]; // which packet ID are these clients sending
        
        // set up our signal handlers
        if (signal(SIGINT, sig_handler) == SIG_ERR)
            std::cout << "\ncan't catch SIGINT\n";

        // start all the clients
        for (uint32_t c = 0; c < clients; c++) {
            // last sent packet
            last_sent += 1;
            current_packet[c] = last_sent;
            sga.id = last_sent;

            // first push and pop
            DMTR_OK(dmtr_push(&push_tokens[c], qd, &sga));
            sent++;
            DMTR_OK(dmtr_pop(&pop_tokens[2*c], qd));
            start_times[c] = boost::chrono::steady_clock::now();
            
            // reset for this client
            DMTR_OK(dmtr_push_tick(&timer_q_push[c], timer_qds[c], timeout));
            timer_times[c] = boost::chrono::steady_clock::now();
            DMTR_OK(dmtr_pop(&pop_tokens[2*c+1], timer_qds[c]));
        } 

        int idx = 0;

        do {
            // wait for a returned value
            ret =  dmtr_wait_any(&wait_out, &idx, pop_tokens, clients*2);

            // one of clients timed out
            if (idx % 2 != 0) {
                // client idx corresponding to timer idx
                uint32_t c = (idx - 1)/2;

                // first packet takes longer
                if (recved != 0) {
                    // actual retry
                    num_retries++;

                    // count time since timer fire
                    auto dt = boost::chrono::steady_clock::now() - timer_times[c];
                    auto sent_dt = boost::chrono::steady_clock::now() - start_times[c];
                    std::cout << "Idx " << idx << " fired after " << dt.count() << " time, since sent: " << sent_dt.count() << " time, recvd so far: " << recved << ", pkt id: " << current_packet[c] <<  std::endl;
                    // drop the push token from this echo
                    if (push_tokens[c] != 0) {
                        DMTR_OK(dmtr_drop(push_tokens[c]));
                        push_tokens[c] = 0;
                    }
                    sga.id = current_packet[c]; // make sure pushed packet has correct ID
                
                    DMTR_OK(dmtr_push(&push_tokens[c], qd, &sga));
                }
                
                // reset the timeout
                if (timer_q_push[c] != 0) {
                    DMTR_OK(dmtr_drop(timer_q_push[c]));
                    timer_q_push[c] = 0;
                }
                
                DMTR_OK(dmtr_push_tick(&timer_q_push[c], timer_qds[c], timeout));
                timer_times[c] = boost::chrono::steady_clock::now();
                DMTR_OK(dmtr_pop(&pop_tokens[2*c+1], timer_qds[c]));
                //start_times[c] = boost::chrono::steady_clock::now();
                continue;
            } else {
                uint32_t c = idx/2;
                auto dt = boost::chrono::steady_clock::now() - start_times[c];

                // check that the  message ID is correct for this client
                if (wait_out.qr_value.sga.id < current_packet[c]) {
                    // old packet coming to haunt us
                    printf("Received pkt with old id\n");
                    continue;
                } else if (wait_out.qr_value.sga.id > current_packet[c]) {
                    std::cerr << "Cannot have packet ID, " << wait_out.qr_value.sga.id << ", greater than current received: " << current_packet[c] << std::endl;
                } else {
                    last_sent += 1;
                    current_packet[c] = last_sent;
                    sga.id = current_packet[c];
                }
                // ping came back
                if (recved != 0) {
                    DMTR_OK(dmtr_record_latency(latency, dt.count()));
                }
                recved++;
                iterations--;

                // finished a full echo
                // drop the push token from this echo
                if (push_tokens[c] != 0) {
                    DMTR_OK(dmtr_drop(push_tokens[c]));
                    push_tokens[c] = 0;
                }
                // drop timer token from this echo
                if (timer_q_push[c] != 0) {
                    DMTR_OK(dmtr_drop(timer_q_push[c]));
                    timer_q_push[c] = 0;
                }

                // free the recv buffer
                DMTR_OK(dmtr_sgafree(&wait_out.qr_value.sga));
                if (wait_out.qr_value.sga.recv_segments != NULL) {
                    rte_pktmbuf_free((struct rte_mbuf*) wait_out.qr_value.sga.recv_segments);
                }


                // send a new packet and reset timer
                DMTR_OK(dmtr_push(&push_tokens[c], qd, &sga));
                sent++;
                DMTR_OK(dmtr_pop(&pop_tokens[2*c], qd));
                start_times[c] = boost::chrono::steady_clock::now();
                
                // set timeout
                DMTR_OK(dmtr_push_tick(&timer_q_push[c], timer_qds[c], timeout));
                timer_times[c] = boost::chrono::steady_clock::now();
            }   
        } while (iterations > 0 && ret == 0);
        std::cout << "Final num retries: " << num_retries << std::endl;
        finish();
        exit(0);

        // free the wait out segment buffers
        DMTR_OK(dmtr_sgafree_segments(&wait_out.qr_value.sga));
    } 

    std::cerr << "Number of clients: " << clients << std::endl;

    dmtr_qtoken_t push_tokens[clients];
    dmtr_qtoken_t pop_tokens[clients];
    uint32_t current_packet[clients];
    boost::chrono::time_point<boost::chrono::steady_clock> start_times[clients];

    // set up our signal handlers
    if (signal(SIGINT, sig_handler) == SIG_ERR)
        std::cout << "\ncan't catch SIGINT\n";
 
    // start all the clients
    for (uint32_t c = 0; c < clients; c++) {
        last_sent++;
        current_packet[c] = last_sent;
        sga.id = last_sent;
        // push message to server
        DMTR_OK(dmtr_push(&push_tokens[c], qd, &sga));
        sent++;
        // async pop
        DMTR_OK(dmtr_pop(&pop_tokens[c], qd));
        // record start time
        start_times[c] = boost::chrono::steady_clock::now();
    }
    
    int ret;
    dmtr_qresult_t wait_out;
    allocate_segments(&wait_out.qr_value.sga, num_recv_segments);
    // receive SGA: should have 1 segment if we're in DMTR_NO_SER,
    // otherwise, the receive sga will have the normal number of segments
    
    //iterations *= clients;
    do {
#ifdef WAIT_FOR_ALL
        // wait for all the clients
        for (uint32_t c = 0; c < clients; c++) {
            ret = dmtr_wait(&wait_out, pop_tokens[c]);
            recved++;
            DMTR_OK(dmtr_drop(push_tokens[c]));
            DMTR_OK(dmtr_sgafree(&wait_out.qr_value.sga));
            if (wait_out.qr_value.sga.recv_segments != NULL) {
                rte_pktmbuf_free((struct rte_mbuf*) wait_out.qr_value.sga.recv_segments);
            }
            // count the iteration
            iterations--;
        }
        auto dt = boost::chrono::steady_clock::now() - start_times[0];
        DMTR_OK(dmtr_record_latency(latency, dt.count()));
        // restart the clock
        start_times[0] = boost::chrono::steady_clock::now();
        // send all again
        for (uint32_t c = 0; c < clients; c++) {
#ifndef TRAILING_REQUESTS        
            // if there are fewer than clients left, then we just wait for the responses
            if (iterations < clients) {
                pop_tokens[c] = 0;
                continue;
            }
#endif
            // start again
            // push back to the server
            last_sent += 1;
            current_packet[c] = last_sent;
            sga.id = last_sent;
            DMTR_OK(dmtr_push(&push_tokens[c], qd, &sga));
            sent++;
            // async pop
            DMTR_OK(dmtr_pop(&pop_tokens[c], qd));
        }
#else
        int idx = 0;
        // wait for a returned value
        ret =  dmtr_wait_any(&wait_out, &idx, pop_tokens, clients);
        // handle the returned value
        //record the time
        auto dt = boost::chrono::steady_clock::now() - start_times[idx];
        DMTR_OK(dmtr_record_latency(latency, dt.count()));
        // should be done by now
        //DMTR_OK(dmtr_wait(NULL, push_tokens[idx]));
        //DMTR_TRUE(ENOTSUP, DMTR_OPC_POP == qr.qr_opcode);
        //DMTR_TRUE(ENOTSUP, qr.qr_value.sga.sga_numsegs == 1);
        //DMTR_TRUE(ENOTSUP, (reinterpret_cast<uint8_t *>(qr.qr_value.sga.sga_segs[0].sgaseg_buf)[0] == FILL_CHAR);
        //DMTR_OK(dmtr_wait(NULL, qt));
        recved++;

        // finished a full echo
        // free the allocated sga
        DMTR_OK(dmtr_sgafree(&wait_out.qr_value.sga));
        if (wait_out.qr_value.sga.recv_segments != NULL) {
            rte_pktmbuf_free((struct rte_mbuf*) wait_out.qr_value.sga.recv_segments);
        }
        // count the iteration
        iterations--;
        // drop the push token from this echo
        if (push_tokens[idx] != 0) {
            DMTR_OK(dmtr_drop(push_tokens[idx]));
            push_tokens[idx] = 0;
        }

#ifndef TRAILING_REQUESTS        
        // if there are fewer than clients left, then we just wait for the responses
        if (iterations < clients) {
            pop_tokens[idx] = 0;
            continue;
        }
#endif
        last_sent++;
        current_packet[idx] = last_sent;
        sga.id = last_sent;
        // start again
        // push back to the server
        DMTR_OK(dmtr_push(&push_tokens[idx], qd, &sga));
        sent++;
        // async pop
        DMTR_OK(dmtr_pop(&pop_tokens[idx], qd));
        // restart the clock
        start_times[idx] = boost::chrono::steady_clock::now();
#endif
    } while (iterations > 0 && ret == 0);

    finish();
    exit(0);
    
    // free the wait out segment buffers
    DMTR_OK(dmtr_sgafree_segments(&wait_out.qr_value.sga));
}
