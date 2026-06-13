//! Bolide List type with reference counting
//!
//! BolideList 使用引用计数管理内存，元素按类型选用最小宽度存储：
//!   Bool → 1 字节, Ptr/Int/Float → 8 字节, RC 类型 → 8 字节(指针)

use std::os::raw::c_void;

use crate::rc::{RcHeader, TypeTag};
use crate::{BolideBigInt, BolideDecimal, BolideDict, BolideDynamic, BolideString};

/// 元素类型标签
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    Int = 0,
    Float = 1,
    Bool = 2,
    String = 3,
    BigInt = 4,
    Decimal = 5,
    List = 6,    // 嵌套列表
    Ptr = 7,     // 通用指针
    Dict = 8,    // 字典
    Dynamic = 9, // 动态类型
}

impl ElementType {
    /// 每元素存储字节数
    #[inline]
    pub fn byte_width(self) -> usize {
        match self {
            ElementType::Bool => 1,
            _ => 8,
        }
    }

    /// 从 u8 标签构造（与 bolide_list_new 的映射一致）
    #[inline]
    pub fn from_u8(tag: u8) -> Self {
        match tag {
            0 => ElementType::Int,
            1 => ElementType::Float,
            2 => ElementType::Bool,
            3 => ElementType::String,
            4 => ElementType::BigInt,
            5 => ElementType::Decimal,
            6 => ElementType::List,
            7 => ElementType::Ptr,
            8 => ElementType::Dict,
            9 => ElementType::Dynamic,
            _ => ElementType::Int,
        }
    }
}

/// Bolide 列表类型（带引用计数）
#[repr(C)]
pub struct BolideList {
    header: RcHeader,
    data: *mut u8, // 元素字节数组
    len: usize,
    capacity: usize, // 已分配元素数（非字节）
    elem_type: ElementType,
}

// 内联的读写辅助（编译成单次有符号/无符号 load/store）
impl BolideList {
    #[inline]
    unsafe fn read_at(&self, idx: usize) -> i64 {
        let p = self.data.add(idx * self.byte_width());
        if self.elem_type as u8 == ElementType::Bool as u8 {
            *p as i64
        } else {
            (p as *const i64).read_unaligned()
        }
    }

    #[inline]
    unsafe fn write_at(&self, idx: usize, val: i64) {
        let p = self.data.add(idx * self.byte_width());
        if self.elem_type as u8 == ElementType::Bool as u8 {
            *p = val as u8;
        } else {
            (p as *mut i64).write_unaligned(val);
        }
    }

    #[inline]
    fn byte_width(&self) -> usize {
        self.elem_type.byte_width()
    }

    /// 创建新列表（ref_count = 1）
    pub fn new(elem_type: ElementType) -> *mut Self {
        Box::into_raw(Box::new(Self {
            header: RcHeader::new(TypeTag::List),
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
            elem_type,
        }))
    }

    /// 创建带初始容量的列表
    pub fn with_capacity(elem_type: ElementType, capacity: usize) -> *mut Self {
        let mut list = Self {
            header: RcHeader::new(TypeTag::List),
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
            elem_type,
        };
        if capacity > 0 {
            list.reserve(capacity);
        }
        Box::into_raw(Box::new(list))
    }

    fn reserve(&mut self, additional: usize) {
        let new_cap = self.len + additional;
        if new_cap <= self.capacity {
            return;
        }

        let new_cap = new_cap.max(self.capacity * 2).max(8);
        let bw = self.byte_width();
        let layout = std::alloc::Layout::array::<u8>(new_cap * bw).unwrap();

        let new_data = if self.data.is_null() {
            unsafe { std::alloc::alloc(layout) }
        } else {
            let old_layout = std::alloc::Layout::array::<u8>(self.capacity * bw).unwrap();
            unsafe { std::alloc::realloc(self.data, old_layout, layout.size()) }
        };

        self.data = new_data;
        self.capacity = new_cap;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, value: i64) {
        if self.len >= self.capacity {
            self.reserve(1);
        }
        unsafe {
            self.write_at(self.len, value);
            self.retain_element(value);
        }
        self.len += 1;
    }

    pub fn pop(&mut self) -> Option<i64> {
        if self.len == 0 {
            None
        } else {
            self.len -= 1;
            unsafe { Some(self.read_at(self.len)) }
        }
    }

