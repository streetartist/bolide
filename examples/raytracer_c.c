/*
 * C 光线追踪器 — 与 Bolide 版同场景、同参数
 * 1920×1080, 4×4 SSAA, 5 spheres, depth 8, gamma 校正
 * 8 线程并行 (pthread)
 *
 * 编译: gcc -O3 -march=native -pthread -o raytracer_c raytracer_c.c -lm
 * 运行: ./raytracer_c
 */

#include <stdio.h>
#include <stdlib.h>
#include <math.h>
#include <pthread.h>
#include <time.h>

#define WIDTH  1920
#define HEIGHT 1080
#define SAMPLES 4    /* 4×4 grid */
#define MAX_DEPTH 8
#define N_SPHERES 5
#define N_THREADS 8

/* ── vec3 ────────────────────────────────────────────── */
typedef struct { double x, y, z; } vec3;

static inline vec3 v3(double x, double y, double z) {
    return (vec3){x, y, z};
}
static inline vec3 vadd(vec3 a, vec3 b) { return v3(a.x+b.x, a.y+b.y, a.z+b.z); }
static inline vec3 vsub(vec3 a, vec3 b) { return v3(a.x-b.x, a.y-b.y, a.z-b.z); }
static inline vec3 vsmul(vec3 v, double s) { return v3(v.x*s, v.y*s, v.z*s); }
static inline vec3 vmul(vec3 a, vec3 b) { return v3(a.x*b.x, a.y*b.y, a.z*b.z); }
static inline double vdot(vec3 a, vec3 b) { return a.x*b.x + a.y*b.y + a.z*b.z; }
static inline double vlen(vec3 v) { return sqrt(vdot(v,v)); }
static inline vec3 vnorm(vec3 v) {
    double l = vlen(v);
    return l < 1e-6 ? v3(0,0,0) : vsmul(v, 1.0/l);
}
static inline vec3 vreflect(vec3 v, vec3 n) {
    return vsub(v, vsmul(n, 2.0 * vdot(v,n)));
}

/* ── Scene ────────────────────────────────────────────── */
static inline vec3 sphere_center(int i) {
    switch (i) {
        case 0: return v3(0, -1000.5, -3);
        case 1: return v3(0, 0, -3);
        case 2: return v3(-1.15, 0, -3);
        case 3: return v3(1.15, 0, -3);
        default: return v3(0.4, 0.45, -3.6);
    }
}
static inline double sphere_radius(int i) {
    return i == 0 ? 1000.0 : (i == 4 ? 0.22 : 0.5);
}
static inline vec3 sphere_color(int i) {
    switch (i) {
        case 0: return v3(0.45, 0.42, 0.38);
        case 1: return v3(0.85, 0.18, 0.18);
        case 2: return v3(0.18, 0.45, 0.85);
        case 3: return v3(0.88, 0.78, 0.22);
        default: return v3(0.18, 0.78, 0.28);
    }
}
static inline int sphere_is_metal(int i) { return i == 3; }

/* ── Ray-sphere intersection ──────────────────────────── */
static double ray_hit(vec3 center, double r, vec3 orig, vec3 dir) {
    vec3 oc = vsub(orig, center);
    double a = vdot(dir, dir);
    double half_b = vdot(oc, dir);
    double c = vdot(oc, oc) - r * r;
    double disc = half_b * half_b - a * c;
    if (disc < 0) return -1;
    double sqrt_d = sqrt(disc);
    double t1 = (-half_b - sqrt_d) / a;
    if (t1 > 1e-4) return t1;
    double t2 = (-half_b + sqrt_d) / a;
    if (t2 > 1e-4) return t2;
    return -1;
}

/* Returns hit distance, sets *hit_idx */
static double scene_hit(vec3 orig, vec3 dir, int *hit_idx) {
    double best = 1e30;
    *hit_idx = -1;
    for (int i = 0; i < N_SPHERES; i++) {
        double t = ray_hit(sphere_center(i), sphere_radius(i), orig, dir);
        if (t > 1e-4 && t < best) { best = t; *hit_idx = i; }
    }
    return *hit_idx >= 0 ? best : -1;
}

/* ── Sky ───────────────────────────────────────────────── */
static vec3 sun_dir(void) { return vnorm(v3(-0.45, 0.8, -0.5)); }

static vec3 sky(vec3 dir) {
    double t = 0.5 * (dir.y + 1.0);
    vec3 white = v3(0.95, 0.95, 0.95);
    vec3 blue  = v3(0.45, 0.65, 0.95);
    return vadd(vsmul(white, 1.0 - t), vsmul(blue, t));
}

