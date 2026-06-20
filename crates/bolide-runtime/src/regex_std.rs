//! Regex helpers for the Bolide standard library.

use crate::{bolide_list_new, bolide_list_push, bolide_string_release, BolideList, BolideString};
use regex::Regex;
use std::ffi::CStr;
use std::os::raw::c_char;

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

fn empty_string() -> *mut BolideString {
    BolideString::new("")
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

fn compile(pattern: *const c_char) -> Option<Regex> {
    Regex::new(cstr_to_str(pattern)?).ok()
}

#[no_mangle]
pub extern "C" fn bolide_regex_is_valid(pattern: *const c_char) -> i64 {
    cstr_to_str(pattern)
        .map(|pattern| Regex::new(pattern).is_ok() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_regex_escape(text: *const c_char) -> *mut BolideString {
    let Some(text) = cstr_to_str(text) else {
        return empty_string();
    };
    BolideString::new(&regex::escape(text))
}

#[no_mangle]
pub extern "C" fn bolide_regex_is_match(pattern: *const c_char, text: *const c_char) -> i64 {
    let Some(re) = compile(pattern) else {
        return 0;
    };
    let Some(text) = cstr_to_str(text) else {
        return 0;
    };
    re.is_match(text) as i64
}

#[no_mangle]
pub extern "C" fn bolide_regex_find(
    pattern: *const c_char,
    text: *const c_char,
) -> *mut BolideString {
    let Some(re) = compile(pattern) else {
        return empty_string();
    };
    let Some(text) = cstr_to_str(text) else {
        return empty_string();
    };
    match re.find(text) {
        Some(m) => BolideString::new(m.as_str()),
        None => empty_string(),
    }
}

#[no_mangle]
pub extern "C" fn bolide_regex_find_all(
    pattern: *const c_char,
    text: *const c_char,
) -> *mut BolideList {
    let Some(re) = compile(pattern) else {
        return string_list(std::iter::empty());
    };
    let Some(text) = cstr_to_str(text) else {
        return string_list(std::iter::empty());
    };
    string_list(re.find_iter(text).map(|m| m.as_str().to_string()))
}

#[no_mangle]
pub extern "C" fn bolide_regex_captures(
    pattern: *const c_char,
    text: *const c_char,
) -> *mut BolideList {
    let Some(re) = compile(pattern) else {
        return string_list(std::iter::empty());
    };
    let Some(text) = cstr_to_str(text) else {
        return string_list(std::iter::empty());
    };
    let Some(caps) = re.captures(text) else {
        return string_list(std::iter::empty());
    };
    let values = (0..caps.len()).map(|i| {
        caps.get(i)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    });
    string_list(values)
}

#[no_mangle]
pub extern "C" fn bolide_regex_replace(
    pattern: *const c_char,
    text: *const c_char,
    replacement: *const c_char,
) -> *mut BolideString {
    let Some(re) = compile(pattern) else {
        return empty_string();
    };
    let Some(text) = cstr_to_str(text) else {
        return empty_string();
    };
    let replacement = cstr_to_str(replacement).unwrap_or("");
    BolideString::new(&re.replace(text, replacement).to_string())
}

#[no_mangle]
pub extern "C" fn bolide_regex_replace_all(
    pattern: *const c_char,
    text: *const c_char,
    replacement: *const c_char,
) -> *mut BolideString {
    let Some(re) = compile(pattern) else {
        return empty_string();
    };
    let Some(text) = cstr_to_str(text) else {
        return empty_string();
    };
    let replacement = cstr_to_str(replacement).unwrap_or("");
    BolideString::new(&re.replace_all(text, replacement).to_string())
}

#[no_mangle]
pub extern "C" fn bolide_regex_split(
    pattern: *const c_char,
    text: *const c_char,
) -> *mut BolideList {
    let Some(re) = compile(pattern) else {
        return string_list(std::iter::empty());
    };
    let Some(text) = cstr_to_str(text) else {
        return string_list(std::iter::empty());
    };
    string_list(re.split(text).map(|part| part.to_string()))
}