    pub fn get(&self, index: usize) -> Option<i64> {
        if index >= self.len {
            None
        } else {
            unsafe { Some(self.read_at(index)) }
        }
    }

    pub fn set(&mut self, index: usize, value: i64) -> bool {
        if index >= self.len {
            false
        } else {
            unsafe {
                let old = self.read_at(index);
                if old != value {
                    self.release_element(old);
                    self.write_at(index, value);
                    self.retain_element(value);
                }
            }
            true
        }
    }

    pub fn elem_type(&self) -> ElementType {
        self.elem_type
    }

    // ==================== RC 操作 ====================

    #[inline]
    pub fn retain(&self) {
        self.header.inc_strong();
    }

    #[inline]
    pub fn release(&self) -> bool {
        self.header.dec_strong()
    }

    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.header.strong_count()
    }

    #[inline]
    pub fn is_moved(&self) -> bool {
        self.header.is_moved()
    }

    #[inline]
    pub fn mark_moved(&self) {
        self.header.mark_moved();
    }

    /// 增加单个元素引用
    unsafe fn retain_element(&self, value: i64) {
        let ptr = value as *mut c_void;
        if ptr.is_null() {
            return;
        }
        match self.elem_type {
            ElementType::String => {
                crate::bolide_string_retain(ptr as *mut BolideString);
            }
            ElementType::BigInt => {
                crate::bolide_bigint_retain(ptr as *mut BolideBigInt);
            }
            ElementType::Decimal => {
                crate::bolide_decimal_retain(ptr as *mut BolideDecimal);
            }
            ElementType::List => {
                bolide_list_retain(ptr as *mut BolideList);
            }
            ElementType::Dict => {
                crate::bolide_dict_retain(ptr as *mut crate::dict::BolideDict);
            }
            ElementType::Dynamic => {
                crate::bolide_dynamic_retain(ptr as *mut crate::dynamic::BolideDynamic);
            }
            _ => {}
        }
    }

    /// 释放单个元素引用
    unsafe fn release_element(&self, value: i64) {
        let ptr = value as *mut c_void;
        if ptr.is_null() {
            return;
        }
        match self.elem_type {
            ElementType::String => {
                crate::bolide_string_release(ptr as *mut BolideString);
            }
            ElementType::BigInt => {
                crate::bolide_bigint_release(ptr as *mut BolideBigInt);
            }
            ElementType::Decimal => {
                crate::bolide_decimal_release(ptr as *mut BolideDecimal);
            }
            ElementType::List => {
                bolide_list_release(ptr as *mut BolideList);
            }
            ElementType::Dict => {
                crate::bolide_dict_release(ptr as *mut crate::dict::BolideDict);
            }
            ElementType::Dynamic => {
                crate::bolide_dynamic_release(ptr as *mut crate::dynamic::BolideDynamic);
            }
            _ => {}
        }
    }

    /// 释放所有元素的引用（仅当 strong_count 归零时调用）
    unsafe fn release_elements(&self) {
        for i in 0..self.len {
            let val = self.read_at(i);
            self.release_element(val);
        }
    }
}

// ==================== FFI 导出 ====================

/// 计算切片选中的下标序列（Python 语义，含负步长）。
/// `len` 为容器长度；`has_start`/`has_end` 表示对应边界是否显式给出，
/// 否则按 step 正负取默认边界。返回升序或降序的下标列表。
pub(crate) fn slice_indices(
    len: i64,
    start: i64,
    end: i64,
    step: i64,
    has_start: bool,
    has_end: bool,
) -> Vec<i64> {
    let mut out = Vec::new();
    if len == 0 || step == 0 {
        return out;
    }
    // 归一化单个边界到 [lo, hi] 钳制范围；def 为缺省值
    let norm = |v: i64, def_lo: i64, def_hi: i64| -> i64 {
        let v = if v < 0 { v + len } else { v };
        v.clamp(def_lo, def_hi)
    };

    if step > 0 {
        // 默认 start=0, end=len；钳制到 [0, len]
        let s = if has_start { norm(start, 0, len) } else { 0 };
        let e = if has_end { norm(end, 0, len) } else { len };
        let mut i = s;
        while i < e {
            out.push(i);
            i += step;
        }
    } else {
        // 负步长：默认 start=len-1, end=-1（含 0）；钳制到 [-1, len-1]
        let s = if has_start {
            norm(start, -1, len - 1)
        } else {
            len - 1
        };
        let e = if has_end { norm(end, -1, len - 1) } else { -1 };
        let mut i = s;
        while i > e {
            out.push(i);
            i += step; // step 为负
        }
    }
    out
}

