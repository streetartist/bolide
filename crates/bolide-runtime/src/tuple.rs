//! Bolide 元组运行时
//!
//! 元组是固定长度的异构容器，支持：
//! - 创建和销毁（类型感知）
//! - 索引访问
//! - 类型感知打印（混合类型正确打印，非仅指针地址）

use std::alloc::{alloc, dealloc, Layout};

use crate::list::{print_element_inline, ElementType};
use crate::rc::{RcHeader, TypeTag};

/// 元组头部结构
/// 布局：[RcHeader][len: usize][padding: 4 bytes][type_tags: ElementType[len]][data: i64[len]]
#[repr(C)]
pub struct BolideTuple {
    header: RcHeader,
    len: usize,
    _pad: u32,
    // 紧随其后: type_tags: [u8; len] (每个元素 1 字节类型标签)
    // 然后是 data: [i64; len] (每个元素 8 字节值)
}

impl BolideTuple {
    unsafe fn type_tags(&self) -> *const u8 {
        (self as *const Self as *const u8).add(std::mem::size_of::<BolideTuple>())
    }

    unsafe fn type_tags_mut(&mut self) -> *mut u8 {
        (self as *mut Self as *mut u8).add(std::mem::size_of::<BolideTuple>())
    }

    unsafe fn data_ptr(&self) -> *const i64 {
        self.type_tags().add(self.len) as *const i64
    }

    unsafe fn data_ptr_mut(&mut self) -> *mut i64 {
        self.type_tags_mut().add(self.len) as *mut i64
    }
}

use std::sync::atomic::{AtomicI64, Ordering};

// Debug: 跟踪 Tuple 分配和释放
static TUPLE_ALLOC_COUNT: AtomicI64 = AtomicI64::new(0);
static TUPLE_FREE_COUNT: AtomicI64 = AtomicI64::new(0);

/// 创建带类型标签的元组（推荐）。type_tags 是每个元素的 ElementType。
#[no_mangle]
pub extern "C" fn bolide_tuple_new_typed(len: usize, type_tags_ptr: *const u8) -> *mut BolideTuple {
    if len == 0 {
        return std::ptr::null_mut();
    }

    let header_size = std::mem::size_of::<BolideTuple>();
    let tags_size = len; // 每个元素 1 字节
    let data_size = len * 8;
    let total_size = header_size + tags_size + data_size;

    unsafe {
        let layout = Layout::from_size_align(total_size, 8).unwrap();
        let ptr = alloc(layout) as *mut BolideTuple;
        if ptr.is_null() {
            return std::ptr::null_mut();
        }

        TUPLE_ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);

        (*ptr).header = RcHeader::new(TypeTag::Object); // Tuple uses Object tag for GC
        (*ptr).len = len;
        (*ptr)._pad = 0;

        // 拷贝类型标签
        if !type_tags_ptr.is_null() {
            let dst = (*ptr).type_tags_mut();
            std::ptr::copy_nonoverlapping(type_tags_ptr, dst, len);
        } else {
            let dst = (*ptr).type_tags_mut();
            std::ptr::write_bytes(dst, 0u8, len);
        }

        // 初始化数据为 0
        let data = (*ptr).data_ptr_mut();
        std::ptr::write_bytes(data, 0u8, len);

        ptr
    }
}

/// 旧接口（兼容）：创建无类型标签的元组（所有元素 Int=0）。
#[no_mangle]
pub extern "C" fn bolide_tuple_new(len: usize) -> *mut BolideTuple {
    bolide_tuple_new_typed(len, std::ptr::null())
}

/// 释放元组（含 RC 类型元素释放）
#[no_mangle]
pub extern "C" fn bolide_tuple_free(ptr: *mut BolideTuple) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        TUPLE_FREE_COUNT.fetch_add(1, Ordering::SeqCst);

        let len = (*ptr).len;
        let tags = (*ptr).type_tags();
        let data = (*ptr).data_ptr();

        // 释放 RC 类型元素
        for i in 0..len {
            let tag = *tags.add(i);
            let val = *data.add(i);
            release_raw_static(
                match tag {
                    3 => ElementType::String,
                    4 => ElementType::BigInt,
                    5 => ElementType::Decimal,
                    6 => ElementType::List,
                    8 => ElementType::Dict,
                    9 => ElementType::Dynamic,
                    _ => {
                        // 基本类型（Int/Float/Bool）不需释放
                        continue;
                    }
                },
                val,
            );
        }

        let header_size = std::mem::size_of::<BolideTuple>();
        let tags_size = len;
        let data_size = len * 8;
        let total_size = header_size + tags_size + data_size;

        let layout = Layout::from_size_align(total_size, 8).unwrap();
        dealloc(ptr as *mut u8, layout);
    }
}

