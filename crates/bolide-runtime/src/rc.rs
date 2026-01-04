//! 引用计数内存管理
//!
//! Bolide 使用引用计数 (RC) 进行内存管理：
//! - 强引用 (BolideRc): 持有对象，计数归零时释放
//! - 弱引用 (BolideWeak): 不持有对象，用于打破循环引用
//!
//! spawn 使用 move 语义：传入数据后原变量失效

use std::cell::Cell;
use std::marker::PhantomData;
use std::os::raw::c_void;
use std::ptr::NonNull;

/// 类型标签，用于运行时类型识别
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeTag {
    String = 1,
    BigInt = 2,
    Decimal = 3,
    List = 4,
    Object = 5,    // 用户自定义对象
    Closure = 6,   // 闭包
    Future = 7,    // Future/Promise
}

/// 对象头，位于每个堆分配对象之前
///
/// 内存布局:
/// ```text
/// +------------------+
/// | strong_count: u32|  4 bytes
/// +------------------+
/// | weak_count: u32  |  4 bytes
/// +------------------+
/// | type_tag: u8     |  1 byte
/// +------------------+
/// | flags: u8        |  1 byte
/// +------------------+
/// | padding          |  6 bytes (对齐到 16)
/// +------------------+
/// | data...          |  实际数据
/// +------------------+
/// ```
#[repr(C)]
pub struct RcHeader {
    /// 强引用计数
    strong_count: Cell<u32>,
    /// 弱引用计数 (包含一个隐式的 +1，当 strong > 0 时)
    weak_count: Cell<u32>,
    /// 类型标签
    pub type_tag: TypeTag,
    /// 标志位
    /// - bit 0: 是否已标记为待释放
    /// - bit 1: 是否被 spawn move
    pub flags: Cell<u8>,
    /// 填充对齐
    _padding: [u8; 6],
}

/// 标志位常量
pub mod flags {
    pub const DROPPING: u8 = 0b0000_0001;
    pub const MOVED: u8 = 0b0000_0010;
}

impl RcHeader {
    /// 创建新的对象头
    #[inline]
    pub fn new(type_tag: TypeTag) -> Self {
        RcHeader {
            strong_count: Cell::new(1),
            weak_count: Cell::new(1), // 隐式 +1
            type_tag,
            flags: Cell::new(0),
            _padding: [0; 6],
        }
    }

    /// 增加强引用计数
    #[inline]
    pub fn inc_strong(&self) {
        let count = self.strong_count.get();
        debug_assert!(count > 0, "inc_strong on dropped object");
        self.strong_count.set(count + 1);
    }

    /// 减少强引用计数，返回是否应该释放数据
    #[inline]
    pub fn dec_strong(&self) -> bool {
        let count = self.strong_count.get();
        debug_assert!(count > 0, "dec_strong underflow");
        self.strong_count.set(count - 1);
        count == 1
    }

    /// 获取强引用计数
    #[inline]
    pub fn strong_count(&self) -> u32 {
        self.strong_count.get()
    }

    /// 增加弱引用计数
    #[inline]
    pub fn inc_weak(&self) {
        let count = self.weak_count.get();
        self.weak_count.set(count + 1);
    }

    /// 减少弱引用计数，返回是否应该释放头部
    #[inline]
    pub fn dec_weak(&self) -> bool {
        let count = self.weak_count.get();
        debug_assert!(count > 0, "dec_weak underflow");
        self.weak_count.set(count - 1);
        count == 1
    }

    /// 获取弱引用计数
    #[inline]
    pub fn weak_count(&self) -> u32 {
        // 返回实际弱引用数（减去隐式的 1）
        self.weak_count.get() - if self.strong_count.get() > 0 { 1 } else { 0 }
    }

    /// 检查对象是否仍然存活
    #[inline]
    pub fn is_alive(&self) -> bool {
        self.strong_count.get() > 0
    }

