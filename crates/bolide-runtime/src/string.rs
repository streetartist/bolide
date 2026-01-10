//! Bolide String type with reference counting
//!
//! BolideString 使用引用计数管理内存：
//! - 创建时 strong_count = 1
//! - clone 时 strong_count += 1（浅拷贝）
//! - drop 时 strong_count -= 1，归零时释放

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

thread_local! {
    // String interner for literals (stores raw pointers with Strong RC=1 owned by interner)
    static STRING_LITERALS: RefCell<HashMap<String, *mut BolideString>> = RefCell::new(HashMap::new());
}

use crate::rc::{TypeTag, flags};

/// RC 对象头（与 rc.rs 中保持一致）
#[repr(C)]
struct RcHeader {
    strong_count: Cell<u32>,
    weak_count: Cell<u32>,
    type_tag: TypeTag,
    flags: Cell<u8>,
    _padding: [u8; 6],
}

/// Bolide 字符串类型（带引用计数）
///
/// 内存布局:
/// ```text
/// +------------------+
/// | RcHeader (16B)   |  引用计数头
/// +------------------+
/// | data: *mut char  |  C 字符串指针
/// +------------------+
/// | len: usize       |  字符串长度
/// +------------------+
/// | capacity: usize  |  分配容量
/// +------------------+
/// ```
#[repr(C)]
pub struct BolideString {
    header: RcHeader,
    data: *mut c_char,
    len: usize,
    capacity: usize,
}

impl BolideString {
    /// 创建新字符串（strong_count = 1）
    pub fn new(s: &str) -> *mut Self {
        let c_string = CString::new(s).unwrap();
        let len = s.len();
        let string = Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::String,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            data: c_string.into_raw(),
            len,
            capacity: len + 1,
        };
        Box::into_raw(Box::new(string))
    }

    /// 获取字符串内容
    pub fn as_str(&self) -> &str {
        if self.data.is_null() {
            return "";
        }
        unsafe {
            let c_str = CStr::from_ptr(self.data);
            c_str.to_str().unwrap_or("")
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    // ==================== RC 操作 ====================

    /// 增加引用计数
    #[inline]
    pub fn retain(&self) {
        let count = self.header.strong_count.get();
        debug_assert!(count > 0, "retain on dropped string");
        self.header.strong_count.set(count + 1);
    }

    /// 减少引用计数，返回是否应该释放
    #[inline]
    pub fn release(&self) -> bool {
        let count = self.header.strong_count.get();
        debug_assert!(count > 0, "release underflow");
        self.header.strong_count.set(count - 1);
        count == 1
    }

    /// 获取引用计数
    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.header.strong_count.get()
    }

    /// 检查是否已被 move
    #[inline]
    pub fn is_moved(&self) -> bool {
        self.header.flags.get() & flags::MOVED != 0
    }

    /// 标记为已 move
    #[inline]
    pub fn mark_moved(&self) {
        self.header.flags.set(self.header.flags.get() | flags::MOVED);
    }

    /// 释放内部数据（仅当 strong_count 归零时调用）
    unsafe fn drop_data(&mut self) {
        if !self.data.is_null() {
            let _ = CString::from_raw(self.data);
            self.data = std::ptr::null_mut();
        }
    }
}

// ==================== FFI 导出 ====================

/// 创建新字符串
#[no_mangle]
pub extern "C" fn bolide_string_new(s: *const c_char) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let c_str = unsafe { CStr::from_ptr(s) };
    BolideString::new(c_str.to_str().unwrap_or(""))
}

/// 从切片创建字符串
#[no_mangle]
pub extern "C" fn bolide_string_from_slice(s: *const i8, len: usize) -> *mut BolideString {
    let slice = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    let s = std::str::from_utf8(slice).unwrap_or("");
    BolideString::new(s)
}

/// 获取字符串字面量（带 Interning）
#[no_mangle]
pub extern "C" fn bolide_string_literal(s: *const i8, len: usize) -> *mut BolideString {
    let slice = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    let s_str = std::str::from_utf8(slice).unwrap_or("");
    
    STRING_LITERALS.with(|interner| {
        let mut map = interner.borrow_mut();
        if let Some(&ptr) = map.get(s_str) {
             // Found. Retain and return a NEW reference.
             unsafe { (*ptr).retain(); }
             ptr
        } else {
             // Not found. Create (RC=1).
             let ptr = BolideString::new(s_str);
             // Interner keeps the original RC=1.
             // We retain to give caller their own reference (RC=2).
             unsafe { (*ptr).retain(); }
             map.insert(s_str.to_string(), ptr);
             ptr
        }
    })
}

/// 增加引用计数（浅拷贝）
#[no_mangle]
pub extern "C" fn bolide_string_retain(s: *mut BolideString) -> *mut BolideString {
    if s.is_null() {
        return s;
    }
    unsafe {
        (*s).retain();
    }
    s
}

