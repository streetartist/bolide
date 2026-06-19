//! Bolide 异常处理运行时
//!
//! 不使用 setjmp/longjmp(与 Cranelift SSA 寄存器分配不兼容:longjmp 恢复
//! callee-saved 寄存器会破坏跨边界的 SSA 值)。改为编译器侧的 catch 落点栈 +
//! 直接分支:throw 把异常值与类型标签存入 thread-local,然后直接 jump 到最近的
//! catch 块。异常值经内存(thread-local)传递,不经寄存器,因此 SSA 安全。
//!
//! 类型标签:
//!   - 基本类型有固定标签(见编译器 type_to_throw_tag)
//!   - 自定义类按声明顺序分配 id(>=100)
//!   catch (e: T) 的类型过滤由编译器在编译期展开为标签比较的 OR 链
//!   (含 T 的所有已知子类),因此 runtime 只需存取 value + tag。
//!
//! 跨函数传播由编译器在调用点检查 pending exception 并执行早返回/跳转。

use std::cell::Cell;
use std::os::raw::c_void;

thread_local! {
    /// 当前待处理异常值
    static EXCEPTION_VALUE: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    /// 当前待处理异常的类型标签
    static EXCEPTION_TAG: Cell<i64> = const { Cell::new(0) };
}

/// 设置当前异常值与类型标签(throw 时调用,随后 JIT 直接跳转到 catch 块)
#[no_mangle]
pub extern "C" fn bolide_exception_set(value: *mut c_void, tag: i64) {
    EXCEPTION_VALUE.with(|ex| ex.set(value));
    EXCEPTION_TAG.with(|t| t.set(tag));
}

/// 取出当前异常值(catch 块绑定变量时调用)
#[no_mangle]
pub extern "C" fn bolide_exception_get() -> *mut c_void {
    EXCEPTION_VALUE.with(|ex| {
        let v = ex.get();
        ex.set(std::ptr::null_mut());
        EXCEPTION_TAG.with(|t| t.set(0));
        v
    })
}

/// 读取当前异常的类型标签(catch 类型过滤时调用,不清空)
#[no_mangle]
pub extern "C" fn bolide_exception_tag() -> i64 {
    EXCEPTION_TAG.with(|t| t.get())
}

/// 是否存在待处理异常。调用方用它判断被调函数是否早返回传播了异常。
#[no_mangle]
pub extern "C" fn bolide_exception_pending() -> i64 {
    EXCEPTION_VALUE.with(|ex| (!ex.get().is_null()) as i64)
}

/// 未捕获异常:打印并退出(throw 在无 catch 落点时调用)
#[no_mangle]
pub extern "C" fn bolide_throw_uncaught(value: *mut c_void) -> ! {
    eprintln!("uncaught exception: {:p}", value);
    std::process::abort();
}
