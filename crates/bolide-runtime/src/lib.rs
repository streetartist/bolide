//! Bolide Runtime Library
//!
//! 提供 Bolide 语言的运行时支持
//!
//! ## 模块结构
//! - `rc`: 引用计数内存管理
//! - `string`: 字符串类型
//! - `bytes`: 二进制缓冲区
//! - `bigint`: 任意精度整数
//! - `decimal`: 任意精度小数
//! - `dynamic`: 动态类型
//! - `list`: 列表类型
//! - `dict`: 字典类型
//! - `print`: 统一打印功能
//! - `fs`: 文件读写
//! - `web`: HTTP 服务
//! - `template`: HTML 模板渲染
//! - `db`: 嵌入式文件数据库
//! - `sqlite`: SQLite 数据库绑定
//! - `gui`: 跨平台 egui 桌面界面
//! - `thread`: 线程和线程池
//! - `channel`: 线程安全通道

mod atomic;
mod bigint;
mod buffer;
mod bytes;
mod channel;
mod closure;
mod coroutine;
mod db;
mod decimal;
pub mod dict;
mod dynamic;
mod env;
mod exception;
mod ffi;
mod fs;
mod gui;
mod hash;
mod io;
mod json;
pub mod list;
mod math;
mod object;
mod print;
mod process;
mod random;
mod rc;
mod regex_std;
mod sqlite;
mod string;
mod sync;
mod template;
mod thread;
mod time;
mod tuple;
mod web;

pub use atomic::*;
pub use bigint::*;
pub use buffer::*;
pub use bytes::*;
pub use channel::*;
pub use closure::*;
pub use coroutine::*;
pub use db::*;
pub use decimal::*;
pub use dict::*;
pub use dynamic::*;
pub use env::*;
pub use exception::*;
pub use ffi::*;
pub use fs::*;
pub use gui::*;
pub use hash::*;
pub use io::*;
pub use json::*;
pub use list::*;
pub use math::*;
pub use object::*;
pub use print::*;
pub use process::*;
pub use random::*;
pub use rc::*;
pub use regex_std::*;
pub use sqlite::*;
pub use string::*;
pub use sync::*;
pub use template::*;
pub use thread::*;
pub use time::*;
pub use tuple::*;
pub use web::*;

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
