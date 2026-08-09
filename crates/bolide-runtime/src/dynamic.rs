//! Bolide Dynamic type with reference counting
//!
//! BolideDynamic 是 Python 风格的动态类型，使用引用计数管理内存

use crate::rc::{RcHeader, TypeTag};
use crate::{
    BolideBigInt, BolideBytes, BolideDecimal, BolideDict, BolideList, BolideString, BolideTuple,
    ElementType,
};
use once_cell::sync::Lazy;

/// newtype 包装 *mut T 使其满足 Send+Sync（不可变单例指针跨线程安全）
struct DynPtr(*mut BolideDynamic);
unsafe impl Send for DynPtr {}
unsafe impl Sync for DynPtr {}

/// 创建一个不可变的堆分配 Dynamic 对象（永不释放，RC 操作无效应）
fn immortal_dynamic(tag: DynamicType, data: DynamicData) -> DynPtr {
    let ptr = Box::into_raw(Box::new(BolideDynamic {
        header: RcHeader::new(TypeTag::Object),
        tag,
        data,
    }));
    unsafe {
        (*ptr).header.make_immortal();
    }
    DynPtr(ptr)
}

// 不可变单例缓存：None / Bool / 小整数 (-128..255) 永不分配
static DYN_NONE: Lazy<DynPtr> =
    Lazy::new(|| immortal_dynamic(DynamicType::None, DynamicData { none: () }));
static DYN_TRUE: Lazy<DynPtr> =
    Lazy::new(|| immortal_dynamic(DynamicType::Bool, DynamicData { bool_val: 1 }));
static DYN_FALSE: Lazy<DynPtr> =
    Lazy::new(|| immortal_dynamic(DynamicType::Bool, DynamicData { bool_val: 0 }));