    /// 标记为已 move（spawn 使用）
    #[inline]
    pub fn mark_moved(&self) {
        self.flags.set(self.flags.get() | flags::MOVED);
    }

    /// 检查是否已 move
    #[inline]
    pub fn is_moved(&self) -> bool {
        self.flags.get() & flags::MOVED != 0
    }
}

/// 带引用计数的指针（强引用）
#[repr(C)]
pub struct BolideRc<T> {
    ptr: NonNull<RcBox<T>>,
    _marker: PhantomData<RcBox<T>>,
}

/// 内部存储结构
#[repr(C)]
struct RcBox<T> {
    header: RcHeader,
    value: T,
}

impl<T> BolideRc<T> {
    /// 创建新的引用计数对象
    pub fn new(value: T, type_tag: TypeTag) -> Self {
        let boxed = Box::new(RcBox {
            header: RcHeader::new(type_tag),
            value,
        });
        BolideRc {
            ptr: NonNull::new(Box::into_raw(boxed)).unwrap(),
            _marker: PhantomData,
        }
    }

    /// 获取原始指针（用于 FFI）
    #[inline]
    pub fn as_ptr(&self) -> *mut T {
        unsafe { &mut (*self.ptr.as_ptr()).value as *mut T }
    }

    /// 从原始指针恢复（用于 FFI）
    ///
    /// # Safety
    /// 指针必须来自 BolideRc::as_ptr()
    #[inline]
    pub unsafe fn from_ptr(ptr: *mut T) -> Self {
        let box_ptr = (ptr as *mut u8).sub(std::mem::size_of::<RcHeader>()) as *mut RcBox<T>;
        BolideRc {
            ptr: NonNull::new_unchecked(box_ptr),
            _marker: PhantomData,
        }
    }

    /// 获取对象头
    #[inline]
    fn header(&self) -> &RcHeader {
        unsafe { &(*self.ptr.as_ptr()).header }
    }

    /// 获取强引用计数
    #[inline]
    pub fn strong_count(&self) -> u32 {
        self.header().strong_count()
    }

    /// 获取弱引用计数
    #[inline]
    pub fn weak_count(&self) -> u32 {
        self.header().weak_count()
    }

    /// 创建弱引用
    pub fn downgrade(&self) -> BolideWeak<T> {
        self.header().inc_weak();
        BolideWeak {
            ptr: self.ptr,
            _marker: PhantomData,
        }
    }

    /// 检查是否已被 move（用于 spawn）
    #[inline]
    pub fn is_moved(&self) -> bool {
        self.header().is_moved()
    }

    /// 标记为已 move（spawn 调用）
    #[inline]
    pub fn mark_moved(&self) {
        self.header().mark_moved();
    }
}

impl<T> Clone for BolideRc<T> {
    fn clone(&self) -> Self {
        self.header().inc_strong();
        BolideRc {
            ptr: self.ptr,
            _marker: PhantomData,
        }
    }
}

impl<T> Drop for BolideRc<T> {
    fn drop(&mut self) {
        if self.header().dec_strong() {
            // 强引用归零，释放数据
            unsafe {
                // 先 drop 内部数据
                std::ptr::drop_in_place(&mut (*self.ptr.as_ptr()).value);
            }
            // 减少隐式弱引用
            if self.header().dec_weak() {
                // 弱引用也归零，释放整个 Box
                unsafe {
                    let _ = Box::from_raw(self.ptr.as_ptr());
                }
            }
        }
    }
}

impl<T> std::ops::Deref for BolideRc<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &(*self.ptr.as_ptr()).value }
    }
}

impl<T> std::ops::DerefMut for BolideRc<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut (*self.ptr.as_ptr()).value }
    }
}

/// 弱引用
#[repr(C)]
pub struct BolideWeak<T> {
    ptr: NonNull<RcBox<T>>,
    _marker: PhantomData<RcBox<T>>,
}