/// 创建新列表
#[no_mangle]
pub extern "C" fn bolide_list_new(elem_type: u8) -> *mut BolideList {
    let elem_type = match elem_type {
        0 => ElementType::Int,
        1 => ElementType::Float,
        2 => ElementType::Bool,
        3 => ElementType::String,
        4 => ElementType::BigInt,
        5 => ElementType::Decimal,
        6 => ElementType::List,
        7 => ElementType::Ptr,
        8 => ElementType::Dict,
        9 => ElementType::Dynamic,
        _ => ElementType::Int,
    };
    BolideList::new(elem_type)
}

/// 创建带初始容量的列表
#[no_mangle]
pub extern "C" fn bolide_list_with_capacity(elem_type: u8, capacity: usize) -> *mut BolideList {
    let elem_type = match elem_type {
        0 => ElementType::Int,
        1 => ElementType::Float,
        2 => ElementType::Bool,
        3 => ElementType::String,
        4 => ElementType::BigInt,
        5 => ElementType::Decimal,
        6 => ElementType::List,
        _ => ElementType::Ptr,
    };
    BolideList::with_capacity(elem_type, capacity)
}

/// 增加引用计数
#[no_mangle]
pub extern "C" fn bolide_list_retain(list: *mut BolideList) -> *mut BolideList {
    if !list.is_null() {
        unsafe {
            (*list).retain();
        }
    }
    list
}

/// 减少引用计数
#[no_mangle]
pub extern "C" fn bolide_list_release(list: *mut BolideList) {
    if list.is_null() {
        return;
    }
    unsafe {
        if (*list).release() {
            (*list).release_elements();
            if !(*list).data.is_null() {
                let bw = (*list).byte_width();
                let layout = std::alloc::Layout::array::<u8>((*list).capacity * bw).unwrap();
                std::alloc::dealloc((*list).data, layout);
            }
            let _ = Box::from_raw(list);
        }
    }
}

/// 兼容旧 API
#[no_mangle]
pub extern "C" fn bolide_list_free(list: *mut BolideList) {
    bolide_list_release(list);
}

/// 深拷贝列表
#[no_mangle]
pub extern "C" fn bolide_list_clone(list: *const BolideList) -> *mut BolideList {
    if list.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let src = &*list;
        let new_list = BolideList::with_capacity(src.elem_type, src.len);
        let dst = &mut *new_list;

        // 复制元素（push 内部已 retain）
        for i in 0..src.len {
            let value = src.read_at(i);
            dst.push(value);
        }

        new_list
    }
}

/// 获取引用计数
#[no_mangle]
pub extern "C" fn bolide_list_ref_count(list: *const BolideList) -> u32 {
    if list.is_null() {
        return 0;
    }
    unsafe { (*list).ref_count() }
}

/// 获取列表长度
#[no_mangle]
pub extern "C" fn bolide_list_len(list: *const BolideList) -> usize {
    if list.is_null() {
        return 0;
    }
    unsafe { (*list).len() }
}

/// 追加元素
#[no_mangle]
pub extern "C" fn bolide_list_push(list: *mut BolideList, value: i64) {
    if list.is_null() {
        return;
    }
    unsafe {
        (*list).push(value);
    }
}

/// 弹出最后一个元素
#[no_mangle]
pub extern "C" fn bolide_list_pop(list: *mut BolideList) -> i64 {
    if list.is_null() {
        return 0;
    }
    unsafe { (*list).pop().unwrap_or(0) }
}

/// 获取指定位置的元素
#[no_mangle]
pub extern "C" fn bolide_list_get(list: *const BolideList, index: usize) -> i64 {
    if list.is_null() {
        return 0;
    }
    unsafe { (*list).get(index).unwrap_or(0) }
}

/// 设置指定位置的元素
#[no_mangle]
pub extern "C" fn bolide_list_set(list: *mut BolideList, index: usize, value: i64) -> i64 {
    if list.is_null() {
        return 0;
    }
    unsafe {
        if (*list).set(index, value) {
            1
        } else {
            0
        }
    }
}

