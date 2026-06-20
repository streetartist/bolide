//! Atomic primitives for thread-safe Bolide code.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

#[repr(C)]
pub struct BolideAtomicInt {
    value: AtomicI64,
}

#[repr(C)]
pub struct BolideAtomicBool {
    value: AtomicBool,
}

#[no_mangle]
pub extern "C" fn bolide_atomic_int_new(value: i64) -> *mut c_void {
    Box::into_raw(Box::new(BolideAtomicInt {
        value: AtomicI64::new(value),
    })) as *mut c_void
}

#[no_mangle]
pub extern "C" fn bolide_atomic_int_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        unsafe {
            drop(Box::from_raw(ptr as *mut BolideAtomicInt));
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_int_get(ptr: *const c_void) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        (*(ptr as *const BolideAtomicInt))
            .value
            .load(Ordering::SeqCst)
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_int_set(ptr: *mut c_void, value: i64) {
    if !ptr.is_null() {
        unsafe {
            (*(ptr as *mut BolideAtomicInt))
                .value
                .store(value, Ordering::SeqCst);
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_int_swap(ptr: *mut c_void, value: i64) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        (*(ptr as *mut BolideAtomicInt))
            .value
            .swap(value, Ordering::SeqCst)
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_int_add(ptr: *mut c_void, value: i64) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        (*(ptr as *mut BolideAtomicInt))
            .value
            .fetch_add(value, Ordering::SeqCst)
            + value
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_int_sub(ptr: *mut c_void, value: i64) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        (*(ptr as *mut BolideAtomicInt))
            .value
            .fetch_sub(value, Ordering::SeqCst)
            - value
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_int_compare_exchange(
    ptr: *mut c_void,
    current: i64,
    next: i64,
) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    let ok = unsafe {
        (*(ptr as *mut BolideAtomicInt)).value.compare_exchange(
            current,
            next,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
    };
    ok.is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_atomic_bool_new(value: i64) -> *mut c_void {
    Box::into_raw(Box::new(BolideAtomicBool {
        value: AtomicBool::new(value != 0),
    })) as *mut c_void
}

#[no_mangle]
pub extern "C" fn bolide_atomic_bool_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        unsafe {
            drop(Box::from_raw(ptr as *mut BolideAtomicBool));
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_bool_get(ptr: *const c_void) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        (*(ptr as *const BolideAtomicBool))
            .value
            .load(Ordering::SeqCst) as i64
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_bool_set(ptr: *mut c_void, value: i64) {
    if !ptr.is_null() {
        unsafe {
            (*(ptr as *mut BolideAtomicBool))
                .value
                .store(value != 0, Ordering::SeqCst);
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_bool_swap(ptr: *mut c_void, value: i64) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        (*(ptr as *mut BolideAtomicBool))
            .value
            .swap(value != 0, Ordering::SeqCst) as i64
    }
}

#[no_mangle]
pub extern "C" fn bolide_atomic_bool_compare_exchange(
    ptr: *mut c_void,
    current: i64,
    next: i64,
) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    let ok = unsafe {
        (*(ptr as *mut BolideAtomicBool)).value.compare_exchange(
            current != 0,
            next != 0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
    };
    ok.is_ok() as i64
}
