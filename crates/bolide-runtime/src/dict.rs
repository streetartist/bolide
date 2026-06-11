//! Bolide Dict type with reference counting
//!
//! BolideDict 使用引用计数管理内存
//! 键值以 i64 存储（可以是值或指针）
//!
//! - 键为 string 时按内容哈希/比较（而非指针），并对键持有强引用
//! - 哈希器使用 FxHash（键来自程序内部，无需抗碰撞攻击的 SipHash）

use std::hash::{Hash, Hasher};
use std::os::raw::c_void;

use rustc_hash::FxHashMap;

use crate::list::ElementType;
use crate::rc::{RcHeader, TypeTag};
use crate::{BolideBigInt, BolideDecimal, BolideList, BolideString};

/// 字典键。string 键按内容哈希与比较，其余按 i64 原值
#[derive(Clone, Copy)]
struct DictKey {
    raw: i64,
    is_str: bool,
}

impl DictKey {
    #[inline]
    fn as_str(&self) -> Option<&str> {
        if !self.is_str {
            return None;
        }
        let ptr = self.raw as *const BolideString;
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { (*ptr).as_str() })
    }
}

impl PartialEq for DictKey {
    fn eq(&self, other: &Self) -> bool {
        if self.raw == other.raw {
            return true;
        }
        match (self.as_str(), other.as_str()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for DictKey {}

impl Hash for DictKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self.as_str() {
            Some(s) => s.hash(state),
            None => self.raw.hash(state),
        }
    }
}

/// Bolide 字典类型（带引用计数）
#[repr(C)]
pub struct BolideDict {
    header: RcHeader,
    key_type: ElementType,
    value_type: ElementType,
    map: FxHashMap<DictKey, i64>,
}

impl BolideDict {
    /// 创建新字典（ref_count = 1）
    pub fn new(key_type: ElementType, value_type: ElementType) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Dict),
            key_type,
            value_type,
            map: FxHashMap::default(),
        }))
    }

    #[inline]
    fn make_key(&self, raw: i64) -> DictKey {
        DictKey {
            raw,
            is_str: self.key_type == ElementType::String,
        }
    }

    /// 获取引用计数
    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.header.strong_count()
    }

    /// 增加引用计数
    pub fn retain(&self) {
        self.header.inc_strong();
    }

    /// 减少引用计数，返回是否应该释放
    pub fn release(&self) -> bool {
        self.header.dec_strong()
    }

    /// 设置键值对
    pub fn set(&mut self, key: i64, value: i64) {
        let k = self.make_key(key);
        // 注意：HashMap::insert 在键已存在时保留原有键对象，
        // 因此仅在新插入时 retain 键
        if let Some(old_value) = self.map.insert(k, value) {
            self.release_raw(self.value_type, old_value);
        } else {
            self.retain_raw(self.key_type, key);
        }
        // 增加新值的引用计数
        self.retain_raw(self.value_type, value);
    }

    /// 获取值（不存在返回 None）
    pub fn get(&self, key: i64) -> Option<i64> {
        self.map.get(&self.make_key(key)).copied()
    }

    /// 检查键是否存在
    pub fn contains(&self, key: i64) -> bool {
        self.map.contains_key(&self.make_key(key))
    }

    /// 移除键值对，返回值（值的所有权转移给调用者）
    pub fn remove(&mut self, key: i64) -> Option<i64> {
        if let Some((stored_key, value)) = self.map.remove_entry(&self.make_key(key)) {
            // 释放字典持有的键引用；值不释放，所有权转移给调用者
            self.release_raw(self.key_type, stored_key.raw);
            Some(value)
        } else {
            None
        }
    }

    /// 获取长度
    #[inline]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 清空字典
    pub fn clear(&mut self) {
        let key_type = self.key_type;
        let value_type = self.value_type;
        for (key, value) in self.map.drain() {
            release_raw_static(key_type, key.raw);
            release_raw_static(value_type, value);
        }
    }

    /// 获取所有键
    pub fn keys(&self) -> Vec<i64> {
        self.map.keys().map(|k| k.raw).collect()
    }

    /// 获取所有值
    pub fn values(&self) -> Vec<i64> {
        self.map.values().copied().collect()
    }

    /// 获取键类型
    #[inline]
    pub fn key_type(&self) -> ElementType {
        self.key_type
    }

    /// 获取值类型
    #[inline]
    pub fn value_type(&self) -> ElementType {
        self.value_type
    }

    /// 检查是否已被 move
    pub fn is_moved(&self) -> bool {
        self.header.is_moved()
    }

    /// 标记为已 move
    pub fn mark_moved(&self) {
        self.header.mark_moved();
    }

    /// 增加 RC 类型元素的引用计数（键/值通用）
    fn retain_raw(&self, elem_type: ElementType, raw: i64) {
        retain_raw_static(elem_type, raw);
    }

    /// 释放 RC 类型元素的引用计数（键/值通用）
    fn release_raw(&self, elem_type: ElementType, raw: i64) {
        release_raw_static(elem_type, raw);
    }
}