/// 获取元素类型
#[no_mangle]
pub extern "C" fn bolide_list_elem_type(list: *const BolideList) -> u8 {
    if list.is_null() {
        return 7;
    }
    unsafe { (*list).elem_type() as u8 }
}

/// 检查是否已被 move
#[no_mangle]
pub extern "C" fn bolide_list_is_moved(list: *const BolideList) -> i32 {
    if list.is_null() {
        return 0;
    }
    unsafe {
        if (*list).is_moved() {
            1
        } else {
            0
        }
    }
}

/// 标记为已 move
#[no_mangle]
pub extern "C" fn bolide_list_mark_moved(list: *mut BolideList) {
    if !list.is_null() {
        unsafe {
            (*list).mark_moved();
        }
    }
}

// ==================== Python-like Methods ====================

unsafe fn ptr_copy(dst: *mut u8, src: *const u8, count: usize, bw: usize) {
    std::ptr::copy(src, dst, count * bw);
}

/// 在指定位置插入元素
#[no_mangle]
pub extern "C" fn bolide_list_insert(list: *mut BolideList, index: usize, value: i64) {
    if list.is_null() {
        return;
    }
    unsafe {
        let list = &mut *list;
        let index = index.min(list.len);
        if list.len >= list.capacity {
            list.reserve(1);
        }
        let bw = list.byte_width();
        if index < list.len {
            ptr_copy(
                list.data.add((index + 1) * bw),
                list.data.add(index * bw),
                list.len - index,
                bw,
            );
        }
        list.write_at(index, value);
        list.len += 1;
        list.retain_element(value);
    }
}

/// 移除并返回指定位置的元素
#[no_mangle]
pub extern "C" fn bolide_list_remove(list: *mut BolideList, index: usize) -> i64 {
    if list.is_null() {
        return 0;
    }
    unsafe {
        let list = &mut *list;
        if index >= list.len {
            return 0;
        }

        let value = list.read_at(index);
        if index < list.len - 1 {
            let bw = list.byte_width();
            ptr_copy(
                list.data.add(index * bw),
                list.data.add((index + 1) * bw),
                list.len - index - 1,
                bw,
            );
        }
        list.len -= 1;
        value
    }
}

/// 清空列表
#[no_mangle]
pub extern "C" fn bolide_list_clear(list: *mut BolideList) {
    if list.is_null() {
        return;
    }
    unsafe {
        let list = &mut *list;
        list.release_elements();
        list.len = 0;
    }
}

/// 原地反转列表
#[no_mangle]
pub extern "C" fn bolide_list_reverse(list: *mut BolideList) {
    if list.is_null() {
        return;
    }
    unsafe {
        let list = &mut *list;
        if list.len <= 1 {
            return;
        }
        let bw = list.byte_width();
        let mut left = 0usize;
        let mut right = list.len - 1;
        while left < right {
            // swap via temp buffer on stack
            let lp = list.data.add(left * bw);
            let rp = list.data.add(right * bw);
            std::ptr::swap_nonoverlapping(lp, rp, bw);
            left += 1;
            right -= 1;
        }
    }
}

/// 扩展列表（用另一个列表的元素）
#[no_mangle]
pub extern "C" fn bolide_list_extend(list: *mut BolideList, other: *const BolideList) {
    if list.is_null() || other.is_null() {
        return;
    }
    unsafe {
        let list = &mut *list;
        let other = &*other;
        list.reserve(other.len);
        for i in 0..other.len {
            let value = other.read_at(i);
            list.push(value);
        }
    }
}

/// 检查列表是否包含指定值
#[no_mangle]
pub extern "C" fn bolide_list_contains(list: *const BolideList, value: i64) -> i64 {
    if list.is_null() {
        return 0;
    }
    unsafe {
        let list = &*list;
        for i in 0..list.len {
            if list.read_at(i) == value {
                return 1;
            }
        }
        0
    }
}

/// 查找值的第一个索引（找不到返回 -1）
#[no_mangle]
pub extern "C" fn bolide_list_index_of(list: *const BolideList, value: i64) -> i64 {
    if list.is_null() {
        return -1;
    }
    unsafe {
        let list = &*list;
        for i in 0..list.len {
            if list.read_at(i) == value {
                return i as i64;
            }
        }
        -1
    }
}

