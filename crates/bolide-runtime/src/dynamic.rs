//! Bolide Dynamic type with reference counting
//!
//! BolideDynamic 是 Python 风格的动态类型，使用引用计数管理内存

use std::cell::Cell;

use crate::rc::{TypeTag, flags};
use crate::{BolideBigInt, BolideDecimal, BolideString, BolideList};

/// RC 对象头
#[repr(C)]
struct RcHeader {
    strong_count: Cell<u32>,
    weak_count: Cell<u32>,
    type_tag: TypeTag,
    flags: Cell<u8>,
    _padding: [u8; 6],
}

/// 动态值类型标签
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DynamicType {
    None = 0,
    Bool = 1,
    Int = 2,
    Float = 3,
    BigInt = 4,
    Decimal = 5,
    String = 6,
    List = 7,
}

/// 动态类型数据联合
#[repr(C)]
pub union DynamicData {
    pub none: (),
    pub bool_val: i64,
    pub int_val: i64,
    pub float_val: f64,
    pub bigint_ptr: *mut BolideBigInt,
    pub decimal_ptr: *mut BolideDecimal,
    pub string_ptr: *mut BolideString,
    pub list_ptr: *mut BolideList,
}

/// Bolide 动态类型（带引用计数）
#[repr(C)]
pub struct BolideDynamic {
    header: RcHeader,
    pub tag: DynamicType,
    pub data: DynamicData,
}