fn retain_raw_static(elem_type: ElementType, raw: i64) {
    let ptr = raw as *mut c_void;
    if ptr.is_null() {
        return;
    }
    match elem_type {
        ElementType::String => unsafe {
            crate::bolide_string_retain(ptr as *mut BolideString);
        },
        ElementType::BigInt => unsafe {
            crate::bolide_bigint_retain(ptr as *mut BolideBigInt);
        },
        ElementType::Decimal => unsafe {
            crate::bolide_decimal_retain(ptr as *mut BolideDecimal);
        },
        ElementType::List => unsafe {
            crate::bolide_list_retain(ptr as *mut BolideList);
        },
        ElementType::Dict => unsafe {
            crate::bolide_dict_retain(ptr as *mut BolideDict);
        },
        ElementType::Dynamic => unsafe {
            crate::bolide_dynamic_retain(ptr as *mut crate::dynamic::BolideDynamic);
        },
        _ => {}
    }
}

fn release_raw_static(elem_type: ElementType, raw: i64) {
    let ptr = raw as *mut c_void;
    if ptr.is_null() {
        return;
    }
    match elem_type {
        ElementType::String => unsafe {
            crate::bolide_string_release(ptr as *mut BolideString);
        },
        ElementType::BigInt => unsafe {
            crate::bolide_bigint_release(ptr as *mut BolideBigInt);
        },
        ElementType::Decimal => unsafe {
            crate::bolide_decimal_release(ptr as *mut BolideDecimal);
        },
        ElementType::List => unsafe {
            crate::bolide_list_release(ptr as *mut BolideList);
        },
        ElementType::Dict => unsafe {
            crate::bolide_dict_release(ptr as *mut BolideDict);
        },
        ElementType::Dynamic => unsafe {
            crate::bolide_dynamic_release(ptr as *mut crate::dynamic::BolideDynamic);
        },
        _ => {}
    }
}

impl Drop for BolideDict {
    fn drop(&mut self) {
        // 释放所有键和值的引用
        for (key, &value) in self.map.iter() {
            release_raw_static(self.key_type, key.raw);
            release_raw_static(self.value_type, value);
        }
    }
}

// ==================== FFI 接口 ====================

/// 创建新字典
#[no_mangle]
pub extern "C" fn bolide_dict_new(key_type: u8, value_type: u8) -> *mut BolideDict {
    let kt = unsafe { std::mem::transmute::<u8, ElementType>(key_type) };
    let vt = unsafe { std::mem::transmute::<u8, ElementType>(value_type) };
    BolideDict::new(kt, vt)
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_dict_retain(dict: *mut BolideDict) {
    if !dict.is_null() {
        unsafe { (*dict).retain(); }
    }
}

/// 减少引用计数
#[no_mangle]
pub extern "C" fn bolide_dict_release(dict: *mut BolideDict) {
    if dict.is_null() { return; }
    unsafe {
        if (*dict).release() {
            let _ = Box::from_raw(dict);
        }
    }
}

/// 克隆字典（深拷贝）
#[no_mangle]
pub extern "C" fn bolide_dict_clone(dict: *const BolideDict) -> *mut BolideDict {
    if dict.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let src = &*dict;
        let new_dict = BolideDict::new(src.key_type, src.value_type);
        let dst = &mut *new_dict;

        for (key, &value) in src.map.iter() {
            dst.set(key.raw, value);
        }

        new_dict
    }
}

