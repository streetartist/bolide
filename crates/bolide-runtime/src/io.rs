//! Streaming file I/O helpers for the Bolide standard library.

use crate::BolideBytes;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::raw::{c_char, c_void};

#[repr(C)]
pub struct BolideFileWriter {
    file: File,
}

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

#[no_mangle]
pub extern "C" fn bolide_io_open_write(path: *const c_char) -> *mut c_void {
    let Some(path) = cstr_to_str(path) else {
        return std::ptr::null_mut();
    };
    match File::create(path) {
        Ok(file) => Box::into_raw(Box::new(BolideFileWriter { file })) as *mut c_void,
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn bolide_io_open_append(path: *const c_char) -> *mut c_void {
    let Some(path) = cstr_to_str(path) else {
        return std::ptr::null_mut();
    };
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => Box::into_raw(Box::new(BolideFileWriter { file })) as *mut c_void,
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn bolide_io_write(writer: *mut c_void, text: *const c_char) -> i64 {
    if writer.is_null() {
        return 0;
    }
    let Some(text) = cstr_to_str(text) else {
        return 0;
    };
    let writer = unsafe { &mut *(writer as *mut BolideFileWriter) };
    writer.file.write_all(text.as_bytes()).is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_io_write_line(writer: *mut c_void, text: *const c_char) -> i64 {
    if writer.is_null() {
        return 0;
    }
    let Some(text) = cstr_to_str(text) else {
        return 0;
    };
    let writer = unsafe { &mut *(writer as *mut BolideFileWriter) };
    let result = writer
        .file
        .write_all(text.as_bytes())
        .and_then(|_| writer.file.write_all(b"\n"));
    result.is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_io_write_bytes(writer: *mut c_void, bytes: *const BolideBytes) -> i64 {
    if writer.is_null() || bytes.is_null() {
        return 0;
    }
    let writer = unsafe { &mut *(writer as *mut BolideFileWriter) };
    let bytes = unsafe { &*bytes };
    writer.file.write_all(bytes.as_slice()).is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_io_flush(writer: *mut c_void) -> i64 {
    if writer.is_null() {
        return 0;
    }
    let writer = unsafe { &mut *(writer as *mut BolideFileWriter) };
    writer.file.flush().is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_io_close(writer: *mut c_void) {
    if writer.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(writer as *mut BolideFileWriter));
    }
}