impl BolideDynamic {
    /// 创建 None 值
    pub fn none() -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Object,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            tag: DynamicType::None,
            data: DynamicData { none: () },
        }))
    }

    pub fn from_bool(value: bool) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Object,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            tag: DynamicType::Bool,
            data: DynamicData { bool_val: if value { 1 } else { 0 } },
        }))
    }

    pub fn from_int(value: i64) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Object,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            tag: DynamicType::Int,
            data: DynamicData { int_val: value },
        }))
    }

    pub fn from_float(value: f64) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Object,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            tag: DynamicType::Float,
            data: DynamicData { float_val: value },
        }))
    }

    pub fn from_bigint(ptr: *mut BolideBigInt) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Object,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            tag: DynamicType::BigInt,
            data: DynamicData { bigint_ptr: ptr },
        }))
    }

    pub fn from_decimal(ptr: *mut BolideDecimal) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Object,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            tag: DynamicType::Decimal,
            data: DynamicData { decimal_ptr: ptr },
        }))
    }

    pub fn from_string(ptr: *mut BolideString) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Object,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            tag: DynamicType::String,
            data: DynamicData { string_ptr: ptr },
        }))
    }

    pub fn from_list(ptr: *mut BolideList) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Object,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            tag: DynamicType::List,
            data: DynamicData { list_ptr: ptr },
        }))
    }

    pub fn get_type(&self) -> DynamicType {
        self.tag
    }

    pub fn type_name(&self) -> &'static str {
        match self.tag {
            DynamicType::None => "none",
            DynamicType::Bool => "bool",
            DynamicType::Int => "int",
            DynamicType::Float => "float",
            DynamicType::BigInt => "bigint",
            DynamicType::Decimal => "decimal",
            DynamicType::String => "str",
            DynamicType::List => "list",
        }
    }

    pub fn is_truthy(&self) -> bool {
        match self.tag {
            DynamicType::None => false,
            DynamicType::Bool => unsafe { self.data.bool_val != 0 },
            DynamicType::Int => unsafe { self.data.int_val != 0 },
            DynamicType::Float => unsafe { self.data.float_val != 0.0 },
            DynamicType::BigInt => unsafe {
                if self.data.bigint_ptr.is_null() { return false; }
                !(*self.data.bigint_ptr).is_zero()
            },
            DynamicType::Decimal => unsafe {
                if self.data.decimal_ptr.is_null() { return false; }
                !(*self.data.decimal_ptr).is_zero()
            },
            DynamicType::String => unsafe {
                if self.data.string_ptr.is_null() { return false; }
                (*self.data.string_ptr).len() > 0
            },
            DynamicType::List => unsafe {
                if self.data.list_ptr.is_null() { return false; }
                crate::bolide_list_len(self.data.list_ptr) > 0
            },
        }
    }

    pub fn to_int(&self) -> i64 {
        match self.tag {
            DynamicType::None => 0,
            DynamicType::Bool => unsafe { self.data.bool_val },
            DynamicType::Int => unsafe { self.data.int_val },
            DynamicType::Float => unsafe { self.data.float_val as i64 },
            DynamicType::BigInt => unsafe {
                if self.data.bigint_ptr.is_null() { 0 }
                else { (*self.data.bigint_ptr).to_i64().unwrap_or(0) }
            },
            DynamicType::Decimal => unsafe {
                if self.data.decimal_ptr.is_null() { 0 }
                else { (*self.data.decimal_ptr).to_i64() }
            },
            DynamicType::String => unsafe {
                if self.data.string_ptr.is_null() { 0 }
                else { (*self.data.string_ptr).as_str().parse().unwrap_or(0) }
            },
            DynamicType::List => 0,
        }
    }

    pub fn to_float(&self) -> f64 {
        match self.tag {
            DynamicType::None => 0.0,
            DynamicType::Bool => unsafe { self.data.bool_val as f64 },
            DynamicType::Int => unsafe { self.data.int_val as f64 },
            DynamicType::Float => unsafe { self.data.float_val },
            DynamicType::BigInt => unsafe {
                if self.data.bigint_ptr.is_null() { 0.0 }
                else { (*self.data.bigint_ptr).to_f64() }
            },
            DynamicType::Decimal => unsafe {
                if self.data.decimal_ptr.is_null() { 0.0 }
                else { (*self.data.decimal_ptr).to_f64() }
            },
            DynamicType::String => unsafe {
                if self.data.string_ptr.is_null() { 0.0 }
                else { (*self.data.string_ptr).as_str().parse().unwrap_or(0.0) }
            },
            DynamicType::List => 0.0,
        }
    }

    pub fn to_string_repr(&self) -> String {
        match self.tag {
            DynamicType::None => "none".to_string(),
            DynamicType::Bool => unsafe {
                if self.data.bool_val != 0 { "true".to_string() } else { "false".to_string() }
            },
            DynamicType::Int => unsafe { self.data.int_val.to_string() },
            DynamicType::Float => unsafe { self.data.float_val.to_string() },
            DynamicType::BigInt => unsafe {
                if self.data.bigint_ptr.is_null() { "null".to_string() }
                else { (*self.data.bigint_ptr).to_string() }
            },
            DynamicType::Decimal => unsafe {
                if self.data.decimal_ptr.is_null() { "null".to_string() }
                else { (*self.data.decimal_ptr).to_string() }
            },
            DynamicType::String => unsafe {
                if self.data.string_ptr.is_null() { "null".to_string() }
                else { (*self.data.string_ptr).as_str().to_string() }
            },
            DynamicType::List => "[...]".to_string(),
        }
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

    /// 释放内部数据的引用
    unsafe fn release_inner(&self) {
        match self.tag {
            DynamicType::BigInt => {
                if !self.data.bigint_ptr.is_null() {
                    crate::bolide_bigint_release(self.data.bigint_ptr);
                }
            },
            DynamicType::Decimal => {
                if !self.data.decimal_ptr.is_null() {
                    crate::bolide_decimal_release(self.data.decimal_ptr);
                }
            },
            DynamicType::String => {
                if !self.data.string_ptr.is_null() {
                    crate::bolide_string_release(self.data.string_ptr);
                }
            },
            DynamicType::List => {
                if !self.data.list_ptr.is_null() {
                    crate::bolide_list_release(self.data.list_ptr);
                }
            },
            _ => {}
        }
    }

    /// 增加内部数据的引用计数
    unsafe fn retain_inner(&self) {
        match self.tag {
            DynamicType::BigInt => {
                if !self.data.bigint_ptr.is_null() {
                    crate::bolide_bigint_retain(self.data.bigint_ptr);
                }
            },
            DynamicType::Decimal => {
                if !self.data.decimal_ptr.is_null() {
                    crate::bolide_decimal_retain(self.data.decimal_ptr);
                }
            },
            DynamicType::String => {
                if !self.data.string_ptr.is_null() {
                    crate::bolide_string_retain(self.data.string_ptr);
                }
            },
            DynamicType::List => {
                if !self.data.list_ptr.is_null() {
                    crate::bolide_list_retain(self.data.list_ptr);
                }
            },
            _ => {}
        }
    }
}

// ==================== FFI 导出 ====================

