//! Bolide 协程运行时
//!
//! 提供 cold Future 风格的协程支持。
//!
//! `async fn` 调用只创建 Future，不默认并行执行；`await` 会在当前线程驱动
//! 尚未启动的 Future。需要后台执行时由 runtime 显式 schedule。

use once_cell::sync::Lazy;
use std::collections::VecDeque;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

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
type CoroutineTask = Box<dyn FnOnce() + Send + 'static>;

enum FutureTask {
    Int(Box<dyn FnOnce() -> i64 + Send + 'static>),
    Float(Box<dyn FnOnce() -> f64 + Send + 'static>),
    Ptr(Box<dyn FnOnce() -> *mut c_void + Send + 'static>),
}

#[derive(Clone)]
struct FutureShared {
    state: Arc<Mutex<CoroutineState>>,
    result: Arc<Mutex<Option<CoroutineResult>>>,
    condvar: Arc<Condvar>,
    on_complete: Arc<Mutex<Option<CompletionCallback>>>,
}

struct CoroutineExecutor {
    state: Mutex<CoroutineExecutorState>,
    ready: Condvar,
    worker_count: AtomicUsize,
    max_workers: usize,
}

struct CoroutineExecutorState {
    queue: VecDeque<CoroutineTask>,
}

static COROUTINE_EXECUTOR: Lazy<CoroutineExecutor> = Lazy::new(CoroutineExecutor::new);

impl CoroutineExecutor {
    fn new() -> Self {
        let default_workers = thread::available_parallelism()
            .map(|n| n.get().saturating_mul(8))
            .unwrap_or(32)
            .clamp(8, 256);
        let max_workers = std::env::var("BOLIDE_ASYNC_WORKERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default_workers)
            .clamp(1, 4096);

        Self {
            state: Mutex::new(CoroutineExecutorState {
                queue: VecDeque::new(),
            }),
            ready: Condvar::new(),
            worker_count: AtomicUsize::new(0),
            max_workers,
        }
    }

    fn spawn(&'static self, task: CoroutineTask) {
        let should_spawn = {
            let mut state = self.state.lock().unwrap();
            state.queue.push_back(task);
            let queued = state.queue.len();
            let workers = self.worker_count.load(Ordering::Acquire);
            let should_spawn = workers == 0 || (queued > workers && workers < self.max_workers);
            self.ready.notify_one();
            should_spawn
        };

        if should_spawn {
            if !self.spawn_worker() {
                self.run_available_inline();
            }
        }
    }

    fn spawn_worker(&'static self) -> bool {
        loop {
            let workers = self.worker_count.load(Ordering::Acquire);
            if workers >= self.max_workers {
                return true;
            }
            if self
                .worker_count
                .compare_exchange(workers, workers + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let executor: &'static CoroutineExecutor = self;
                let spawn_result = thread::Builder::new()
                    .name("bolide-async".to_string())
                    .spawn(move || executor.worker_loop());
                if spawn_result.is_err() {
                    self.worker_count.fetch_sub(1, Ordering::AcqRel);
                    return false;
                }
                return true;
            }
        }
    }

    fn worker_loop(&'static self) {
        loop {
            let task = {
                let mut state = self.state.lock().unwrap();
                loop {
                    if let Some(task) = state.queue.pop_front() {
                        break task;
                    }
                    state = self.ready.wait(state).unwrap();
                }
            };

            task();
        }
    }

    fn run_available_inline(&'static self) {
        loop {
            let task = {
                let mut state = self.state.lock().unwrap();
                state.queue.pop_front()
            };
            match task {
                Some(task) => task(),
                None => return,
            }
        }
    }
}

fn spawn_coroutine_task(task: impl FnOnce() + Send + 'static) {
    COROUTINE_EXECUTOR.spawn(Box::new(task));
}

fn complete_shared(shared: &FutureShared, result: CoroutineResult) {
    let callback;
    {
        let mut on_complete_guard = shared.on_complete.lock().unwrap();
        let mut state = shared.state.lock().unwrap();
        if *state == CoroutineState::Running {
            *shared.result.lock().unwrap() = Some(result);
            *state = CoroutineState::Completed;
            shared.condvar.notify_all();
            callback = on_complete_guard.take();
        } else {
            callback = None;
        }
    }

    if let Some(cb) = callback {
        cb();
    }
}

fn run_future_task(shared: FutureShared, task: FutureTask) {
    if *shared.state.lock().unwrap() != CoroutineState::Running {
        return;
    }

    match task {
        FutureTask::Int(task) => {
            let val = task();
            complete_shared(&shared, CoroutineResult { int_val: val });
        }
        FutureTask::Float(task) => {
            let val = task();
            complete_shared(&shared, CoroutineResult { float_val: val });
        }
        FutureTask::Ptr(task) => {
            let val = task();
            complete_shared(&shared, CoroutineResult { ptr_val: val });
        }
    }
}

/// 协程 Future
pub struct BolideFuture {
    state: Arc<Mutex<CoroutineState>>,
    result: Arc<Mutex<Option<CoroutineResult>>>,
    condvar: Arc<Condvar>,
    on_complete: Arc<Mutex<Option<CompletionCallback>>>,
    task: Mutex<Option<FutureTask>>,
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
            task: Mutex::new(None),
        }
    }

    fn with_task(task: FutureTask) -> Self {
        Self {
            state: Arc::new(Mutex::new(CoroutineState::Running)),
            result: Arc::new(Mutex::new(None)),
            condvar: Arc::new(Condvar::new()),
            on_complete: Arc::new(Mutex::new(None)),
            task: Mutex::new(Some(task)),
        }
    }

    fn shared(&self) -> FutureShared {
        FutureShared {
            state: self.state.clone(),
            result: self.result.clone(),
            condvar: self.condvar.clone(),
            on_complete: self.on_complete.clone(),
        }
    }

    fn take_task(&self) -> Option<FutureTask> {
        self.task.lock().unwrap().take()
    }

    fn run_pending(&self) {
        if let Some(task) = self.take_task() {
            run_future_task(self.shared(), task);
        }
    }

    /// 设置结果并标记完成
    pub fn complete(&self, result: CoroutineResult) {
        complete_shared(&self.shared(), result);
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
        self.run_pending();
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

/// 启动协程（返回 int）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_int(
    func_ptr: extern "C" fn() -> i64,
) -> *mut BolideFuture {
    let func_addr = func_ptr as usize;
    let task = FutureTask::Int(Box::new(move || {
        let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(func_addr as *const c_void) };
        f()
    }));
    Box::into_raw(Box::new(BolideFuture::with_task(task)))
}

/// 启动协程（返回 float）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_float(
    func_ptr: extern "C" fn() -> f64,
) -> *mut BolideFuture {
    let func_addr = func_ptr as usize;
    let task = FutureTask::Float(Box::new(move || {
        let f: extern "C" fn() -> f64 = unsafe { std::mem::transmute(func_addr as *const c_void) };
        f()
    }));
    Box::into_raw(Box::new(BolideFuture::with_task(task)))
}

/// 启动协程（返回指针）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_ptr(
    func_ptr: extern "C" fn() -> *mut c_void,
) -> *mut BolideFuture {
    let func_addr = func_ptr as usize;
    let task = FutureTask::Ptr(Box::new(move || {
        let f: extern "C" fn() -> *mut c_void =
            unsafe { std::mem::transmute(func_addr as *const c_void) };
        f()
    }));
    Box::into_raw(Box::new(BolideFuture::with_task(task)))
}