/// 减少引用计数，归零时释放
#[no_mangle]
pub extern "C" fn bolide_string_release(s: *mut BolideString) {
    if s.is_null() {
        return;
    }
    unsafe {
        if (*s).release() {
            // 引用计数归零，释放数据
            (*s).drop_data();
            let _ = Box::from_raw(s);
        }
    }
}

/// 深拷贝字符串（创建新对象，ref_count = 1）
#[no_mangle]
pub extern "C" fn bolide_string_clone(s: *const BolideString) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let s = unsafe { &*s };
    BolideString::new(s.as_str())
}

/// 释放字符串（兼容旧 API，等同于 release）
#[no_mangle]
pub extern "C" fn bolide_string_free(s: *mut BolideString) {
    bolide_string_release(s);
}

/// 获取字符串长度
#[no_mangle]
pub extern "C" fn bolide_string_len(s: *const BolideString) -> usize {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).len() }
}

/// 获取引用计数
#[no_mangle]
pub extern "C" fn bolide_string_ref_count(s: *const BolideString) -> u32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).ref_count() }
}

/// 字符串拼接（返回新字符串，ref_count = 1）
#[no_mangle]
pub extern "C" fn bolide_string_concat(a: *const BolideString, b: *const BolideString) -> *mut BolideString {
    let a_str = if a.is_null() { "" } else { unsafe { (*a).as_str() } };
    let b_str = if b.is_null() { "" } else { unsafe { (*b).as_str() } };
    let result = format!("{}{}", a_str, b_str);
    BolideString::new(&result)
}

/// 字符串比较
#[no_mangle]
pub extern "C" fn bolide_string_eq(a: *const BolideString, b: *const BolideString) -> i64 {
    if a.is_null() && b.is_null() {
        return 1;
    }
    if a.is_null() || b.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    let b = unsafe { &*b };
    if a.as_str() == b.as_str() { 1 } else { 0 }
}

/// 检查是否已被 move
#[no_mangle]
pub extern "C" fn bolide_string_is_moved(s: *const BolideString) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe {
        if (*s).is_moved() { 1 } else { 0 }
    }
}

/// 标记为已 move（spawn 使用）
#[no_mangle]
pub extern "C" fn bolide_string_mark_moved(s: *mut BolideString) {
    if !s.is_null() {
        unsafe { (*s).mark_moved(); }
    }
}

// ==================== 类型转换 ====================

#[no_mangle]
pub extern "C" fn bolide_string_from_int(value: i64) -> *mut BolideString {
    BolideString::new(&value.to_string())
}

#[no_mangle]
pub extern "C" fn bolide_string_from_float(value: f64) -> *mut BolideString {
    BolideString::new(&value.to_string())
}

#[no_mangle]
pub extern "C" fn bolide_string_from_bool(value: i64) -> *mut BolideString {
    let s = if value != 0 { "true" } else { "false" };
    BolideString::new(s)
}

/// 从 Rust String 创建 BolideString（内部使用）
pub fn bolide_string_from_rust(s: &str) -> *mut BolideString {
    BolideString::new(s)
}

/// 获取 BolideString 的 C 字符串指针（用于 FFI）
#[no_mangle]
pub extern "C" fn bolide_string_as_cstr(s: *const BolideString) -> *const c_char {
    if s.is_null() {
        return std::ptr::null();
    }
    unsafe { (*s).data }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_new() {
        let s = BolideString::new("hello");
        unsafe {
            assert_eq!((*s).as_str(), "hello");
            assert_eq!((*s).ref_count(), 1);
            bolide_string_release(s);
        }
    }

    #[test]
    fn test_string_retain_release() {
        let s = BolideString::new("test");
        unsafe {
            assert_eq!((*s).ref_count(), 1);

            bolide_string_retain(s);
            assert_eq!((*s).ref_count(), 2);

            bolide_string_retain(s);
            assert_eq!((*s).ref_count(), 3);

            bolide_string_release(s);
            assert_eq!((*s).ref_count(), 2);

            bolide_string_release(s);
            assert_eq!((*s).ref_count(), 1);

            bolide_string_release(s);
            // s 已被释放，不能再访问
        }
    }

    #[test]
    fn test_string_concat() {
        let a = BolideString::new("hello ");
        let b = BolideString::new("world");
        let c = bolide_string_concat(a, b);
        unsafe {
            assert_eq!((*c).as_str(), "hello world");
            assert_eq!((*c).ref_count(), 1);

            bolide_string_release(a);
            bolide_string_release(b);
            bolide_string_release(c);
        }
    }

    #[test]
    fn test_string_move_flag() {
        let s = BolideString::new("movable");
        unsafe {
            assert!(!(*s).is_moved());
            (*s).mark_moved();
            assert!((*s).is_moved());
            bolide_string_release(s);
        }
    }
}
