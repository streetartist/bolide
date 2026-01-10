//! Bolide List type with reference counting
//!
//! BolideList 使用引用计数管理内存
//! 元素以 i64 存储（可以是值或指针）

use std::cell::Cell;
use std::os::raw::c_void;

use crate::rc::{TypeTag, flags};
use crate::{BolideString, BolideBigInt, BolideDecimal};

/// RC 对象头
#[repr(C)]
struct RcHeader {
    strong_count: Cell<u32>,
    weak_count: Cell<u32>,
    type_tag: TypeTag,
    flags: Cell<u8>,
    _padding: [u8; 6],
}

/// 元素类型标签
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    Int = 0,
    Float = 1,
    Bool = 2,
    String = 3,
    BigInt = 4,
    Decimal = 5,
    List = 6,    // 嵌套列表
    Ptr = 7,     // 通用指针
    Dict = 8,    // 字典
    Dynamic = 9, // 动态类型
}


/// Bolide 列表类型（带引用计数）
#[repr(C)]
pub struct BolideList {
    header: RcHeader,
    data: *mut i64,      // 元素数组（i64 可存值或指针）
    len: usize,
    capacity: usize,
    elem_type: ElementType,
}

impl BolideList {
    /// 创建新列表（ref_count = 1）
    pub fn new(elem_type: ElementType) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::List,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
            elem_type,
        }))
    }

    /// 创建带初始容量的列表
    pub fn with_capacity(elem_type: ElementType, capacity: usize) -> *mut Self {
        let mut list = Self {
            header: RcHeader {
                strong_count: Cell::new(1),
                weak_count: Cell::new(1),
                type_tag: TypeTag::List,
                flags: Cell::new(0),
                _padding: [0; 6],
            },
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
            elem_type,
        };
        if capacity > 0 {
            list.reserve(capacity);
        }
        Box::into_raw(Box::new(list))
    }

    fn reserve(&mut self, additional: usize) {
        let new_cap = self.len + additional;
        if new_cap <= self.capacity {
            return;
        }

        let new_cap = new_cap.max(self.capacity * 2).max(8);
        let layout = std::alloc::Layout::array::<i64>(new_cap).unwrap();

        let new_data = if self.data.is_null() {
            unsafe { std::alloc::alloc(layout) as *mut i64 }
        } else {
            let old_layout = std::alloc::Layout::array::<i64>(self.capacity).unwrap();
            unsafe {
                std::alloc::realloc(self.data as *mut u8, old_layout, layout.size()) as *mut i64
            }
        };

        self.data = new_data;
        self.capacity = new_cap;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, value: i64) {
        if self.len >= self.capacity {
            self.reserve(1);
        }
        unsafe {
            *self.data.add(self.len) = value;
            self.retain_element(value);
        }
        self.len += 1;
    }

    pub fn pop(&mut self) -> Option<i64> {
        if self.len == 0 {
            None
        } else {
            self.len -= 1;
            unsafe { Some(*self.data.add(self.len)) }
        }
    }

    pub fn get(&self, index: usize) -> Option<i64> {
        if index >= self.len {
            None
        } else {
            unsafe { Some(*self.data.add(index)) }
        }
    }

    pub fn set(&mut self, index: usize, value: i64) -> bool {
        if index >= self.len {
            false
        } else {
            unsafe {
                let p = self.data.add(index);
                let old = *p;
                if old != value {
                    self.release_element(old);
                    *p = value;
                    self.retain_element(value);
                }
            }
            true
        }
    }

    pub fn elem_type(&self) -> ElementType {
        self.elem_type
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

    /// 增加单个元素引用
    unsafe fn retain_element(&self, value: i64) {
        let ptr = value as *mut c_void;
        if ptr.is_null() { return; }
        match self.elem_type {
            ElementType::String => { crate::bolide_string_retain(ptr as *mut BolideString); }
            ElementType::BigInt => { crate::bolide_bigint_retain(ptr as *mut BolideBigInt); }
            ElementType::Decimal => { crate::bolide_decimal_retain(ptr as *mut BolideDecimal); }
            ElementType::List => { bolide_list_retain(ptr as *mut BolideList); }
            ElementType::Dict => { crate::bolide_dict_retain(ptr as *mut crate::dict::BolideDict); }
            ElementType::Dynamic => { crate::bolide_dynamic_retain(ptr as *mut crate::dynamic::BolideDynamic); }
            _ => {}
        }
    }

    /// 释放单个元素引用
    unsafe fn release_element(&self, value: i64) {
        let ptr = value as *mut c_void;
        if ptr.is_null() { return; }
        match self.elem_type {
            ElementType::String => { crate::bolide_string_release(ptr as *mut BolideString); }
            ElementType::BigInt => { crate::bolide_bigint_release(ptr as *mut BolideBigInt); }
            ElementType::Decimal => { crate::bolide_decimal_release(ptr as *mut BolideDecimal); }
            ElementType::List => { bolide_list_release(ptr as *mut BolideList); }
            ElementType::Dict => { crate::bolide_dict_release(ptr as *mut crate::dict::BolideDict); }
            ElementType::Dynamic => { crate::bolide_dynamic_release(ptr as *mut crate::dynamic::BolideDynamic); }
            _ => {}
        }
    }

    /// 释放所有元素的引用（仅当 strong_count 归零时调用）
    unsafe fn release_elements(&self) {
        for i in 0..self.len {
            let val = *self.data.add(i);
            self.release_element(val);
        }
    }

    /// 增加所有元素的引用计数（用于 clone）
    unsafe fn retain_elements(&self) {
        for i in 0..self.len {
            let val = *self.data.add(i);
            self.retain_element(val);
        }
    }
}

