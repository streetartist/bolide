//! Bolide 协程运行时
//!
//! 提供 Hot Future 风格的协程支持

use std::sync::{Arc, Mutex, Condvar};
use std::thread;
use std::os::raw::c_void;

/// 协程状态
#[derive(Clone, Copy, PartialEq)]
enum CoroutineState {
    Running,
    Completed,
    Cancelled,
}

/// 协程结果联合体
#[repr(C)]
#[derive(Clone, Copy)]
pub union CoroutineResult {
    pub int_val: i64,
    pub float_val: f64,
    pub ptr_val: *mut c_void,
}

unsafe impl Send for CoroutineResult {}
unsafe impl Sync for CoroutineResult {}

/// 完成回调类型
type CompletionCallback = Box<dyn Fn() + Send + Sync>;

/// 协程 Future
pub struct BolideFuture {
    state: Arc<Mutex<CoroutineState>>,
    result: Arc<Mutex<Option<CoroutineResult>>>,
    condvar: Arc<Condvar>,
    on_complete: Arc<Mutex<Option<CompletionCallback>>>,
}

unsafe impl Send for BolideFuture {}
unsafe impl Sync for BolideFuture {}

impl BolideFuture {
    /// 创建新的 Future
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(CoroutineState::Running)),
            result: Arc::new(Mutex::new(None)),
            condvar: Arc::new(Condvar::new()),
            on_complete: Arc::new(Mutex::new(None)),
        }
    }

    /// 设置结果并标记完成
    pub fn complete(&self, result: CoroutineResult) {
        let callback;
        {
            // 锁顺序：on_complete → state（与 on_complete 方法一致）
            let mut on_complete_guard = self.on_complete.lock().unwrap();
            let mut state = self.state.lock().unwrap();
            if *state == CoroutineState::Running {
                *self.result.lock().unwrap() = Some(result);
                *state = CoroutineState::Completed;
                self.condvar.notify_all();
                callback = on_complete_guard.take();
            } else {
                callback = None;
            }
        }
        // 在锁外调用回调，避免死锁
        if let Some(cb) = callback {
            cb();
        }
    }

    /// 注册完成回调（如果已完成则立即调用）
    pub fn on_complete(&self, callback: CompletionCallback) -> bool {
        // 先设置回调，再检查状态，避免竞态
        let mut on_complete_guard = self.on_complete.lock().unwrap();
        let state = self.state.lock().unwrap();

        if *state == CoroutineState::Completed {
            drop(state);
            drop(on_complete_guard);
            callback();
            true
        } else if *state == CoroutineState::Running {
            *on_complete_guard = Some(callback);
            false
        } else {
            false
        }
    }

    /// 等待结果
    pub fn await_result(&self) -> Option<CoroutineResult> {
        let mut state = self.state.lock().unwrap();
        while *state == CoroutineState::Running {
            state = self.condvar.wait(state).unwrap();
        }
        self.result.lock().unwrap().clone()
    }

    /// 取消协程
    pub fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        if *state == CoroutineState::Running {
            *state = CoroutineState::Cancelled;
            self.condvar.notify_all();
        }
    }

    /// 检查是否完成
    pub fn is_completed(&self) -> bool {
        *self.state.lock().unwrap() == CoroutineState::Completed
    }

    /// 检查是否取消
    pub fn is_cancelled(&self) -> bool {
        *self.state.lock().unwrap() == CoroutineState::Cancelled
    }
}

impl Default for BolideFuture {
    fn default() -> Self {
        Self::new()
    }
}

// ==================== FFI 导出 ====================

/// 包装函数指针使其可跨线程发送
#[derive(Clone, Copy)]
struct SendFnPtr(*const c_void);
unsafe impl Send for SendFnPtr {}