/* ── Trace (recursive) ────────────────────────────────── */
static vec3 trace(vec3 orig, vec3 dir, int depth) {
    if (depth <= 0) return v3(0, 0, 0);

    int idx;
    double t = scene_hit(orig, dir, &idx);
    if (idx < 0) return sky(dir);

    vec3 point  = vadd(orig, vsmul(dir, t));
    vec3 center = sphere_center(idx);
    vec3 normal = vnorm(vsub(point, center));
    vec3 albedo = sphere_color(idx);

    if (sphere_is_metal(idx)) {
        vec3 ref_dir = vreflect(dir, normal);
        vec3 bounce  = vadd(point, vsmul(normal, 1e-3));
        return vmul(albedo, trace(bounce, ref_dir, depth - 1));
    }

    double amb = 0.1;
    vec3 ld = sun_dir();
    vec3 light_rgb = v3(1.0, 0.95, 0.88);

    vec3 s_orig = vadd(point, vsmul(normal, 1e-3));
    int sh_idx;
    scene_hit(s_orig, ld, &sh_idx);
    int shadowed = sh_idx >= 0;

    vec3 color = vsmul(albedo, amb);
    if (!shadowed) {
        double diff = fmax(0, vdot(normal, ld));
        color = vadd(color, vsmul(vmul(albedo, light_rgb), diff));

        vec3 view_dir = vsmul(dir, -1);
        vec3 half = vnorm(vadd(ld, view_dir));
        double spec = pow(fmax(0, vdot(normal, half)), 32);
        color = vadd(color, vsmul(v3(0.3, 0.3, 0.3), spec));
    }
    return color;
}

/* ── Gamma ─────────────────────────────────────────────── */
static inline double gamma_correct(double c) {
    return pow(fmax(0, c), 0.454545);
}

/* ── Render a horizontal strip ────────────────────────── */
typedef struct {
    int y0, y1;
    char *buf;      /* output buffer for this strip */
} StripJob;

static void* render_strip(void *arg) {
    StripJob *job = (StripJob*)arg;
    double aspect = (double)WIDTH / HEIGHT;
    vec3 cam = v3(0, 1.2, 4.5);
    double du = aspect / WIDTH;   /* half pixel in u */
    double dv = 1.0 / HEIGHT;     /* half pixel in v  */

    size_t cap = (size_t)(job->y1 - job->y0) * WIDTH * 18 + 4096;
    job->buf = malloc(cap);
    char *p = job->buf;
    char *end = job->buf + cap;

    for (int y = job->y0; y < job->y1; y++) {
        for (int x = 0; x < WIDTH; x++) {
            double uc = ((double)x / WIDTH - 0.5) * 2.0 * aspect;
            double vc = (0.5 - (double)y / HEIGHT) * 2.0;

            double sr = 0, sg = 0, sb = 0;
            for (int sy = 0; sy < SAMPLES; sy++) {
                double sv = vc + (sy * 0.5 - 0.75) * dv;
                for (int sx = 0; sx < SAMPLES; sx++) {
                    double su = uc + (sx * 0.5 - 0.75) * du;
                    vec3 c = trace(cam, vnorm(v3(su, sv, -1.5)), MAX_DEPTH);
                    sr += c.x; sg += c.y; sb += c.z;
                }
            }
            sr /= SAMPLES * SAMPLES; sg /= SAMPLES * SAMPLES; sb /= SAMPLES * SAMPLES;

            int r = (int)(gamma_correct(sr) * 255 + 0.5);
            int g = (int)(gamma_correct(sg) * 255 + 0.5);
            int b = (int)(gamma_correct(sb) * 255 + 0.5);
            if (r > 255) r = 255; if (r < 0) r = 0;
            if (g > 255) g = 255; if (g < 0) g = 0;
            if (b > 255) b = 255; if (b < 0) b = 0;

            p += snprintf(p, end - p, "%d %d %d\n", r, g, b);
        }
    }
    return NULL;
}

/* ── Main ──────────────────────────────────────────────── */
int main(void) {
    printf("C Ray Tracer — %dx%d — %d spheres, %d threads, %dx%d SSAA\n",
           WIDTH, HEIGHT, N_SPHERES, N_THREADS, SAMPLES, SAMPLES);

    pthread_t threads[N_THREADS];
    StripJob   jobs[N_THREADS];
    int step = HEIGHT / N_THREADS;

    clock_t t0 = clock();

    for (int i = 0; i < N_THREADS; i++) {
        jobs[i].y0 = i * step;
        jobs[i].y1 = (i == N_THREADS - 1) ? HEIGHT : (i + 1) * step;
        jobs[i].buf = NULL;
        pthread_create(&threads[i], NULL, render_strip, &jobs[i]);
    }

    for (int i = 0; i < N_THREADS; i++)
        pthread_join(threads[i], NULL);

    clock_t t1 = clock();
    double elapsed = (double)(t1 - t0) / CLOCKS_PER_SEC;

    /* Write PPM */
    FILE *f = fopen("tmp/raytracer_c.ppm", "w");
    fprintf(f, "P3\n%d %d\n255\n", WIDTH, HEIGHT);
    for (int i = 0; i < N_THREADS; i++) {
        if (jobs[i].buf) {
            fputs(jobs[i].buf, f);
            free(jobs[i].buf);
        }
    }
    fclose(f);

    printf("Done: %dx%d in %.1f s -> tmp/raytracer_c.ppm\n", WIDTH, HEIGHT, elapsed);
    return 0;
}
