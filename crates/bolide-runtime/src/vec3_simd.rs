//! Bolide vec3 SIMD primitives (x86_64 SSE2+, AVX for batch ops)
//! All functions operate on scalar f64 — Cranelift handles register allocation.
//! Use SSE2 intrinsics to accelerate dot/cross/normalize/reflect.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

// ============================================================
// Construction helpers (returned as multiple f64 — Bolide packs into tuple)
// ============================================================

/// Scale: returns (vx*s, vy*s, vz*s) — zero allocation
#[no_mangle]
pub extern "C" fn bolide_vec3_scale(
    vx: f64, vy: f64, vz: f64, s: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) {
    unsafe {
        *rx = vx * s;
        *ry = vy * s;
        *rz = vz * s;
    }
}

/// Add: a + b
#[no_mangle]
pub extern "C" fn bolide_vec3_add(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let a = _mm_set_pd(ay, ax);
        let b = _mm_set_pd(by, bx);
        let r = _mm_add_pd(a, b);
        *rx = _mm_cvtsd_f64(r);
        *ry = _mm_cvtsd_f64(_mm_unpackhi_pd(r, r));
        *rz = az + bz;
        return;
    }
    #[cfg(not(target_arch = "x86_64"))]
    unsafe {
        *rx = ax + bx;
        *ry = ay + by;
        *rz = az + bz;
    }
}

/// Sub: a - b
#[no_mangle]
pub extern "C" fn bolide_vec3_sub(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let a = _mm_set_pd(ay, ax);
        let b = _mm_set_pd(by, bx);
        let r = _mm_sub_pd(a, b);
        *rx = _mm_cvtsd_f64(r);
        *ry = _mm_cvtsd_f64(_mm_unpackhi_pd(r, r));
        *rz = az - bz;
        return;
    }
    #[cfg(not(target_arch = "x86_64"))]
    unsafe {
        *rx = ax - bx;
        *ry = ay - by;
        *rz = az - bz;
    }
}

// ============================================================
// Dot / Length / Normalize
// ============================================================

/// Dot product: a·b
#[no_mangle]
pub extern "C" fn bolide_vec3_dot(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
) -> f64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let a_xy = _mm_set_pd(ay, ax);
        let b_xy = _mm_set_pd(by, bx);
        let mul = _mm_mul_pd(a_xy, b_xy);
        let hadd = _mm_hadd_pd(mul, mul);
        _mm_cvtsd_f64(hadd) + az * bz
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        ax * bx + ay * by + az * bz
    }
}

/// Length: |v|
#[no_mangle]
pub extern "C" fn bolide_vec3_len(ax: f64, ay: f64, az: f64) -> f64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let a_xy = _mm_set_pd(ay, ax);
        let sq = _mm_mul_pd(a_xy, a_xy);
        let hadd = _mm_hadd_pd(sq, sq);
        (_mm_cvtsd_f64(hadd) + az * az).sqrt()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        (ax * ax + ay * ay + az * az).sqrt()
    }
}

/// Normalize + return length. Writes (nx, ny, nz) to out pointers.
/// Returns original length (0 if degenerate).
#[no_mangle]
pub extern "C" fn bolide_vec3_normalize(
    ax: f64, ay: f64, az: f64,
    nx: *mut f64, ny: *mut f64, nz: *mut f64,
) -> f64 {
    let len = bolide_vec3_len(ax, ay, az);
    if len < 1e-10 {
        unsafe { *nx = 0.0; *ny = 0.0; *nz = 0.0; }
        return 0.0;
    }
    let inv = 1.0 / len;
    unsafe {
        *nx = ax * inv;
        *ny = ay * inv;
        *nz = az * inv;
    }
    len
}

// ============================================================
// Cross / Reflect / Refract
// ============================================================

/// Cross product: a × b
#[no_mangle]
pub extern "C" fn bolide_vec3_cross(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) {
    unsafe {
        *rx = ay * bz - az * by;
        *ry = az * bx - ax * bz;
        *rz = ax * by - ay * bx;
    }
}

/// Reflect: self = v - 2*dot(v,n)*n
#[no_mangle]
pub extern "C" fn bolide_vec3_reflect(
    vx: f64, vy: f64, vz: f64,
    nx: f64, ny: f64, nz: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) {
    let d = 2.0 * (vx * nx + vy * ny + vz * nz);
    unsafe {
        *rx = vx - nx * d;
        *ry = vy - ny * d;
        *rz = vz - nz * d;
    }
}

/// Refract: Snell's law. Returns false for total internal reflection.
/// etai_over_etat = n1/n2 (refractive index ratio)
#[no_mangle]
pub extern "C" fn bolide_vec3_refract(
    uvx: f64, uvy: f64, uvz: f64,
    nx: f64, ny: f64, nz: f64,
    etai_over_etat: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) -> i64 {
    let cos_theta = (-uvx * nx - uvy * ny - uvz * nz).min(1.0);
    let sin_theta = (1.0 - cos_theta * cos_theta).sqrt();
    let cannot_refract = etai_over_etat * sin_theta > 1.0;
    if cannot_refract {
        return 0;
    }
    let r_out_perp_x = etai_over_etat * (uvx + nx * cos_theta);
    let r_out_perp_y = etai_over_etat * (uvy + ny * cos_theta);
    let r_out_perp_z = etai_over_etat * (uvz + nz * cos_theta);
    let r_out_parallel_scale = -((1.0 - r_out_perp_x * r_out_perp_x
        - r_out_perp_y * r_out_perp_y
        - r_out_perp_z * r_out_perp_z).abs().sqrt());
    unsafe {
        *rx = r_out_perp_x + nx * r_out_parallel_scale;
        *ry = r_out_perp_y + ny * r_out_parallel_scale;
        *rz = r_out_perp_z + nz * r_out_parallel_scale;
    }
    1
}

// ============================================================
// Utility
// ============================================================

/// Linear interpolation: (1-t)*a + t*b
#[no_mangle]
pub extern "C" fn bolide_vec3_lerp(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
    t: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) {
    let s = 1.0 - t;
    unsafe {
        *rx = s * ax + t * bx;
        *ry = s * ay + t * by;
        *rz = s * az + t * bz;
    }
}

/// Component-wise min
#[no_mangle]
pub extern "C" fn bolide_vec3_min(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) {
    unsafe {
        *rx = ax.min(bx);
        *ry = ay.min(by);
        *rz = az.min(bz);
    }
}

/// Component-wise max
#[no_mangle]
pub extern "C" fn bolide_vec3_max(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
    rx: *mut f64, ry: *mut f64, rz: *mut f64,
) {
    unsafe {
        *rx = ax.max(bx);
        *ry = ay.max(by);
        *rz = az.max(bz);
    }
}

/// Distance between two points
#[no_mangle]
pub extern "C" fn bolide_vec3_dist(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
) -> f64 {
    let dx = ax - bx;
    let dy = ay - by;
    let dz = az - bz;
    bolide_vec3_len(dx, dy, dz)
}
