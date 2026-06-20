#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

enum {
    WIDTH = 640,
    HEIGHT = 360,
    MAX_ITER = 900,
    WORKERS = 8,
    BYTES_PER_PIXEL_TEXT = 16
};

typedef struct {
    int y0;
    int y1;
    char *out;
    size_t len;
} BandJob;

static long long now_ms(void) {
    LARGE_INTEGER freq;
    LARGE_INTEGER counter;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&counter);
    return (long long)(counter.QuadPart * 1000 / freq.QuadPart);
}

static int iter_at(int px, int py) {
    double cx = ((double)px / (double)WIDTH) * 3.5 - 2.5;
    double cy = ((double)py / (double)HEIGHT) * 2.0 - 1.0;
    double x = 0.0;
    double y = 0.0;
    int iter = 0;

    while (x * x + y * y <= 4.0 && iter < MAX_ITER) {
        double next_x = x * x - y * y + cx;
        y = 2.0 * x * y + cy;
        x = next_x;
        iter++;
    }

    return iter;
}

static int append_color(char *dst, int iter) {
    if (iter >= MAX_ITER) {
        memcpy(dst, "8 10 18\n", 8);
        return 8;
    }

    int t = iter * 255 / MAX_ITER;
    int r = t;
    int g = (t * t) / 255;
    int b = 255 - t;
    return sprintf(dst, "%d %d %d\n", r, g, b);
}

static void render_band(BandJob *job) {
    char *p = job->out;
    for (int y = job->y0; y < job->y1; y++) {
        for (int x = 0; x < WIDTH; x++) {
            p += append_color(p, iter_at(x, y));
        }
    }
    job->len = (size_t)(p - job->out);
}

static DWORD WINAPI render_band_thread(LPVOID param) {
    render_band((BandJob *)param);
    return 0;
}

static char *render_serial(size_t *len) {
    size_t cap = 64 + (size_t)WIDTH * HEIGHT * BYTES_PER_PIXEL_TEXT;
    char *image = (char *)malloc(cap);
    if (!image) {
        return NULL;
    }

    int header_len = sprintf(image, "P3\n%d %d\n255\n", WIDTH, HEIGHT);
    BandJob job = {0, HEIGHT, image + header_len, 0};
    render_band(&job);
    *len = (size_t)header_len + job.len;
    return image;
}

static char *render_parallel(size_t *len) {
    size_t cap = 64 + (size_t)WIDTH * HEIGHT * BYTES_PER_PIXEL_TEXT;
    char *image = (char *)malloc(cap);
    if (!image) {
        return NULL;
    }

    int header_len = sprintf(image, "P3\n%d %d\n255\n", WIDTH, HEIGHT);
    int step = HEIGHT / WORKERS;
    BandJob jobs[WORKERS];
    HANDLE threads[WORKERS];

    for (int i = 0; i < WORKERS; i++) {
        int y0 = i * step;
        int y1 = (i == WORKERS - 1) ? HEIGHT : (i + 1) * step;
        size_t offset = (size_t)header_len + (size_t)y0 * WIDTH * BYTES_PER_PIXEL_TEXT;
        jobs[i].y0 = y0;
        jobs[i].y1 = y1;
        jobs[i].out = image + offset;
        jobs[i].len = 0;
        threads[i] = CreateThread(NULL, 0, render_band_thread, &jobs[i], 0, NULL);
        if (!threads[i]) {
            fprintf(stderr, "CreateThread failed for worker %d\n", i);
            free(image);
            return NULL;
        }
    }

    WaitForMultipleObjects(WORKERS, threads, TRUE, INFINITE);
    for (int i = 0; i < WORKERS; i++) {
        CloseHandle(threads[i]);
    }

    char *compact = image + header_len;
    for (int i = 0; i < WORKERS; i++) {
        memmove(compact, jobs[i].out, jobs[i].len);
        compact += jobs[i].len;
    }
    *len = (size_t)(compact - image);
    return image;
}

static int write_file(const char *path, const char *data, size_t len) {
    FILE *f = fopen(path, "wb");
    if (!f) {
        return 0;
    }
    size_t written = fwrite(data, 1, len, f);
    fclose(f);
    return written == len;
}

int main(void) {
    system("if not exist tmp mkdir tmp");

    printf("C Mandelbrot rendering demo\n");
    printf("image: %dx%d, max_iter=%d\n", WIDTH, HEIGHT, MAX_ITER);

    size_t serial_len = 0;
    long long serial_start = now_ms();
    char *serial = render_serial(&serial_len);
    long long serial_ms = now_ms() - serial_start;
    if (!serial) {
        fprintf(stderr, "serial render allocation failed\n");
        return 1;
    }
    printf("serial: %lld ms -> tmp/mandelbrot_c_serial.ppm\n", serial_ms);
    write_file("tmp/mandelbrot_c_serial.ppm", serial, serial_len);

    size_t parallel_len = 0;
    long long parallel_start = now_ms();
    char *parallel = render_parallel(&parallel_len);
    long long parallel_ms = now_ms() - parallel_start;
    if (!parallel) {
        free(serial);
        fprintf(stderr, "parallel render allocation failed\n");
        return 1;
    }
    printf("parallel: %lld ms -> tmp/mandelbrot_c_parallel.ppm\n", parallel_ms);
    write_file("tmp/mandelbrot_c_parallel.ppm", parallel, parallel_len);

    if (serial_len == parallel_len && memcmp(serial, parallel, serial_len) == 0) {
        printf("check: serial and parallel images are identical\n");
    } else {
        printf("check: images differ\n");
    }

    if (parallel_ms > 0) {
        printf("speedup: %.2fx\n", (double)serial_ms / (double)parallel_ms);
    }

    free(serial);
    free(parallel);
    return 0;
}
