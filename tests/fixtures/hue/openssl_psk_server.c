#define _POSIX_C_SOURCE 200809L
/* Test-only strict PSK oracle. Arguments must contain dummy credentials only. */
#include <openssl/ssl.h>
#include <arpa/inet.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <unistd.h>

static char expected_identity[256];
static unsigned char expected_key[256];
static unsigned int expected_key_len;

static int unhex(const char *input, unsigned char *output, size_t capacity) {
    size_t length = strlen(input);
    if (length == 0 || length % 2 != 0 || length / 2 > capacity) return -1;
    for (size_t i = 0; i < length / 2; i++) {
        char pair[3] = {input[i * 2], input[i * 2 + 1], 0};
        char *end;
        if (strspn(pair, "0123456789abcdefABCDEF") != 2) return -1;
        output[i] = (unsigned char)strtoul(pair, &end, 16);
        if (*end) return -1;
    }
    return (int)(length / 2);
}

static unsigned int psk_cb(SSL *ssl, const char *identity,
                          unsigned char *out, unsigned int capacity) {
    (void)ssl;
    if (identity == NULL || strcmp(identity, expected_identity) != 0) return 0;
    if (capacity < expected_key_len) return 0;
    memcpy(out, expected_key, expected_key_len);
    return expected_key_len;
}

static int retry(SSL *ssl, int result, int fd) {
    int error = SSL_get_error(ssl, result);
    if (error != SSL_ERROR_WANT_READ && error != SSL_ERROR_WANT_WRITE) return 0;
    struct timeval timeout = {0, 20000};
    struct timeval timer;
    if (DTLSv1_get_timeout(ssl, &timer) &&
        (timer.tv_sec == 0 && timer.tv_usec < timeout.tv_usec)) timeout = timer;
    fd_set readers, writers;
    FD_ZERO(&readers);
    FD_ZERO(&writers);
    if (error == SSL_ERROR_WANT_READ) FD_SET(fd, &readers);
    else FD_SET(fd, &writers);
    if (select(fd + 1, &readers, &writers, NULL, &timeout) < 0 && errno != EINTR) return 0;
    return DTLSv1_handle_timeout(ssl) >= 0;
}

int main(int argc, char **argv) {
    if (argc != 6) return 2;
    int identity_len = unhex(argv[1], (unsigned char *)expected_identity, sizeof(expected_identity) - 1);
    int key_len = unhex(argv[2], expected_key, sizeof(expected_key));
    if (identity_len < 0 || key_len < 0 || memchr(expected_identity, 0, (size_t)identity_len)) return 2;
    expected_key_len = (unsigned int)key_len;
    alarm(8);
    SSL_CTX *ctx = SSL_CTX_new(DTLS_server_method());
    if (!ctx || !SSL_CTX_set_min_proto_version(ctx, DTLS1_2_VERSION) ||
        !SSL_CTX_set_max_proto_version(ctx, DTLS1_2_VERSION) ||
        !SSL_CTX_set_cipher_list(ctx, argv[3])) return 2;
    SSL_CTX_set_psk_server_callback(ctx, psk_cb);
    int fd = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
    struct sockaddr_in local = {.sin_family = AF_INET, .sin_port = 0};
    local.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (fd < 0 || bind(fd, (struct sockaddr *)&local, sizeof(local))) return 2;
    socklen_t address_len = sizeof(local);
    if (getsockname(fd, (struct sockaddr *)&local, &address_len)) return 2;
    FILE *output = fopen(argv[4], "wb");
    FILE *metadata = fopen(argv[5], "wb");
    if (!output || !metadata) return 2;
    printf("READY %u\n", (unsigned int)ntohs(local.sin_port));
    fflush(stdout);
    struct sockaddr_in peer;
    socklen_t peer_len = sizeof(peer);
    unsigned char buffer[65536];
    while (recvfrom(fd, buffer, sizeof(buffer), MSG_PEEK, (struct sockaddr *)&peer, &peer_len) < 0) {
        if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) return 2;
        fd_set readers;
        FD_ZERO(&readers);
        FD_SET(fd, &readers);
        struct timeval timeout = {0, 20000};
        (void)select(fd + 1, &readers, NULL, NULL, &timeout);
    }
    if (connect(fd, (struct sockaddr *)&peer, peer_len)) return 2;
    BIO *bio = BIO_new_dgram(fd, BIO_NOCLOSE);
    SSL *ssl = SSL_new(ctx);
    if (!bio || !ssl || BIO_ctrl(bio, BIO_CTRL_DGRAM_SET_CONNECTED, 0, &peer) <= 0) return 2;
    SSL_set_bio(ssl, bio, bio);
    int result;
    while ((result = SSL_accept(ssl)) != 1) {
        if (!retry(ssl, result, fd)) goto done;
    }
    fprintf(metadata, "%s\n%s\n%s\n", SSL_get_psk_identity(ssl), SSL_get_version(ssl), SSL_get_cipher_name(ssl));
    fflush(metadata);
    for (;;) {
        result = SSL_read(ssl, buffer, sizeof(buffer));
        if (result > 0) {
            if (fwrite(buffer, 1, (size_t)result, output) != (size_t)result) break;
            fflush(output);
        } else if (!retry(ssl, result, fd)) break;
    }
 done:
    fclose(output);
    fclose(metadata);
    SSL_free(ssl);
    SSL_CTX_free(ctx);
    close(fd);
    return 0;
}
