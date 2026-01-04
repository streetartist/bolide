//! Bolide BigInt type with reference counting
//!
//! BolideBigInt 使用引用计数管理内存

use num_bigint::BigInt;
use num_traits::{Zero, Signed, ToPrimitive};
use std::cell::Cell;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::rc::{TypeTag, flags};

// Debug: 跟踪分配和释放
static BIGINT_ALLOC_COUNT: AtomicI64 = AtomicI64::new(0);
static BIGINT_FREE_COUNT: AtomicI64 = AtomicI64::new(0);

/// RC 对象头
#[repr(C)]
struct RcHeader {
    strong_count: Cell<u32>,
    weak_count: Cell<u32>,
    type_tag: TypeTag,
    flags: Cell<u8>,
    _padding: [u8; 6],
}

/// Bolide 大整数类型（带引用计数）
#[repr(C)]
pub struct BolideBigInt {
    header: RcHeader,
    inner: BigInt,
}

impl BolideBigInt {
    /// 创建新 BigInt（ref_count = 1）
    pub fn new(value: i64) -> *mut Self {
        BIGINT_ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::BigInt,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            inner: BigInt::from(value),
        }))
    }

    pub fn from_bigint(inner: BigInt) -> *mut Self {
        BIGINT_ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::BigInt,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            inner,
        }))
    }

    pub fn from_str(s: &str) -> Option<*mut Self> {
        s.parse::<BigInt>().ok().map(Self::from_bigint)
    }

    pub fn inner(&self) -> &BigInt {
        &self.inner
    }

    pub fn to_i64(&self) -> Option<i64> {
        self.inner.to_i64()
    }

    pub fn to_f64(&self) -> f64 {
        self.inner.to_f64().unwrap_or(f64::NAN)
    }

    pub fn to_string(&self) -> String {
        self.inner.to_string()
    }

    pub fn is_zero(&self) -> bool {
        self.inner.is_zero()
    }

    // ==================== RC 操作 ====================

    #[inline]
    pub fn retain(&self) {
        let count = self.header.strong_count.get();
        self.header.strong_count.set(count + 1);
    }

    #[inline]
    pub fn release(&self) -> bool {
        let count = self.header.strong_count.get();
        self.header.strong_count.set(count - 1);
        count == 1
    }

    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.header.strong_count.get()
    }

    #[inline]
    pub fn is_moved(&self) -> bool {
        self.header.flags.get() & flags::MOVED != 0
    }

    #[inline]
    pub fn mark_moved(&self) {
        self.header.flags.set(self.header.flags.get() | flags::MOVED);
    }
}

// ==================== FFI 导出 ====================

#[no_mangle]
pub extern "C" fn bolide_bigint_from_i64(value: i64) -> *mut BolideBigInt {
    BolideBigInt::new(value)
}