/// 启动协程（返回 int）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_int(
    func_ptr: extern "C" fn() -> i64
) -> *mut BolideFuture {
    let future = Box::new(BolideFuture::new());
    let future_ptr = Box::into_raw(future);

    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let state = unsafe { (*future_ptr).state.clone() };
    let result = unsafe { (*future_ptr).result.clone() };
    let condvar = unsafe { (*future_ptr).condvar.clone() };
    let on_complete = unsafe { (*future_ptr).on_complete.clone() };

    thread::spawn(move || {
        let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(send_fn) };
        let val = f();

        let callback;
        {
            let mut on_complete_guard = on_complete.lock().unwrap();
            let mut s = state.lock().unwrap();
            if *s == CoroutineState::Running {
                *result.lock().unwrap() = Some(CoroutineResult { int_val: val });
                *s = CoroutineState::Completed;
                condvar.notify_all();
                callback = on_complete_guard.take();
            } else {
                callback = None;
            }
        }
        if let Some(cb) = callback {
            cb();
        }
    });

    future_ptr
}

/// 启动协程（返回 float）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_float(
    func_ptr: extern "C" fn() -> f64
) -> *mut BolideFuture {
    let future = Box::new(BolideFuture::new());
    let future_ptr = Box::into_raw(future);

    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let state = unsafe { (*future_ptr).state.clone() };
    let result = unsafe { (*future_ptr).result.clone() };
    let condvar = unsafe { (*future_ptr).condvar.clone() };
    let on_complete = unsafe { (*future_ptr).on_complete.clone() };

    thread::spawn(move || {
        let f: extern "C" fn() -> f64 = unsafe { std::mem::transmute(send_fn) };
        let val = f();

        let callback;
        {
            let mut on_complete_guard = on_complete.lock().unwrap();
            let mut s = state.lock().unwrap();
            if *s == CoroutineState::Running {
                *result.lock().unwrap() = Some(CoroutineResult { float_val: val });
                *s = CoroutineState::Completed;
                condvar.notify_all();
                callback = on_complete_guard.take();
            } else {
                callback = None;
            }
        }
        if let Some(cb) = callback {
            cb();
        }
    });

    future_ptr
}

/// 启动协程（返回指针）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_ptr(
    func_ptr: extern "C" fn() -> *mut c_void
) -> *mut BolideFuture {
    let future = Box::new(BolideFuture::new());
    let future_ptr = Box::into_raw(future);

    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let state = unsafe { (*future_ptr).state.clone() };
    let result = unsafe { (*future_ptr).result.clone() };
    let condvar = unsafe { (*future_ptr).condvar.clone() };
    let on_complete = unsafe { (*future_ptr).on_complete.clone() };

    thread::spawn(move || {
        let f: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(send_fn) };
        let val = f();

        let callback;
        {
            let mut on_complete_guard = on_complete.lock().unwrap();
            let mut s = state.lock().unwrap();
            if *s == CoroutineState::Running {
                *result.lock().unwrap() = Some(CoroutineResult { ptr_val: val });
                *s = CoroutineState::Completed;
                condvar.notify_all();
                callback = on_complete_guard.take();
            } else {
                callback = None;
            }
        }
        if let Some(cb) = callback {
            cb();
        }
    });

    future_ptr
}

/// 等待协程结果（int）
#[no_mangle]
pub extern "C" fn bolide_coroutine_await_int(future: *mut BolideFuture) -> i64 {
    if future.is_null() { return 0; }
    let future = unsafe { &*future };
    future.await_result().map(|r| unsafe { r.int_val }).unwrap_or(0)
}

/// 等待协程结果（float）
#[no_mangle]
pub extern "C" fn bolide_coroutine_await_float(future: *mut BolideFuture) -> f64 {
    if future.is_null() { return 0.0; }
    let future = unsafe { &*future };
    future.await_result().map(|r| unsafe { r.float_val }).unwrap_or(0.0)
}

/// 等待协程结果（指针）
#[no_mangle]
pub extern "C" fn bolide_coroutine_await_ptr(future: *mut BolideFuture) -> *mut c_void {
    if future.is_null() { return std::ptr::null_mut(); }
    let future = unsafe { &*future };
    future.await_result().map(|r| unsafe { r.ptr_val }).unwrap_or(std::ptr::null_mut())
}