impl<T> BolideWeak<T> {
    /// 尝试升级为强引用
    pub fn upgrade(&self) -> Option<BolideRc<T>> {
        let header = unsafe { &(*self.ptr.as_ptr()).header };
        if header.is_alive() {
            header.inc_strong();
            Some(BolideRc {
                ptr: self.ptr,
                _marker: PhantomData,
            })
        } else {
            None
        }
    }

    /// 获取原始指针（用于 FFI，可能返回无效指针）
    #[inline]
    pub fn as_ptr(&self) -> *mut T {
        unsafe { &mut (*self.ptr.as_ptr()).value as *mut T }
    }
}

impl<T> Clone for BolideWeak<T> {
    fn clone(&self) -> Self {
        unsafe { (*self.ptr.as_ptr()).header.inc_weak() };
        BolideWeak {
            ptr: self.ptr,
            _marker: PhantomData,
        }
    }
}

impl<T> Drop for BolideWeak<T> {
    fn drop(&mut self) {
        let header = unsafe { &(*self.ptr.as_ptr()).header };
        if header.dec_weak() {
            // 弱引用归零，释放整个 Box（数据已在强引用归零时释放）
            unsafe {
                let _ = Box::from_raw(self.ptr.as_ptr());
            }
        }
    }
}

// ==================== FFI 接口 ====================

/// 不透明的 RC 对象指针
pub type BolideRcPtr = *mut c_void;
pub type BolideWeakPtr = *mut c_void;

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_rc_retain(ptr: BolideRcPtr) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let header = (ptr as *mut RcHeader).sub(1);
        (*header).inc_strong();
    }
}

/// 减少引用计数
#[no_mangle]
pub extern "C" fn bolide_rc_release(ptr: BolideRcPtr, type_tag: u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let header_ptr = (ptr as *mut RcHeader).sub(1);
        let header = &*header_ptr;

        if header.dec_strong() {
            // 根据类型释放数据
            drop_by_type(ptr, type_tag);

            // 减少隐式弱引用
            if header.dec_weak() {
                // 释放头部
                let layout = std::alloc::Layout::from_size_align(
                    std::mem::size_of::<RcHeader>() + get_type_size(type_tag),
                    16
                ).unwrap();
                std::alloc::dealloc(header_ptr as *mut u8, layout);
            }
        }
    }
}

/// 创建弱引用
#[no_mangle]
pub extern "C" fn bolide_rc_downgrade(ptr: BolideRcPtr) -> BolideWeakPtr {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let header = (ptr as *mut RcHeader).sub(1);
        (*header).inc_weak();
        ptr // 返回相同指针，通过类型区分
    }
}

/// 尝试升级弱引用为强引用
#[no_mangle]
pub extern "C" fn bolide_weak_upgrade(ptr: BolideWeakPtr) -> BolideRcPtr {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let header = &*((ptr as *mut RcHeader).sub(1));
        if header.is_alive() {
            header.inc_strong();
            ptr
        } else {
            std::ptr::null_mut()
        }
    }
}

/// 释放弱引用
#[no_mangle]
pub extern "C" fn bolide_weak_release(ptr: BolideWeakPtr, type_tag: u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let header_ptr = (ptr as *mut RcHeader).sub(1);
        let header = &*header_ptr;

        if header.dec_weak() {
            // 释放头部
            let layout = std::alloc::Layout::from_size_align(
                std::mem::size_of::<RcHeader>() + get_type_size(type_tag),
                16
            ).unwrap();
            std::alloc::dealloc(header_ptr as *mut u8, layout);
        }
    }
}

/// 获取强引用计数
#[no_mangle]
pub extern "C" fn bolide_rc_strong_count(ptr: BolideRcPtr) -> u32 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        let header = &*((ptr as *mut RcHeader).sub(1));
        header.strong_count()
    }
}

