#include <stdio.h>
#include <stdlib.h>

#ifdef _WIN32
#include <windows.h>
static long long monotonic_ms(void) {
    LARGE_INTEGER freq;
    LARGE_INTEGER counter;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&counter);
    return (long long)((counter.QuadPart * 1000LL) / freq.QuadPart);
}
#else
#include <time.h>
static long long monotonic_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000LL + ts.tv_nsec / 1000000LL;
}
#endif

static int arg_or(int argc, char **argv, int index, int fallback) {
    if (argc > index) {
        return atoi(argv[index]);
    }
    return fallback;
}

static long long fib(int n) {
    if (n < 2) {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

int main(int argc, char **argv) {
    int n = arg_or(argc, argv, 1, 35);
    int reps = arg_or(argc, argv, 2, 1);

    long long start = monotonic_ms();
    long long result = 0;
    for (int r = 0; r < reps; r++) {
        result = fib(n);
    }
    long long elapsed = monotonic_ms() - start;

    printf("c fib n=%d reps=%d ms=%lld checksum=%lld\n", n, reps, elapsed, result);
    return 0;
}
