//! File-system helpers for the Bolide standard library.

use crate::{
    bolide_list_new, bolide_list_push, bolide_string_as_cstr, bolide_string_release, BolideBytes,
    BolideList, BolideString,
};
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

fn path_string_or_empty(value: std::io::Result<PathBuf>) -> *mut BolideString {
    match value {
        Ok(path) => BolideString::new(&path.to_string_lossy()),
        Err(_) => BolideString::new(""),
    }
}

fn path_component_or_empty<F>(path: *const c_char, f: F) -> *mut BolideString
where
    F: FnOnce(&Path) -> Option<String>,
{
    let Some(path) = cstr_to_str(path) else {
        return BolideString::new("");
    };
    let value = f(Path::new(path)).unwrap_or_default();
    BolideString::new(&value)
}

fn empty_string_list() -> *mut BolideList {
    bolide_list_new(3)
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

fn metadata_i64<F>(path: *const c_char, f: F) -> i64
where
    F: FnOnce(fs::Metadata) -> Option<i64>,
{
    let Some(path) = cstr_to_str(path) else {
        return -1;
    };
    fs::metadata(path).ok().and_then(f).unwrap_or(-1)
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
pub extern "C" fn bolide_fs_read_bytes(path: *const c_char) -> *mut BolideBytes {
    let Some(path) = cstr_to_str(path) else {
        return BolideBytes::new();
    };
    match fs::read(path) {
        Ok(bytes) => BolideBytes::from_slice(&bytes),
        Err(_) => BolideBytes::new(),
    }
}

#[no_mangle]
pub extern "C" fn bolide_fs_read_lines(path: *const c_char) -> *mut BolideList {
    let Some(path) = cstr_to_str(path) else {
        return empty_string_list();
    };
    match fs::read_to_string(path) {
        Ok(content) => string_list(content.lines().map(|line| line.to_string())),
        Err(_) => empty_string_list(),
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
pub extern "C" fn bolide_fs_write_bytes(path: *const c_char, data: *const BolideBytes) -> i64 {
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    if data.is_null() {
        return 0;
    }
    let data = unsafe { &*data };
    match File::create(path).and_then(|mut file| {
        file.write_all(data.as_slice())?;
        file.flush()
    }) {
        Ok(()) => 1,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_fs_append_bytes(path: *const c_char, data: *const BolideBytes) -> i64 {
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    if data.is_null() {
        return 0;
    }
    let data = unsafe { &*data };
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| {
            file.write_all(data.as_slice())?;
            file.flush()
        }) {
        Ok(()) => 1,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_fs_touch(path: *const c_char) -> i64 {
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_fs_exists(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| Path::new(path).exists() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_is_file(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| Path::new(path).is_file() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_is_dir(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| Path::new(path).is_dir() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_is_symlink(path: *const c_char) -> i64 {
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink() as i64)
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
pub extern "C" fn bolide_fs_copy(from: *const c_char, to: *const c_char) -> i64 {
    let Some(from) = cstr_to_str(from) else {
        return 0;
    };
    let Some(to) = cstr_to_str(to) else {
        return 0;
    };
    fs::copy(from, to).map(|n| n as i64).unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn bolide_fs_rename(from: *const c_char, to: *const c_char) -> i64 {
    let Some(from) = cstr_to_str(from) else {
        return 0;
    };
    let Some(to) = cstr_to_str(to) else {
        return 0;
    };
    fs::rename(from, to).is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_fs_create_dir(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| fs::create_dir(path).is_ok() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_create_dir_all(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| fs::create_dir_all(path).is_ok() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_remove_dir(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| fs::remove_dir(path).is_ok() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_remove_dir_all(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| fs::remove_dir_all(path).is_ok() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_read_dir(path: *const c_char) -> *mut BolideList {
    let Some(path) = cstr_to_str(path) else {
        return empty_string_list();
    };
    let Ok(entries) = fs::read_dir(path) else {
        return empty_string_list();
    };
    string_list(
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path().to_string_lossy().to_string()),
    )
}

#[no_mangle]
pub extern "C" fn bolide_fs_file_name(path: *const c_char) -> *mut BolideString {
    path_component_or_empty(path, |path| {
        path.file_name().map(|s| s.to_string_lossy().to_string())
    })
}

#[no_mangle]
pub extern "C" fn bolide_fs_parent(path: *const c_char) -> *mut BolideString {
    path_component_or_empty(path, |path| {
        path.parent().map(|s| s.to_string_lossy().to_string())
    })
}

#[no_mangle]
pub extern "C" fn bolide_fs_extension(path: *const c_char) -> *mut BolideString {
    path_component_or_empty(path, |path| {
        path.extension().map(|s| s.to_string_lossy().to_string())
    })
}

#[no_mangle]
pub extern "C" fn bolide_fs_stem(path: *const c_char) -> *mut BolideString {
    path_component_or_empty(path, |path| {
        path.file_stem().map(|s| s.to_string_lossy().to_string())
    })
}

#[no_mangle]
pub extern "C" fn bolide_fs_join(base: *const c_char, child: *const c_char) -> *mut BolideString {
    let Some(base) = cstr_to_str(base) else {
        return BolideString::new("");
    };
    let Some(child) = cstr_to_str(child) else {
        return BolideString::new("");
    };
    BolideString::new(&Path::new(base).join(child).to_string_lossy())
}

#[no_mangle]
pub extern "C" fn bolide_fs_canonicalize(path: *const c_char) -> *mut BolideString {
    let Some(path) = cstr_to_str(path) else {
        return BolideString::new("");
    };
    path_string_or_empty(fs::canonicalize(path))
}

#[no_mangle]
pub extern "C" fn bolide_fs_current_dir() -> *mut BolideString {
    path_string_or_empty(std::env::current_dir())
}

#[no_mangle]
pub extern "C" fn bolide_fs_set_current_dir(path: *const c_char) -> i64 {
    cstr_to_str(path)
        .map(|path| std::env::set_current_dir(path).is_ok() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_fs_len(path: *const c_char) -> i64 {
    metadata_i64(path, |m| Some(m.len() as i64))
}

#[no_mangle]
pub extern "C" fn bolide_fs_modified(path: *const c_char) -> i64 {
    metadata_i64(path, |m| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    })
}

#[no_mangle]
pub extern "C" fn bolide_fs_created(path: *const c_char) -> i64 {
    metadata_i64(path, |m| {
        m.created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    })
}

#[no_mangle]
pub extern "C" fn bolide_fs_readonly(path: *const c_char) -> i64 {
    metadata_i64(path, |m| Some(m.permissions().readonly() as i64))
}

#[no_mangle]
pub extern "C" fn bolide_fs_set_readonly(path: *const c_char, readonly: i64) -> i64 {
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    let Ok(metadata) = fs::metadata(path) else {
        return 0;
    };
    let mut permissions = metadata.permissions();
    permissions.set_readonly(readonly != 0);
    fs::set_permissions(path, permissions).is_ok() as i64
}

#[no_mangle]
pub extern "C" fn bolide_fs_read_bolide_text(path: *const BolideString) -> *mut BolideString {
    bolide_fs_read_text(bolide_string_as_cstr(path))
}
