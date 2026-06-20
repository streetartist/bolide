//! Environment helpers for the Bolide standard library.

use crate::{bolide_list_new, bolide_list_push, bolide_string_release, BolideList, BolideString};
use std::ffi::CStr;
use std::os::raw::c_char;

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

fn string_list(items: impl IntoIterator<Item = String>) -> *mut BolideList {
    let list = bolide_list_new(3);
    for item in items {
        let s = BolideString::new(&item);
        bolide_list_push(list, s as i64);
        bolide_string_release(s);
    }
    list
}

#[no_mangle]
pub extern "C" fn bolide_env_get(key: *const c_char) -> *mut BolideString {
    let Some(key) = cstr_to_str(key) else {
        return BolideString::new("");
    };
    BolideString::new(&std::env::var(key).unwrap_or_default())
}

#[no_mangle]
pub extern "C" fn bolide_env_get_or(
    key: *const c_char,
    default: *const c_char,
) -> *mut BolideString {
    let Some(key) = cstr_to_str(key) else {
        return BolideString::new(cstr_to_str(default).unwrap_or(""));
    };
    let default = cstr_to_str(default).unwrap_or("");
    BolideString::new(&std::env::var(key).unwrap_or_else(|_| default.to_string()))
}

#[no_mangle]
pub extern "C" fn bolide_env_contains(key: *const c_char) -> i64 {
    cstr_to_str(key)
        .map(|key| std::env::var_os(key).is_some() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_env_set(key: *const c_char, value: *const c_char) -> i64 {
    let Some(key) = cstr_to_str(key) else {
        return 0;
    };
    let Some(value) = cstr_to_str(value) else {
        return 0;
    };
    std::env::set_var(key, value);
    1
}

#[no_mangle]
pub extern "C" fn bolide_env_remove(key: *const c_char) -> i64 {
    let Some(key) = cstr_to_str(key) else {
        return 0;
    };
    std::env::remove_var(key);
    1
}

#[no_mangle]
pub extern "C" fn bolide_env_args() -> *mut BolideList {
    string_list(std::env::args())
}

#[no_mangle]
pub extern "C" fn bolide_env_vars() -> *mut BolideList {
    string_list(std::env::vars().map(|(key, value)| format!("{}={}", key, value)))
}

#[no_mangle]
pub extern "C" fn bolide_env_current_exe() -> *mut BolideString {
    BolideString::new(
        &std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    )
}

#[no_mangle]
pub extern "C" fn bolide_env_temp_dir() -> *mut BolideString {
    BolideString::new(&std::env::temp_dir().to_string_lossy())
}

#[no_mangle]
pub extern "C" fn bolide_env_home_dir() -> *mut BolideString {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    BolideString::new(&home)
}

#[no_mangle]
pub extern "C" fn bolide_env_os() -> *mut BolideString {
    BolideString::new(std::env::consts::OS)
}

#[no_mangle]
pub extern "C" fn bolide_env_arch() -> *mut BolideString {
    BolideString::new(std::env::consts::ARCH)
}

#[no_mangle]
pub extern "C" fn bolide_env_family() -> *mut BolideString {
    BolideString::new(std::env::consts::FAMILY)
}

#[no_mangle]
pub extern "C" fn bolide_env_exe_suffix() -> *mut BolideString {
    BolideString::new(std::env::consts::EXE_SUFFIX)
}

#[no_mangle]
pub extern "C" fn bolide_env_exit(code: i64) {
    std::process::exit(code as i32);
}