/// 统计值出现的次数
#[no_mangle]
pub extern "C" fn bolide_list_count(list: *const BolideList, value: i64) -> i64 {
    if list.is_null() {
        return 0;
    }
    unsafe {
        let list = &*list;
        let mut count = 0i64;
        for i in 0..list.len {
            if list.read_at(i) == value {
                count += 1;
            }
        }
        count
    }
}

/// 原地排序（仅支持 Int 和 Float 类型）
#[no_mangle]
pub extern "C" fn bolide_list_sort(list: *mut BolideList) {
    if list.is_null() {
        return;
    }
    unsafe {
        let list = &mut *list;
        if list.len <= 1 {
            return;
        }

        match list.elem_type {
            ElementType::Int => {
                let slice = std::slice::from_raw_parts_mut(list.data as *mut i64, list.len);
                slice.sort();
            }
            ElementType::Float => {
                let slice = std::slice::from_raw_parts_mut(list.data as *mut i64, list.len);
                slice.sort_by(|a, b| {
                    let fa = f64::from_bits(*a as u64);
                    let fb = f64::from_bits(*b as u64);
                    fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            _ => {}
        }
    }
}

/// 切片（返回新列表）
#[no_mangle]
pub extern "C" fn bolide_list_slice(
    list: *const BolideList,
    start: i64,
    end: i64,
) -> *mut BolideList {
    if list.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let src = &*list;
        let len = src.len as i64;
        let start = if start < 0 {
            (len + start).max(0)
        } else {
            start.min(len)
        } as usize;
        let end = if end < 0 {
            (len + end).max(0)
        } else {
            end.min(len)
        } as usize;

        if start >= end {
            return BolideList::new(src.elem_type);
        }

        let slice_len = end - start;
        let new_list = BolideList::with_capacity(src.elem_type, slice_len);
        let dst = &mut *new_list;

        // push 内部已 retain
        for i in start..end {
            let value = src.read_at(i);
            dst.push(value);
        }

        new_list
    }
}

/// 带步长的切片（返回新列表）。Python 语义：
///   flags bit0=has_start, bit1=has_end；step 由编译器恒传具体值（缺省 1）。
///   step<0 时反向，默认边界相应调整。
#[no_mangle]
pub extern "C" fn bolide_list_slice_step(
    list: *const BolideList,
    start: i64,
    end: i64,
    step: i64,
    flags: i64,
) -> *mut BolideList {
    if list.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let src = &*list;
        let len = src.len as i64;
        let step = if step == 0 { 1 } else { step };
        let has_start = flags & 1 != 0;
        let has_end = flags & 2 != 0;

        let indices = slice_indices(len, start, end, step, has_start, has_end);
        let new_list = BolideList::with_capacity(src.elem_type, indices.len());
        let dst = &mut *new_list;
        for i in indices {
            // push 内部已 retain
            dst.push(src.read_at(i as usize));
        }
        new_list
    }
}

/// 检查列表是否为空
#[no_mangle]
pub extern "C" fn bolide_list_is_empty(list: *const BolideList) -> i64 {
    if list.is_null() {
        return 1;
    }
    unsafe {
        if (*list).len == 0 {
            1
        } else {
            0
        }
    }
}

/// 获取第一个元素
#[no_mangle]
pub extern "C" fn bolide_list_first(list: *const BolideList) -> i64 {
    if list.is_null() {
        return 0;
    }
    unsafe {
        let list = &*list;
        if list.len == 0 {
            return 0;
        }
        list.read_at(0)
    }
}

/// 获取最后一个元素
#[no_mangle]
pub extern "C" fn bolide_list_last(list: *const BolideList) -> i64 {
    if list.is_null() {
        return 0;
    }
    unsafe {
        let list = &*list;
        if list.len == 0 {
            return 0;
        }
        list.read_at(list.len - 1)
    }
}

/// 打印列表
#[no_mangle]
pub extern "C" fn bolide_print_list(list: *const BolideList) {
    print_list_inline(list);
    println!();
}

/// 回调函数指针（裸地址）。实际签名由源/结果元素类型决定，
/// 调用前 transmute 成正确的 fn 类型——float 走 XMM 寄存器、整数走通用寄存器，
/// ABI 不同，必须按类型分派，否则参数/返回会错位。
pub type RawCallback = *const u8;

/// 调用回调：参数和返回值各自可能是 float（按位存于 i64 槽）。
/// `param_is_float` / `ret_is_float` 决定 transmute 的目标 fn 签名。
#[inline]
unsafe fn invoke_callback(
    cb: RawCallback,
    arg: i64,
    param_is_float: bool,
    ret_is_float: bool,
) -> i64 {
    match (param_is_float, ret_is_float) {
        (false, false) => {
            let f: extern "C" fn(i64) -> i64 = std::mem::transmute(cb);
            f(arg)
        }
        (false, true) => {
            let f: extern "C" fn(i64) -> f64 = std::mem::transmute(cb);
            f(arg).to_bits() as i64
        }
        (true, false) => {
            let f: extern "C" fn(f64) -> i64 = std::mem::transmute(cb);
            f(f64::from_bits(arg as u64))
        }
        (true, true) => {
            let f: extern "C" fn(f64) -> f64 = std::mem::transmute(cb);
            f(f64::from_bits(arg as u64)).to_bits() as i64
        }
    }
}

/// 对列表每个元素调用回调，返回新列表。
/// `result_elem_type` 是回调返回值的元素类型 tag（map 可改变元素类型，如 int->str）。
/// float 参数/返回经位模式在 i64 槽传递，运行时按元素类型 transmute 回调签名。
#[no_mangle]
pub extern "C" fn bolide_list_map(
    list: *const BolideList,
    callback: RawCallback,
    result_elem_type: u8,
) -> *mut BolideList {
    if list.is_null() || callback.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let src = &*list;
        let result_et = ElementType::from_u8(result_elem_type);
        let param_is_float = matches!(src.elem_type, ElementType::Float);
        let ret_is_float = matches!(result_et, ElementType::Float);
        let dst = BolideList::with_capacity(result_et, src.len);
        for i in 0..src.len {
            let val = src.read_at(i);
            let new_val = invoke_callback(callback, val, param_is_float, ret_is_float);
            bolide_list_push(dst, new_val);
        }
        dst
    }
}

