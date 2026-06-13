//! 闭包对象运行时支持
//!
//! 闭包对象布局（紧接 RcHeader 之后的数据区，共 32 字节）：
//! ```text
//! +-------------------+
//! | fn_ptr:  *mut u8  |  offset 0   闭包入口（lifted 函数地址）
//! +-------------------+
//! | env_ptr: *mut u8  |  offset 8   捕获变量环境块
//! +-------------------+
//! | env_size: i64     |  offset 16  环境块字节数（用于 free）
//! +-------------------+
//! | meta_ptr: *const i64 | offset 24 捕获释放元数据（可为 null）
//! +-------------------+
//! ```
//!
//! `env` 由编译器用 `bolide_alloc` 分配；闭包 RC 归零时释放 env，并按
//! `meta` 描述释放其中的 RC 捕获值，避免泄漏。
//!
//! meta 布局：`[n_caps: i64][tag_0: i64][tag_1: i64]...`，tag 取 TypeTag
//! （0 表示非 RC，跳过）。meta 由编译器写入静态/泄漏内存，运行时只读。

use std::os::raw::c_void;

use crate::rc::TypeTag;

/// 数据区大小：fn_ptr + env_ptr + env_size + meta_ptr
const CLOSURE_DATA_SIZE: usize = 32;

#[inline]
fn data_ptr_i64(closure: *mut c_void) -> *mut i64 {
    closure as *mut i64
}

/// 创建闭包对象。
///
/// - `fn_ptr`：lifted 函数地址，签名 `(env_ptr, ...params) -> ret`
/// - `env_ptr`：捕获环境块（bolide_alloc 分配），无捕获时可为 null
/// - `env_size`：env 字节数（free 时使用）
/// - `meta_ptr`：捕获释放元数据，无 RC 捕获时为 null
#[no_mangle]
pub extern "C" fn bolide_closure_new(
    fn_ptr: *mut u8,
    env_ptr: *mut u8,
    env_size: i64,
    meta_ptr: *const i64,
) -> *mut c_void {
    let data = crate::bolide_rc_alloc(CLOSURE_DATA_SIZE as i64, TypeTag::Closure as u8);
    if data.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let p = data_ptr_i64(data);
        p.write(fn_ptr as i64);
        p.add(1).write(env_ptr as i64);
        p.add(2).write(env_size);
        p.add(3).write(meta_ptr as i64);
    }
    data
}

/// 从闭包对象获取 fn_ptr
#[no_mangle]
pub extern "C" fn bolide_closure_fn_ptr(closure: *mut c_void) -> *mut u8 {
    if closure.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { (*data_ptr_i64(closure)) as *mut u8 }
}

/// 从闭包对象获取 env_ptr
#[no_mangle]
pub extern "C" fn bolide_closure_env_ptr(closure: *mut c_void) -> *mut u8 {
    if closure.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { (*data_ptr_i64(closure).add(1)) as *mut u8 }
}

/// 增加闭包引用计数（透传 RC 接口）
#[no_mangle]
pub extern "C" fn bolide_closure_retain(closure: *mut c_void) {
    crate::bolide_rc_retain(closure);
}

/// 减少闭包引用计数（透传 RC 接口，归零时走 closure 释放路径）
#[no_mangle]
pub extern "C" fn bolide_closure_release(closure: *mut c_void) {
    crate::bolide_rc_release(closure, TypeTag::Closure as u8);
}

/// 释放一个 RC 捕获值（按 tag 分派到具体类型的 release）。
/// tag 取值见编译器 `capture_release_tag`（与 TypeTag 对齐，另加 Tuple=10/WeakObj=11）。
unsafe fn release_capture(value: i64, tag: i64) {
    if value == 0 {
        return;
    }
    let ptr = value as *mut c_void;
    match tag {
        1 => crate::bolide_string_release(ptr as *mut crate::BolideString),
        2 => crate::bolide_bigint_release(ptr as *mut crate::BolideBigInt),
        3 => crate::bolide_decimal_release(ptr as *mut crate::BolideDecimal),
        4 => crate::bolide_list_release(ptr as *mut crate::BolideList),
        5 => crate::object_release(ptr as *mut u8),
        6 => bolide_closure_release(ptr),
        8 => crate::bolide_dict_release(ptr as *mut crate::BolideDict),
        9 => crate::bolide_dynamic_release(ptr as *mut crate::BolideDynamic),
        10 => {
            crate::bolide_tuple_release(ptr as *mut crate::BolideTuple);
        }
        11 => crate::object_weak_release(ptr as *mut u8),
        _ => {}
    }
}

/// 闭包 RC 归零时的释放逻辑：释放 RC 捕获 + 释放 env 内存。
///
/// 由 rc.rs `drop_by_type` 在 strong_count 归零时调用，仅一次。
///
/// # Safety
/// closure 必须是 bolide_closure_new 返回的有效指针。
pub unsafe fn closure_drop_hook(closure: *mut c_void) {
    if closure.is_null() {
        return;
    }
    let p = data_ptr_i64(closure);
    let env_ptr = (*p.add(1)) as *mut u8;
    let env_size = *p.add(2);
    let meta_ptr = (*p.add(3)) as *const i64;

    // 释放 env 内的 RC 捕获
    if !meta_ptr.is_null() && !env_ptr.is_null() {
        let n_caps = *meta_ptr;
        let env_i64 = env_ptr as *const i64;
        for i in 0..n_caps {
            let tag = *meta_ptr.add(1 + i as usize);
            if tag != 0 {
                let value = *env_i64.add(i as usize);
                release_capture(value, tag);
            }
        }
    }

    // 释放 env 内存
    if !env_ptr.is_null() && env_size > 0 {
        crate::bolide_free(env_ptr as *mut c_void, env_size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_closure_new_and_getters() {
        extern "C" fn dummy_fn(_env: *mut u8) -> i64 {
            42
        }
        let env = crate::bolide_alloc(8);
        let closure = bolide_closure_new(dummy_fn as *mut u8, env as *mut u8, 8, std::ptr::null());
        assert!(!closure.is_null());
        assert_eq!(bolide_closure_fn_ptr(closure), dummy_fn as *mut u8);
        assert_eq!(bolide_closure_env_ptr(closure), env as *mut u8);

        // 释放闭包对象（env 也被释放）
        bolide_closure_release(closure);
    }
}
