#define _POSIX_C_SOURCE 200809L
/* Test-only strict PSK oracle. Arguments must contain dummy credentials only. */
#ifdef _WIN32
#define _CRT_SECURE_NO_WARNINGS
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <io.h>
typedef SOCKET socket_type;
typedef int socklen_t;
#define BAD_SOCKET INVALID_SOCKET
#else
#include <arpa/inet.h>
#include <fcntl.h>
#include <time.h>
typedef int socket_type;
#define BAD_SOCKET (-1)
#endif
#include <openssl/ssl.h>
#include <openssl/rand.h>
#include <openssl/crypto.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifndef _WIN32
#include <sys/select.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

static double deadline;
static double seconds(void) {
#ifdef _WIN32
    return (double)GetTickCount64() / 1000.0;
#else
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) exit(2);
    return (double)now.tv_sec + (double)now.tv_nsec / 1000000000.0;
#endif
}
static void watchdog(void) { if (seconds() >= deadline) exit(2); }
static int waiting(void) {
#ifdef _WIN32
    int error = WSAGetLastError();
    return error == WSAEWOULDBLOCK || error == WSAEINTR;
#else
    return errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR;
#endif
}
static int control_ready(void) {
#ifdef _WIN32
    DWORD available = 0;
    return PeekNamedPipe(GetStdHandle(STD_INPUT_HANDLE), NULL, 0, NULL,
                         &available, NULL) && available > 0;
#else
    fd_set controls;
    FD_ZERO(&controls);
    FD_SET(STDIN_FILENO, &controls);
    struct timeval immediate = {0, 0};
    return select(STDIN_FILENO + 1, &controls, NULL, NULL, &immediate) > 0;
#endif
}
static void pause_briefly(void) {
#ifdef _WIN32
    Sleep(20);
#else
    struct timespec pause = {0, 20000000};
    (void)nanosleep(&pause, NULL);
#endif
}

static char expected_identity[256];
static unsigned char expected_key[256];
static unsigned int expected_key_len;
static FILE *metadata;
/* This bounded fixture serves one connected peer and one association. */
static unsigned char expected_cookie[32];
static unsigned int cookie_challenges;
static unsigned int cookies_verified;

static int cookie_generate_cb(SSL *ssl, unsigned char *cookie,
                              unsigned int *length) {
    (void)ssl;
    memcpy(cookie, expected_cookie, sizeof(expected_cookie));
    *length = (unsigned int)sizeof(expected_cookie);
    return 1;
}

static int cookie_verify_cb(SSL *ssl, const unsigned char *cookie,
                            unsigned int length) {
    (void)ssl;
    if (length != sizeof(expected_cookie) ||
        CRYPTO_memcmp(cookie, expected_cookie, sizeof(expected_cookie)) != 0) return 0;
    cookies_verified++;
    return 1;
}