/// 过滤列表元素，callback(val) 返回非零则保留（返回值始终是 bool/i64）。
/// 参数可能是 float，按源元素类型 transmute 回调签名。
#[no_mangle]
pub extern "C" fn bolide_list_filter(
    list: *const BolideList,
    callback: RawCallback,
) -> *mut BolideList {
    if list.is_null() || callback.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let src = &*list;
        let param_is_float = matches!(src.elem_type, ElementType::Float);
        let dst = BolideList::new(src.elem_type);
        for i in 0..src.len {
            let val = src.read_at(i);
            if invoke_callback(callback, val, param_is_float, false) != 0 {
                bolide_list_push(dst, val);
            }
        }
        dst
    }
}

pub(crate) fn print_element_inline(elem_type: ElementType, val: i64) {
    match elem_type {
        ElementType::Int => print!("{}", val),
        ElementType::Float => print!("{}", f64::from_bits(val as u64)),
        ElementType::Bool => print!("{}", if val != 0 { "true" } else { "false" }),
        ElementType::String => {
            let s = val as *const BolideString;
            if !s.is_null() {
                unsafe { print!("\"{}\"", (*s).as_str()) };
            } else {
                print!("null");
            }
        }
        ElementType::BigInt => {
            let b = val as *const BolideBigInt;
            if !b.is_null() {
                unsafe { print!("{}", (*b).to_string()) };
            } else {
                print!("null");
            }
        }
        ElementType::Decimal => {
            let d = val as *const BolideDecimal;
            if !d.is_null() {
                unsafe { print!("{}", (*d).to_string()) };
            } else {
                print!("null");
            }
        }
        ElementType::List => print_list_inline(val as *const BolideList),
        ElementType::Dict => crate::dict::print_dict_inline(val as *const BolideDict),
        ElementType::Dynamic => {
            let d = val as *const BolideDynamic;
            if !d.is_null() {
                unsafe { print!("{}", (*d).to_string_repr()) };
            } else {
                print!("null");
            }
        }
        ElementType::Ptr => print!("0x{:x}", val),
    }
}

