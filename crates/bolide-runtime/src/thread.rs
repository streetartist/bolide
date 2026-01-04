//! Bolide 线程运行时
//!
//! 提供线程创建、线程池和 Future 支持
//! 使用 trampoline 方案，运行时只处理无参函数

use std::sync::{Arc, Mutex, Condvar};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::collections::VecDeque;
use std::os::raw::c_void;

/// 包装函数指针使其可跨线程发送
#[derive(Clone, Copy)]
struct SendFnPtr(*const c_void);
unsafe impl Send for SendFnPtr {}

/// 线程结果联合体
#[repr(C)]
#[derive(Clone, Copy)]
pub union ThreadResult {
    pub int_val: i64,
    pub float_val: f64,
    pub ptr_val: *mut c_void,
}

unsafe impl Send for ThreadResult {}
unsafe impl Sync for ThreadResult {}

/// 线程句柄
#[repr(C)]
pub struct BolideThreadHandle {
    handle: Option<JoinHandle<ThreadResult>>,
    result: ThreadResult,
    has_result: bool,
    cancelled: Arc<AtomicBool>,
}

unsafe impl Send for BolideThreadHandle {}
unsafe impl Sync for BolideThreadHandle {}

/// 线程池
pub struct BolideThreadPool {
    workers: Vec<Worker>,
    sender: Arc<Mutex<VecDeque<Job>>>,
    condvar: Arc<Condvar>,
    shutdown: Arc<Mutex<bool>>,
}

type Job = Box<dyn FnOnce() -> ThreadResult + Send + 'static>;

struct Worker {
    thread: Option<JoinHandle<()>>,
}

impl BolideThreadPool {
    pub fn new(size: usize) -> Self {
        let sender: Arc<Mutex<VecDeque<Job>>> = Arc::new(Mutex::new(VecDeque::new()));
        let condvar = Arc::new(Condvar::new());
        let shutdown = Arc::new(Mutex::new(false));

        let mut workers = Vec::with_capacity(size);

        for _ in 0..size {
            let sender = Arc::clone(&sender);
            let condvar = Arc::clone(&condvar);
            let shutdown = Arc::clone(&shutdown);

            let thread = thread::spawn(move || {
                loop {
                    let job = {
                        let mut queue = sender.lock().unwrap();
                        while queue.is_empty() {
                            if *shutdown.lock().unwrap() {
                                return;
                            }
                            queue = condvar.wait(queue).unwrap();
                        }
                        queue.pop_front()
                    };

                    if let Some(job) = job {
                        job();
                    }
                }
            });

            workers.push(Worker {
                thread: Some(thread),
            });
        }

        BolideThreadPool {
            workers,
            sender,
            condvar,
            shutdown,
        }
    }

    pub fn shutdown(&mut self) {
        *self.shutdown.lock().unwrap() = true;
        self.condvar.notify_all();

        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for BolideThreadPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 线程池任务句柄
#[repr(C)]
pub struct BolidePoolHandle {
    result: Arc<Mutex<Option<ThreadResult>>>,
    completed: Arc<(Mutex<bool>, Condvar)>,
}

unsafe impl Send for BolidePoolHandle {}
unsafe impl Sync for BolidePoolHandle {}

// ==================== 线程 spawn FFI ====================

/// 创建新线程执行返回 int 的无参函数
#[no_mangle]
pub extern "C" fn bolide_thread_spawn_int(func_ptr: extern "C" fn() -> i64) -> *mut BolideThreadHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let cancelled = Arc::new(AtomicBool::new(false));

    let handle = thread::spawn(move || {
        let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(send_fn) };
        ThreadResult { int_val: f() }
    });

    Box::into_raw(Box::new(BolideThreadHandle {
        handle: Some(handle),
        result: ThreadResult { int_val: 0 },
        has_result: false,
        cancelled,
    }))
}

/// 创建新线程执行返回 float 的无参函数
#[no_mangle]
pub extern "C" fn bolide_thread_spawn_float(func_ptr: extern "C" fn() -> f64) -> *mut BolideThreadHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let cancelled = Arc::new(AtomicBool::new(false));

    let handle = thread::spawn(move || {
        let f: extern "C" fn() -> f64 = unsafe { std::mem::transmute(send_fn) };
        ThreadResult { float_val: f() }
    });

    Box::into_raw(Box::new(BolideThreadHandle {
        handle: Some(handle),
        result: ThreadResult { float_val: 0.0 },
        has_result: false,
        cancelled,
    }))
}

