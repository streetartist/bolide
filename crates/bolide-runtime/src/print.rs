//! Bolide 统一打印模块
//!
//! 所有打印相关的函数集中在这里，提供清晰的 API:
//! - `bolide_print_*`: 各类型的打印函数
//! - 内部使用各类型的 to_string 方法

use crate::{BolideBigInt, BolideDecimal, BolideDynamic, BolideString};

// ==================== 基本类型打印 ====================

/// 打印整数
#[no_mangle]
pub extern "C" fn bolide_print_int(value: i64) {
    println!("{}", value);
}

/// 打印浮点数
#[no_mangle]
pub extern "C" fn bolide_print_float(value: f64) {
    println!("{}", value);
}

/// 打印布尔值
#[no_mangle]
pub extern "C" fn bolide_print_bool(value: i64) {
    println!("{}", if value != 0 { "true" } else { "false" });
}

// ==================== 复合类型打印 ====================

/// 打印 BigInt
#[no_mangle]
pub extern "C" fn bolide_print_bigint(ptr: *const BolideBigInt) {
    if ptr.is_null() {
        println!("null");
        return;
    }
    let value = unsafe { &*ptr };
    println!("{}", value.to_string());
}

/// 打印 Decimal
#[no_mangle]
pub extern "C" fn bolide_print_decimal(ptr: *const BolideDecimal) {
    if ptr.is_null() {
        println!("null");
        return;
    }
    let value = unsafe { &*ptr };
    println!("{}", value.to_string());
}

/// 打印 String
#[no_mangle]
pub extern "C" fn bolide_print_string(ptr: *const BolideString) {
    if ptr.is_null() {
        println!("null");
        return;
    }
    let value = unsafe { &*ptr };
    println!("{}", value.as_str());
}

/// 打印 Dynamic (自动识别类型)
#[no_mangle]
pub extern "C" fn bolide_print_dynamic(ptr: *const BolideDynamic) {
    if ptr.is_null() {
        println!("null");
        return;
    }
    let value = unsafe { &*ptr };
    println!("{}", value.to_string_repr());
}

// ==================== 辅助函数 ====================

/// 打印换行
#[no_mangle]
pub extern "C" fn bolide_println() {
    println!();
}

/// 打印整数不换行
#[no_mangle]
pub extern "C" fn bolide_print_int_inline(value: i64) {
    print!("{}", value);
}

/// 打印浮点数不换行
#[no_mangle]
pub extern "C" fn bolide_print_float_inline(value: f64) {
    print!("{}", value);
}
