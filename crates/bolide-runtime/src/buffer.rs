//! Growable text buffer for efficient string construction.

use crate::BolideString;
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};

#[repr(C)]
pub struct BolideTextBuffer {
    data: String,
}

fn text_from_ptr<'a>(value: *const c_char) -> &'a str {
    if value.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(value).to_str().unwrap_or("") }
}

#[no_mangle]
pub extern "C" fn bolide_buffer_new() -> *mut c_void {
    Box::into_raw(Box::new(BolideTextBuffer {
        data: String::new(),
    })) as *mut c_void
}

#[no_mangle]
pub extern "C" fn bolide_buffer_with_capacity(capacity: i64) -> *mut c_void {
    let capacity = capacity.max(0) as usize;
    Box::into_raw(Box::new(BolideTextBuffer {
        data: String::with_capacity(capacity),
    })) as *mut c_void
}

#[no_mangle]
pub extern "C" fn bolide_buffer_free(buffer: *mut c_void) {
    if buffer.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(buffer as *mut BolideTextBuffer));
    }
}

#[no_mangle]
pub extern "C" fn bolide_buffer_len(buffer: *const c_void) -> i64 {
    if buffer.is_null() {
        return 0;
    }
    let buffer = unsafe { &*(buffer as *const BolideTextBuffer) };
    buffer.data.len() as i64
}

#[no_mangle]
pub extern "C" fn bolide_buffer_capacity(buffer: *const c_void) -> i64 {
    if buffer.is_null() {
        return 0;
    }
    let buffer = unsafe { &*(buffer as *const BolideTextBuffer) };
    buffer.data.capacity() as i64
}

#[no_mangle]
pub extern "C" fn bolide_buffer_reserve(buffer: *mut c_void, additional: i64) {
    if buffer.is_null() || additional <= 0 {
        return;
    }
    let buffer = unsafe { &mut *(buffer as *mut BolideTextBuffer) };
    buffer.data.reserve(additional as usize);
}

#[no_mangle]
pub extern "C" fn bolide_buffer_clear(buffer: *mut c_void) {
    if buffer.is_null() {
        return;
    }
    let buffer = unsafe { &mut *(buffer as *mut BolideTextBuffer) };
    buffer.data.clear();
}

#[no_mangle]
pub extern "C" fn bolide_buffer_push(buffer: *mut c_void, value: *const c_char) {
    if buffer.is_null() {
        return;
    }
    let text = text_from_ptr(value);
    let buffer = unsafe { &mut *(buffer as *mut BolideTextBuffer) };
    buffer.data.push_str(text);
}

#[no_mangle]
pub extern "C" fn bolide_buffer_push_line(buffer: *mut c_void, value: *const c_char) {
    if buffer.is_null() {
        return;
    }
    let text = text_from_ptr(value);
    let buffer = unsafe { &mut *(buffer as *mut BolideTextBuffer) };
    buffer.data.push_str(text);
    buffer.data.push('\n');
}

#[no_mangle]
pub extern "C" fn bolide_buffer_push_int(buffer: *mut c_void, value: i64) {
    if buffer.is_null() {
        return;
    }
    let buffer = unsafe { &mut *(buffer as *mut BolideTextBuffer) };
    buffer.data.push_str(&value.to_string());
}

#[no_mangle]
pub extern "C" fn bolide_buffer_push_float(buffer: *mut c_void, value: f64) {
    if buffer.is_null() {
        return;
    }
    let buffer = unsafe { &mut *(buffer as *mut BolideTextBuffer) };
    buffer.data.push_str(&value.to_string());
}

#[no_mangle]
pub extern "C" fn bolide_buffer_push_bool(buffer: *mut c_void, value: i64) {
    if buffer.is_null() {
        return;
    }
    let text = if value != 0 { "true" } else { "false" };
    let buffer = unsafe { &mut *(buffer as *mut BolideTextBuffer) };
    buffer.data.push_str(text);
}

#[no_mangle]
pub extern "C" fn bolide_buffer_to_string(buffer: *const c_void) -> *mut BolideString {
    if buffer.is_null() {
        return BolideString::new("");
    }
    let buffer = unsafe { &*(buffer as *const BolideTextBuffer) };
    BolideString::new(&buffer.data)
}
