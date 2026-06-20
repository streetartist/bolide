#include <math.h>
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

static void push_body(
    double *x,
    double *y,
    double *z,
    double *vx,
    double *vy,
    double *vz,
    double *mass,
    int i
) {
    double fi = (double)i;
    x[i] = sin(fi * 0.013) * 100.0 + (double)(i % 17);
    y[i] = cos(fi * 0.017) * 100.0 - (double)(i % 23);
    z[i] = sin(fi * 0.019) * cos(fi * 0.011) * 50.0;
    vx[i] = cos(fi * 0.007) * 0.01;
    vy[i] = sin(fi * 0.009) * 0.01;
    vz[i] = cos(fi * 0.005) * 0.01;
    mass[i] = 0.5 + (double)((i * 13) % 97) / 97.0;
}

static void init_bodies(
    int n,
    double *x,
    double *y,
    double *z,
    double *vx,
    double *vy,
    double *vz,
    double *mass
) {
    for (int i = 0; i < n; i++) {
        push_body(x, y, z, vx, vy, vz, mass, i);
    }
}

static void simulate(
    int n,
    int steps,
    double *x,
    double *y,
    double *z,
    double *vx,
    double *vy,
    double *vz,
    double *mass
) {
    const double dt = 0.01;
    const double softening = 0.01;

    for (int step = 0; step < steps; step++) {
        for (int i = 0; i < n; i++) {
            for (int j = i + 1; j < n; j++) {
                double dx = x[j] - x[i];
                double dy = y[j] - y[i];
                double dz = z[j] - z[i];
                double dist_sq = dx * dx + dy * dy + dz * dz + softening;
                double inv_dist = 1.0 / sqrt(dist_sq);
                double inv_dist3 = inv_dist * inv_dist * inv_dist;
                double force = inv_dist3 * dt;
                double mi = mass[i];
                double mj = mass[j];
                double fx = dx * force;
                double fy = dy * force;
                double fz = dz * force;

                vx[i] += fx * mj;
                vy[i] += fy * mj;
                vz[i] += fz * mj;
                vx[j] -= fx * mi;
                vy[j] -= fy * mi;
                vz[j] -= fz * mi;
            }
        }

        for (int k = 0; k < n; k++) {
            x[k] += vx[k] * dt;
            y[k] += vy[k] * dt;
            z[k] += vz[k] * dt;
        }
    }
}

static double checksum(int n, double *x, double *y, double *z, double *vx) {
    double sum = 0.0;
    for (int i = 0; i < n; i++) {
        sum += x[i] * 0.13 + y[i] * 0.17 + z[i] * 0.19 + vx[i] * 23.0;
    }
    return sum;
}

int main(int argc, char **argv) {
    int n = arg_or(argc, argv, 1, 900);
    int steps = arg_or(argc, argv, 2, 120);

    double *x = (double *)calloc((size_t)n, sizeof(double));
    double *y = (double *)calloc((size_t)n, sizeof(double));
    double *z = (double *)calloc((size_t)n, sizeof(double));
    double *vx = (double *)calloc((size_t)n, sizeof(double));
    double *vy = (double *)calloc((size_t)n, sizeof(double));
    double *vz = (double *)calloc((size_t)n, sizeof(double));
    double *mass = (double *)calloc((size_t)n, sizeof(double));
    if (!x || !y || !z || !vx || !vy || !vz || !mass) {
        fprintf(stderr, "allocation failed\n");
        return 2;
    }

    init_bodies(n, x, y, z, vx, vy, vz, mass);

    long long start = monotonic_ms();
    simulate(n, steps, x, y, z, vx, vy, vz, mass);
    long long elapsed = monotonic_ms() - start;
    double sum = checksum(n, x, y, z, vx);

    printf("c nbody bodies=%d steps=%d ms=%lld checksum=%.17g\n", n, steps, elapsed, sum);

    free(x);
    free(y);
    free(z);
    free(vx);
    free(vy);
    free(vz);
    free(mass);
    return 0;
}