/// 设置键值对
#[no_mangle]
pub extern "C" fn bolide_dict_set(dict: *mut BolideDict, key: i64, value: i64) {
    if dict.is_null() { return; }
    unsafe { (*dict).set(key, value); }
}

/// 获取值（不存在返回 0）
#[no_mangle]
pub extern "C" fn bolide_dict_get(dict: *const BolideDict, key: i64) -> i64 {
    if dict.is_null() { return 0; }
    unsafe { (*dict).get(key).unwrap_or(0) }
}

/// 检查键是否存在
#[no_mangle]
pub extern "C" fn bolide_dict_contains(dict: *const BolideDict, key: i64) -> i64 {
    if dict.is_null() { return 0; }
    unsafe { if (*dict).contains(key) { 1 } else { 0 } }
}

/// 移除键值对，返回值
#[no_mangle]
pub extern "C" fn bolide_dict_remove(dict: *mut BolideDict, key: i64) -> i64 {
    if dict.is_null() { return 0; }
    unsafe { (*dict).remove(key).unwrap_or(0) }
}

/// 获取长度
#[no_mangle]
pub extern "C" fn bolide_dict_len(dict: *const BolideDict) -> i64 {
    if dict.is_null() { return 0; }
    unsafe { (*dict).len() as i64 }
}

/// 是否为空
#[no_mangle]
pub extern "C" fn bolide_dict_is_empty(dict: *const BolideDict) -> i64 {
    if dict.is_null() { return 1; }
    unsafe { if (*dict).is_empty() { 1 } else { 0 } }
}

/// 清空字典
#[no_mangle]
pub extern "C" fn bolide_dict_clear(dict: *mut BolideDict) {
    if dict.is_null() { return; }
    unsafe { (*dict).clear(); }
}

/// 获取所有键（返回新列表）
#[no_mangle]
pub extern "C" fn bolide_dict_keys(dict: *const BolideDict) -> *mut BolideList {
    if dict.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let d = &*dict;
        let keys = d.keys();
        let list = crate::list::BolideList::with_capacity(d.key_type, keys.len());
        for key in keys {
            crate::bolide_list_push(list, key);
        }
        list
    }
}

/// 获取所有值（返回新列表）
#[no_mangle]
pub extern "C" fn bolide_dict_values(dict: *const BolideDict) -> *mut BolideList {
    if dict.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let d = &*dict;
        let values = d.values();
        let list = crate::list::BolideList::with_capacity(d.value_type, values.len());
        for value in values {
            // push 内部会对 RC 类型元素 retain
            crate::bolide_list_push(list, value);
        }
        list
    }
}

/// 获取键类型
#[no_mangle]
pub extern "C" fn bolide_dict_key_type(dict: *const BolideDict) -> u8 {
    if dict.is_null() { return 0; }
    unsafe { (*dict).key_type() as u8 }
}

/// 获取值类型
#[no_mangle]
pub extern "C" fn bolide_dict_value_type(dict: *const BolideDict) -> u8 {
    if dict.is_null() { return 0; }
    unsafe { (*dict).value_type() as u8 }
}

/// 检查是否已被 move
#[no_mangle]
pub extern "C" fn bolide_dict_is_moved(dict: *const BolideDict) -> i64 {
    if dict.is_null() { return 0; }
    unsafe { if (*dict).is_moved() { 1 } else { 0 } }
}

/// 标记为已 move
#[no_mangle]
pub extern "C" fn bolide_dict_mark_moved(dict: *mut BolideDict) {
    if !dict.is_null() {
        unsafe { (*dict).mark_moved(); }
    }
}