pub(crate) fn print_list_inline(list: *const BolideList) {
    if list.is_null() {
        print!("null");
        return;
    }
    unsafe {
        let list = &*list;
        print!("[");
        for i in 0..list.len {
            if i > 0 {
                print!(", ");
            }
            let val = list.read_at(i);
            print_element_inline(list.elem_type, val);
        }
        print!("]");
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_rc() {
        let list = BolideList::new(ElementType::Int);
        unsafe {
            assert_eq!((*list).ref_count(), 1);

            bolide_list_retain(list);
            assert_eq!((*list).ref_count(), 2);

            bolide_list_release(list);
            assert_eq!((*list).ref_count(), 1);

            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_bool_elements() {
        let list = BolideList::new(ElementType::Bool);
        unsafe {
            bolide_list_push(list, 1); // true
            bolide_list_push(list, 0); // false
            bolide_list_push(list, 1);

            assert_eq!((*list).len(), 3);
            assert_eq!(bolide_list_get(list, 0), 1);
            assert_eq!(bolide_list_get(list, 1), 0);
            assert_eq!(bolide_list_get(list, 2), 1);

            // Bool 元素应占 1 字节
            assert_eq!((*list).byte_width(), 1);

            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_operations() {
        let list = BolideList::new(ElementType::Int);
        unsafe {
            bolide_list_push(list, 10);
            bolide_list_push(list, 20);
            bolide_list_push(list, 30);

            assert_eq!((*list).len(), 3);
            assert_eq!(bolide_list_get(list, 0), 10);
            assert_eq!(bolide_list_get(list, 1), 20);
            assert_eq!(bolide_list_get(list, 2), 30);

            bolide_list_set(list, 1, 25);
            assert_eq!(bolide_list_get(list, 1), 25);

            assert_eq!(bolide_list_pop(list), 30);
            assert_eq!((*list).len(), 2);

            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_with_strings() {
        let list = BolideList::new(ElementType::String);
        unsafe {
            let s1 = crate::BolideString::new("hello");
            let s2 = crate::BolideString::new("world");

            bolide_list_push(list, s1 as i64);
            bolide_list_push(list, s2 as i64);

            assert_eq!((*list).len(), 2);

            let got = bolide_list_get(list, 0) as *const crate::BolideString;
            assert_eq!((*got).as_str(), "hello");

            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_clone() {
        let list = BolideList::new(ElementType::Int);
        unsafe {
            bolide_list_push(list, 100);
            bolide_list_push(list, 200);

            let cloned = bolide_list_clone(list);
            assert_eq!((*cloned).len(), 2);
            assert_eq!(bolide_list_get(cloned, 0), 100);
            assert_eq!(bolide_list_get(cloned, 1), 200);
            assert_eq!((*cloned).ref_count(), 1);

            bolide_list_release(list);
            bolide_list_release(cloned);
        }
    }

    #[test]
    fn test_list_reverse() {
        let list = BolideList::new(ElementType::Int);
        unsafe {
            bolide_list_push(list, 1);
            bolide_list_push(list, 2);
            bolide_list_push(list, 3);
            bolide_list_reverse(list);
            assert_eq!(bolide_list_get(list, 0), 3);
            assert_eq!(bolide_list_get(list, 1), 2);
            assert_eq!(bolide_list_get(list, 2), 1);
            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_reverse_bool() {
        let list = BolideList::new(ElementType::Bool);
        unsafe {
            bolide_list_push(list, 1);
            bolide_list_push(list, 0);
            bolide_list_push(list, 1);
            bolide_list_reverse(list);
            assert_eq!(bolide_list_get(list, 0), 1);
            assert_eq!(bolide_list_get(list, 1), 0);
            assert_eq!(bolide_list_get(list, 2), 1);
            bolide_list_release(list);
        }
    }

    #[test]
    fn test_list_slice() {
        let list = BolideList::new(ElementType::Int);
        unsafe {
            bolide_list_push(list, 10);
            bolide_list_push(list, 20);
            bolide_list_push(list, 30);
            let sliced = bolide_list_slice(list, 0, 2);
            assert_eq!((*sliced).len(), 2);
            assert_eq!(bolide_list_get(sliced, 0), 10);
            assert_eq!(bolide_list_get(sliced, 1), 20);
            bolide_list_release(list);
            bolide_list_release(sliced);
        }
    }
}