// ==================== FFI 导出 ====================

/// 创建新列表
#[no_mangle]
pub extern "C" fn bolide_list_new(elem_type: u8) -> *mut BolideList {
    let elem_type = match elem_type {
        0 => ElementType::Int,
        1 => ElementType::Float,
        2 => ElementType::Bool,
        3 => ElementType::String,
        4 => ElementType::BigInt,
        5 => ElementType::Decimal,
        6 => ElementType::List,
        7 => ElementType::Ptr,
        8 => ElementType::Dict,
        9 => ElementType::Dynamic,
        _ => ElementType::Int,
    };
    BolideList::new(elem_type)
}

/// 创建带初始容量的列表
#[no_mangle]
pub extern "C" fn bolide_list_with_capacity(elem_type: u8, capacity: usize) -> *mut BolideList {
    let elem_type = match elem_type {
        0 => ElementType::Int,
        1 => ElementType::Float,
        2 => ElementType::Bool,
        3 => ElementType::String,
        4 => ElementType::BigInt,
        5 => ElementType::Decimal,
        6 => ElementType::List,
        _ => ElementType::Ptr,
    };
    BolideList::with_capacity(elem_type, capacity)
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_list_retain(list: *mut BolideList) -> *mut BolideList {
    if !list.is_null() {
        unsafe { (*list).retain(); }
    }
    list
}

/// 减少引用计数
#[no_mangle]
pub extern "C" fn bolide_list_release(list: *mut BolideList) {
    if list.is_null() { return; }
    unsafe {
        if (*list).release() {
            // 释放所有元素
            (*list).release_elements();
            // 释放数据数组
            if !(*list).data.is_null() {
                let layout = std::alloc::Layout::array::<i64>((*list).capacity).unwrap();
                std::alloc::dealloc((*list).data as *mut u8, layout);
            }
            // 释放列表本身
            let _ = Box::from_raw(list);
        }
    }
}

/// 兼容旧 API
#[no_mangle]
pub extern "C" fn bolide_list_free(list: *mut BolideList) {
    bolide_list_release(list);
}

/// 深拷贝列表
#[no_mangle]
pub extern "C" fn bolide_list_clone(list: *const BolideList) -> *mut BolideList {
    if list.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let src = &*list;
        let new_list = BolideList::with_capacity(src.elem_type, src.len);
        let dst = &mut *new_list;

        // 复制元素
        for i in 0..src.len {
            let value = *src.data.add(i);
            dst.push(value);
        }

        // 增加所有元素的引用计数
        dst.retain_elements();

        new_list
    }
}

