//! Bolide Runtime Library
//!
//! 提供 Bolide 语言的运行时支持
//!
//! ## 模块结构
//! - `rc`: 引用计数内存管理
//! - `string`: 字符串类型
//! - `bigint`: 任意精度整数
//! - `decimal`: 任意精度小数
//! - `dynamic`: 动态类型
//! - `list`: 列表类型
//! - `print`: 统一打印功能
//! - `thread`: 线程和线程池
//! - `channel`: 线程安全通道

mod rc;
mod string;
mod bigint;
mod decimal;
mod dynamic;
mod list;
mod print;
mod thread;
mod channel;
mod object;
mod coroutine;
mod tuple;
mod ffi;

pub use rc::*;
pub use string::*;
pub use bigint::*;
pub use decimal::*;
pub use dynamic::*;
pub use list::*;
pub use print::*;
pub use thread::*;
pub use channel::*;
pub use object::*;
pub use coroutine::*;
pub use tuple::*;
pub use ffi::*;

use std::alloc::{alloc, dealloc, Layout};
use std::os::raw::c_void;

/// 分配内存（用于 spawn 环境块）
#[no_mangle]
pub extern "C" fn bolide_alloc(size: i64) -> *mut c_void {
    if size <= 0 {
        return std::ptr::null_mut();
    }
    let layout = Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { alloc(layout) as *mut c_void }
}

/// 释放内存
#[no_mangle]
pub extern "C" fn bolide_free(ptr: *mut c_void, size: i64) {
    if ptr.is_null() || size <= 0 {
        return;
    }
    let layout = Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { dealloc(ptr as *mut u8, layout) }
}