// ==================== Debug Stats ====================

/// 打印 Tuple 内存统计
#[no_mangle]
pub extern "C" fn bolide_tuple_debug_stats() {
    let alloc = TUPLE_ALLOC_COUNT.load(Ordering::SeqCst);
    let free = TUPLE_FREE_COUNT.load(Ordering::SeqCst);
    println!(
        "[Tuple Stats] alloc: {}, free: {}, leak: {}",
        alloc,
        free,
        alloc - free
    );
}

// ==================== 元素访问 ====================

/// 设置元组元素 + 类型标签
#[no_mangle]
pub extern "C" fn bolide_tuple_set_typed(
    ptr: *mut BolideTuple,
    index: usize,
    value: i64,
    type_tag: u8,
) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let len = (*ptr).len;
        if index >= len {
            eprintln!("Tuple index out of bounds: {} >= {}", index, len);
            return;
        }

        // 释放旧值（如果旧类型是 RC 类型）
        let old_tag = *(*ptr).type_tags().add(index);
        let old_val = *(*ptr).data_ptr().add(index);
        release_raw_static(old_tag.into(), old_val);

        // 存储新值与类型
        *(*ptr).type_tags_mut().add(index) = type_tag;
        *(*ptr).data_ptr_mut().add(index) = value;
    }
}

/// 设置元组元素 (向后兼容，无类型标签，默认 Int)
#[no_mangle]
pub extern "C" fn bolide_tuple_set(ptr: *mut BolideTuple, index: usize, value: i64) {
    bolide_tuple_set_typed(ptr, index, value, 0); // 0 = Int
}

/// 获取元组元素 (i64)
#[no_mangle]
pub extern "C" fn bolide_tuple_get(ptr: *const BolideTuple, index: usize) -> i64 {
    if ptr.is_null() {
        return 0;
    }

    unsafe {
        let len = (*ptr).len;
        if index >= len {
            eprintln!("Tuple index out of bounds: {} >= {}", index, len);
            return 0;
        }

        *(*ptr).data_ptr().add(index)
    }
}

/// 获取元组元素类型标签
#[no_mangle]
pub extern "C" fn bolide_tuple_get_type(ptr: *const BolideTuple, index: usize) -> u8 {
    if ptr.is_null() {
        return 0;
    }

    unsafe {
        let len = (*ptr).len;
        if index >= len {
            return 0;
        }

        *(*ptr).type_tags().add(index)
    }
}

/// 获取元组长度
#[no_mangle]
pub extern "C" fn bolide_tuple_len(ptr: *const BolideTuple) -> usize {
    if ptr.is_null() {
        return 0;
    }
    unsafe { (*ptr).len }
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_tuple_retain(ptr: *mut BolideTuple) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr).header.inc_strong();
    }
}

/// 减少引用计数，返回是否已释放
#[no_mangle]
pub extern "C" fn bolide_tuple_release(ptr: *mut BolideTuple) -> bool {
    if ptr.is_null() {
        return true;
    }
    unsafe {
        if (*ptr).header.dec_strong() {
            bolide_tuple_free(ptr);
            true
        } else {
            false
        }
    }
}

