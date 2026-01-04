//! Bolide Decimal type with reference counting
//!
//! BolideDecimal 使用引用计数管理内存

use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use std::cell::Cell;

use crate::rc::{TypeTag, flags};

/// RC 对象头
#[repr(C)]
struct RcHeader {
    strong_count: Cell<u32>,
    weak_count: Cell<u32>,
    type_tag: TypeTag,
    flags: Cell<u8>,
    _padding: [u8; 6],
}

/// Bolide 精确小数类型（带引用计数）
#[repr(C)]
pub struct BolideDecimal {
    header: RcHeader,
    inner: Decimal,
}

impl BolideDecimal {
    /// 创建新 Decimal（ref_count = 1）
    pub fn new(value: i64) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Decimal,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            inner: Decimal::from(value),
        }))
    }

    pub fn from_f64(value: f64) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Decimal,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            inner: Decimal::from_f64(value).unwrap_or(Decimal::ZERO),
        }))
    }

    pub fn from_decimal(inner: Decimal) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Decimal,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            inner,
        }))
    }

    pub fn from_str(s: &str) -> Option<*mut Self> {
        Decimal::from_str(s).ok().map(Self::from_decimal)
    }

    pub fn inner(&self) -> &Decimal {
        &self.inner
    }

    pub fn to_i64(&self) -> i64 {
        self.inner.to_i64().unwrap_or(0)
    }

    pub fn to_f64(&self) -> f64 {
        self.inner.to_f64().unwrap_or(0.0)
    }

    pub fn to_string(&self) -> String {
        self.inner.to_string()
    }

    pub fn is_zero(&self) -> bool {
        self.inner.is_zero()
    }

    pub fn is_positive(&self) -> bool {
        self.inner.is_sign_positive() && !self.inner.is_zero()
    }

    pub fn is_negative(&self) -> bool {
        self.inner.is_sign_negative()
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
pub extern "C" fn bolide_decimal_from_i64(value: i64) -> *mut BolideDecimal {
    BolideDecimal::new(value)
}

#[no_mangle]
pub extern "C" fn bolide_decimal_from_f64(value: f64) -> *mut BolideDecimal {
    BolideDecimal::from_f64(value)
}

#[no_mangle]
pub extern "C" fn bolide_decimal_from_str(s: *const i8, len: usize) -> *mut BolideDecimal {
    let slice = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    let s = std::str::from_utf8(slice).unwrap_or("");
    BolideDecimal::from_str(s).unwrap_or(std::ptr::null_mut())
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_decimal_retain(d: *mut BolideDecimal) -> *mut BolideDecimal {
    if !d.is_null() {
        unsafe { (*d).retain(); }
    }
    d
}

/// 减少引用计数
#[no_mangle]
pub extern "C" fn bolide_decimal_release(d: *mut BolideDecimal) {
    if d.is_null() { return; }
    unsafe {
        if (*d).release() {
            let _ = Box::from_raw(d);
        }
    }
}

/// 深拷贝
#[no_mangle]
pub extern "C" fn bolide_decimal_clone(a: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideDecimal::from_decimal(a.inner)
}

/// 兼容旧 API
#[no_mangle]
pub extern "C" fn bolide_decimal_free(d: *mut BolideDecimal) {
    bolide_decimal_release(d);
}

#[no_mangle]
pub extern "C" fn bolide_decimal_ref_count(d: *const BolideDecimal) -> u32 {
    if d.is_null() { return 0; }
    unsafe { (*d).ref_count() }
}

#[no_mangle]
pub extern "C" fn bolide_decimal_to_i64(a: *const BolideDecimal) -> i64 {
    if a.is_null() { return 0; }
    unsafe { (*a).to_i64() }
}

#[no_mangle]
pub extern "C" fn bolide_decimal_to_f64(a: *const BolideDecimal) -> f64 {
    if a.is_null() { return 0.0; }
    unsafe { (*a).to_f64() }
}

// ==================== 算术运算（返回新对象，ref_count = 1）====================

#[no_mangle]
pub extern "C" fn bolide_decimal_add(a: *const BolideDecimal, b: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    BolideDecimal::from_decimal(a.inner + b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_decimal_sub(a: *const BolideDecimal, b: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    BolideDecimal::from_decimal(a.inner - b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_decimal_mul(a: *const BolideDecimal, b: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    BolideDecimal::from_decimal(a.inner * b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_decimal_div(a: *const BolideDecimal, b: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    if b.is_zero() { return std::ptr::null_mut(); }
    BolideDecimal::from_decimal(a.inner / b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_decimal_rem(a: *const BolideDecimal, b: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() || b.is_null() { return std::ptr::null_mut(); }
    let (a, b) = unsafe { (&*a, &*b) };
    if b.is_zero() { return std::ptr::null_mut(); }
    BolideDecimal::from_decimal(a.inner % b.inner)
}

#[no_mangle]
pub extern "C" fn bolide_decimal_neg(a: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideDecimal::from_decimal(-a.inner)
}

// ==================== 比较运算 ====================

#[no_mangle]
pub extern "C" fn bolide_decimal_eq(a: *const BolideDecimal, b: *const BolideDecimal) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner == b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_decimal_ne(a: *const BolideDecimal, b: *const BolideDecimal) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner != b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_decimal_lt(a: *const BolideDecimal, b: *const BolideDecimal) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner < b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_decimal_le(a: *const BolideDecimal, b: *const BolideDecimal) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner <= b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_decimal_gt(a: *const BolideDecimal, b: *const BolideDecimal) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner > b.inner { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_decimal_ge(a: *const BolideDecimal, b: *const BolideDecimal) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let (a, b) = unsafe { (&*a, &*b) };
    if a.inner >= b.inner { 1 } else { 0 }
}

// ==================== 数学函数 ====================

#[no_mangle]
pub extern "C" fn bolide_decimal_abs(a: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideDecimal::from_decimal(a.inner.abs())
}

#[no_mangle]
pub extern "C" fn bolide_decimal_floor(a: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideDecimal::from_decimal(a.inner.floor())
}

#[no_mangle]
pub extern "C" fn bolide_decimal_ceil(a: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideDecimal::from_decimal(a.inner.ceil())
}

#[no_mangle]
pub extern "C" fn bolide_decimal_round(a: *const BolideDecimal) -> *mut BolideDecimal {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideDecimal::from_decimal(a.inner.round())
}

#[no_mangle]
pub extern "C" fn bolide_decimal_round_dp(a: *const BolideDecimal, dp: u32) -> *mut BolideDecimal {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };
    BolideDecimal::from_decimal(a.inner.round_dp(dp))
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decimal_rc() {
        let a = BolideDecimal::new(100);
        unsafe {
            assert_eq!((*a).ref_count(), 1);

            bolide_decimal_retain(a);
            assert_eq!((*a).ref_count(), 2);

            bolide_decimal_release(a);
            assert_eq!((*a).ref_count(), 1);

            bolide_decimal_release(a);
        }
    }

    #[test]
    fn test_decimal_arithmetic() {
        let a = BolideDecimal::new(100);
        let b = BolideDecimal::new(30);
        let c = bolide_decimal_add(a, b);
        unsafe {
            assert_eq!((*c).to_i64(), 130);
            assert_eq!((*c).ref_count(), 1);

            bolide_decimal_release(a);
            bolide_decimal_release(b);
            bolide_decimal_release(c);
        }
    }

    #[test]
    fn test_decimal_from_f64() {
        let d = BolideDecimal::from_f64(3.14);
        unsafe {
            assert!((*d).to_f64() > 3.13 && (*d).to_f64() < 3.15);
            assert_eq!((*d).ref_count(), 1);
            bolide_decimal_release(d);
        }
    }
}
