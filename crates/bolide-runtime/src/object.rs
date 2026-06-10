//! 对象运行时支持
//!
//! 提供类实例的内存管理
//!
//! 内存安全模型：
//! - 强引用计数归零时，对象进入"僵尸"状态（ref_count == 0），
//!   只要还有弱引用存在，分配本身不会被释放，
//!   这样 weak/unowned 引用始终可以安全地检查对象是否存活。
//! - 弱引用计数（含一个隐式 +1，当 strong > 0 时）归零时，整个分配被释放。

use std::alloc::{alloc, dealloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 对象头部结构（每个对象都有）
#[repr(C)]
pub struct ObjectHeader {
    pub ref_count: AtomicUsize,
    /// 弱引用计数（含隐式 +1，当 strong > 0 时）
    pub weak_count: AtomicUsize,
    pub data_size: usize,  // 数据部分大小
}

const HEADER_SIZE: usize = std::mem::size_of::<ObjectHeader>();

#[inline]
unsafe fn header_of(data_ptr: *mut u8) -> *mut ObjectHeader {
    data_ptr.sub(HEADER_SIZE) as *mut ObjectHeader
}

unsafe fn dealloc_object(header_ptr: *mut u8) {
    let header = header_ptr as *mut ObjectHeader;
    let data_size = (*header).data_size;
    let total_size = HEADER_SIZE + data_size;
    let layout = Layout::from_size_align(total_size, 8).unwrap();
    dealloc(header_ptr, layout);
}

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
        (*header).weak_count = AtomicUsize::new(1); // 隐式 +1
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
        let header = header_of(data_ptr);
        (*header).ref_count.fetch_add(1, Ordering::SeqCst);
    }
}

/// 减少引用计数，强引用归零时对象进入僵尸状态；
/// 仅当弱引用也归零时才真正释放内存
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
            // 强引用归零，移除隐式弱引用
            let old_weak = (*header).weak_count.fetch_sub(1, Ordering::SeqCst);
            if old_weak == 1 {
                // 没有弱引用存在，释放整个分配
                dealloc_object(header_ptr);
            }
            // 否则保留分配（僵尸状态），供 weak/unowned 检查存活
        }
    }
}

/// 增加弱引用计数（weak/unowned 引用创建时调用）
#[no_mangle]
pub extern "C" fn object_weak_retain(data_ptr: *mut u8) {
    if data_ptr.is_null() {
        return;
    }
    unsafe {
        let header = header_of(data_ptr);
        (*header).weak_count.fetch_add(1, Ordering::SeqCst);
    }
}

/// 减少弱引用计数，归零时释放分配（数据已在强引用归零时失效）
#[no_mangle]
pub extern "C" fn object_weak_release(data_ptr: *mut u8) {
    if data_ptr.is_null() {
        return;
    }
    unsafe {
        let header_ptr = (data_ptr as *mut u8).sub(HEADER_SIZE);
        let header = header_ptr as *mut ObjectHeader;
        let old_weak = (*header).weak_count.fetch_sub(1, Ordering::SeqCst);
        if old_weak == 1 {
            dealloc_object(header_ptr);
        }
    }
}

/// 检查对象是否存活（weak/unowned 访问前由编译器插入调用）
/// 死对象访问是确定性错误：打印诊断信息后中止进程，而非未定义行为
#[no_mangle]
pub extern "C" fn object_assert_alive(data_ptr: *mut u8) {
    if data_ptr.is_null() {
        eprintln!("runtime error: access through nil weak/unowned reference");
        std::process::abort();
    }
    unsafe {
        let header = header_of(data_ptr);
        if (*header).ref_count.load(Ordering::SeqCst) == 0 {
            eprintln!("runtime error: weak/unowned reference accessed after object was deallocated");
            std::process::abort();
        }
    }
}

/// 检查对象是否存活（返回 1/0，不中止进程）
#[no_mangle]
pub extern "C" fn object_is_alive(data_ptr: *mut u8) -> i64 {
    if data_ptr.is_null() {
        return 0;
    }
    unsafe {
        let header = header_of(data_ptr);
        if (*header).ref_count.load(Ordering::SeqCst) > 0 { 1 } else { 0 }
    }
}

/// 获取对象当前强引用计数（null 返回 0）
#[no_mangle]
pub extern "C" fn object_ref_count(data_ptr: *mut u8) -> i64 {
    if data_ptr.is_null() {
        return 0;
    }
    unsafe {
        let header = header_of(data_ptr);
        (*header).ref_count.load(Ordering::SeqCst) as i64
    }
}

/// 克隆弱引用（增加弱引用计数，返回相同指针）
#[no_mangle]
pub extern "C" fn object_weak_clone(data_ptr: *mut u8) -> *mut u8 {
    if !data_ptr.is_null() {
        object_weak_retain(data_ptr);
    }
    data_ptr
}

/// 克隆对象（增加引用计数）
#[no_mangle]
pub extern "C" fn object_clone(data_ptr: *mut u8) -> *mut u8 {
    if !data_ptr.is_null() {
        object_retain(data_ptr);
    }
    data_ptr
}
