// Fixture-only guard: librqbit 8.1.1 otherwise binds 0.0.0.0.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <string.h>
static int loopback(const struct sockaddr *a) {
    if (a->sa_family == AF_INET) return (ntohl(((const struct sockaddr_in*)a)->sin_addr.s_addr) >> 24) == 127;
    if (a->sa_family == AF_INET6) return IN6_IS_ADDR_LOOPBACK(&((const struct sockaddr_in6*)a)->sin6_addr);
    return 1;
}
int bind(int fd, const struct sockaddr *a, socklen_t n) {
    int (*real)(int,const struct sockaddr*,socklen_t) = dlsym(RTLD_NEXT,"bind");
    struct sockaddr_storage s; if (n > sizeof(s)) { errno=EINVAL; return -1; }
    memcpy(&s,a,n);
    if(a->sa_family == AF_INET) ((struct sockaddr_in*)&s)->sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    if(a->sa_family == AF_INET6) ((struct sockaddr_in6*)&s)->sin6_addr=in6addr_loopback;
    return real(fd,(const struct sockaddr*)&s,n);
}
int connect(int fd, const struct sockaddr *a, socklen_t n) {
    int (*real)(int,const struct sockaddr*,socklen_t) = dlsym(RTLD_NEXT,"connect");
    if (!loopback(a)) { errno=EACCES; return -1; }
    return real(fd,a,n);
}