/// 创建新线程执行返回指针的无参函数（用于 string, bigint, decimal 等）
#[no_mangle]
pub extern "C" fn bolide_thread_spawn_ptr(func_ptr: extern "C" fn() -> *mut c_void) -> *mut BolideThreadHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let cancelled = Arc::new(AtomicBool::new(false));

    let handle = thread::spawn(move || {
        let f: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(send_fn) };
        ThreadResult { ptr_val: f() }
    });

    Box::into_raw(Box::new(BolideThreadHandle {
        handle: Some(handle),
        result: ThreadResult { ptr_val: std::ptr::null_mut() },
        has_result: false,
        cancelled,
    }))
}

// ==================== 带环境的线程 spawn FFI ====================

/// 创建新线程执行带环境的返回 int 的函数
#[no_mangle]
pub extern "C" fn bolide_thread_spawn_int_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> i64,
    env: *mut c_void,
) -> *mut BolideThreadHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let env_addr = env as usize;
    let cancelled = Arc::new(AtomicBool::new(false));

    let handle = thread::spawn(move || {
        let f: extern "C" fn(*mut c_void) -> i64 = unsafe { std::mem::transmute(send_fn) };
        let env_ptr = env_addr as *mut c_void;
        ThreadResult { int_val: f(env_ptr) }
    });

    Box::into_raw(Box::new(BolideThreadHandle {
        handle: Some(handle),
        result: ThreadResult { int_val: 0 },
        has_result: false,
        cancelled,
    }))
}

/// 创建新线程执行带环境的返回 float 的函数
#[no_mangle]
pub extern "C" fn bolide_thread_spawn_float_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> f64,
    env: *mut c_void,
) -> *mut BolideThreadHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let env_addr = env as usize;
    let cancelled = Arc::new(AtomicBool::new(false));

    let handle = thread::spawn(move || {
        let f: extern "C" fn(*mut c_void) -> f64 = unsafe { std::mem::transmute(send_fn) };
        let env_ptr = env_addr as *mut c_void;
        ThreadResult { float_val: f(env_ptr) }
    });

    Box::into_raw(Box::new(BolideThreadHandle {
        handle: Some(handle),
        result: ThreadResult { float_val: 0.0 },
        has_result: false,
        cancelled,
    }))
}

/// 创建新线程执行带环境的返回指针的函数
#[no_mangle]
pub extern "C" fn bolide_thread_spawn_ptr_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> *mut c_void,
    env: *mut c_void,
) -> *mut BolideThreadHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let env_addr = env as usize;
    let cancelled = Arc::new(AtomicBool::new(false));

    let handle = thread::spawn(move || {
        let f: extern "C" fn(*mut c_void) -> *mut c_void = unsafe { std::mem::transmute(send_fn) };
        let env_ptr = env_addr as *mut c_void;
        ThreadResult { ptr_val: f(env_ptr) }
    });

    Box::into_raw(Box::new(BolideThreadHandle {
        handle: Some(handle),
        result: ThreadResult { ptr_val: std::ptr::null_mut() },
        has_result: false,
        cancelled,
    }))
}

/// 等待线程完成并获取 int 类型结果
#[no_mangle]
pub extern "C" fn bolide_thread_join_int(handle: *mut BolideThreadHandle) -> i64 {
    if handle.is_null() {
        return 0;
    }

    let handle = unsafe { &mut *handle };

    if !handle.has_result {
        if let Some(join_handle) = handle.handle.take() {
            match join_handle.join() {
                Ok(result) => {
                    handle.result = result;
                    handle.has_result = true;
                }
                Err(_) => return 0,
            }
        }
    }

    unsafe { handle.result.int_val }
}

/// 等待线程完成并获取 float 类型结果
#[no_mangle]
pub extern "C" fn bolide_thread_join_float(handle: *mut BolideThreadHandle) -> f64 {
    if handle.is_null() {
        return 0.0;
    }

    let handle = unsafe { &mut *handle };

    if !handle.has_result {
        if let Some(join_handle) = handle.handle.take() {
            match join_handle.join() {
                Ok(result) => {
                    handle.result = result;
                    handle.has_result = true;
                }
                Err(_) => return 0.0,
            }
        }
    }

    unsafe { handle.result.float_val }
}

