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
static FILE *metadata;

/* Observe actual OpenSSL-generated alerts; never create records or do crypto. */
static void message_cb(int writing, int version, int type, const void *data,
                       size_t length, SSL *ssl, void *arg) {
    (void)version; (void)ssl; (void)arg;
    const unsigned char *bytes = data;
    if (writing && type == SSL3_RT_ALERT && length == 2 && bytes[0] == 2) {
        fprintf(metadata, "FATAL %u\n", (unsigned int)bytes[1]);
        fflush(metadata);
    }
}

static int certificate(SSL_CTX *ctx) {
    EVP_PKEY *key = EVP_RSA_gen(2048);
    X509 *cert = X509_new();
    int ok = 0;
    if (!key || !cert || !X509_set_version(cert, 2) ||
        !ASN1_INTEGER_set(X509_get_serialNumber(cert), 1) ||
        !X509_gmtime_adj(X509_getm_notBefore(cert), 0) ||
        !X509_gmtime_adj(X509_getm_notAfter(cert), 3600) ||
        !X509_set_pubkey(cert, key)) goto done;
    X509_NAME *name = X509_get_subject_name(cert);
    if (!X509_NAME_add_entry_by_txt(name, "CN", MBSTRING_ASC,
        (const unsigned char *)"hue-test-only", -1, -1, 0) ||
        !X509_set_issuer_name(cert, name) || !X509_sign(cert, key, EVP_sha256()) ||
        !SSL_CTX_use_certificate(ctx, cert) || !SSL_CTX_use_PrivateKey(ctx, key)) goto done;
    ok = 1;
 done:
    X509_free(cert);
    EVP_PKEY_free(key);
    return ok;
}

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
    if (argc != 9) return 2;
    int identity_len = unhex(argv[1], (unsigned char *)expected_identity, sizeof(expected_identity) - 1);
    int key_len = unhex(argv[2], expected_key, sizeof(expected_key));
    if (identity_len < 0 || key_len < 0 || memchr(expected_identity, 0, (size_t)identity_len)) return 2;
    expected_key_len = (unsigned int)key_len;
    alarm(8);
    SSL_CTX *ctx = SSL_CTX_new(DTLS_server_method());
    if (!ctx || !SSL_CTX_set_min_proto_version(ctx, DTLS1_2_VERSION) ||
        !SSL_CTX_set_max_proto_version(ctx, DTLS1_2_VERSION) ||
        !SSL_CTX_set_cipher_list(ctx, argv[3])) return 2;
    if (!strcmp(argv[6], "non-ems")) SSL_CTX_set_options(ctx, SSL_OP_NO_EXTENDED_MASTER_SECRET);
    if (!strcmp(argv[8], "certificate")) {
        if (!certificate(ctx) || !SSL_CTX_set_cipher_list(ctx, "ECDHE-RSA-AES128-GCM-SHA256")) return 2;
    } else SSL_CTX_set_psk_server_callback(ctx, psk_cb);
    int family = !strcmp(argv[7], "6") ? AF_INET6 : AF_INET;
    int fd = socket(family, SOCK_DGRAM | SOCK_NONBLOCK, 0);
    struct sockaddr_storage local = {0};
    socklen_t address_len;
    if (family == AF_INET6) {
        struct sockaddr_in6 *address = (struct sockaddr_in6 *)&local;
        address->sin6_family = AF_INET6;
        address->sin6_addr = in6addr_loopback;
        address_len = sizeof(*address);
    } else {
        struct sockaddr_in *address = (struct sockaddr_in *)&local;
        address->sin_family = AF_INET;
        address->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        address_len = sizeof(*address);
    }
    if (fd < 0 || bind(fd, (struct sockaddr *)&local, address_len)) return 2;
    if (getsockname(fd, (struct sockaddr *)&local, &address_len)) return 2;
    FILE *output = fopen(argv[4], "wb");
    metadata = fopen(argv[5], "wb");
    if (!output || !metadata) return 2;
    SSL_CTX_set_msg_callback(ctx, message_cb);
    unsigned int port = family == AF_INET6 ? ntohs(((struct sockaddr_in6 *)&local)->sin6_port)
                                        : ntohs(((struct sockaddr_in *)&local)->sin_port);
    printf("READY %u\n", port);
    fflush(stdout);
    struct sockaddr_storage peer;
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
    fprintf(metadata, "%s\n%s\n%s\nEMS %d\n", SSL_get_psk_identity(ssl), SSL_get_version(ssl), SSL_get_cipher_name(ssl), SSL_get_extms_support(ssl) == 1);
    fflush(metadata);
    int silent = 0;
    for (;;) {
        fd_set controls;
        FD_ZERO(&controls);
        FD_SET(STDIN_FILENO, &controls);
        struct timeval immediate = {0, 0};
        if (select(STDIN_FILENO + 1, &controls, NULL, NULL, &immediate) > 0) {
            char command[32];
            if (!fgets(command, sizeof(command), stdin)) break;
            if (!strcmp(command, "close\n")) {
                /* SSL_shutdown emits an authenticated epoch-one close_notify. */
                do { result = SSL_shutdown(ssl); } while (result < 0 && retry(ssl, result, fd));
                if (result < 0) break;
                silent = 1;
                printf("DONE close\n");
            } else if (!strcmp(command, "silence\n")) {
                silent = 1;
                printf("DONE silence\n");
            } else break;
            fflush(stdout);
        }
        if (silent) {
            struct timeval pause = {0, 20000};
            (void)select(0, NULL, NULL, NULL, &pause);
            continue;
        }
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
