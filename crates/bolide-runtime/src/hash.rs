//! Fast hashing helpers for the Bolide standard library.

use crate::{BolideBytes, BolideString};

fn str_bytes(text: *const BolideString) -> &'static [u8] {
    if text.is_null() {
        return &[];
    }
    unsafe { (*text).as_str().as_bytes() }
}

fn bytes_slice(bytes: *const BolideBytes) -> &'static [u8] {
    if bytes.is_null() {
        return &[];
    }
    unsafe { (*bytes).as_slice() }
}

fn fnv1a32(data: &[u8]) -> i64 {
    let mut hash: u32 = 0x811c9dc5;
    for &byte in data {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash as i64
}

fn crc32(data: &[u8]) -> i64 {
    let mut crc: u32 = 0xffff_ffff;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    (!crc) as i64
}

#[no_mangle]
pub extern "C" fn bolide_hash_fnv1a(text: *const BolideString) -> i64 {
    fnv1a32(str_bytes(text))
}

#[no_mangle]
pub extern "C" fn bolide_hash_fnv1a_bytes(bytes: *const BolideBytes) -> i64 {
    fnv1a32(bytes_slice(bytes))
}

#[no_mangle]
pub extern "C" fn bolide_hash_crc32(text: *const BolideString) -> i64 {
    crc32(str_bytes(text))
}

#[no_mangle]
pub extern "C" fn bolide_hash_crc32_bytes(bytes: *const BolideBytes) -> i64 {
    crc32(bytes_slice(bytes))
}
