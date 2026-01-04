//! Bolide 通道运行时
//!
//! 提供线程安全的通道实现，用于线程间通信

use std::sync::{Arc, Mutex, Condvar};
use std::collections::VecDeque;

/// 通道内部状态（单个 Mutex 保护，保证原子性）
struct ChannelInner {
    queue: VecDeque<i64>,
    closed: bool,
}

/// Select 通知器（用于事件驱动的 select）
pub struct SelectNotifier {
    condvar: Condvar,
    mutex: Mutex<()>,
}

impl SelectNotifier {
    pub fn new() -> Self {
        Self {
            condvar: Condvar::new(),
            mutex: Mutex::new(()),
        }
    }

    /// 通知所有等待的 select
    pub fn notify(&self) {
        let _guard = self.mutex.lock().unwrap();
        self.condvar.notify_all();
    }

    /// 等待通知（带超时）
    pub fn wait_timeout(&self, timeout: std::time::Duration) -> bool {
        let guard = self.mutex.lock().unwrap();
        let result = self.condvar.wait_timeout(guard, timeout).unwrap();
        !result.1.timed_out()
    }
}

impl Default for SelectNotifier {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局 select 通知器
static GLOBAL_SELECT_NOTIFIER: once_cell::sync::Lazy<Arc<SelectNotifier>> =
    once_cell::sync::Lazy::new(|| Arc::new(SelectNotifier::new()));

/// 线程安全通道
pub struct BolideChannel {
    /// 内部状态（队列 + 关闭标志，原子操作）
    inner: Mutex<ChannelInner>,
    /// 条件变量，用于阻塞等待
    condvar: Condvar,
    /// 通道容量（0 表示无限）
    capacity: usize,
    /// 共享的 select 通知器（用于事件驱动的 select）
    select_notifier: Arc<SelectNotifier>,
}

impl BolideChannel {
    /// 创建无缓冲通道
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(ChannelInner {
                queue: VecDeque::new(),
                closed: false,
            }),
            condvar: Condvar::new(),
            capacity: 0,
            select_notifier: Arc::clone(&GLOBAL_SELECT_NOTIFIER),
        }
    }

    /// 创建带缓冲的通道
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(ChannelInner {
                queue: VecDeque::with_capacity(capacity),
                closed: false,
            }),
            condvar: Condvar::new(),
            capacity,
            select_notifier: Arc::clone(&GLOBAL_SELECT_NOTIFIER),
        }
    }

    /// 发送消息（阻塞）
    pub fn send(&self, value: i64) -> bool {
        let mut inner = self.inner.lock().unwrap();

        // 原子检查关闭状态
        if inner.closed {
            return false;
        }

        // 如果有容量限制，等待队列有空间
        if self.capacity > 0 {
            while inner.queue.len() >= self.capacity {
                if inner.closed {
                    return false;
                }
                inner = self.condvar.wait(inner).unwrap();
            }
        }

        inner.queue.push_back(value);
        self.condvar.notify_one();
        self.select_notifier.notify();  // 通知 select
        true
    }

    /// 接收消息（阻塞）
    pub fn recv(&self) -> Option<i64> {
        let mut inner = self.inner.lock().unwrap();

        loop {
            if let Some(value) = inner.queue.pop_front() {
                self.condvar.notify_one();
                return Some(value);
            }

            if inner.closed && inner.queue.is_empty() {
                return None;
            }

            inner = self.condvar.wait(inner).unwrap();
        }
    }

    /// 尝试接收消息（非阻塞）
    pub fn try_recv(&self) -> Option<i64> {
        let mut inner = self.inner.lock().unwrap();
        let value = inner.queue.pop_front();
        if value.is_some() {
            self.condvar.notify_one();
        }
        value
    }

    /// 关闭通道
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        self.condvar.notify_all();
        self.select_notifier.notify();
    }

    /// 检查通道是否已关闭
    pub fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().closed
    }
}

impl Default for BolideChannel {
    fn default() -> Self {
        Self::new()
    }
}

// ==================== FFI 导出 ====================

/// 创建无缓冲通道
#[no_mangle]
pub extern "C" fn bolide_channel_create() -> *mut BolideChannel {
    Box::into_raw(Box::new(BolideChannel::new()))
}

/// 创建带缓冲的通道
#[no_mangle]
pub extern "C" fn bolide_channel_create_buffered(capacity: i64) -> *mut BolideChannel {
    Box::into_raw(Box::new(BolideChannel::with_capacity(capacity as usize)))
}

