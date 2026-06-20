//! Bolide bytes type with reference counting.

use crate::rc::{RcHeader, TypeTag};
use crate::BolideString;

#[repr(C)]
pub struct BolideBytes {
    header: RcHeader,
    data: Vec<u8>,
}

impl BolideBytes {
    pub fn new() -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Bytes),
            data: Vec::new(),
        }))
    }

    pub fn with_capacity(capacity: usize) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Bytes),
            data: Vec::with_capacity(capacity),
        }))
    }

    pub fn from_slice(bytes: &[u8]) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Bytes),
            data: bytes.to_vec(),
        }))
    }

    pub fn clone_preserve_capacity(bytes: &Self) -> *mut Self {
        let mut data = Vec::with_capacity(bytes.data.capacity());
        data.extend_from_slice(bytes.as_slice());
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::Bytes),
            data,
        }))
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    #[inline]
    pub fn retain(&self) {
        self.header.inc_strong();
    }

    #[inline]
    pub fn release(&self) -> bool {
        self.header.dec_strong()
    }

    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.header.strong_count()
    }
}

#[no_mangle]
pub extern "C" fn bolide_bytes_new() -> *mut BolideBytes {
    BolideBytes::new()
}

#[no_mangle]
pub extern "C" fn bolide_bytes_with_capacity(capacity: i64) -> *mut BolideBytes {
    BolideBytes::with_capacity(capacity.max(0) as usize)
}

#[no_mangle]
pub extern "C" fn bolide_bytes_from_string(text: *const BolideString) -> *mut BolideBytes {
    if text.is_null() {
        return BolideBytes::new();
    }
    let text = unsafe { &*text };
    BolideBytes::from_slice(text.as_str().as_bytes())
}

#[no_mangle]
pub extern "C" fn bolide_bytes_from_slice(ptr: *const u8, len: usize) -> *mut BolideBytes {
    if ptr.is_null() || len == 0 {
        return BolideBytes::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    BolideBytes::from_slice(bytes)
}

#[no_mangle]
pub extern "C" fn bolide_bytes_retain(bytes: *mut BolideBytes) -> *mut BolideBytes {
    if !bytes.is_null() {
        unsafe { (*bytes).retain() };
    }
    bytes
}

#[no_mangle]
pub extern "C" fn bolide_bytes_release(bytes: *mut BolideBytes) {
    if bytes.is_null() {
        return;
    }
    unsafe {
        if (*bytes).release() {
            drop(Box::from_raw(bytes));
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_bytes_clone(bytes: *const BolideBytes) -> *mut BolideBytes {
    if bytes.is_null() {
        return BolideBytes::new();
    }
    let bytes = unsafe { &*bytes };
    BolideBytes::clone_preserve_capacity(bytes)
}

#[no_mangle]
pub extern "C" fn bolide_bytes_len(bytes: *const BolideBytes) -> i64 {
    if bytes.is_null() {
        return 0;
    }
    unsafe { (*bytes).data.len() as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_bytes_capacity(bytes: *const BolideBytes) -> i64 {
    if bytes.is_null() {
        return 0;
    }
    unsafe { (&(*bytes).data).capacity() as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_bytes_reserve(bytes: *mut BolideBytes, additional: i64) {
    if bytes.is_null() || additional <= 0 {
        return;
    }
    let bytes = unsafe { &mut *bytes };
    bytes.data.reserve(additional as usize);
}

#[no_mangle]
pub extern "C" fn bolide_bytes_clear(bytes: *mut BolideBytes) {
    if bytes.is_null() {
        return;
    }
    let bytes = unsafe { &mut *bytes };
    bytes.data.clear();
}

#[no_mangle]
pub extern "C" fn bolide_bytes_get(bytes: *const BolideBytes, index: i64) -> i64 {
    if bytes.is_null() || index < 0 {
        return 0;
    }
    unsafe { (&(*bytes).data).get(index as usize).copied().unwrap_or(0) as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_bytes_set(bytes: *mut BolideBytes, index: i64, value: i64) -> i64 {
    if bytes.is_null() || index < 0 {
        return 0;
    }
    let bytes = unsafe { &mut *bytes };
    let Some(slot) = bytes.data.get_mut(index as usize) else {
        return 0;
    };
    *slot = value as u8;
    1
}

#[no_mangle]
pub extern "C" fn bolide_bytes_push(bytes: *mut BolideBytes, value: i64) {
    if bytes.is_null() {
        return;
    }
    unsafe { (*bytes).data.push(value as u8) };
}

#[no_mangle]
pub extern "C" fn bolide_bytes_extend(bytes: *mut BolideBytes, other: *const BolideBytes) {
    if bytes.is_null() || other.is_null() {
        return;
    }
    let bytes = unsafe { &mut *bytes };
    let other = unsafe { &*other };
    bytes.data.extend_from_slice(other.as_slice());
}

#[no_mangle]
pub extern "C" fn bolide_bytes_to_string_lossy(bytes: *const BolideBytes) -> *mut BolideString {
    if bytes.is_null() {
        return BolideString::new("");
    }
    let bytes = unsafe { &*bytes };
    BolideString::new(&String::from_utf8_lossy(bytes.as_slice()))
}

#[no_mangle]
pub extern "C" fn bolide_print_bytes(bytes: *const BolideBytes) {
    if bytes.is_null() {
        println!("null");
        return;
    }
    let bytes = unsafe { &*bytes };
    print!("[");
    for (i, byte) in bytes.as_slice().iter().enumerate() {
        if i > 0 {
            print!(", ");
        }
        print!("{}", byte);
    }
    println!("]");
}

#[no_mangle]
pub extern "C" fn bolide_print_bytes_inline(bytes: *const BolideBytes) {
    if bytes.is_null() {
        print!("null");
        return;
    }
    let bytes = unsafe { &*bytes };
    print!("[");
    for (i, byte) in bytes.as_slice().iter().enumerate() {
        if i > 0 {
            print!(", ");
        }
        print!("{}", byte);
    }
    print!("]");
}