/// 带步长的元组切片（返回新元组，RC=1）。Python 语义，按元素索引。
///   flags bit0=has_start, bit1=has_end；step 由编译器恒传具体值（缺省 1）。
/// 空结果返回 null（与 bolide_tuple_new_typed 对 len==0 的约定一致）。
#[no_mangle]
pub extern "C" fn bolide_tuple_slice_step(
    ptr: *const BolideTuple,
    start: i64,
    end: i64,
    step: i64,
    flags: i64,
) -> *mut BolideTuple {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let src = &*ptr;
        let len = src.len as i64;
        let step = if step == 0 { 1 } else { step };
        let has_start = flags & 1 != 0;
        let has_end = flags & 2 != 0;

        let indices = crate::list::slice_indices(len, start, end, step, has_start, has_end);
        if indices.is_empty() {
            return std::ptr::null_mut();
        }

        let src_tags = src.type_tags();
        let src_data = src.data_ptr();

        // 收集结果的 tags
        let new_len = indices.len();
        let tags: Vec<u8> = indices.iter().map(|&i| *src_tags.add(i as usize)).collect();

        let new_tuple = bolide_tuple_new_typed(new_len, tags.as_ptr());
        if new_tuple.is_null() {
            return std::ptr::null_mut();
        }
        let dst_data = (*new_tuple).data_ptr_mut();
        for (di, &si) in indices.iter().enumerate() {
            let tag = *src_tags.add(si as usize);
            let val = *src_data.add(si as usize);
            // 对 RC 类型元素 retain（新元组持有一份引用）
            retain_raw_static(ElementType::from(tag), val);
            *dst_data.add(di) = val;
        }
        new_tuple
    }
}

// ==================== 打印 ====================

/// 类型感知打印元组
#[no_mangle]
pub extern "C" fn bolide_print_tuple(ptr: *const BolideTuple) {
    if ptr.is_null() {
        println!("()");
        return;
    }

    unsafe {
        let len = (*ptr).len;
        let tags = (*ptr).type_tags();
        let data = (*ptr).data_ptr();

        print!("(");
        for i in 0..len {
            if i > 0 {
                print!(", ");
            }
            let tag = *tags.add(i);
            let val = *data.add(i);
            let elem_type: ElementType = unsafe { std::mem::transmute(tag) };
            print_element_inline(elem_type, val);
        }
        println!(")");
    }
}

// ==================== 内部辅助 ====================

fn retain_raw_static(elem_type: ElementType, raw: i64) {
    let ptr = raw as *mut std::ffi::c_void;
    if ptr.is_null() {
        return;
    }
    use ElementType::*;
    match elem_type {
        String => {
            use crate::BolideString;
            crate::bolide_string_retain(ptr as *mut BolideString);
        }
        BigInt => {
            use crate::BolideBigInt;
            crate::bolide_bigint_retain(ptr as *mut BolideBigInt);
        }
        Decimal => {
            use crate::BolideDecimal;
            crate::bolide_decimal_retain(ptr as *mut BolideDecimal);
        }
        List => {
            use crate::BolideList;
            crate::bolide_list_retain(ptr as *mut BolideList);
        }
        Dict => {
            use crate::BolideDict;
            crate::bolide_dict_retain(ptr as *mut BolideDict);
        }
        Dynamic => {
            use crate::BolideDynamic;
            crate::bolide_dynamic_retain(ptr as *mut BolideDynamic);
        }
        _ => {} // Int/Float/Bool/Ptr 不需要 retain
    }
}

fn release_raw_static(elem_type: ElementType, raw: i64) {
    let ptr = raw as *mut std::ffi::c_void;
    if ptr.is_null() {
        return;
    }
    use ElementType::*;
    match elem_type {
        String => {
            use crate::BolideString;
            crate::bolide_string_release(ptr as *mut BolideString);
        }
        BigInt => {
            use crate::BolideBigInt;
            crate::bolide_bigint_release(ptr as *mut BolideBigInt);
        }
        Decimal => {
            use crate::BolideDecimal;
            crate::bolide_decimal_release(ptr as *mut BolideDecimal);
        }
        List => {
            use crate::BolideList;
            crate::bolide_list_release(ptr as *mut BolideList);
        }
        Dict => {
            use crate::BolideDict;
            crate::bolide_dict_release(ptr as *mut BolideDict);
        }
        Dynamic => {
            use crate::BolideDynamic;
            crate::bolide_dynamic_release(ptr as *mut BolideDynamic);
        }
        _ => {} // Int/Float/Bool/Ptr 不需要释放
    }
}

impl From<u8> for ElementType {
    fn from(v: u8) -> Self {
        unsafe { std::mem::transmute(v) }
    }
}