/* Observe actual OpenSSL-generated alerts; never create records or do crypto. */
static void message_cb(int writing, int version, int type, const void *data,
                       size_t length, SSL *ssl, void *arg) {
    (void)version; (void)ssl; (void)arg;
    const unsigned char *bytes = data;
    if (writing && type == SSL3_RT_HANDSHAKE && length > 0 &&
        bytes[0] == DTLS1_MT_HELLO_VERIFY_REQUEST) cookie_challenges++;
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

static int retry(SSL *ssl, int result, socket_type fd) {
    watchdog();
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
    if (select((int)fd + 1, &readers, &writers, NULL, &timeout) < 0 && !waiting()) return 0;
    return DTLSv1_handle_timeout(ssl) >= 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--version")) {
        printf("%s\n%s\n", OPENSSL_VERSION_TEXT, OpenSSL_version(OPENSSL_VERSION));
        return 0;
    }
    if (argc != 10) return 2;
#ifdef _WIN32
    WSADATA winsock;
    if (WSAStartup(MAKEWORD(2, 2), &winsock)) return 2;
#endif
    int identity_len = unhex(argv[1], (unsigned char *)expected_identity, sizeof(expected_identity) - 1);
    int key_len = unhex(argv[2], expected_key, sizeof(expected_key));
    if (identity_len < 0 || key_len < 0 || memchr(expected_identity, 0, (size_t)identity_len)) return 2;
    expected_key_len = (unsigned int)key_len;
    deadline = seconds() + 8.0;
    SSL_CTX *ctx = SSL_CTX_new(DTLS_server_method());
    if (!ctx || !SSL_CTX_set_min_proto_version(ctx, DTLS1_2_VERSION) ||
        !SSL_CTX_set_max_proto_version(ctx, DTLS1_2_VERSION) ||
        !SSL_CTX_set_cipher_list(ctx, argv[3])) return 2;
    if (!strcmp(argv[9], "cookie")) {
        if (RAND_bytes(expected_cookie, (int)sizeof(expected_cookie)) != 1) return 2;
        SSL_CTX_set_cookie_generate_cb(ctx, cookie_generate_cb);
        SSL_CTX_set_cookie_verify_cb(ctx, cookie_verify_cb);
        SSL_CTX_set_options(ctx, SSL_OP_COOKIE_EXCHANGE);
    } else if (strcmp(argv[9], "no-cookie")) return 2;
    if (!strcmp(argv[6], "non-ems")) SSL_CTX_set_options(ctx, SSL_OP_NO_EXTENDED_MASTER_SECRET);
    if (!strcmp(argv[8], "certificate")) {
        if (!certificate(ctx) || !SSL_CTX_set_cipher_list(ctx, "ECDHE-RSA-AES128-GCM-SHA256")) return 2;
    } else SSL_CTX_set_psk_server_callback(ctx, psk_cb);
    int family = !strcmp(argv[7], "6") ? AF_INET6 : AF_INET;
    socket_type fd = socket(family, SOCK_DGRAM, 0);
    if (fd == BAD_SOCKET) return 2;
#ifdef _WIN32
    u_long nonblocking = 1;
    if (ioctlsocket(fd, FIONBIO, &nonblocking)) return 2;
#else
    if (fcntl(fd, F_SETFL, O_NONBLOCK)) return 2;
#endif
    struct sockaddr_storage local = {0};
    socklen_t address_len;
    if (family == AF_INET6) {
        struct sockaddr_in6 *address = (struct sockaddr_in6 *)&local;
        address->sin6_family = AF_INET6;
        address->sin6_addr = in6addr_loopback;
        address_len = (socklen_t)sizeof(*address);
    } else {
        struct sockaddr_in *address = (struct sockaddr_in *)&local;
        address->sin_family = AF_INET;
        address->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        address_len = (socklen_t)sizeof(*address);
    }
    if (bind(fd, (struct sockaddr *)&local, address_len)) return 2;
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
    socklen_t peer_len = (socklen_t)sizeof(peer);
    unsigned char buffer[65536];
    while (recvfrom(fd, (char *)buffer, (int)sizeof(buffer), MSG_PEEK, (struct sockaddr *)&peer, &peer_len) < 0) {
        watchdog();
        if (!waiting()) return 2;
        fd_set readers;
        FD_ZERO(&readers);
        FD_SET(fd, &readers);
        struct timeval timeout = {0, 20000};
        (void)select((int)fd + 1, &readers, NULL, NULL, &timeout);
    }
    if (connect(fd, (struct sockaddr *)&peer, peer_len)) return 2;
    BIO *bio = BIO_new_dgram((int)fd, BIO_NOCLOSE);
    SSL *ssl = SSL_new(ctx);
    if (!bio || !ssl || BIO_ctrl(bio, BIO_CTRL_DGRAM_SET_CONNECTED, 0, &peer) <= 0) return 2;
    SSL_set_bio(ssl, bio, bio);
    int result;
    while ((result = SSL_accept(ssl)) != 1) {
        if (!retry(ssl, result, fd)) goto done;
    }
    fprintf(metadata, "%s\n%s\n%s\nEMS %d\n", SSL_get_psk_identity(ssl), SSL_get_version(ssl), SSL_get_cipher_name(ssl), SSL_get_extms_support(ssl) == 1);
    fprintf(metadata, "COOKIE_CHALLENGES %u\nCOOKIES_VERIFIED %u\n",
            cookie_challenges, cookies_verified);
    fflush(metadata);
    int silent = 0;
    for (;;) {
        watchdog();
        if (control_ready()) {
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
            pause_briefly();
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
#ifdef _WIN32
    closesocket(fd);
    WSACleanup();
#else
    close(fd);
#endif
    return 0;
}