/// 打印字典
#[no_mangle]
pub extern "C" fn bolide_print_dict(dict: *const BolideDict) {
    if dict.is_null() {
        println!("{{}}");
        return;
    }
    unsafe {
        let d = &*dict;
        print!("{{");
        let mut first = true;
        for (key, &value) in d.map.iter() {
            if !first { print!(", "); }
            first = false;

            // 打印键
            match d.key_type {
                ElementType::Int => print!("{}", key.raw),
                ElementType::String => {
                    let s = key.raw as *const BolideString;
                    if !s.is_null() {
                        print!("\"{}\"", (*s).as_str());
                    } else {
                        print!("null");
                    }
                }
                _ => print!("{}", key.raw),
            }

            print!(": ");

            // 打印值
            match d.value_type {
                ElementType::Int => print!("{}", value),
                ElementType::Float => print!("{}", f64::from_bits(value as u64)),
                ElementType::Bool => print!("{}", if value != 0 { "true" } else { "false" }),
                ElementType::String => {
                    let s = value as *const BolideString;
                    if !s.is_null() {
                        print!("\"{}\"", (*s).as_str());
                    } else {
                        print!("null");
                    }
                }
                _ => print!("{}", value),
            }
        }
        println!("}}");
    }
}

// ==================== 迭代器支持 (for 循环) ====================

/// 创建字典迭代器（返回键的列表用于迭代）
#[no_mangle]
pub extern "C" fn bolide_dict_iter(dict: *const BolideDict) -> *mut BolideList {
    // 使用 keys() 返回的列表进行迭代
    bolide_dict_keys(dict)
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dict_basic() {
        let dict = BolideDict::new(ElementType::Int, ElementType::Int);
        unsafe {
            bolide_dict_set(dict, 1, 100);
            bolide_dict_set(dict, 2, 200);
            bolide_dict_set(dict, 3, 300);

            assert_eq!((*dict).len(), 3);
            assert_eq!(bolide_dict_get(dict, 1), 100);
            assert_eq!(bolide_dict_get(dict, 2), 200);
            assert_eq!(bolide_dict_get(dict, 3), 300);
            assert_eq!(bolide_dict_get(dict, 999), 0); // not found

            assert_eq!(bolide_dict_contains(dict, 1), 1);
            assert_eq!(bolide_dict_contains(dict, 999), 0);

            let removed = bolide_dict_remove(dict, 2);
            assert_eq!(removed, 200);
            assert_eq!((*dict).len(), 2);

            bolide_dict_release(dict);
        }
    }

    #[test]
    fn test_dict_clone() {
        let dict = BolideDict::new(ElementType::Int, ElementType::Int);
        unsafe {
            bolide_dict_set(dict, 1, 10);
            bolide_dict_set(dict, 2, 20);

            let cloned = bolide_dict_clone(dict);
            assert_eq!((*cloned).len(), 2);
            assert_eq!(bolide_dict_get(cloned, 1), 10);

            bolide_dict_release(dict);
            bolide_dict_release(cloned);
        }
    }

    #[test]
    fn test_dict_string_key_by_content() {
        // 两个内容相同但指针不同的字符串键应命中同一项
        let dict = BolideDict::new(ElementType::String, ElementType::Int);
        unsafe {
            let k1 = crate::BolideString::new("hello");
            let k2 = crate::BolideString::new("hello"); // 不同指针，相同内容
            assert_ne!(k1 as i64, k2 as i64);

            bolide_dict_set(dict, k1 as i64, 42);
            assert_eq!(bolide_dict_get(dict, k2 as i64), 42);
            assert_eq!(bolide_dict_contains(dict, k2 as i64), 1);

            // 覆盖写不重复插入
            bolide_dict_set(dict, k2 as i64, 43);
            assert_eq!((*dict).len(), 1);
            assert_eq!(bolide_dict_get(dict, k1 as i64), 43);

            crate::bolide_string_release(k1);
            crate::bolide_string_release(k2);
            bolide_dict_release(dict);
        }
    }

    #[test]
    fn test_dict_retains_string_key() {
        // 字典应对键持有强引用：调用者释放后键仍可安全使用
        let dict = BolideDict::new(ElementType::String, ElementType::Int);
        unsafe {
            let k = crate::BolideString::new("key");
            assert_eq!((*k).ref_count(), 1);

            bolide_dict_set(dict, k as i64, 7);
            assert_eq!((*k).ref_count(), 2); // 字典 retain 了键

            crate::bolide_string_release(k); // 调用者放弃所有权

            let probe = crate::BolideString::new("key");
            assert_eq!(bolide_dict_get(dict, probe as i64), 7);
            crate::bolide_string_release(probe);

            bolide_dict_release(dict);
        }
    }
}