#[no_mangle]
pub extern "C" fn bolide_bigint_from_str(s: *const i8, len: usize) -> *mut BolideBigInt {
    let slice = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    let s = std::str::from_utf8(slice).unwrap_or("");
    BolideBigInt::from_str(s).unwrap_or(std::ptr::null_mut())
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_bigint_retain(b: *mut BolideBigInt) -> *mut BolideBigInt {
    if !b.is_null() {
        unsafe { (*b).retain(); }
    }
    b
}

/// 减少引用计数
#[no_mangle]
pub extern "C" fn bolide_bigint_release(b: *mut BolideBigInt) {
    if b.is_null() { return; }
    unsafe {
        if (*b).release() {
            BIGINT_FREE_COUNT.fetch_add(1, Ordering::SeqCst);
            let _ = Box::from_raw(b);
        }
    }
}

/// 深拷贝
#[no_mangle]
pub extern "C" fn bolide_bigint_clone(a: *const BolideBigInt) -> *mut BolideBigInt {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideBigInt::from_bigint(a.inner.clone())
}

/// 兼容旧 API
#[no_mangle]
pub extern "C" fn bolide_bigint_free(b: *mut BolideBigInt) {
    bolide_bigint_release(b);
}

#[no_mangle]
pub extern "C" fn bolide_bigint_ref_count(b: *const BolideBigInt) -> u32 {
    if b.is_null() { return 0; }
    unsafe { (*b).ref_count() }
}

#[no_mangle]
pub extern "C" fn bolide_bigint_to_i64(a: *const BolideBigInt) -> i64 {
    if a.is_null() { return 0; }
    unsafe { (*a).to_i64().unwrap_or(0) }
}

#[no_mangle]
pub extern "C" fn bolide_bigint_to_f64(a: *const BolideBigInt) -> f64 {
    if a.is_null() { return 0.0; }
    unsafe { (*a).to_f64() }
}

// ==================== 算术运算（返回新对象，ref_count = 1）====================

#[no_mangle]
pub extern "C" fn bolide_bigint_add(a: *const BolideBigInt, b: *const BolideBigInt) -> *mut BolideBigInt {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    BolideBigInt::from_bigint(&a.inner + &b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_bigint_sub(a: *const BolideBigInt, b: *const BolideBigInt) -> *mut BolideBigInt {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    BolideBigInt::from_bigint(&a.inner - &b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_bigint_mul(a: *const BolideBigInt, b: *const BolideBigInt) -> *mut BolideBigInt {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    BolideBigInt::from_bigint(&a.inner * &b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_bigint_div(a: *const BolideBigInt, b: *const BolideBigInt) -> *mut BolideBigInt {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    if b.is_zero() { return std::ptr::null_mut(); }
    BolideBigInt::from_bigint(&a.inner / &b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_bigint_rem(a: *const BolideBigInt, b: *const BolideBigInt) -> *mut BolideBigInt {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    if b.is_zero() { return std::ptr::null_mut(); }
    BolideBigInt::from_bigint(&a.inner % &b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_bigint_neg(a: *const BolideBigInt) -> *mut BolideBigInt {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideBigInt::from_bigint(-&a.inner)
}

// ==================== 比较运算 ====================

#[no_mangle]
pub extern "C" fn bolide_bigint_eq(a: *const BolideBigInt, b: *const BolideBigInt) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner == b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_bigint_ne(a: *const BolideBigInt, b: *const BolideBigInt) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner != b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_bigint_lt(a: *const BolideBigInt, b: *const BolideBigInt) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner < b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_bigint_le(a: *const BolideBigInt, b: *const BolideBigInt) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner <= b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_bigint_gt(a: *const BolideBigInt, b: *const BolideBigInt) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner > b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_bigint_ge(a: *const BolideBigInt, b: *const BolideBigInt) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner >= b.inner { 1 } else { 0 }
}

// ==================== Debug Stats ====================

/// 打印 BigInt 内存统计
#[no_mangle]
pub extern "C" fn bolide_bigint_debug_stats() {
    let alloc = BIGINT_ALLOC_COUNT.load(Ordering::SeqCst);
    let free = BIGINT_FREE_COUNT.load(Ordering::SeqCst);
    println!("[BigInt Stats] alloc: {}, free: {}, leak: {}", alloc, free, alloc - free);
}

/// 重置统计计数器
#[no_mangle]
pub extern "C" fn bolide_bigint_reset_stats() {
    BIGINT_ALLOC_COUNT.store(0, Ordering::SeqCst);
    BIGINT_FREE_COUNT.store(0, Ordering::SeqCst);
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bigint_rc() {
        let a = BolideBigInt::new(100);
        unsafe {
            assert_eq!((*a).ref_count(), 1);

            bolide_bigint_retain(a);
            assert_eq!((*a).ref_count(), 2);

            bolide_bigint_release(a);
            assert_eq!((*a).ref_count(), 1);

            bolide_bigint_release(a);
        }
    }

    #[test]
    fn test_bigint_arithmetic() {
        let a = BolideBigInt::new(100);
        let b = BolideBigInt::new(50);
        let c = bolide_bigint_add(a, b);
        unsafe {
            assert_eq!((*c).to_i64(), Some(150));
            assert_eq!((*c).ref_count(), 1);

            bolide_bigint_release(a);
            bolide_bigint_release(b);
            bolide_bigint_release(c);
        }
    }
}
