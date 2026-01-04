//! 对象运行时支持
//!
//! 提供类实例的内存管理

use std::alloc::{alloc, dealloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 对象头部结构（每个对象都有）
#[repr(C)]
pub struct ObjectHeader {
    pub ref_count: AtomicUsize,
    pub data_size: usize,  // 数据部分大小
}

const HEADER_SIZE: usize = std::mem::size_of::<ObjectHeader>();

/// 分配对象内存
/// size: 对象数据大小（不含头部）
/// 返回: 指向对象数据的指针（头部在前面）
#[no_mangle]
pub extern "C" fn object_alloc(size: usize) -> *mut u8 {
    let total_size = HEADER_SIZE + size;
    let layout = Layout::from_size_align(total_size, 8).unwrap();

    unsafe {
        let ptr = alloc(layout);
        if ptr.is_null() {
            panic!("Object allocation failed");
        }

        // 初始化头部
        let header = ptr as *mut ObjectHeader;
        (*header).ref_count = AtomicUsize::new(1);
        (*header).data_size = size;

        // 返回数据部分的指针
        ptr.add(HEADER_SIZE)
    }
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn object_retain(data_ptr: *mut u8) {
    if data_ptr.is_null() {
        return;
    }
    unsafe {
        let header = (data_ptr as *mut u8).sub(HEADER_SIZE) as *mut ObjectHeader;
        (*header).ref_count.fetch_add(1, Ordering::SeqCst);
    }
}

/// 减少引用计数，如果为0则释放
#[no_mangle]
pub extern "C" fn object_release(data_ptr: *mut u8) {
    if data_ptr.is_null() {
        return;
    }
    unsafe {
        let header_ptr = (data_ptr as *mut u8).sub(HEADER_SIZE);
        let header = header_ptr as *mut ObjectHeader;

        let old_count = (*header).ref_count.fetch_sub(1, Ordering::SeqCst);
        if old_count == 1 {
            // 引用计数为0，释放内存
            let data_size = (*header).data_size;
            let total_size = HEADER_SIZE + data_size;
            let layout = Layout::from_size_align(total_size, 8).unwrap();
            dealloc(header_ptr, layout);
        }
    }
}

/// 克隆对象（增加引用计数）
#[no_mangle]
pub extern "C" fn object_clone(data_ptr: *mut u8) -> *mut u8 {
    if !data_ptr.is_null() {
        object_retain(data_ptr);
    }
    data_ptr
}
