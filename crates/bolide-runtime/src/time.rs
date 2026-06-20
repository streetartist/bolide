//! Time helpers for the Bolide standard library.

use once_cell::sync::Lazy;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static START: Lazy<Instant> = Lazy::new(Instant::now);

fn unix_duration() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

#[no_mangle]
pub extern "C" fn bolide_time_now() -> i64 {
    unix_duration().as_secs() as i64
}

#[no_mangle]
pub extern "C" fn bolide_time_now_ms() -> i64 {
    unix_duration().as_millis().min(i64::MAX as u128) as i64
}

#[no_mangle]
pub extern "C" fn bolide_time_now_us() -> i64 {
    unix_duration().as_micros().min(i64::MAX as u128) as i64
}

#[no_mangle]
pub extern "C" fn bolide_time_monotonic_ms() -> i64 {
    START.elapsed().as_millis().min(i64::MAX as u128) as i64
}

#[no_mangle]
pub extern "C" fn bolide_time_sleep_ms(ms: i64) {
    if ms > 0 {
        thread::sleep(Duration::from_millis(ms as u64));
    }
}