/// 等待线程完成并获取指针类型结果
#[no_mangle]
pub extern "C" fn bolide_thread_join_ptr(handle: *mut BolideThreadHandle) -> *mut c_void {
    if handle.is_null() {
        return std::ptr::null_mut();
    }

    let handle = unsafe { &mut *handle };

    if !handle.has_result {
        if let Some(join_handle) = handle.handle.take() {
            match join_handle.join() {
                Ok(result) => {
                    handle.result = result;
                    handle.has_result = true;
                }
                Err(_) => return std::ptr::null_mut(),
            }
        }
    }

    unsafe { handle.result.ptr_val }
}

/// 释放线程句柄
#[no_mangle]
pub extern "C" fn bolide_thread_handle_free(handle: *mut BolideThreadHandle) {
    if !handle.is_null() {
        unsafe {
            let _ = Box::from_raw(handle);
        }
    }
}

/// 取消线程（设置取消标志）
#[no_mangle]
pub extern "C" fn bolide_thread_cancel(handle: *mut BolideThreadHandle) {
    if !handle.is_null() {
        unsafe {
            (*handle).cancelled.store(true, Ordering::SeqCst);
        }
    }
}

/// 检查线程是否已被取消
#[no_mangle]
pub extern "C" fn bolide_thread_is_cancelled(handle: *const BolideThreadHandle) -> i64 {
    if handle.is_null() {
        return 0;
    }
    unsafe {
        if (*handle).cancelled.load(Ordering::SeqCst) { 1 } else { 0 }
    }
}

// ==================== 线程池 FFI ====================

struct SendPtr(*mut BolideThreadPool);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

static POOL_CONTEXT: Mutex<Option<SendPtr>> = Mutex::new(None);

/// 创建线程池
#[no_mangle]
pub extern "C" fn bolide_pool_create(size: i64) -> *mut BolideThreadPool {
    let pool = BolideThreadPool::new(size as usize);
    Box::into_raw(Box::new(pool))
}

/// 设置当前线程池上下文
#[no_mangle]
pub extern "C" fn bolide_pool_enter(pool: *mut BolideThreadPool) {
    let mut ctx = POOL_CONTEXT.lock().unwrap();
    *ctx = Some(SendPtr(pool));
}

/// 清除当前线程池上下文
#[no_mangle]
pub extern "C" fn bolide_pool_exit() {
    let mut ctx = POOL_CONTEXT.lock().unwrap();
    *ctx = None;
}

/// 检查是否在线程池上下文中
#[no_mangle]
pub extern "C" fn bolide_pool_is_active() -> i64 {
    let ctx = POOL_CONTEXT.lock().unwrap();
    if ctx.is_some() { 1 } else { 0 }
}

/// 在线程池中执行返回 int 的任务
#[no_mangle]
pub extern "C" fn bolide_pool_spawn_int(func_ptr: extern "C" fn() -> i64) -> *mut BolidePoolHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);

    let result: Arc<Mutex<Option<ThreadResult>>> = Arc::new(Mutex::new(None));
    let completed = Arc::new((Mutex::new(false), Condvar::new()));

    let result_clone = Arc::clone(&result);
    let completed_clone = Arc::clone(&completed);

    let ctx = POOL_CONTEXT.lock().unwrap();
    if let Some(ref send_ptr) = *ctx {
        let pool = unsafe { &*send_ptr.0 };

        let job = Box::new(move || {
            let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(send_fn) };
            let res = ThreadResult { int_val: f() };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            res
        });

        {
            let mut queue = pool.sender.lock().unwrap();
            queue.push_back(job);
        }
        pool.condvar.notify_one();
    } else {
        // 不在线程池上下文中，创建普通线程
        thread::spawn(move || {
            let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(send_fn) };
            let res = ThreadResult { int_val: f() };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        });
    }

    Box::into_raw(Box::new(BolidePoolHandle { result, completed }))
}

/// 在线程池中执行返回 float 的任务
#[no_mangle]
pub extern "C" fn bolide_pool_spawn_float(func_ptr: extern "C" fn() -> f64) -> *mut BolidePoolHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);

    let result: Arc<Mutex<Option<ThreadResult>>> = Arc::new(Mutex::new(None));
    let completed = Arc::new((Mutex::new(false), Condvar::new()));

    let result_clone = Arc::clone(&result);
    let completed_clone = Arc::clone(&completed);

    let ctx = POOL_CONTEXT.lock().unwrap();
    if let Some(ref send_ptr) = *ctx {
        let pool = unsafe { &*send_ptr.0 };

        let job = Box::new(move || {
            let f: extern "C" fn() -> f64 = unsafe { std::mem::transmute(send_fn) };
            let res = ThreadResult { float_val: f() };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            res
        });

        {
            let mut queue = pool.sender.lock().unwrap();
            queue.push_back(job);
        }
        pool.condvar.notify_one();
    } else {
        thread::spawn(move || {
            let f: extern "C" fn() -> f64 = unsafe { std::mem::transmute(send_fn) };
            let res = ThreadResult { float_val: f() };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        });
    }

    Box::into_raw(Box::new(BolidePoolHandle { result, completed }))
}