#[no_mangle]
pub extern "C" fn bolide_dynamic_none() -> *mut BolideDynamic {
    BolideDynamic::none()
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_bool(value: i64) -> *mut BolideDynamic {
    BolideDynamic::from_bool(value != 0)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_int(value: i64) -> *mut BolideDynamic {
    BolideDynamic::from_int(value)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_float(value: f64) -> *mut BolideDynamic {
    BolideDynamic::from_float(value)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_bigint(ptr: *mut BolideBigInt) -> *mut BolideDynamic {
    BolideDynamic::from_bigint(ptr)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_decimal(ptr: *mut BolideDecimal) -> *mut BolideDynamic {
    BolideDynamic::from_decimal(ptr)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_string(ptr: *mut BolideString) -> *mut BolideDynamic {
    BolideDynamic::from_string(ptr)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_list(ptr: *mut BolideList) -> *mut BolideDynamic {
    BolideDynamic::from_list(ptr)
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_dynamic_retain(d: *mut BolideDynamic) -> *mut BolideDynamic {
    if !d.is_null() {
        unsafe { (*d).retain(); }
    }
    d
}

/// 减少引用计数
#[no_mangle]
pub extern "C" fn bolide_dynamic_release(d: *mut BolideDynamic) {
    if d.is_null() { return; }
    unsafe {
        if (*d).release() {
            (*d).release_inner();
            let _ = Box::from_raw(d);
        }
    }
}

/// 深拷贝
#[no_mangle]
pub extern "C" fn bolide_dynamic_clone(a: *const BolideDynamic) -> *mut BolideDynamic {
    if a.is_null() { return std::ptr::null_mut(); }
    let a = unsafe { &*a };

    match a.tag {
        DynamicType::None => BolideDynamic::none(),
        DynamicType::Bool => unsafe { BolideDynamic::from_bool(a.data.bool_val != 0) },
        DynamicType::Int => unsafe { BolideDynamic::from_int(a.data.int_val) },
        DynamicType::Float => unsafe { BolideDynamic::from_float(a.data.float_val) },
        DynamicType::BigInt => unsafe {
            if a.data.bigint_ptr.is_null() {
                BolideDynamic::from_bigint(std::ptr::null_mut())
            } else {
                let cloned = crate::bolide_bigint_clone(a.data.bigint_ptr);
                BolideDynamic::from_bigint(cloned)
            }
        },
        DynamicType::Decimal => unsafe {
            if a.data.decimal_ptr.is_null() {
                BolideDynamic::from_decimal(std::ptr::null_mut())
            } else {
                let cloned = crate::bolide_decimal_clone(a.data.decimal_ptr);
                BolideDynamic::from_decimal(cloned)
            }
        },
        DynamicType::String => unsafe {
            if a.data.string_ptr.is_null() {
                BolideDynamic::from_string(std::ptr::null_mut())
            } else {
                let cloned = crate::bolide_string_clone(a.data.string_ptr);
                BolideDynamic::from_string(cloned)
            }
        },
        DynamicType::List => unsafe {
            if a.data.list_ptr.is_null() {
                BolideDynamic::from_list(std::ptr::null_mut())
            } else {
                let cloned = crate::bolide_list_clone(a.data.list_ptr);
                BolideDynamic::from_list(cloned)
            }
        },
    }
}

/// 兼容旧 API
#[no_mangle]
pub extern "C" fn bolide_dynamic_free(d: *mut BolideDynamic) {
    bolide_dynamic_release(d);
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_ref_count(d: *const BolideDynamic) -> u32 {
    if d.is_null() { return 0; }
    unsafe { (*d).ref_count() }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_get_type(a: *const BolideDynamic) -> i64 {
    if a.is_null() { return 0; }
    let a = unsafe { &*a };
    a.tag as i64
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_is_truthy(a: *const BolideDynamic) -> i64 {
    if a.is_null() { return 0; }
    let a = unsafe { &*a };
    if a.is_truthy() { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_to_int(a: *const BolideDynamic) -> i64 {
    if a.is_null() { return 0; }
    let a = unsafe { &*a };
    a.to_int()
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_to_float(a: *const BolideDynamic) -> f64 {
    if a.is_null() { return 0.0; }
    let a = unsafe { &*a };
    a.to_float()
}

// ==================== 动态算术运算 ====================

#[no_mangle]
pub extern "C" fn bolide_dynamic_add(a: *const BolideDynamic, b: *const BolideDynamic) -> *mut BolideDynamic {
    if a.is_null() || b.is_null() { return bolide_dynamic_none(); }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    match (a.tag, b.tag) {
        (DynamicType::Int, DynamicType::Int) => unsafe {
            BolideDynamic::from_int(a.data.int_val + b.data.int_val)
        },
        (DynamicType::Float, DynamicType::Float) => unsafe {
            BolideDynamic::from_float(a.data.float_val + b.data.float_val)
        },
        (DynamicType::Int, DynamicType::Float) | (DynamicType::Float, DynamicType::Int) => {
            BolideDynamic::from_float(a.to_float() + b.to_float())
        },
        (DynamicType::BigInt, DynamicType::BigInt) => unsafe {
            let result = crate::bolide_bigint_add(a.data.bigint_ptr, b.data.bigint_ptr);
            BolideDynamic::from_bigint(result)
        },
        (DynamicType::Decimal, DynamicType::Decimal) => unsafe {
            let result = crate::bolide_decimal_add(a.data.decimal_ptr, b.data.decimal_ptr);
            BolideDynamic::from_decimal(result)
        },
        (DynamicType::String, DynamicType::String) => unsafe {
            let result = crate::bolide_string_concat(a.data.string_ptr, b.data.string_ptr);
            BolideDynamic::from_string(result)
        },
        _ => {
            BolideDynamic::from_float(a.to_float() + b.to_float())
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_sub(a: *const BolideDynamic, b: *const BolideDynamic) -> *mut BolideDynamic {
    if a.is_null() || b.is_null() { return bolide_dynamic_none(); }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    match (a.tag, b.tag) {
        (DynamicType::Int, DynamicType::Int) => unsafe {
            BolideDynamic::from_int(a.data.int_val - b.data.int_val)
        },
        (DynamicType::Float, DynamicType::Float) => unsafe {
            BolideDynamic::from_float(a.data.float_val - b.data.float_val)
        },
        (DynamicType::BigInt, DynamicType::BigInt) => unsafe {
            let result = crate::bolide_bigint_sub(a.data.bigint_ptr, b.data.bigint_ptr);
            BolideDynamic::from_bigint(result)
        },
        (DynamicType::Decimal, DynamicType::Decimal) => unsafe {
            let result = crate::bolide_decimal_sub(a.data.decimal_ptr, b.data.decimal_ptr);
            BolideDynamic::from_decimal(result)
        },
        _ => {
            BolideDynamic::from_float(a.to_float() - b.to_float())
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_mul(a: *const BolideDynamic, b: *const BolideDynamic) -> *mut BolideDynamic {
    if a.is_null() || b.is_null() { return bolide_dynamic_none(); }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    match (a.tag, b.tag) {
        (DynamicType::Int, DynamicType::Int) => unsafe {
            BolideDynamic::from_int(a.data.int_val * b.data.int_val)
        },
        (DynamicType::Float, DynamicType::Float) => unsafe {
            BolideDynamic::from_float(a.data.float_val * b.data.float_val)
        },
        (DynamicType::BigInt, DynamicType::BigInt) => unsafe {
            let result = crate::bolide_bigint_mul(a.data.bigint_ptr, b.data.bigint_ptr);
            BolideDynamic::from_bigint(result)
        },
        (DynamicType::Decimal, DynamicType::Decimal) => unsafe {
            let result = crate::bolide_decimal_mul(a.data.decimal_ptr, b.data.decimal_ptr);
            BolideDynamic::from_decimal(result)
        },
        _ => {
            BolideDynamic::from_float(a.to_float() * b.to_float())
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_div(a: *const BolideDynamic, b: *const BolideDynamic) -> *mut BolideDynamic {
    if a.is_null() || b.is_null() { return bolide_dynamic_none(); }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    match (a.tag, b.tag) {
        (DynamicType::Int, DynamicType::Int) => unsafe {
            if b.data.int_val == 0 { return bolide_dynamic_none(); }
            BolideDynamic::from_int(a.data.int_val / b.data.int_val)
        },
        (DynamicType::BigInt, DynamicType::BigInt) => unsafe {
            let result = crate::bolide_bigint_div(a.data.bigint_ptr, b.data.bigint_ptr);
            BolideDynamic::from_bigint(result)
        },
        (DynamicType::Decimal, DynamicType::Decimal) => unsafe {
            let result = crate::bolide_decimal_div(a.data.decimal_ptr, b.data.decimal_ptr);
            BolideDynamic::from_decimal(result)
        },
        _ => {
            let bf = b.to_float();
            if bf == 0.0 { return bolide_dynamic_none(); }
            BolideDynamic::from_float(a.to_float() / bf)
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_neg(a: *const BolideDynamic) -> *mut BolideDynamic {
    if a.is_null() { return bolide_dynamic_none(); }
    let a = unsafe { &*a };

    match a.tag {
        DynamicType::Int => unsafe {
            BolideDynamic::from_int(-a.data.int_val)
        },
        DynamicType::Float => unsafe {
            BolideDynamic::from_float(-a.data.float_val)
        },
        DynamicType::BigInt => unsafe {
            let result = crate::bolide_bigint_neg(a.data.bigint_ptr);
            BolideDynamic::from_bigint(result)
        },
        DynamicType::Decimal => unsafe {
            let result = crate::bolide_decimal_neg(a.data.decimal_ptr);
            BolideDynamic::from_decimal(result)
        },
        _ => bolide_dynamic_none(),
    }
}

// ==================== 比较运算 ====================

#[no_mangle]
pub extern "C" fn bolide_dynamic_eq(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    if a.is_null() && b.is_null() { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    if a.tag != b.tag {
        return if (a.to_float() - b.to_float()).abs() < 1e-10 { 1 } else { 0 };
    }

    match a.tag {
        DynamicType::None => 1,
        DynamicType::Bool => unsafe { if a.data.bool_val == b.data.bool_val { 1 } else { 0 } },
        DynamicType::Int => unsafe { if a.data.int_val == b.data.int_val { 1 } else { 0 } },
        DynamicType::Float => unsafe { if (a.data.float_val - b.data.float_val).abs() < 1e-10 { 1 } else { 0 } },
        DynamicType::BigInt => unsafe { crate::bolide_bigint_eq(a.data.bigint_ptr, b.data.bigint_ptr) },
        DynamicType::Decimal => unsafe { crate::bolide_decimal_eq(a.data.decimal_ptr, b.data.decimal_ptr) },
        DynamicType::String => unsafe { crate::bolide_string_eq(a.data.string_ptr, b.data.string_ptr) },
        DynamicType::List => 0, // 列表比较暂不实现
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_lt(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    if a.is_null() || b.is_null() { return 0; }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    match (a.tag, b.tag) {
        (DynamicType::Int, DynamicType::Int) => unsafe { if a.data.int_val < b.data.int_val { 1 } else { 0 } },
        (DynamicType::Float, DynamicType::Float) => unsafe { if a.data.float_val < b.data.float_val { 1 } else { 0 } },
        (DynamicType::BigInt, DynamicType::BigInt) => unsafe { crate::bolide_bigint_lt(a.data.bigint_ptr, b.data.bigint_ptr) },
        (DynamicType::Decimal, DynamicType::Decimal) => unsafe { crate::bolide_decimal_lt(a.data.decimal_ptr, b.data.decimal_ptr) },
        _ => if a.to_float() < b.to_float() { 1 } else { 0 },
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_le(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    if bolide_dynamic_lt(a, b) == 1 || bolide_dynamic_eq(a, b) == 1 { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_gt(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    bolide_dynamic_lt(b, a)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_ge(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    if bolide_dynamic_gt(a, b) == 1 || bolide_dynamic_eq(a, b) == 1 { 1 } else { 0 }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynamic_rc() {
        let d = BolideDynamic::from_int(42);
        unsafe {
            assert_eq!((*d).ref_count(), 1);

            bolide_dynamic_retain(d);
            assert_eq!((*d).ref_count(), 2);

            bolide_dynamic_release(d);
            assert_eq!((*d).ref_count(), 1);

            bolide_dynamic_release(d);
        }
    }

    #[test]
    fn test_dynamic_with_string() {
        let s = crate::BolideString::new("hello");
        let d = BolideDynamic::from_string(s);
        unsafe {
            assert_eq!((*d).ref_count(), 1);
            assert_eq!((*d).tag, DynamicType::String);

            // 释放 dynamic 会自动释放内部的 string
            bolide_dynamic_release(d);
        }
    }

    #[test]
    fn test_dynamic_clone() {
        let d1 = BolideDynamic::from_int(100);
        let d2 = bolide_dynamic_clone(d1);
        unsafe {
            assert_eq!((*d1).to_int(), 100);
            assert_eq!((*d2).to_int(), 100);
            assert_eq!((*d1).ref_count(), 1);
            assert_eq!((*d2).ref_count(), 1);

            bolide_dynamic_release(d1);
            bolide_dynamic_release(d2);
        }
    }

    #[test]
    fn test_dynamic_arithmetic() {
        let a = BolideDynamic::from_int(10);
        let b = BolideDynamic::from_int(3);

        let sum = bolide_dynamic_add(a, b);
        let diff = bolide_dynamic_sub(a, b);
        let prod = bolide_dynamic_mul(a, b);
        let quot = bolide_dynamic_div(a, b);

        unsafe {
            assert_eq!((*sum).to_int(), 13);
            assert_eq!((*diff).to_int(), 7);
            assert_eq!((*prod).to_int(), 30);
            assert_eq!((*quot).to_int(), 3);

            bolide_dynamic_release(a);
            bolide_dynamic_release(b);
            bolide_dynamic_release(sum);
            bolide_dynamic_release(diff);
            bolide_dynamic_release(prod);
            bolide_dynamic_release(quot);
        }
    }
}
