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

static int iter_at(int px, int py, int width, int height, int max_iter) {
    double cx = ((double)px / (double)width) * 3.5 - 2.5;
    double cy = ((double)py / (double)height) * 2.0 - 1.0;
    double x = 0.0;
    double y = 0.0;
    int iter = 0;
    while (x * x + y * y <= 4.0 && iter < max_iter) {
        double next_x = x * x - y * y + cx;
        y = 2.0 * x * y + cy;
        x = next_x;
        iter++;
    }
    return iter;
}

static long long render(int width, int height, int max_iter) {
    long long sum = 0;
    for (int py = 0; py < height; py++) {
        for (int px = 0; px < width; px++) {
            sum += iter_at(px, py, width, height, max_iter);
        }
    }
    return sum;
}

int main(int argc, char **argv) {
    int width = arg_or(argc, argv, 1, 1200);
    int height = arg_or(argc, argv, 2, 1200);
    int max_iter = arg_or(argc, argv, 3, 256);

    long long start = monotonic_ms();
    long long sum = render(width, height, max_iter);
    long long elapsed = monotonic_ms() - start;

    printf("c mandelbrot w=%d h=%d max_iter=%d ms=%lld checksum=%lld\n",
           width, height, max_iter, elapsed, sum);
    return 0;
}