/// 显式调度 Future 到协程执行器。
#[no_mangle]
pub extern "C" fn bolide_coroutine_schedule(future: *mut BolideFuture) -> i64 {
    if future.is_null() {
        return 0;
    }

    let future = unsafe { &*future };
    let Some(task) = future.take_task() else {
        return 0;
    };
    let shared = future.shared();
    spawn_coroutine_task(move || {
        run_future_task(shared, task);
    });
    1
}

/// 等待协程结果（int）
#[no_mangle]
pub extern "C" fn bolide_coroutine_await_int(future: *mut BolideFuture) -> i64 {
    if future.is_null() {
        return 0;
    }
    let future = unsafe { &*future };
    future
        .await_result()
        .map(|r| unsafe { r.int_val })
        .unwrap_or(0)
}

/// 等待协程结果（float）
#[no_mangle]
pub extern "C" fn bolide_coroutine_await_float(future: *mut BolideFuture) -> f64 {
    if future.is_null() {
        return 0.0;
    }
    let future = unsafe { &*future };
    future
        .await_result()
        .map(|r| unsafe { r.float_val })
        .unwrap_or(0.0)
}

/// 等待协程结果（指针）
#[no_mangle]
pub extern "C" fn bolide_coroutine_await_ptr(future: *mut BolideFuture) -> *mut c_void {
    if future.is_null() {
        return std::ptr::null_mut();
    }
    let future = unsafe { &*future };
    future
        .await_result()
        .map(|r| unsafe { r.ptr_val })
        .unwrap_or(std::ptr::null_mut())
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
        unsafe {
            let _ = Box::from_raw(future);
        }
    }
}

// ==================== 带环境的协程启动 ====================

/// 启动协程（带环境，返回 int）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_int_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> i64,
    env: *mut c_void,
) -> *mut BolideFuture {
    let func_addr = func_ptr as usize;
    let env_addr = env as usize;
    let task = FutureTask::Int(Box::new(move || {
        let f: extern "C" fn(*mut c_void) -> i64 =
            unsafe { std::mem::transmute(func_addr as *const c_void) };
        f(env_addr as *mut c_void)
    }));
    Box::into_raw(Box::new(BolideFuture::with_task(task)))
}

/// 启动协程（带环境，返回 float）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_float_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> f64,
    env: *mut c_void,
) -> *mut BolideFuture {
    let func_addr = func_ptr as usize;
    let env_addr = env as usize;
    let task = FutureTask::Float(Box::new(move || {
        let f: extern "C" fn(*mut c_void) -> f64 =
            unsafe { std::mem::transmute(func_addr as *const c_void) };
        f(env_addr as *mut c_void)
    }));
    Box::into_raw(Box::new(BolideFuture::with_task(task)))
}

/// 启动协程（带环境，返回 ptr）
#[no_mangle]
pub extern "C" fn bolide_coroutine_spawn_ptr_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> *mut c_void,
    env: *mut c_void,
) -> *mut BolideFuture {
    let func_addr = func_ptr as usize;
    let env_addr = env as usize;
    let task = FutureTask::Ptr(Box::new(move || {
        let f: extern "C" fn(*mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(func_addr as *const c_void) };
        f(env_addr as *mut c_void)
    }));
    Box::into_raw(Box::new(BolideFuture::with_task(task)))
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
    if future.is_null() {
        return;
    }
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
pub extern "C" fn bolide_select_wait_first(futures: *const *mut BolideFuture, count: i64) -> i64 {
    if futures.is_null() || count <= 0 {
        return -1;
    }

    let futures_slice = unsafe { std::slice::from_raw_parts(futures, count as usize) };

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

    for &future_ptr in futures_slice {
        let _ = bolide_coroutine_schedule(future_ptr);
    }

    // 等待第一个完成（零轮询，纯事件驱动）
    ctx.wait_winner() as i64
}