/// 取消协程
#[no_mangle]
pub extern "C" fn bolide_coroutine_cancel(future: *mut BolideFuture) {
    if !future.is_null() {
        let future = unsafe { &*future };
        future.cancel();
    }
}

/// 释放 Future
#[no_mangle]
pub extern "C" fn bolide_coroutine_free(future: *mut BolideFuture) {
    if !future.is_null() {
        unsafe { let _ = Box::from_raw(future); }
    }
}

// ==================== 带环境的协程启动 ====================

/// 启动协程（带环境，返回 int）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_int_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> i64,
    env: *mut c_void,
) -> *mut BolideFuture {
    let future = Box::new(BolideFuture::new());
    let future_ptr = Box::into_raw(future);

    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let send_env = SendFnPtr(env);
    let state = unsafe { (*future_ptr).state.clone() };
    let result = unsafe { (*future_ptr).result.clone() };
    let condvar = unsafe { (*future_ptr).condvar.clone() };
    let on_complete = unsafe { (*future_ptr).on_complete.clone() };

    thread::spawn(move || {
        let f: extern "C" fn(*mut c_void) -> i64 = unsafe { std::mem::transmute(send_fn) };
        let e: *mut c_void = unsafe { std::mem::transmute(send_env) };
        let val = f(e);

        let callback;
        {
            let mut on_complete_guard = on_complete.lock().unwrap();
            let mut s = state.lock().unwrap();
            if *s == CoroutineState::Running {
                *result.lock().unwrap() = Some(CoroutineResult { int_val: val });
                *s = CoroutineState::Completed;
                condvar.notify_all();
                callback = on_complete_guard.take();
            } else {
                callback = None;
            }
        }
        if let Some(cb) = callback {
            cb();
        }
    });

    future_ptr
}

/// 启动协程（带环境，返回 float）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_float_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> f64,
    env: *mut c_void,
) -> *mut BolideFuture {
    let future = Box::new(BolideFuture::new());
    let future_ptr = Box::into_raw(future);

    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let send_env = SendFnPtr(env);
    let state = unsafe { (*future_ptr).state.clone() };
    let result = unsafe { (*future_ptr).result.clone() };
    let condvar = unsafe { (*future_ptr).condvar.clone() };
    let on_complete = unsafe { (*future_ptr).on_complete.clone() };

    thread::spawn(move || {
        let f: extern "C" fn(*mut c_void) -> f64 = unsafe { std::mem::transmute(send_fn) };
        let e: *mut c_void = unsafe { std::mem::transmute(send_env) };
        let val = f(e);

        let callback;
        {
            let mut on_complete_guard = on_complete.lock().unwrap();
            let mut s = state.lock().unwrap();
            if *s == CoroutineState::Running {
                *result.lock().unwrap() = Some(CoroutineResult { float_val: val });
                *s = CoroutineState::Completed;
                condvar.notify_all();
                callback = on_complete_guard.take();
            } else {
                callback = None;
            }
        }
        if let Some(cb) = callback {
            cb();
        }
    });

    future_ptr
}

/// 启动协程（带环境，返回 ptr）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_ptr_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> *mut c_void,
    env: *mut c_void,
) -> *mut BolideFuture {
    let future = Box::new(BolideFuture::new());
    let future_ptr = Box::into_raw(future);

    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let send_env = SendFnPtr(env);
    let state = unsafe { (*future_ptr).state.clone() };
    let result = unsafe { (*future_ptr).result.clone() };
    let condvar = unsafe { (*future_ptr).condvar.clone() };
    let on_complete = unsafe { (*future_ptr).on_complete.clone() };

    thread::spawn(move || {
        let f: extern "C" fn(*mut c_void) -> *mut c_void = unsafe { std::mem::transmute(send_fn) };
        let e: *mut c_void = unsafe { std::mem::transmute(send_env) };
        let val = f(e);

        let callback;
        {
            let mut on_complete_guard = on_complete.lock().unwrap();
            let mut s = state.lock().unwrap();
            if *s == CoroutineState::Running {
                *result.lock().unwrap() = Some(CoroutineResult { ptr_val: val });
                *s = CoroutineState::Completed;
                condvar.notify_all();
                callback = on_complete_guard.take();
            } else {
                callback = None;
            }
        }
        if let Some(cb) = callback {
            cb();
        }
    });

    future_ptr
}

