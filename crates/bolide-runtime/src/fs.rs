//! File-system helpers for the Bolide standard library.

use crate::{bolide_string_as_cstr, BolideString};
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::raw::c_char;
use std::path::Path;

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

#[no_mangle]
pub extern "C" fn bolide_fs_read_text(path: *const c_char) -> *mut BolideString {
    let Some(path) = cstr_to_str(path) else {
        return BolideString::new("");
    };
    match fs::read_to_string(path) {
        Ok(content) => BolideString::new(&content),
        Err(_) => BolideString::new(""),
    }
}

#[no_mangle]
pub extern "C" fn bolide_fs_write_text(path: *const c_char, content: *const c_char) -> i64 {
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    let Some(content) = cstr_to_str(content) else {
        return 0;
    };
    match File::create(path).and_then(|mut file| {
        file.write_all(content.as_bytes())?;
        file.flush()
    }) {
        Ok(()) => 1,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_fs_append_text(path: *const c_char, content: *const c_char) -> i64 {
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    let Some(content) = cstr_to_str(content) else {
        return 0;
    };

    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| {
            file.write_all(content.as_bytes())?;
            file.flush()
        }) {
        Ok(()) => 1,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_fs_exists(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| Path::new(path).exists() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_remove_file(path: *const c_char) -> i64 {
    let Some(path_str) = cstr_to_str(path) else {
        return 0;
    };
    if fs::remove_file(path_str).is_ok() {
        return 1;
    }
    0
}

#[no_mangle]
pub extern "C" fn bolide_fs_create_dir_all(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| fs::create_dir_all(path).is_ok() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_read_bolide_text(path: *const BolideString) -> *mut BolideString {
    bolide_fs_read_text(bolide_string_as_cstr(path))
}