/// 获取引用计数
#[no_mangle]
pub extern "C" fn bolide_list_ref_count(list: *const BolideList) -> u32 {
    if list.is_null() { return 0; }
    unsafe { (*list).ref_count() }
}

/// 获取列表长度
#[no_mangle]
pub extern "C" fn bolide_list_len(list: *const BolideList) -> usize {
    if list.is_null() { return 0; }
    unsafe { (*list).len() }
}

/// 追加元素
#[no_mangle]
pub extern "C" fn bolide_list_push(list: *mut BolideList, value: i64) {
    if list.is_null() { return; }
    unsafe { (*list).push(value); }
}

/// 弹出最后一个元素
#[no_mangle]
pub extern "C" fn bolide_list_pop(list: *mut BolideList) -> i64 {
    if list.is_null() { return 0; }
    unsafe { (*list).pop().unwrap_or(0) }
}

/// 获取指定位置的元素
#[no_mangle]
pub extern "C" fn bolide_list_get(list: *const BolideList, index: usize) -> i64 {
    if list.is_null() { return 0; }
    unsafe { (*list).get(index).unwrap_or(0) }
}

/// 设置指定位置的元素
#[no_mangle]
pub extern "C" fn bolide_list_set(list: *mut BolideList, index: usize, value: i64) -> i64 {
    if list.is_null() { return 0; }
    unsafe { if (*list).set(index, value) { 1 } else { 0 } }
}

/// 获取元素类型
#[no_mangle]
pub extern "C" fn bolide_list_elem_type(list: *const BolideList) -> u8 {
    if list.is_null() { return 7; }
    unsafe { (*list).elem_type() as u8 }
}

/// 检查是否已被 move
#[no_mangle]
pub extern "C" fn bolide_list_is_moved(list: *const BolideList) -> i32 {
    if list.is_null() { return 0; }
    unsafe { if (*list).is_moved() { 1 } else { 0 } }
}

/// 标记为已 move
#[no_mangle]
pub extern "C" fn bolide_list_mark_moved(list: *mut BolideList) {
    if !list.is_null() {
        unsafe { (*list).mark_moved(); }
    }
}

// ==================== Python-like Methods ====================

/// 在指定位置插入元素
#[no_mangle]
pub extern "C" fn bolide_list_insert(list: *mut BolideList, index: usize, value: i64) {
    if list.is_null() { return; }
    unsafe {
        let list = &mut *list;
        let index = index.min(list.len); // 允许在末尾插入
        
        // 确保有空间
        if list.len >= list.capacity {
            list.reserve(1);
        }
        
        // 移动元素腾出空间
        if index < list.len {
            std::ptr::copy(
                list.data.add(index),
                list.data.add(index + 1),
                list.len - index,
            );
        }
        
        // 插入新元素
        *list.data.add(index) = value;
        list.len += 1;
        list.retain_element(value);
    }
}

/// 移除并返回指定位置的元素
#[no_mangle]
pub extern "C" fn bolide_list_remove(list: *mut BolideList, index: usize) -> i64 {
    if list.is_null() { return 0; }
    unsafe {
        let list = &mut *list;
        if index >= list.len {
            return 0;
        }
        
        let value = *list.data.add(index);
        
        // 移动后续元素
        if index < list.len - 1 {
            std::ptr::copy(
                list.data.add(index + 1),
                list.data.add(index),
                list.len - index - 1,
            );
        }
        
        list.len -= 1;
        value
    }
}

/// 清空列表
#[no_mangle]
pub extern "C" fn bolide_list_clear(list: *mut BolideList) {
    if list.is_null() { return; }
    unsafe {
        let list = &mut *list;
        // 释放所有元素的引用
        list.release_elements();
        list.len = 0;
    }
}

