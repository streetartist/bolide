//! Bolide Dict type with reference counting
//!
//! BolideDict 使用引用计数管理内存
//! 键值以 i64 存储（可以是值或指针）

use std::cell::Cell;
use std::collections::HashMap;
use std::os::raw::c_void;

use crate::rc::{TypeTag, flags};
use crate::{BolideString, BolideBigInt, BolideDecimal, BolideList};
use crate::list::ElementType;

/// RC 对象头
#[repr(C)]
struct RcHeader {
    strong_count: Cell<u32>,
    weak_count: Cell<u32>,
    type_tag: TypeTag,
    flags: Cell<u8>,
    _padding: [u8; 6],
}

/// Bolide 字典类型（带引用计数）
#[repr(C)]
pub struct BolideDict {
    header: RcHeader,
    data: *mut HashMap<i64, i64>,  // 使用 Box 管理的 HashMap
    len: usize,
    key_type: ElementType,
    value_type: ElementType,
}

impl BolideDict {
    /// 创建新字典（ref_count = 1）
    pub fn new(key_type: ElementType, value_type: ElementType) -> *mut Self {
        let map = Box::into_raw(Box::new(HashMap::new()));
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::Dict,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            data: map,
            len: 0,
            key_type,
            value_type,
        }))
    }

    /// 获取引用计数
    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.header.strong_count.get()
    }

    /// 增加引用计数
    pub fn retain(&self) {
        let count = self.header.strong_count.get();
        self.header.strong_count.set(count + 1);
    }

    /// 减少引用计数，返回是否应该释放
    pub fn release(&self) -> bool {
        let count = self.header.strong_count.get();
        debug_assert!(count > 0, "release on already freed dict");
        self.header.strong_count.set(count - 1);
        count == 1
    }

    /// 设置键值对
    pub fn set(&mut self, key: i64, value: i64) {
        unsafe {
            let map = &mut *self.data;
            // 如果是覆盖，需要释放旧值
            if let Some(old_value) = map.insert(key, value) {
                self.release_value(old_value);
            } else {
                self.len += 1;
            }
            // 增加新值的引用计数
            self.retain_value(value);
        }
    }

    /// 获取值（不存在返回 0）
    pub fn get(&self, key: i64) -> Option<i64> {
        unsafe {
            let map = &*self.data;
            map.get(&key).copied()
        }
    }

    /// 检查键是否存在
    pub fn contains(&self, key: i64) -> bool {
        unsafe {
            let map = &*self.data;
            map.contains_key(&key)
        }
    }

    /// 移除键值对，返回值
    pub fn remove(&mut self, key: i64) -> Option<i64> {
        unsafe {
            let map = &mut *self.data;
            if let Some(value) = map.remove(&key) {
                self.len -= 1;
                // 注意：不释放值，因为我们返回它
                Some(value)
            } else {
                None
            }
        }
    }

    /// 获取长度
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 清空字典
    pub fn clear(&mut self) {
        unsafe {
            let map = &mut *self.data;
            // 释放所有值的引用
            for (_, value) in map.drain() {
                self.release_value(value);
            }
            self.len = 0;
        }
    }

    /// 获取所有键
    pub fn keys(&self) -> Vec<i64> {
        unsafe {
            let map = &*self.data;
            map.keys().copied().collect()
        }
    }

    /// 获取所有值
    pub fn values(&self) -> Vec<i64> {
        unsafe {
            let map = &*self.data;
            map.values().copied().collect()
        }
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
        self.header.flags.get() & flags::MOVED != 0
    }

    /// 标记为已 move
    pub fn mark_moved(&self) {
        self.header.flags.set(self.header.flags.get() | flags::MOVED);
    }

    /// 增加值的引用计数
    fn retain_value(&self, value: i64) {
        let ptr = value as *mut c_void;
        if ptr.is_null() { return; }
        match self.value_type {
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
            ElementType::Dynamic => unsafe {
                crate::bolide_dynamic_retain(ptr as *mut crate::dynamic::BolideDynamic);
            },
            _ => {}
        }
    }

    /// 释放值的引用计数
    fn release_value(&self, value: i64) {
        let ptr = value as *mut c_void;
        if ptr.is_null() { return; }
        match self.value_type {
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
            ElementType::Dynamic => unsafe {
                crate::bolide_dynamic_release(ptr as *mut crate::dynamic::BolideDynamic);
            },
            _ => {}
        }
    }
}

impl Drop for BolideDict {
    fn drop(&mut self) {
        unsafe {
            // 释放所有值
            if !self.data.is_null() {
                let map = &*self.data;
                for (_, &value) in map.iter() {
                    self.release_value(value);
                }
                // 释放 HashMap
                let _ = Box::from_raw(self.data);
            }
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
        
        let src_map = &*src.data;
        for (&key, &value) in src_map.iter() {
            dst.set(key, value);
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
        let list = crate::list::BolideList::new(d.key_type);
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
        let list = crate::list::BolideList::new(d.value_type);
        for value in values {
            crate::bolide_list_push(list, value);
            // 增加值的引用计数（因为 values() 不增加）
            d.retain_value(value);
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
        let map = &*d.data;
        print!("{{");
        let mut first = true;
        for (&key, &value) in map.iter() {
            if !first { print!(", "); }
            first = false;
            
            // 打印键
            match d.key_type {
                ElementType::Int => print!("{}", key),
                ElementType::String => {
                    let s = key as *const BolideString;
                    if !s.is_null() {
                        print!("\"{}\"", (*s).as_str());
                    } else {
                        print!("null");
                    }
                }
                _ => print!("{}", key),
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
}
