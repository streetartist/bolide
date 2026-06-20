//! Synchronization primitives for Bolide standard library.

use crate::{bolide_dynamic_clone, bolide_dynamic_release, BolideDynamic, DynamicType};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

#[repr(C)]
pub struct BolideMutexValue {
    value: Mutex<usize>,
}

#[repr(C)]
pub struct BolideRwLockValue {
    value: RwLock<usize>,
}

#[repr(C)]
pub struct BolideOnceFlag {
    done: AtomicBool,
}

fn clone_or_none(value: *const BolideDynamic) -> *mut BolideDynamic {
    if value.is_null() {
        crate::BolideDynamic::none()
    } else {
        bolide_dynamic_clone(value)
    }
}

unsafe fn dynamic_int_value(value: *const BolideDynamic) -> Option<i64> {
    if value.is_null() {
        return None;
    }
    let value = &*value;
    if value.tag == DynamicType::Int {
        Some(value.data.int_val)
    } else {
        None
    }
}

fn release_addr(addr: usize) {
    if addr != 0 {
        bolide_dynamic_release(addr as *mut BolideDynamic);
    }
}

#[no_mangle]
pub extern "C" fn bolide_sync_mutex_new(value: *const BolideDynamic) -> *mut c_void {
    let value = clone_or_none(value) as usize;
    Box::into_raw(Box::new(BolideMutexValue {
        value: Mutex::new(value),
    })) as *mut c_void
}

#[no_mangle]
pub extern "C" fn bolide_sync_mutex_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let value = Box::from_raw(ptr as *mut BolideMutexValue);
        let inner = *value.value.lock().unwrap();
        release_addr(inner);
    }
}

#[no_mangle]
pub extern "C" fn bolide_sync_mutex_get(ptr: *const c_void) -> *mut BolideDynamic {
    if ptr.is_null() {
        return crate::BolideDynamic::none();
    }
    let value = unsafe { &*(ptr as *const BolideMutexValue) };
    let inner = *value.value.lock().unwrap();
    clone_or_none(inner as *const BolideDynamic)
}

#[no_mangle]
pub extern "C" fn bolide_sync_mutex_set(ptr: *mut c_void, next: *const BolideDynamic) {
    if ptr.is_null() {
        return;
    }
    let value = unsafe { &*(ptr as *const BolideMutexValue) };
    let next = clone_or_none(next) as usize;
    let mut guard = value.value.lock().unwrap();
    let old = std::mem::replace(&mut *guard, next);
    release_addr(old);
}

#[no_mangle]
pub extern "C" fn bolide_sync_mutex_swap(
    ptr: *mut c_void,
    next: *const BolideDynamic,
) -> *mut BolideDynamic {
    if ptr.is_null() {
        return crate::BolideDynamic::none();
    }
    let value = unsafe { &*(ptr as *const BolideMutexValue) };
    let next = clone_or_none(next) as usize;
    let mut guard = value.value.lock().unwrap();
    let old = std::mem::replace(&mut *guard, next);
    if old == 0 {
        crate::BolideDynamic::none()
    } else {
        old as *mut BolideDynamic
    }
}

#[no_mangle]
pub extern "C" fn bolide_sync_mutex_add_int(ptr: *mut c_void, delta: i64) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    let value = unsafe { &*(ptr as *const BolideMutexValue) };
    let mut guard = value.value.lock().unwrap();
    let current = unsafe { dynamic_int_value(*guard as *const BolideDynamic) }.unwrap_or(0);
    let next_value = current + delta;
    let next = crate::BolideDynamic::from_int(next_value) as usize;
    let old = std::mem::replace(&mut *guard, next);
    release_addr(old);
    next_value
}

#[no_mangle]
pub extern "C" fn bolide_sync_rwlock_new(value: *const BolideDynamic) -> *mut c_void {
    let value = clone_or_none(value) as usize;
    Box::into_raw(Box::new(BolideRwLockValue {
        value: RwLock::new(value),
    })) as *mut c_void
}

#[no_mangle]
pub extern "C" fn bolide_sync_rwlock_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let value = Box::from_raw(ptr as *mut BolideRwLockValue);
        let inner = *value.value.read().unwrap();
        release_addr(inner);
    }
}

#[no_mangle]
pub extern "C" fn bolide_sync_rwlock_get(ptr: *const c_void) -> *mut BolideDynamic {
    if ptr.is_null() {
        return crate::BolideDynamic::none();
    }
    let value = unsafe { &*(ptr as *const BolideRwLockValue) };
    let inner = *value.value.read().unwrap();
    clone_or_none(inner as *const BolideDynamic)
}

#[no_mangle]
pub extern "C" fn bolide_sync_rwlock_set(ptr: *mut c_void, next: *const BolideDynamic) {
    if ptr.is_null() {
        return;
    }
    let value = unsafe { &*(ptr as *const BolideRwLockValue) };
    let next = clone_or_none(next) as usize;
    let mut guard = value.value.write().unwrap();
    let old = std::mem::replace(&mut *guard, next);
    release_addr(old);
}

#[no_mangle]
pub extern "C" fn bolide_sync_rwlock_add_int(ptr: *mut c_void, delta: i64) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    let value = unsafe { &*(ptr as *const BolideRwLockValue) };
    let mut guard = value.value.write().unwrap();
    let current = unsafe { dynamic_int_value(*guard as *const BolideDynamic) }.unwrap_or(0);
    let next_value = current + delta;
    let next = crate::BolideDynamic::from_int(next_value) as usize;
    let old = std::mem::replace(&mut *guard, next);
    release_addr(old);
    next_value
}

#[no_mangle]
pub extern "C" fn bolide_sync_once_new() -> *mut c_void {
    Box::into_raw(Box::new(BolideOnceFlag {
        done: AtomicBool::new(false),
    })) as *mut c_void
}

#[no_mangle]
pub extern "C" fn bolide_sync_once_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        unsafe {
            drop(Box::from_raw(ptr as *mut BolideOnceFlag));
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_sync_once_check(ptr: *const c_void) -> i64 {
    if ptr.is_null() {
        return 1;
    }
    let flag = unsafe { &*(ptr as *const BolideOnceFlag) };
    flag.done.load(Ordering::SeqCst) as i64
}

#[no_mangle]
pub extern "C" fn bolide_sync_once_try_begin(ptr: *mut c_void) -> i64 {
    if ptr.is_null() {
        return 0;
    }
    let flag = unsafe { &*(ptr as *const BolideOnceFlag) };
    flag.done
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_sync_once_reset(ptr: *mut c_void) {
    if !ptr.is_null() {
        let flag = unsafe { &*(ptr as *const BolideOnceFlag) };
        flag.done.store(false, Ordering::SeqCst);
    }
}