/// 在线程池中执行返回指针的任务
#[no_mangle]
pub extern "C" fn bolide_pool_spawn_ptr(func_ptr: extern "C" fn() -> *mut c_void) -> *mut BolidePoolHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);

    let result: Arc<Mutex<Option<ThreadResult>>> = Arc::new(Mutex::new(None));
    let completed = Arc::new((Mutex::new(false), Condvar::new()));

    let result_clone = Arc::clone(&result);
    let completed_clone = Arc::clone(&completed);

    let ctx = POOL_CONTEXT.lock().unwrap();
    if let Some(ref send_ptr) = *ctx {
        let pool = unsafe { &*send_ptr.0 };

        let job = Box::new(move || {
            let f: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(send_fn) };
            let res = ThreadResult { ptr_val: f() };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            res
        });

        {
            let mut queue = pool.sender.lock().unwrap();
            queue.push_back(job);
        }
        pool.condvar.notify_one();
    } else {
        thread::spawn(move || {
            let f: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(send_fn) };
            let res = ThreadResult { ptr_val: f() };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        });
    }

    Box::into_raw(Box::new(BolidePoolHandle { result, completed }))
}

// ==================== 带环境的线程池 spawn FFI ====================

/// 在线程池中执行带环境的返回 int 的任务
#[no_mangle]
pub extern "C" fn bolide_pool_spawn_int_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> i64,
    env: *mut c_void,
) -> *mut BolidePoolHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let env_addr = env as usize;

    let result: Arc<Mutex<Option<ThreadResult>>> = Arc::new(Mutex::new(None));
    let completed = Arc::new((Mutex::new(false), Condvar::new()));

    let result_clone = Arc::clone(&result);
    let completed_clone = Arc::clone(&completed);

    let ctx = POOL_CONTEXT.lock().unwrap();
    if let Some(ref send_ptr) = *ctx {
        let pool = unsafe { &*send_ptr.0 };

        let job = Box::new(move || {
            let f: extern "C" fn(*mut c_void) -> i64 = unsafe { std::mem::transmute(send_fn) };
            let env_ptr = env_addr as *mut c_void;
            let res = ThreadResult { int_val: f(env_ptr) };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            res
        });

        {
            let mut queue = pool.sender.lock().unwrap();
            queue.push_back(job);
        }
        pool.condvar.notify_one();
    } else {
        thread::spawn(move || {
            let f: extern "C" fn(*mut c_void) -> i64 = unsafe { std::mem::transmute(send_fn) };
            let env_ptr = env_addr as *mut c_void;
            let res = ThreadResult { int_val: f(env_ptr) };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        });
    }

    Box::into_raw(Box::new(BolidePoolHandle { result, completed }))
}

/// 在线程池中执行带环境的返回 float 的任务
#[no_mangle]
pub extern "C" fn bolide_pool_spawn_float_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> f64,
    env: *mut c_void,
) -> *mut BolidePoolHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let env_addr = env as usize;

    let result: Arc<Mutex<Option<ThreadResult>>> = Arc::new(Mutex::new(None));
    let completed = Arc::new((Mutex::new(false), Condvar::new()));

    let result_clone = Arc::clone(&result);
    let completed_clone = Arc::clone(&completed);

    let ctx = POOL_CONTEXT.lock().unwrap();
    if let Some(ref send_ptr) = *ctx {
        let pool = unsafe { &*send_ptr.0 };

        let job = Box::new(move || {
            let f: extern "C" fn(*mut c_void) -> f64 = unsafe { std::mem::transmute(send_fn) };
            let env_ptr = env_addr as *mut c_void;
            let res = ThreadResult { float_val: f(env_ptr) };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            res
        });

        {
            let mut queue = pool.sender.lock().unwrap();
            queue.push_back(job);
        }
        pool.condvar.notify_one();
    } else {
        thread::spawn(move || {
            let f: extern "C" fn(*mut c_void) -> f64 = unsafe { std::mem::transmute(send_fn) };
            let env_ptr = env_addr as *mut c_void;
            let res = ThreadResult { float_val: f(env_ptr) };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        });
    }

    Box::into_raw(Box::new(BolidePoolHandle { result, completed }))
}

