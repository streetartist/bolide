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

// Mirror the Bolide version: use a 64-bit element array (matching list<int>)
// and rebuild it each rep so allocation cost is comparable.
static long long count_primes(int limit, int reps) {
    long long count = 0;
    for (int r = 0; r < reps; r++) {
        long long *flags = (long long *)malloc((size_t)(limit + 1) * sizeof(long long));
        if (!flags) {
            fprintf(stderr, "allocation failed\n");
            exit(2);
        }
        for (int i = 0; i <= limit; i++) {
            flags[i] = 1;
        }

        count = 0;
        for (int p = 2; p <= limit; p++) {
            if (flags[p] == 1) {
                count++;
                for (int m = p + p; m <= limit; m += p) {
                    flags[m] = 0;
                }
            }
        }
        free(flags);
    }
    return count;
}

int main(int argc, char **argv) {
    int limit = arg_or(argc, argv, 1, 5000000);
    int reps = arg_or(argc, argv, 2, 1);

    long long start = monotonic_ms();
    long long count = count_primes(limit, reps);
    long long elapsed = monotonic_ms() - start;

    printf("c sieve limit=%d reps=%d ms=%lld checksum=%lld\n", limit, reps, elapsed, count);
    return 0;
}