/// 原地反转列表
#[no_mangle]
pub extern "C" fn bolide_list_reverse(list: *mut BolideList) {
    if list.is_null() { return; }
    unsafe {
        let list = &mut *list;
        if list.len <= 1 { return; }
        
        let mut left = 0;
        let mut right = list.len - 1;
        while left < right {
            let tmp = *list.data.add(left);
            *list.data.add(left) = *list.data.add(right);
            *list.data.add(right) = tmp;
            left += 1;
            right -= 1;
        }
    }
}

/// 扩展列表（用另一个列表的元素）
#[no_mangle]
pub extern "C" fn bolide_list_extend(list: *mut BolideList, other: *const BolideList) {
    if list.is_null() || other.is_null() { return; }
    unsafe {
        let list = &mut *list;
        let other = &*other;
        
        // 确保有足够空间
        list.reserve(other.len);
        
        // 复制元素
        for i in 0..other.len {
            let value = *other.data.add(i);
            list.push(value);
        }
        
        // 增加元素引用计数（如果是引用类型）
        for i in (list.len - other.len)..list.len {
            let ptr = *list.data.add(i) as *mut std::os::raw::c_void;
            if ptr.is_null() { continue; }
            match list.elem_type {
                ElementType::String => {
                    crate::bolide_string_retain(ptr as *mut crate::BolideString);
                }
                ElementType::BigInt => {
                    crate::bolide_bigint_retain(ptr as *mut crate::BolideBigInt);
                }
                ElementType::Decimal => {
                    crate::bolide_decimal_retain(ptr as *mut crate::BolideDecimal);
                }
                ElementType::List => {
                    bolide_list_retain(ptr as *mut BolideList);
                }
                _ => {}
            }
        }
    }
}

/// 检查列表是否包含指定值
#[no_mangle]
pub extern "C" fn bolide_list_contains(list: *const BolideList, value: i64) -> i64 {
    if list.is_null() { return 0; }
    unsafe {
        let list = &*list;
        for i in 0..list.len {
            if *list.data.add(i) == value {
                return 1;
            }
        }
        0
    }
}

/// 查找值的第一个索引（找不到返回 -1）
#[no_mangle]
pub extern "C" fn bolide_list_index_of(list: *const BolideList, value: i64) -> i64 {
    if list.is_null() { return -1; }
    unsafe {
        let list = &*list;
        for i in 0..list.len {
            if *list.data.add(i) == value {
                return i as i64;
            }
        }
        -1
    }
}

/// 统计值出现的次数
#[no_mangle]
pub extern "C" fn bolide_list_count(list: *const BolideList, value: i64) -> i64 {
    if list.is_null() { return 0; }
    unsafe {
        let list = &*list;
        let mut count = 0i64;
        for i in 0..list.len {
            if *list.data.add(i) == value {
                count += 1;
            }
        }
        count
    }
}