/// 在线程池中执行带环境的返回指针的任务
#[no_mangle]
pub extern "C" fn bolide_pool_spawn_ptr_with_env(
    func_ptr: extern "C" fn(*mut c_void) -> *mut c_void,
    env: *mut c_void,
) -> *mut BolidePoolHandle {
    let send_fn = SendFnPtr(func_ptr as *const c_void);
    let env_addr = env as usize;

    let result: Arc<Mutex<Option<ThreadResult>>> = Arc::new(Mutex::new(None));
    let completed = Arc::new((Mutex::new(false), Condvar::new()));

    let result_clone = Arc::clone(&result);
    let completed_clone = Arc::clone(&completed);

    let ctx = POOL_CONTEXT.lock().unwrap();
    if let Some(ref send_ptr) = *ctx {
        let pool = unsafe { &*send_ptr.0 };

        let job = Box::new(move || {
            let f: extern "C" fn(*mut c_void) -> *mut c_void = unsafe { std::mem::transmute(send_fn) };
            let env_ptr = env_addr as *mut c_void;
            let res = ThreadResult { ptr_val: f(env_ptr) };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            res
        });

        {
            let mut queue = pool.sender.lock().unwrap();
            queue.push_back(job);
        }
        pool.condvar.notify_one();
    } else {
        thread::spawn(move || {
            let f: extern "C" fn(*mut c_void) -> *mut c_void = unsafe { std::mem::transmute(send_fn) };
            let env_ptr = env_addr as *mut c_void;
            let res = ThreadResult { ptr_val: f(env_ptr) };
            *result_clone.lock().unwrap() = Some(res);
            let (lock, cvar) = &*completed_clone;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        });
    }

    Box::into_raw(Box::new(BolidePoolHandle { result, completed }))
}

/// 等待线程池任务完成并获取 int 结果
#[no_mangle]
pub extern "C" fn bolide_pool_join_int(handle: *mut BolidePoolHandle) -> i64 {
    if handle.is_null() {
        return 0;
    }

    let handle = unsafe { &*handle };
    let (lock, cvar) = &*handle.completed;

    let mut completed = lock.lock().unwrap();
    while !*completed {
        completed = cvar.wait(completed).unwrap();
    }

    match handle.result.lock().unwrap().take() {
        Some(res) => unsafe { res.int_val },
        None => 0,
    }
}

/// 等待线程池任务完成并获取 float 结果
#[no_mangle]
pub extern "C" fn bolide_pool_join_float(handle: *mut BolidePoolHandle) -> f64 {
    if handle.is_null() {
        return 0.0;
    }

    let handle = unsafe { &*handle };
    let (lock, cvar) = &*handle.completed;

    let mut completed = lock.lock().unwrap();
    while !*completed {
        completed = cvar.wait(completed).unwrap();
    }

    match handle.result.lock().unwrap().take() {
        Some(res) => unsafe { res.float_val },
        None => 0.0,
    }
}

/// 等待线程池任务完成并获取指针结果
#[no_mangle]
pub extern "C" fn bolide_pool_join_ptr(handle: *mut BolidePoolHandle) -> *mut c_void {
    if handle.is_null() {
        return std::ptr::null_mut();
    }

    let handle = unsafe { &*handle };
    let (lock, cvar) = &*handle.completed;

    let mut completed = lock.lock().unwrap();
    while !*completed {
        completed = cvar.wait(completed).unwrap();
    }

    match handle.result.lock().unwrap().take() {
        Some(res) => unsafe { res.ptr_val },
        None => std::ptr::null_mut(),
    }
}

/// 释放线程池任务句柄
#[no_mangle]
pub extern "C" fn bolide_pool_handle_free(handle: *mut BolidePoolHandle) {
    if !handle.is_null() {
        unsafe {
            let _ = Box::from_raw(handle);
        }
    }
}

/// 销毁线程池
#[no_mangle]
pub extern "C" fn bolide_pool_destroy(pool: *mut BolideThreadPool) {
    if !pool.is_null() {
        unsafe {
            let mut pool = Box::from_raw(pool);
            pool.shutdown();
        }
    }
}
