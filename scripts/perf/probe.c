// Kindle benchmark probe: observe existing display ioctls without adding waits.
// Build as an LD_PRELOAD library (-DPRELOAD), an object linked into Verso
// (-DLINKED, for static builds that ignore LD_PRELOAD) and a small
// touch/marker helper (see README).
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

void verso_bench_mark(const char *name) {
    const char *path = getenv("VERSO_BENCH_TRACE");
    if (!path) return;
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (fd < 0) return;
    dprintf(fd, "%.9f MARK %d %s\n", now(), getpid(), name);
    close(fd);
}

#if defined(PRELOAD) || defined(LINKED)
// The first priority runs before C++ static constructors, so the first malloc
// already sees one CPU (Zig's allocator caches the CPU count on first use).
__attribute__((constructor(101))) static void started(void) {
    // Before main, so every thread and the pre-layout child inherit it.
    if (getenv("VERSO_BENCH_ONE_CPU")) {
        cpu_set_t one;
        CPU_ZERO(&one);
        CPU_SET(0, &one);
        if (sched_setaffinity(0, sizeof(one), &one)) verso_bench_mark("one_cpu_failed");
    }
    verso_bench_mark("process_start");
}

// Matches <sys/ioctl.h>: musl's request is an int.
#ifdef __GLIBC__
typedef unsigned long request_t;
#else
typedef int request_t;
#endif

#ifdef LINKED
// Defined in the executable, so it replaces libc's ioctl for Verso's own calls.
static int original(int fd, request_t request, void *arg) {
    return (int)syscall(SYS_ioctl, fd, (unsigned long)request, arg);
}
#endif

int ioctl(int fd, request_t request, ...) {
#ifdef PRELOAD
    static int (*original)(int, request_t, ...);
    if (!original) original = dlsym(RTLD_NEXT, "ioctl");
#endif
    va_list args;
    va_start(args, request);
    void *arg = va_arg(args, void *);
    va_end(args);
    // Both installed apps use Kindle's MXCFB (magic F) display interface.
    unsigned int nr = (unsigned long)request & 0xffff;
    if (nr != 0x462e && nr != 0x462f && nr != 0x4637) return original(fd, request, arg);
    uint32_t *data = arg;
    double before = now();
    // Copy before the ioctl: WAIT requests may modify their argument.
    uint32_t top = 0, left = 0, width = 0, height = 0, waveform = 0, mode = 0, marker = data[0];
    if (nr == 0x462e) {
        top = data[0]; left = data[1]; width = data[2]; height = data[3];
        waveform = data[4]; mode = data[5]; marker = data[6];
    }
    int result = original(fd, request, arg);
    int saved_errno = errno;
    const char *path = getenv("VERSO_BENCH_TRACE");
    if (path) {
        int out = open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
        if (out >= 0) {
            dprintf(out, "%.9f IOCTL %d %lx %.9f %d %u %u %u %u %u %u %u\n",
                    before, getpid(), (unsigned long)request, now(), result, marker, waveform, mode, left, top, width, height);
            close(out);
        }
    }
    errno = saved_errno;
    return result;
}
#else
struct event { int32_t sec, usec; uint16_t type, code; int32_t value; };
static void event(int fd, uint16_t type, uint16_t code, int32_t value) {
    struct event e = {0, 0, type, code, value};
    if (write(fd, &e, sizeof(e)) != sizeof(e)) { perror("touch"); exit(1); }
}

int main(int argc, char **argv) {
    if (argc == 3 && !strcmp(argv[1], "mark")) { verso_bench_mark(argv[2]); return 0; }
    int held = argc == 8 && !strcmp(argv[1], "hold-drag");
    int dragged = held || (argc == 8 && !strcmp(argv[1], "swipe"));
    if (!dragged && (argc != 6 || strcmp(argv[1], "tap"))) {
        fprintf(stderr, "usage: probe mark LABEL | probe tap VERSO_PID X Y LABEL | probe hold-drag VERSO_PID X1 Y1 X2 Y2 LABEL\n");
        return 2;
    }
    char path[80], comm[32] = {0};
    snprintf(path, sizeof(path), "/proc/%d/comm", atoi(argv[2]));
    FILE *f = fopen(path, "r");
    if (!f || !fgets(comm, sizeof(comm), f) || strcmp(comm, "verso\n")) {
        fprintf(stderr, "refusing touch: expected a running Verso PID\n"); return 1;
    }
    fclose(f);
    int fd = open("/dev/input/event1", O_WRONLY | O_CLOEXEC);
    if (fd < 0) { perror("touchscreen"); return 1; }
    event(fd, 3, 0x2f, 0); event(fd, 3, 0x39, 123);
    event(fd, 3, 0x35, atoi(argv[3])); event(fd, 3, 0x36, atoi(argv[4])); event(fd, 0, 0, 0);
    if (dragged) {
        usleep(held ? 650000 : 50000);
        event(fd, 3, 0x35, atoi(argv[5])); event(fd, 3, 0x36, atoi(argv[6])); event(fd, 0, 0, 0);
        usleep(held ? 250000 : 50000);
    } else { usleep(50000); }
    // The boundary is finger release, excluding the deliberate 50 ms contact.
    verso_bench_mark(argv[argc - 1]);
    event(fd, 3, 0x39, -1); event(fd, 0, 0, 0);
    close(fd);
    return 0;
}
#endif