/// 原地排序（仅支持 Int 和 Float 类型）
#[no_mangle]
pub extern "C" fn bolide_list_sort(list: *mut BolideList) {
    if list.is_null() { return; }
    unsafe {
        let list = &mut *list;
        if list.len <= 1 { return; }
        
        match list.elem_type {
            ElementType::Int => {
                // 转换为 slice 并排序
                let slice = std::slice::from_raw_parts_mut(list.data, list.len);
                slice.sort();
            }
            ElementType::Float => {
                let slice = std::slice::from_raw_parts_mut(list.data, list.len);
                slice.sort_by(|a, b| {
                    let fa = f64::from_bits(*a as u64);
                    let fb = f64::from_bits(*b as u64);
                    fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            _ => {
                // 其他类型不支持排序
            }
        }
    }
}

/// 切片（返回新列表）
#[no_mangle]
pub extern "C" fn bolide_list_slice(list: *const BolideList, start: i64, end: i64) -> *mut BolideList {
    if list.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let src = &*list;
        
        // 处理负索引和边界
        let len = src.len as i64;
        let start = if start < 0 { (len + start).max(0) } else { start.min(len) } as usize;
        let end = if end < 0 { (len + end).max(0) } else { end.min(len) } as usize;
        
        if start >= end {
            return BolideList::new(src.elem_type);
        }
        
        let slice_len = end - start;
        let new_list = BolideList::with_capacity(src.elem_type, slice_len);
        let dst = &mut *new_list;
        
        for i in start..end {
            let value = *src.data.add(i);
            dst.push(value);
        }
        
        // 增加元素引用计数
        dst.retain_elements();
        
        new_list
    }
}

/// 检查列表是否为空
#[no_mangle]
pub extern "C" fn bolide_list_is_empty(list: *const BolideList) -> i64 {
    if list.is_null() { return 1; }
    unsafe { if (*list).len == 0 { 1 } else { 0 } }
}

/// 获取第一个元素
#[no_mangle]
pub extern "C" fn bolide_list_first(list: *const BolideList) -> i64 {
    if list.is_null() { return 0; }
    unsafe {
        let list = &*list;
        if list.len == 0 { return 0; }
        *list.data
    }
}

/// 获取最后一个元素
#[no_mangle]
pub extern "C" fn bolide_list_last(list: *const BolideList) -> i64 {
    if list.is_null() { return 0; }
    unsafe {
        let list = &*list;
        if list.len == 0 { return 0; }
        *list.data.add(list.len - 1)
    }
}

/// 打印列表
#[no_mangle]
pub extern "C" fn bolide_print_list(list: *const BolideList) {
    if list.is_null() {
        println!("[]");
        return;
    }
    unsafe {
        let list = &*list;
        print!("[");
        for i in 0..list.len {
            if i > 0 { print!(", "); }
            let val = *list.data.add(i);
            match list.elem_type {
                ElementType::Int => print!("{}", val),
                ElementType::Float => print!("{}", f64::from_bits(val as u64)),
                ElementType::Bool => print!("{}", if val != 0 { "true" } else { "false" }),
                ElementType::String => {
                    let s = val as *const crate::BolideString;
                    if !s.is_null() {
                        print!("\"{}\"", (*s).as_str());
                    } else {
                        print!("null");
                    }
                }
                _ => print!("0x{:x}", val),
            }
        }
        println!("]");
    }
}

// ==================== 测试 ====================

#[cfg(test)]

mod tests {
    use super::*;

    #[test]
    fn test_list_rc() {
        let list = BolideList::new(ElementType::Int);
        unsafe {
            assert_eq!((*list).ref_count(), 1);

            bolide_list_retain(list);
            assert_eq!((*list).ref_count(), 2);

            bolide_list_release(list);
            assert_eq!((*list).ref_count(), 1);

            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_operations() {
        let list = BolideList::new(ElementType::Int);
        unsafe {
            bolide_list_push(list, 10);
            bolide_list_push(list, 20);
            bolide_list_push(list, 30);

            assert_eq!((*list).len(), 3);
            assert_eq!(bolide_list_get(list, 0), 10);
            assert_eq!(bolide_list_get(list, 1), 20);
            assert_eq!(bolide_list_get(list, 2), 30);

            bolide_list_set(list, 1, 25);
            assert_eq!(bolide_list_get(list, 1), 25);

            assert_eq!(bolide_list_pop(list), 30);
            assert_eq!((*list).len(), 2);

            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_with_strings() {
        let list = BolideList::new(ElementType::String);
        unsafe {
            let s1 = crate::BolideString::new("hello");
            let s2 = crate::BolideString::new("world");

            bolide_list_push(list, s1 as i64);
            bolide_list_push(list, s2 as i64);

            assert_eq!((*list).len(), 2);

            // 获取并验证字符串
            let got = bolide_list_get(list, 0) as *const crate::BolideString;
            assert_eq!((*got).as_str(), "hello");

            // 释放列表（会自动释放所有字符串）
            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_clone() {
        let list = BolideList::new(ElementType::Int);
        unsafe {
            bolide_list_push(list, 100);
            bolide_list_push(list, 200);

            let cloned = bolide_list_clone(list);
            assert_eq!((*cloned).len(), 2);
            assert_eq!(bolide_list_get(cloned, 0), 100);
            assert_eq!(bolide_list_get(cloned, 1), 200);
            assert_eq!((*cloned).ref_count(), 1);

            bolide_list_release(list);
            bolide_list_release(cloned);
        }
    }
}
