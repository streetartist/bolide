//! Process helpers for the Bolide standard library.

use crate::{BolideList, BolideString};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::process::Command;

#[repr(C)]
pub struct BolideProcessResult {
    status: i64,
    stdout: String,
    stderr: String,
}

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

fn list_to_strings(args: *const BolideList) -> Vec<String> {
    if args.is_null() {
        return Vec::new();
    }
    let list = unsafe { &*args };
    let mut out = Vec::with_capacity(list.len());
    for i in 0..list.len() {
        let raw = list.get(i).unwrap_or(0);
        if raw != 0 {
            let s = unsafe { &*(raw as *const BolideString) };
            out.push(s.as_str().to_string());
        }
    }
    out
}

fn result(status: i64, stdout: String, stderr: String) -> *mut BolideProcessResult {
    Box::into_raw(Box::new(BolideProcessResult {
        status,
        stdout,
        stderr,
    }))
}

#[no_mangle]
pub extern "C" fn bolide_process_run(
    program: *const c_char,
    args: *const BolideList,
) -> *mut BolideProcessResult {
    let Some(program) = cstr_to_str(program) else {
        return result(-1, String::new(), "missing program".to_string());
    };
    match Command::new(program).args(list_to_strings(args)).output() {
        Ok(output) => result(
            output.status.code().map(i64::from).unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ),
        Err(err) => result(-1, String::new(), err.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn bolide_process_run_shell(command: *const c_char) -> *mut BolideProcessResult {
    let Some(command) = cstr_to_str(command) else {
        return result(-1, String::new(), "missing command".to_string());
    };
    #[cfg(windows)]
    let output = Command::new("cmd").args(["/C", command]).output();
    #[cfg(not(windows))]
    let output = Command::new("sh").args(["-c", command]).output();

    match output {
        Ok(output) => result(
            output.status.code().map(i64::from).unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ),
        Err(err) => result(-1, String::new(), err.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn bolide_process_status(res: *const BolideProcessResult) -> i64 {
    if res.is_null() {
        return -1;
    }
    unsafe { (*res).status }
}

#[no_mangle]
pub extern "C" fn bolide_process_stdout(res: *const BolideProcessResult) -> *mut BolideString {
    if res.is_null() {
        return BolideString::new("");
    }
    BolideString::new(&unsafe { &*res }.stdout)
}

#[no_mangle]
pub extern "C" fn bolide_process_stderr(res: *const BolideProcessResult) -> *mut BolideString {
    if res.is_null() {
        return BolideString::new("");
    }
    BolideString::new(&unsafe { &*res }.stderr)
}

#[no_mangle]
pub extern "C" fn bolide_process_success(res: *const BolideProcessResult) -> i64 {
    (bolide_process_status(res) == 0) as i64
}

#[no_mangle]
pub extern "C" fn bolide_process_free(res: *mut BolideProcessResult) {
    if !res.is_null() {
        unsafe {
            drop(Box::from_raw(res));
        }
    }
}
