//! Pseudo-random helpers for the Bolide standard library.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static RNG_STATE: AtomicU64 = AtomicU64::new(0);

fn initial_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let addr_mix = (&RNG_STATE as *const AtomicU64 as usize) as u64;
    nanos ^ addr_mix.rotate_left(17) ^ 0xa076_1d64_78bd_642f
}

fn mix64(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn next_u64() -> u64 {
    let mut state = RNG_STATE.load(Ordering::Relaxed);
    if state == 0 {
        let seed = mix64(initial_seed()).max(1);
        let _ = RNG_STATE.compare_exchange(0, seed, Ordering::SeqCst, Ordering::Relaxed);
        state = RNG_STATE.load(Ordering::Relaxed);
    }

    loop {
        let next = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        match RNG_STATE.compare_exchange_weak(state, next, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => return mix64(next),
            Err(actual) => state = actual,
        }
    }
}

fn bounded_u64(bound: u64) -> u64 {
    if bound == 0 {
        return 0;
    }
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = next_u64();
        if value >= threshold {
            return value % bound;
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_random_seed(seed: i64) {
    let mixed = mix64(seed as u64).max(1);
    RNG_STATE.store(mixed, Ordering::SeqCst);
}

#[no_mangle]
pub extern "C" fn bolide_random_int(max: i64) -> i64 {
    if max <= 0 {
        return 0;
    }
    bounded_u64(max as u64) as i64
}

#[no_mangle]
pub extern "C" fn bolide_random_range(min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    min + bounded_u64((max - min) as u64) as i64
}

#[no_mangle]
pub extern "C" fn bolide_random_float() -> f64 {
    const SCALE: f64 = 1.0 / ((1u64 << 53) as f64);
    ((next_u64() >> 11) as f64) * SCALE
}

#[no_mangle]
pub extern "C" fn bolide_random_bool() -> i64 {
    (next_u64() & 1) as i64
}
