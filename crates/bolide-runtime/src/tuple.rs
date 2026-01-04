//! Bolide 元组运行时
//!
//! 元组是固定长度的异构容器，支持：
//! - 创建和销毁
//! - 索引访问
//! - 打印

use std::alloc::{Layout, alloc, dealloc};

/// 元组头部结构
/// 元素数据紧随其后 (每个元素 8 字节)
#[repr(C)]
pub struct BolideTuple {
    /// 元素数量
    len: usize,
}

impl BolideTuple {
    /// 获取元素指针
    fn data_ptr(&self) -> *const i64 {
        unsafe {
            (self as *const Self as *const u8)
                .add(std::mem::size_of::<BolideTuple>()) as *const i64
        }
    }

    /// 获取可变元素指针
    fn data_ptr_mut(&mut self) -> *mut i64 {
        unsafe {
            (self as *mut Self as *mut u8)
                .add(std::mem::size_of::<BolideTuple>()) as *mut i64
        }
    }
}

// ==================== 创建和销毁 ====================

/// 创建指定长度的元组
#[no_mangle]
pub extern "C" fn bolide_tuple_new(len: usize) -> *mut BolideTuple {
    if len == 0 {
        return std::ptr::null_mut();
    }

    let header_size = std::mem::size_of::<BolideTuple>();
    let data_size = len * 8;
    let total_size = header_size + data_size;

    unsafe {
        let layout = Layout::from_size_align(total_size, 8).unwrap();
        let ptr = alloc(layout) as *mut BolideTuple;
        if ptr.is_null() {
            return std::ptr::null_mut();
        }

        (*ptr).len = len;
        // 初始化为 0
        let data = (*ptr).data_ptr_mut();
        for i in 0..len {
            *data.add(i) = 0;
        }

        ptr
    }
}

/// 释放元组
#[no_mangle]
pub extern "C" fn bolide_tuple_free(ptr: *mut BolideTuple) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let len = (*ptr).len;
        let header_size = std::mem::size_of::<BolideTuple>();
        let data_size = len * 8;
        let total_size = header_size + data_size;

        let layout = Layout::from_size_align(total_size, 8).unwrap();
        dealloc(ptr as *mut u8, layout);
    }
}

// ==================== 元素访问 ====================

/// 设置元组元素 (i64)
#[no_mangle]
pub extern "C" fn bolide_tuple_set(ptr: *mut BolideTuple, index: usize, value: i64) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let len = (*ptr).len;
        if index >= len {
            eprintln!("Tuple index out of bounds: {} >= {}", index, len);
            return;
        }

        let data = (*ptr).data_ptr_mut();
        *data.add(index) = value;
    }
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

        let data = (*ptr).data_ptr();
        *data.add(index)
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

// ==================== 打印 ====================

/// 打印元组 (简单版本，所有元素作为 i64 打印)
#[no_mangle]
pub extern "C" fn bolide_print_tuple(ptr: *const BolideTuple) {
    if ptr.is_null() {
        println!("()");
        return;
    }

    unsafe {
        let len = (*ptr).len;
        let data = (*ptr).data_ptr();

        print!("(");
        for i in 0..len {
            if i > 0 {
                print!(", ");
            }
            print!("{}", *data.add(i));
        }
        println!(")");
    }
}
