//! Bolide 统一打印模块
//!
//! 所有打印相关的函数集中在这里，提供清晰的 API:
//! - `bolide_print_*`: 各类型的打印函数
//! - 内部使用各类型的 to_string 方法

use crate::{BolideBigInt, BolideDecimal, BolideDynamic, BolideString};
use std::io::{self, BufRead, Write};
use std::sync::{Arc, OnceLock, RwLock};

/// Output sink used by embedders such as the Android IDE.
pub type OutputHook = Arc<dyn Fn(&str) + Send + Sync + 'static>;
/// Blocking input provider used by embedders such as the Android IDE.
pub type InputHook = Arc<dyn Fn(&str) -> String + Send + Sync + 'static>;

static OUTPUT_HOOK: OnceLock<RwLock<Option<OutputHook>>> = OnceLock::new();
static INPUT_HOOK: OnceLock<RwLock<Option<InputHook>>> = OnceLock::new();

fn output_hook() -> &'static RwLock<Option<OutputHook>> {
    OUTPUT_HOOK.get_or_init(|| RwLock::new(None))
}

fn input_hook() -> &'static RwLock<Option<InputHook>> {
    INPUT_HOOK.get_or_init(|| RwLock::new(None))
}

/// Installs process-wide I/O hooks. With no hooks installed the runtime keeps
/// its original stdin/stdout behaviour, so existing CLI users are unaffected.
pub fn set_io_hooks(output: Option<OutputHook>, input: Option<InputHook>) {
    *output_hook().write().expect("output hook lock poisoned") = output;
    *input_hook().write().expect("input hook lock poisoned") = input;
}

/// Removes embedder I/O hooks and restores stdin/stdout behaviour.
pub fn clear_io_hooks() {
    set_io_hooks(None, None);
}

/// Writes runtime output through an embedder hook or stdout.
pub fn write_output(text: &str) {
    let hook = output_hook()
        .read()
        .expect("output hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(text);
    } else {
        print!("{}", text);
        io::stdout().flush().ok();
    }
}

fn write_line(text: &str) {
    write_output(text);
    write_output("\n");
}

fn read_input(prompt: &str) -> String {
    let hook = input_hook()
        .read()
        .expect("input hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        if !prompt.is_empty() {
            write_output(prompt);
        }
        return hook(prompt);
    }

    if !prompt.is_empty() {
        print!("{}", prompt);
    }
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().lock().read_line(&mut input).ok();
    input
        .trim_end_matches(&['\r', '\n'][..])
        .to_string()
}

// ==================== 基本类型打印 ====================

/// 打印整数
#[no_mangle]
pub extern "C" fn bolide_print_int(value: i64) {
    write_line(&value.to_string());
}

/// 打印浮点数
#[no_mangle]
pub extern "C" fn bolide_print_float(value: f64) {
    write_line(&value.to_string());
}

/// 打印布尔值
#[no_mangle]
pub extern "C" fn bolide_print_bool(value: i64) {
    write_line(if value != 0 { "true" } else { "false" });
}

// ==================== 复合类型打印 ====================

/// 打印 BigInt
#[no_mangle]
pub extern "C" fn bolide_print_bigint(ptr: *const BolideBigInt) {
    if ptr.is_null() {
        write_line("null");
        return;
    }
    let value = unsafe { &*ptr };
    write_line(&value.to_string());
}

/// 打印 Decimal
#[no_mangle]
pub extern "C" fn bolide_print_decimal(ptr: *const BolideDecimal) {
    if ptr.is_null() {
        write_line("null");
        return;
    }
    let value = unsafe { &*ptr };
    write_line(&value.to_string());
}

/// 打印 String
#[no_mangle]
pub extern "C" fn bolide_print_string(ptr: *const BolideString) {
    if ptr.is_null() {
        write_line("null");
        return;
    }
    let value = unsafe { &*ptr };
    write_line(value.as_str());
}

/// 打印 Dynamic (自动识别类型)
#[no_mangle]
pub extern "C" fn bolide_print_dynamic(ptr: *const BolideDynamic) {
    if ptr.is_null() {
        write_line("null");
        return;
    }
    let value = unsafe { &*ptr };
    write_line(&value.to_string_repr());
}

// ==================== 辅助函数 ====================

/// 打印换行
#[no_mangle]
pub extern "C" fn bolide_println() {
    write_output("\n");
}

/// 打印整数不换行
#[no_mangle]
pub extern "C" fn bolide_print_int_inline(value: i64) {
    write_output(&value.to_string());
}

/// 打印浮点数不换行
#[no_mangle]
pub extern "C" fn bolide_print_float_inline(value: f64) {
    write_output(&value.to_string());
}

#[no_mangle]
pub extern "C" fn bolide_print_bool_inline(value: i64) {
    write_output(if value != 0 { "true" } else { "false" });
}

#[no_mangle]
pub extern "C" fn bolide_print_bigint_inline(ptr: *const BolideBigInt) {
    if ptr.is_null() {
        write_output("null");
        return;
    }
    let value = unsafe { &*ptr };
    write_output(&value.to_string());
}

#[no_mangle]
pub extern "C" fn bolide_print_decimal_inline(ptr: *const BolideDecimal) {
    if ptr.is_null() {
        write_output("null");
        return;
    }
    let value = unsafe { &*ptr };
    write_output(&value.to_string());
}

#[no_mangle]
pub extern "C" fn bolide_print_string_inline(ptr: *const BolideString) {
    if ptr.is_null() {
        write_output("null");
        return;
    }
    let value = unsafe { &*ptr };
    write_output(value.as_str());
}

#[no_mangle]
pub extern "C" fn bolide_print_dynamic_inline(ptr: *const BolideDynamic) {
    if ptr.is_null() {
        write_output("null");
        return;
    }
    let value = unsafe { &*ptr };
    write_output(&value.to_string_repr());
}

#[no_mangle]
pub extern "C" fn bolide_print_tuple_start() {
    write_output("(");
}

#[no_mangle]
pub extern "C" fn bolide_print_tuple_separator() {
    write_output(", ");
}

#[no_mangle]
pub extern "C" fn bolide_print_tuple_end_inline() {
    write_output(")");
}

// ==================== 输入函数 ====================

/// 读取用户输入（无提示）
#[no_mangle]
pub extern "C" fn bolide_input() -> *mut BolideString {
    BolideString::new(&read_input(""))
}

/// 读取用户输入（带提示）
#[no_mangle]
pub extern "C" fn bolide_input_prompt(prompt: *const BolideString) -> *mut BolideString {
    let prompt = if prompt.is_null() {
        ""
    } else {
        unsafe { &*prompt }.as_str()
    };
    BolideString::new(&read_input(prompt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn embedder_hooks_receive_output_and_supply_input() {
        static TEST_LOCK: Mutex<()> = Mutex::new(());
        let _guard = TEST_LOCK.lock().unwrap();
        let output = Arc::new(Mutex::new(String::new()));
        let captured = output.clone();
        set_io_hooks(
            Some(Arc::new(move |text| captured.lock().unwrap().push_str(text))),
            Some(Arc::new(|prompt| format!("answer-for-{prompt}"))),
        );

        bolide_print_string(BolideString::new("hello"));
        let prompt = BolideString::new("name: ");
        let answer = bolide_input_prompt(prompt);

        assert_eq!(&*output.lock().unwrap(), "hello\nname: ");
        assert_eq!(unsafe { &*answer }.as_str(), "answer-for-name: ");
        unsafe {
            crate::bolide_string_release(answer);
            crate::bolide_string_release(prompt);
        }
        clear_io_hooks();
    }
}
