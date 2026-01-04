//! FFI 运行时支持

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use libloading::Library;

/// 全局库缓存
static LOADED_LIBS: Mutex<Option<HashMap<String, Library>>> = Mutex::new(None);

/// 初始化库缓存
fn init_libs() {
    let mut libs = LOADED_LIBS.lock().unwrap();
    if libs.is_none() {
        *libs = Some(HashMap::new());
    }
}

/// 加载动态库并返回句柄
#[no_mangle]
pub extern "C" fn bolide_ffi_load_library(path_ptr: *const i8) -> i64 {
    init_libs();

    let path = unsafe {
        std::ffi::CStr::from_ptr(path_ptr)
            .to_str()
            .unwrap_or("")
            .to_string()
    };

    let mut libs = LOADED_LIBS.lock().unwrap();
    let libs = libs.as_mut().unwrap();

    // 如果已加载，返回成功
    if libs.contains_key(&path) {
        return 1;
    }

    // 加载库
    match unsafe { Library::new(&path) } {
        Ok(lib) => {
            libs.insert(path, lib);
            1 // 成功
        }
        Err(e) => {
            eprintln!("[FFI] Failed to load library: {}", e);
            0 // 失败
        }
    }
}

/// 获取函数指针
#[no_mangle]
pub extern "C" fn bolide_ffi_get_symbol(
    lib_path_ptr: *const i8,
    symbol_name_ptr: *const i8,
) -> *const c_void {
    init_libs();

    let lib_path = unsafe {
        std::ffi::CStr::from_ptr(lib_path_ptr)
            .to_str()
            .unwrap_or("")
            .to_string()
    };

    let symbol_name = unsafe {
        std::ffi::CStr::from_ptr(symbol_name_ptr)
            .to_str()
            .unwrap_or("")
    };

    let libs = LOADED_LIBS.lock().unwrap();
    let libs = libs.as_ref().unwrap();

    if let Some(lib) = libs.get(&lib_path) {
        unsafe {
            match lib.get::<*const c_void>(symbol_name.as_bytes()) {
                Ok(sym) => *sym,
                Err(e) => {
                    eprintln!("[FFI] Symbol '{}' not found: {}", symbol_name, e);
                    std::ptr::null()
                }
            }
        }
    } else {
        eprintln!("[FFI] Library not loaded: {}", lib_path);
        std::ptr::null()
    }
}

/// 释放所有加载的库
#[no_mangle]
pub extern "C" fn bolide_ffi_cleanup() {
    let mut libs = LOADED_LIBS.lock().unwrap();
    *libs = None;
}

// ============ 回调测试函数 ============

/// 测试回调：调用传入的函数指针
#[no_mangle]
pub extern "C" fn bolide_test_callback(
    callback: extern "C" fn(i64, i64) -> i64,
    a: i64,
    b: i64,
) -> i64 {
    callback(a, b)
}

/// 测试回调：对数组元素应用函数
#[no_mangle]
pub extern "C" fn bolide_map_int(
    callback: extern "C" fn(i64) -> i64,
    value: i64,
) -> i64 {
    callback(value)
}