// ==================== Scope 管理 ====================

use std::cell::RefCell;

thread_local! {
    static SCOPE_FUTURES: RefCell<Vec<Vec<*mut BolideFuture>>> = RefCell::new(Vec::new());
}

/// 进入新的 await scope
#[no_mangle]
pub extern "C" fn bolide_scope_enter() {
    SCOPE_FUTURES.with(|stack| {
        stack.borrow_mut().push(Vec::new());
    });
}

/// 注册 Future 到当前 scope
#[no_mangle]
pub extern "C" fn bolide_scope_register(future: *mut BolideFuture) {
    if future.is_null() { return; }
    SCOPE_FUTURES.with(|stack| {
        if let Some(current) = stack.borrow_mut().last_mut() {
            current.push(future);
        }
    });
}

/// 退出 scope 并等待所有未完成的 Future
#[no_mangle]
pub extern "C" fn bolide_scope_exit() {
    SCOPE_FUTURES.with(|stack| {
        if let Some(futures) = stack.borrow_mut().pop() {
            for future_ptr in futures {
                if !future_ptr.is_null() {
                    let future = unsafe { &*future_ptr };
                    let _ = future.await_result();
                }
            }
        }
    });
}

// ==================== Select 支持 ====================

/// Select 上下文 - 用于通知机制
struct SelectContext {
    winner: Mutex<Option<usize>>,
    condvar: Condvar,
}

impl SelectContext {
    fn new() -> Self {
        Self {
            winner: Mutex::new(None),
            condvar: Condvar::new(),
        }
    }

    /// 尝试设置获胜者（只有第一个成功）
    fn try_set_winner(&self, index: usize) -> bool {
        let mut winner = self.winner.lock().unwrap();
        if winner.is_none() {
            *winner = Some(index);
            self.condvar.notify_all();
            true
        } else {
            false
        }
    }

    /// 等待获胜者
    fn wait_winner(&self) -> usize {
        let mut winner = self.winner.lock().unwrap();
        while winner.is_none() {
            winner = self.condvar.wait(winner).unwrap();
        }
        winner.unwrap()
    }
}

/// 等待第一个完成的 Future，返回其索引（0-based）
#[no_mangle]
pub extern "C" fn bolide_select_wait_first(
    futures: *const *mut BolideFuture,
    count: i64,
) -> i64 {
    if futures.is_null() || count <= 0 {
        return -1;
    }

    let futures_slice = unsafe {
        std::slice::from_raw_parts(futures, count as usize)
    };

    // 先检查是否有已完成的（按顺序，保证确定性）
    for (i, &future_ptr) in futures_slice.iter().enumerate() {
        if !future_ptr.is_null() {
            let future = unsafe { &*future_ptr };
            if future.is_completed() {
                return i as i64;
            }
        }
    }

    let ctx = Arc::new(SelectContext::new());
    let mut has_pending = false;

    // 使用回调机制：为每个 Future 注册完成回调
    for (i, &future_ptr) in futures_slice.iter().enumerate() {
        if !future_ptr.is_null() {
            let future = unsafe { &*future_ptr };
            // 再次检查，避免竞态
            if future.is_completed() {
                return i as i64;
            }
            has_pending = true;
            let ctx_clone = ctx.clone();
            let idx = i;

            // 注册回调，Future 完成时会自动调用
            future.on_complete(Box::new(move || {
                ctx_clone.try_set_winner(idx);
            }));
        }
    }

    if !has_pending {
        return -1;
    }

    // 等待第一个完成（零轮询，纯事件驱动）
    ctx.wait_winner() as i64
}