/// 发送消息到通道
/// 返回 1 表示成功，0 表示失败（通道已关闭）
#[no_mangle]
pub extern "C" fn bolide_channel_send(channel: *mut BolideChannel, value: i64) -> i64 {
    if channel.is_null() {
        return 0;
    }

    let channel = unsafe { &*channel };
    if channel.send(value) { 1 } else { 0 }
}

/// 从通道接收消息（阻塞）
/// 如果通道已关闭且为空，返回 0
#[no_mangle]
pub extern "C" fn bolide_channel_recv(channel: *mut BolideChannel) -> i64 {
    if channel.is_null() {
        return 0;
    }

    let channel = unsafe { &*channel };
    channel.recv().unwrap_or(0)
}

/// 尝试从通道接收消息（非阻塞）
/// 成功时 *success = 1，失败时 *success = 0
#[no_mangle]
pub extern "C" fn bolide_channel_try_recv(
    channel: *mut BolideChannel,
    success: *mut i64,
) -> i64 {
    if channel.is_null() {
        if !success.is_null() {
            unsafe { *success = 0; }
        }
        return 0;
    }

    let channel = unsafe { &*channel };
    match channel.try_recv() {
        Some(value) => {
            if !success.is_null() {
                unsafe { *success = 1; }
            }
            value
        }
        None => {
            if !success.is_null() {
                unsafe { *success = 0; }
            }
            0
        }
    }
}

/// 关闭通道
#[no_mangle]
pub extern "C" fn bolide_channel_close(channel: *mut BolideChannel) {
    if !channel.is_null() {
        let channel = unsafe { &*channel };
        channel.close();
    }
}

/// 检查通道是否已关闭
#[no_mangle]
pub extern "C" fn bolide_channel_is_closed(channel: *mut BolideChannel) -> i64 {
    if channel.is_null() {
        return 1;
    }

    let channel = unsafe { &*channel };
    if channel.is_closed() { 1 } else { 0 }
}

/// 释放通道
#[no_mangle]
pub extern "C" fn bolide_channel_free(channel: *mut BolideChannel) {
    if !channel.is_null() {
        unsafe {
            let _ = Box::from_raw(channel);
        }
    }
}

// ==================== Select 支持 ====================

use std::time::{Duration, Instant};

/// Select 操作：同时等待多个 channel
/// channels: channel 指针数组
/// count: channel 数量
/// timeout_ms: 超时时间（毫秒），-1 表示无超时，-2 表示有 default（非阻塞）
/// value: 输出参数，接收到的值
/// 返回值: 选中的 channel 索引，-1 表示超时，-2 表示 default 被选中
#[no_mangle]
pub extern "C" fn bolide_channel_select(
    channels: *const *mut BolideChannel,
    count: i64,
    timeout_ms: i64,
    value: *mut i64,
) -> i64 {
    if channels.is_null() || count <= 0 {
        return -1;
    }

    let channel_slice = unsafe {
        std::slice::from_raw_parts(channels, count as usize)
    };

    // 收集有效的 channel 引用
    let channel_refs: Vec<&BolideChannel> = channel_slice
        .iter()
        .filter_map(|&ptr| {
            if ptr.is_null() { None } else { Some(unsafe { &*ptr }) }
        })
        .collect();

    if channel_refs.is_empty() {
        return -1;
    }

    let has_default = timeout_ms == -2;
    let has_timeout = timeout_ms >= 0;
    let deadline = if has_timeout {
        Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
    } else {
        None
    };

    loop {
        // 尝试从每个 channel 非阻塞接收
        for (idx, ch) in channel_refs.iter().enumerate() {
            if let Some(val) = ch.try_recv() {
                if !value.is_null() {
                    unsafe { *value = val; }
                }
                return idx as i64;
            }
        }

        // 如果有 default 分支，立即返回
        if has_default {
            return -2;
        }

        // 检查超时
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                return -1;  // 超时
            }
        }

        // 检查是否所有 channel 都已关闭
        let all_closed = channel_refs.iter().all(|ch| ch.is_closed());
        if all_closed {
            return -1;
        }

        // 事件驱动等待：等待任意 channel 有数据
        let wait_duration = if let Some(dl) = deadline {
            let remaining = dl.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return -1;  // 超时
            }
            remaining
        } else {
            Duration::from_millis(100)  // 无超时时，最多等待 100ms 后重新检查
        };

        GLOBAL_SELECT_NOTIFIER.wait_timeout(wait_duration);
    }
}