/// 获取弱引用计数
#[no_mangle]
pub extern "C" fn bolide_rc_weak_count(ptr: BolideRcPtr) -> u32 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        let header = &*((ptr as *mut RcHeader).sub(1));
        header.weak_count()
    }
}

/// 检查对象是否已被 move
#[no_mangle]
pub extern "C" fn bolide_rc_is_moved(ptr: BolideRcPtr) -> i32 {
    if ptr.is_null() {
        return 0;
    }
    unsafe {
        let header = &*((ptr as *mut RcHeader).sub(1));
        if header.is_moved() { 1 } else { 0 }
    }
}

/// 标记对象为已 move（spawn 使用）
#[no_mangle]
pub extern "C" fn bolide_rc_mark_moved(ptr: BolideRcPtr) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let header = &*((ptr as *mut RcHeader).sub(1));
        header.mark_moved();
    }
}

// ==================== 内部辅助函数 ====================

/// 根据类型标签释放数据
unsafe fn drop_by_type(ptr: *mut c_void, type_tag: u8) {
    match type_tag {
        1 => { // String
            crate::bolide_string_free(ptr as *mut crate::BolideString);
        }
        2 => { // BigInt
            crate::bolide_bigint_free(ptr as *mut crate::BolideBigInt);
        }
        3 => { // Decimal
            crate::bolide_decimal_free(ptr as *mut crate::BolideDecimal);
        }
        4 => { // List
            crate::bolide_list_free(ptr as *mut crate::BolideList);
        }
        _ => {
            // 其他类型暂不处理
        }
    }
}

/// 获取类型的数据大小
fn get_type_size(type_tag: u8) -> usize {
    match type_tag {
        1 => std::mem::size_of::<crate::BolideString>(),
        2 => std::mem::size_of::<crate::BolideBigInt>(),
        3 => std::mem::size_of::<crate::BolideDecimal>(),
        4 => std::mem::size_of::<crate::BolideList>(),
        _ => 8,  // 默认指针大小
    }
}

// ==================== 带 RC 的分配函数 ====================

/// 分配带引用计数头的内存
#[no_mangle]
pub extern "C" fn bolide_rc_alloc(size: i64, type_tag: u8) -> BolideRcPtr {
    if size <= 0 {
        return std::ptr::null_mut();
    }

    let total_size = std::mem::size_of::<RcHeader>() + size as usize;
    let layout = std::alloc::Layout::from_size_align(total_size, 16).unwrap();

    unsafe {
        let ptr = std::alloc::alloc(layout);
        if ptr.is_null() {
            return std::ptr::null_mut();
        }

        // 初始化头部
        let header = ptr as *mut RcHeader;
        std::ptr::write(header, RcHeader {
            strong_count: Cell::new(1),
            weak_count: Cell::new(1),
            type_tag: std::mem::transmute(type_tag),
            flags: Cell::new(0),
            _padding: [0; 6],
        });

        // 返回数据部分的指针
        ptr.add(std::mem::size_of::<RcHeader>()) as BolideRcPtr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rc_basic() {
        let rc1 = BolideRc::new(42i64, TypeTag::BigInt);
        assert_eq!(*rc1, 42);
        assert_eq!(rc1.strong_count(), 1);

        let rc2 = rc1.clone();
        assert_eq!(rc1.strong_count(), 2);
        assert_eq!(rc2.strong_count(), 2);

        drop(rc2);
        assert_eq!(rc1.strong_count(), 1);
    }

    #[test]
    fn test_weak() {
        let rc = BolideRc::new(100i64, TypeTag::BigInt);
        let weak = rc.downgrade();

        assert_eq!(rc.weak_count(), 1);
        assert!(weak.upgrade().is_some());

        drop(rc);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn test_move_flag() {
        let rc = BolideRc::new(999i64, TypeTag::BigInt);
        assert!(!rc.is_moved());

        rc.mark_moved();
        assert!(rc.is_moved());
    }
}