static SMALL_INTS: Lazy<Vec<DynPtr>> = Lazy::new(|| {
    (-128i64..=255)
        .map(|i| immortal_dynamic(DynamicType::Int, DynamicData { int_val: i }))
        .collect()
});

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
    Bytes = 8,
    Dict = 9,
    Tuple = 10,
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
    pub bytes_ptr: *mut BolideBytes,
    pub dict_ptr: *mut BolideDict,
    pub tuple_ptr: *mut BolideTuple,
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
        DYN_NONE.0
    }

    pub fn from_bool(value: bool) -> *mut Self {
        if value {
            DYN_TRUE.0
        } else {
            DYN_FALSE.0
        }
    }

    pub fn from_int(value: i64) -> *mut Self {
        if value >= -128 && value <= 255 {
            SMALL_INTS[(value + 128) as usize].0
        } else {
            Box::into_raw(Box::new(Self {
                header: RcHeader::new(TypeTag::Object),
                tag: DynamicType::Int,
                data: DynamicData { int_val: value },
            }))
        }
    }

    pub fn from_float(value: f64) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Object),
            tag: DynamicType::Float,
            data: DynamicData { float_val: value },
        }))
    }

    pub fn from_bigint(ptr: *mut BolideBigInt) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Object),
            tag: DynamicType::BigInt,
            data: DynamicData { bigint_ptr: ptr },
        }))
    }

    pub fn from_decimal(ptr: *mut BolideDecimal) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Object),
            tag: DynamicType::Decimal,
            data: DynamicData { decimal_ptr: ptr },
        }))
    }

    pub fn from_string(ptr: *mut BolideString) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Object),
            tag: DynamicType::String,
            data: DynamicData { string_ptr: ptr },
        }))
    }

    pub fn from_list(ptr: *mut BolideList) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Object),
            tag: DynamicType::List,
            data: DynamicData { list_ptr: ptr },
        }))
    }

    pub fn from_bytes(ptr: *mut BolideBytes) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Object),
            tag: DynamicType::Bytes,
            data: DynamicData { bytes_ptr: ptr },
        }))
    }

    pub fn from_dict(ptr: *mut BolideDict) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Object),
            tag: DynamicType::Dict,
            data: DynamicData { dict_ptr: ptr },
        }))
    }

    pub fn from_tuple(ptr: *mut BolideTuple) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Object),
            tag: DynamicType::Tuple,
            data: DynamicData { tuple_ptr: ptr },
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
            DynamicType::Bytes => "bytes",
            DynamicType::Dict => "dict",
            DynamicType::Tuple => "tuple",
        }
    }

    pub fn is_truthy(&self) -> bool {
        match self.tag {
            DynamicType::None => false,
            DynamicType::Bool => unsafe { self.data.bool_val != 0 },
            DynamicType::Int => unsafe { self.data.int_val != 0 },
            DynamicType::Float => unsafe { self.data.float_val != 0.0 },
            DynamicType::BigInt => unsafe {
                if self.data.bigint_ptr.is_null() {
                    return false;
                }
                !(*self.data.bigint_ptr).is_zero()
            },
            DynamicType::Decimal => unsafe {
                if self.data.decimal_ptr.is_null() {
                    return false;
                }
                !(*self.data.decimal_ptr).is_zero()
            },
            DynamicType::String => unsafe {
                if self.data.string_ptr.is_null() {
                    return false;
                }
                (*self.data.string_ptr).len() > 0
            },
            DynamicType::List => unsafe {
                if self.data.list_ptr.is_null() {
                    return false;
                }
                crate::bolide_list_len(self.data.list_ptr) > 0
            },
            DynamicType::Bytes => unsafe {
                if self.data.bytes_ptr.is_null() {
                    return false;
                }
                crate::bolide_bytes_len(self.data.bytes_ptr) > 0
            },
            DynamicType::Dict => unsafe {
                if self.data.dict_ptr.is_null() {
                    return false;
                }
                crate::bolide_dict_len(self.data.dict_ptr) > 0
            },
            DynamicType::Tuple => unsafe {
                if self.data.tuple_ptr.is_null() {
                    return false;
                }
                crate::bolide_tuple_len(self.data.tuple_ptr) > 0
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
                if self.data.bigint_ptr.is_null() {
                    0
                } else {
                    (*self.data.bigint_ptr).to_i64().unwrap_or(0)
                }
            },
            DynamicType::Decimal => unsafe {
                if self.data.decimal_ptr.is_null() {
                    0
                } else {
                    (*self.data.decimal_ptr).to_i64()
                }
            },
            DynamicType::String => unsafe {
                if self.data.string_ptr.is_null() {
                    0
                } else {
                    (*self.data.string_ptr).as_str().parse().unwrap_or(0)
                }
            },
            DynamicType::List => 0,
            DynamicType::Bytes => 0,
            DynamicType::Dict => 0,
            DynamicType::Tuple => 0,
        }
    }

    pub fn to_float(&self) -> f64 {
        match self.tag {
            DynamicType::None => 0.0,
            DynamicType::Bool => unsafe { self.data.bool_val as f64 },
            DynamicType::Int => unsafe { self.data.int_val as f64 },
            DynamicType::Float => unsafe { self.data.float_val },
            DynamicType::BigInt => unsafe {
                if self.data.bigint_ptr.is_null() {
                    0.0
                } else {
                    (*self.data.bigint_ptr).to_f64()
                }
            },
            DynamicType::Decimal => unsafe {
                if self.data.decimal_ptr.is_null() {
                    0.0
                } else {
                    (*self.data.decimal_ptr).to_f64()
                }
            },
            DynamicType::String => unsafe {
                if self.data.string_ptr.is_null() {
                    0.0
                } else {
                    (*self.data.string_ptr).as_str().parse().unwrap_or(0.0)
                }
            },
            DynamicType::List => 0.0,
            DynamicType::Bytes => 0.0,
            DynamicType::Dict => 0.0,
            DynamicType::Tuple => 0.0,
        }
    }

    pub fn to_string_repr(&self) -> String {
        match self.tag {
            DynamicType::None => "none".to_string(),
            DynamicType::Bool => unsafe {
                if self.data.bool_val != 0 {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            },
            DynamicType::Int => unsafe { self.data.int_val.to_string() },
            DynamicType::Float => unsafe { self.data.float_val.to_string() },
            DynamicType::BigInt => unsafe {
                if self.data.bigint_ptr.is_null() {
                    "null".to_string()
                } else {
                    (*self.data.bigint_ptr).to_string()
                }
            },
            DynamicType::Decimal => unsafe {
                if self.data.decimal_ptr.is_null() {
                    "null".to_string()
                } else {
                    (*self.data.decimal_ptr).to_string()
                }
            },
            DynamicType::String => unsafe {
                if self.data.string_ptr.is_null() {
                    "null".to_string()
                } else {
                    (*self.data.string_ptr).as_str().to_string()
                }
            },
            DynamicType::List => "[...]".to_string(),
            DynamicType::Dict => "{...}".to_string(),
            DynamicType::Tuple => "(...)".to_string(),
            DynamicType::Bytes => unsafe {
                if self.data.bytes_ptr.is_null() {
                    "null".to_string()
                } else {
                    let bytes = (*self.data.bytes_ptr).as_slice();
                    let mut repr = String::from("[");
                    for (i, byte) in bytes.iter().enumerate() {
                        if i > 0 {
                            repr.push_str(", ");
                        }
                        repr.push_str(&byte.to_string());
                    }
                    repr.push(']');
                    repr
                }
            },
        }
    }

    // ==================== RC 操作 ====================

    #[inline]
    pub fn retain(&self) {
        self.header.inc_strong();
    }

    #[inline]
    pub fn release(&self) -> bool {
        self.header.dec_strong()
    }

    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.header.strong_count()
    }

    #[inline]
    pub fn is_moved(&self) -> bool {
        self.header.is_moved()
    }

    #[inline]
    pub fn mark_moved(&self) {
        self.header.mark_moved();
    }

    /// 释放内部数据的引用
    unsafe fn release_inner(&self) {
        match self.tag {
            DynamicType::BigInt => {
                if !self.data.bigint_ptr.is_null() {
                    crate::bolide_bigint_release(self.data.bigint_ptr);
                }
            }
            DynamicType::Decimal => {
                if !self.data.decimal_ptr.is_null() {
                    crate::bolide_decimal_release(self.data.decimal_ptr);
                }
            }
            DynamicType::String => {
                if !self.data.string_ptr.is_null() {
                    crate::bolide_string_release(self.data.string_ptr);
                }
            }
            DynamicType::List => {
                if !self.data.list_ptr.is_null() {
                    crate::bolide_list_release(self.data.list_ptr);
                }
            }
            DynamicType::Bytes => {
                if !self.data.bytes_ptr.is_null() {
                    crate::bolide_bytes_release(self.data.bytes_ptr);
                }
            }
            DynamicType::Dict => {
                if !self.data.dict_ptr.is_null() {
                    crate::bolide_dict_release(self.data.dict_ptr);
                }
            }
            DynamicType::Tuple => {
                if !self.data.tuple_ptr.is_null() {
                    crate::bolide_tuple_release(self.data.tuple_ptr);
                }
            }
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
            }
            DynamicType::Decimal => {
                if !self.data.decimal_ptr.is_null() {
                    crate::bolide_decimal_retain(self.data.decimal_ptr);
                }
            }
            DynamicType::String => {
                if !self.data.string_ptr.is_null() {
                    crate::bolide_string_retain(self.data.string_ptr);
                }
            }
            DynamicType::List => {
                if !self.data.list_ptr.is_null() {
                    crate::bolide_list_retain(self.data.list_ptr);
                }
            }
            DynamicType::Bytes => {
                if !self.data.bytes_ptr.is_null() {
                    crate::bolide_bytes_retain(self.data.bytes_ptr);
                }
            }
            DynamicType::Dict => {
                if !self.data.dict_ptr.is_null() {
                    crate::bolide_dict_retain(self.data.dict_ptr);
                }
            }
            DynamicType::Tuple => {
                if !self.data.tuple_ptr.is_null() {
                    crate::bolide_tuple_retain(self.data.tuple_ptr);
                }
            }
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

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_bytes(ptr: *mut BolideBytes) -> *mut BolideDynamic {
    BolideDynamic::from_bytes(ptr)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_dict(ptr: *mut BolideDict) -> *mut BolideDynamic {
    BolideDynamic::from_dict(ptr)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_from_tuple(ptr: *mut BolideTuple) -> *mut BolideDynamic {
    BolideDynamic::from_tuple(ptr)
}

/// 解箱 dynamic 为 tuple 指针（非 Tuple 标签时返回 null）。
#[no_mangle]
pub extern "C" fn bolide_dynamic_to_tuple(a: *const BolideDynamic) -> *mut BolideTuple {
    if a.is_null() {
        return std::ptr::null_mut();
    }
    let a = unsafe { &*a };
    if a.tag == DynamicType::Tuple {
        unsafe { a.data.tuple_ptr }
    } else {
        std::ptr::null_mut()
    }
}

/// dynamic 长度：按标签分发到 list / dict / str / bytes / tuple。
#[no_mangle]
pub extern "C" fn bolide_dynamic_len(d: *const BolideDynamic) -> i64 {
    if d.is_null() {
        return 0;
    }
    let a = unsafe { &*d };
    match a.tag {
        DynamicType::List => crate::bolide_list_len(unsafe { a.data.list_ptr }) as i64,
        DynamicType::Dict => crate::bolide_dict_len(unsafe { a.data.dict_ptr }) as i64,
        DynamicType::String => crate::bolide_string_len(unsafe { a.data.string_ptr }) as i64,
        DynamicType::Bytes => crate::bolide_bytes_len(unsafe { a.data.bytes_ptr }) as i64,
        DynamicType::Tuple => crate::bolide_tuple_len(unsafe { a.data.tuple_ptr }) as i64,
        _ => 0,
    }
}

/// 把容器元素（tag + 裸值）装箱成 dynamic 指针返回。
/// RC 元素（str/bigint/decimal/bytes/list/dict/tuple）先 retain 一份，
/// dynamic 元素直接 retain 后返回；标量直接装箱。
fn box_dynamic_element(tag: u8, raw: i64) -> i64 {
    let elem = ElementType::from_u8(tag);
    match elem {
        ElementType::Dynamic => {
            let d = raw as *mut BolideDynamic;
            if !d.is_null() {
                unsafe {
                    (*d).retain();
                }
            }
            raw
        }
        ElementType::Int => BolideDynamic::from_int(raw) as i64,
        ElementType::Float => BolideDynamic::from_float(f64::from_bits(raw as u64)) as i64,
        ElementType::Bool => BolideDynamic::from_bool(raw != 0) as i64,
        ElementType::String => {
            let p = raw as *mut BolideString;
            if !p.is_null() {
                crate::bolide_string_retain(p);
            }
            BolideDynamic::from_string(p) as i64
        }
        ElementType::BigInt => {
            let p = raw as *mut BolideBigInt;
            if !p.is_null() {
                crate::bolide_bigint_retain(p);
            }
            BolideDynamic::from_bigint(p) as i64
        }
        ElementType::Decimal => {
            let p = raw as *mut BolideDecimal;
            if !p.is_null() {
                crate::bolide_decimal_retain(p);
            }
            BolideDynamic::from_decimal(p) as i64
        }
        ElementType::Bytes => {
            let p = raw as *mut BolideBytes;
            if !p.is_null() {
                crate::bolide_bytes_retain(p);
            }
            BolideDynamic::from_bytes(p) as i64
        }
        ElementType::List => {
            let p = raw as *mut BolideList;
            if !p.is_null() {
                crate::bolide_list_retain(p);
            }
            BolideDynamic::from_list(p) as i64
        }
        ElementType::Dict => {
            let p = raw as *mut BolideDict;
            if !p.is_null() {
                crate::bolide_dict_retain(p);
            }
            BolideDynamic::from_dict(p) as i64
        }
        _ => BolideDynamic::from_int(raw) as i64,
    }
}

/// dynamic 索引读取：按标签分发到 list / dict / tuple，并把元素装箱成 dynamic。
/// idx 对 list/tuple 是整数下标，对 dict 是键指针（i64 承载）。
#[no_mangle]
pub extern "C" fn bolide_dynamic_index(d: *const BolideDynamic, idx: i64) -> i64 {
    if d.is_null() {
        return 0;
    }
    let a = unsafe { &*d };
    match a.tag {
        DynamicType::List => {
            let list = unsafe { a.data.list_ptr };
            let raw = crate::bolide_list_get(list, idx as usize);
            box_dynamic_element(crate::bolide_list_element_type(list), raw)
        }
        DynamicType::Dict => {
            let dict = unsafe { a.data.dict_ptr };
            let raw = crate::bolide_dict_get(dict, idx);
            box_dynamic_element(crate::bolide_dict_value_type(dict), raw)
        }
        DynamicType::Tuple => {
            let tup = unsafe { a.data.tuple_ptr };
            let raw = crate::bolide_tuple_get(tup, idx as usize);
            box_dynamic_element(crate::bolide_tuple_get_type(tup, idx as usize), raw)
        }
        _ => 0,
    }
}

/// dynamic 索引写入：按标签分发到 list / dict / tuple。
#[no_mangle]
pub extern "C" fn bolide_dynamic_index_set(d: *mut BolideDynamic, idx: i64, val: i64) {
    if d.is_null() {
        return;
    }
    let a = unsafe { &*d };
    match a.tag {
        DynamicType::List => {
            crate::bolide_list_set(unsafe { a.data.list_ptr }, idx as usize, val);
        }
        DynamicType::Dict => {
            crate::bolide_dict_set(unsafe { a.data.dict_ptr }, idx, val);
        }
        DynamicType::Tuple => {
            crate::bolide_tuple_set(unsafe { a.data.tuple_ptr }, idx as usize, val);
        }
        _ => {}
    }
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_dynamic_retain(d: *mut BolideDynamic) -> *mut BolideDynamic {
    if !d.is_null() {
        unsafe {
            (*d).retain();
        }
    }
    d
}

/// 减少引用计数
#[no_mangle]
pub extern "C" fn bolide_dynamic_release(d: *mut BolideDynamic) {
    if d.is_null() {
        return;
    }
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
    if a.is_null() {
        return std::ptr::null_mut();
    }
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
        DynamicType::Bytes => unsafe {
            if a.data.bytes_ptr.is_null() {
                BolideDynamic::from_bytes(std::ptr::null_mut())
            } else {
                let cloned = crate::bolide_bytes_clone(a.data.bytes_ptr);
                BolideDynamic::from_bytes(cloned)
            }
        },
        DynamicType::Dict => unsafe {
            if a.data.dict_ptr.is_null() {
                BolideDynamic::from_dict(std::ptr::null_mut())
            } else {
                let cloned = crate::bolide_dict_clone(a.data.dict_ptr);
                BolideDynamic::from_dict(cloned)
            }
        },
        DynamicType::Tuple => unsafe {
            if a.data.tuple_ptr.is_null() {
                BolideDynamic::from_tuple(std::ptr::null_mut())
            } else {
                let cloned = crate::bolide_tuple_clone(a.data.tuple_ptr);
                BolideDynamic::from_tuple(cloned)
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
    if d.is_null() {
        return 0;
    }
    unsafe { (*d).ref_count() }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_get_type(a: *const BolideDynamic) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    a.tag as i64
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_is_truthy(a: *const BolideDynamic) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    if a.is_truthy() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_to_int(a: *const BolideDynamic) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    a.to_int()
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_to_float(a: *const BolideDynamic) -> f64 {
    if a.is_null() {
        return 0.0;
    }
    let a = unsafe { &*a };
    a.to_float()
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_to_string(a: *const BolideDynamic) -> *mut BolideString {
    if a.is_null() {
        return BolideString::new("none");
    }
    let a = unsafe { &*a };
    BolideString::new(&a.to_string_repr())
}

/// 解箱 dynamic 为 list 指针（非 List 标签时返回 null）。
#[no_mangle]
pub extern "C" fn bolide_dynamic_to_list(a: *const BolideDynamic) -> *mut BolideList {
    if a.is_null() {
        return std::ptr::null_mut();
    }
    let a = unsafe { &*a };
    if a.tag == DynamicType::List {
        unsafe { a.data.list_ptr }
    } else {
        std::ptr::null_mut()
    }
}

/// 解箱 dynamic 为 dict 指针（非 Dict 标签时返回 null）。
#[no_mangle]
pub extern "C" fn bolide_dynamic_to_dict(a: *const BolideDynamic) -> *mut BolideDict {
    if a.is_null() {
        return std::ptr::null_mut();
    }
    let a = unsafe { &*a };
    if a.tag == DynamicType::Dict {
        unsafe { a.data.dict_ptr }
    } else {
        std::ptr::null_mut()
    }
}

// ==================== 动态算术运算 ====================

#[no_mangle]
pub extern "C" fn bolide_dynamic_add(
    a: *const BolideDynamic,
    b: *const BolideDynamic,
) -> *mut BolideDynamic {
    if a.is_null() || b.is_null() {
        return bolide_dynamic_none();
    }
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
        }
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
        _ => BolideDynamic::from_float(a.to_float() + b.to_float()),
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_sub(
    a: *const BolideDynamic,
    b: *const BolideDynamic,
) -> *mut BolideDynamic {
    if a.is_null() || b.is_null() {
        return bolide_dynamic_none();
    }
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
        _ => BolideDynamic::from_float(a.to_float() - b.to_float()),
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_mul(
    a: *const BolideDynamic,
    b: *const BolideDynamic,
) -> *mut BolideDynamic {
    if a.is_null() || b.is_null() {
        return bolide_dynamic_none();
    }
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
        _ => BolideDynamic::from_float(a.to_float() * b.to_float()),
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_div(
    a: *const BolideDynamic,
    b: *const BolideDynamic,
) -> *mut BolideDynamic {
    if a.is_null() || b.is_null() {
        return bolide_dynamic_none();
    }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    match (a.tag, b.tag) {
        (DynamicType::Int, DynamicType::Int) => unsafe {
            if b.data.int_val == 0 {
                return bolide_dynamic_none();
            }
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
            if bf == 0.0 {
                return bolide_dynamic_none();
            }
            BolideDynamic::from_float(a.to_float() / bf)
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_neg(a: *const BolideDynamic) -> *mut BolideDynamic {
    if a.is_null() {
        return bolide_dynamic_none();
    }
    let a = unsafe { &*a };

    match a.tag {
        DynamicType::Int => unsafe { BolideDynamic::from_int(-a.data.int_val) },
        DynamicType::Float => unsafe { BolideDynamic::from_float(-a.data.float_val) },
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
    if a.is_null() && b.is_null() {
        return 1;
    }
    if a.is_null() || b.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    if a.tag != b.tag {
        return if (a.to_float() - b.to_float()).abs() < 1e-10 {
            1
        } else {
            0
        };
    }

    match a.tag {
        DynamicType::None => 1,
        DynamicType::Bool => unsafe {
            if a.data.bool_val == b.data.bool_val {
                1
            } else {
                0
            }
        },
        DynamicType::Int => unsafe {
            if a.data.int_val == b.data.int_val {
                1
            } else {
                0
            }
        },
        DynamicType::Float => unsafe {
            if (a.data.float_val - b.data.float_val).abs() < 1e-10 {
                1
            } else {
                0
            }
        },
        DynamicType::BigInt => unsafe {
            crate::bolide_bigint_eq(a.data.bigint_ptr, b.data.bigint_ptr)
        },
        DynamicType::Decimal => unsafe {
            crate::bolide_decimal_eq(a.data.decimal_ptr, b.data.decimal_ptr)
        },
        DynamicType::String => unsafe {
            crate::bolide_string_eq(a.data.string_ptr, b.data.string_ptr)
        },
        DynamicType::List => 0, // 列表比较暂不实现
        DynamicType::Bytes => unsafe {
            if a.data.bytes_ptr.is_null() || b.data.bytes_ptr.is_null() {
                if a.data.bytes_ptr.is_null() && b.data.bytes_ptr.is_null() {
                    1
                } else {
                    0
                }
            } else if (*a.data.bytes_ptr).as_slice() == (*b.data.bytes_ptr).as_slice() {
                1
            } else {
                0
            }
        },
        DynamicType::Dict => 0, // 字典比较暂不实现
        DynamicType::Tuple => 0, // 元组比较暂不实现
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_lt(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    if a.is_null() || b.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    let b = unsafe { &*b };

    match (a.tag, b.tag) {
        (DynamicType::Int, DynamicType::Int) => unsafe {
            if a.data.int_val < b.data.int_val {
                1
            } else {
                0
            }
        },
        (DynamicType::Float, DynamicType::Float) => unsafe {
            if a.data.float_val < b.data.float_val {
                1
            } else {
                0
            }
        },
        (DynamicType::BigInt, DynamicType::BigInt) => unsafe {
            crate::bolide_bigint_lt(a.data.bigint_ptr, b.data.bigint_ptr)
        },
        (DynamicType::Decimal, DynamicType::Decimal) => unsafe {
            crate::bolide_decimal_lt(a.data.decimal_ptr, b.data.decimal_ptr)
        },
        _ => {
            if a.to_float() < b.to_float() {
                1
            } else {
                0
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_le(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    if bolide_dynamic_lt(a, b) == 1 || bolide_dynamic_eq(a, b) == 1 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_gt(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    bolide_dynamic_lt(b, a)
}

#[no_mangle]
pub extern "C" fn bolide_dynamic_ge(a: *const BolideDynamic, b: *const BolideDynamic) -> i64 {
    if bolide_dynamic_gt(a, b) == 1 || bolide_dynamic_eq(a, b) == 1 {
        1
    } else {
        0
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynamic_rc() {
        // 使用超出小整数缓存的值以测试完整 RC 语义
        let d = BolideDynamic::from_int(1000);
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
