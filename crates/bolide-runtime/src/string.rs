//! Bolide String type with reference counting
//!
//! BolideString 使用引用计数管理内存：
//! - 创建时 strong_count = 1
//! - clone 时 strong_count += 1（浅拷贝）
//! - drop 时 strong_count -= 1，归零时释放

use std::alloc::{alloc, dealloc, Layout};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_char;

thread_local! {
    // String interner for literals (stores raw pointers with Strong RC=1 owned by interner)
    static STRING_LITERALS: RefCell<HashMap<String, *mut BolideString>> = RefCell::new(HashMap::new());
}

use crate::rc::{RcHeader, TypeTag};

/// Bolide 字符串类型（带引用计数）
///
/// 内存布局:
/// ```text
/// +------------------+
/// | RcHeader (16B)   |  引用计数头
/// +------------------+
/// | len: usize       |  字符串长度
/// +------------------+
/// | bytes[len + 1]   |  UTF-8 数据和末尾 NUL
/// +------------------+
/// ```
#[repr(C)]
pub struct BolideString {
    header: RcHeader,
    len: usize,
}

impl BolideString {
    #[inline]
    fn layout_for_len(len: usize) -> Layout {
        let size = std::mem::size_of::<Self>()
            .checked_add(len)
            .and_then(|n| n.checked_add(1))
            .expect("BolideString allocation size overflow");
        Layout::from_size_align(size, std::mem::align_of::<Self>()).unwrap()
    }

    #[inline]
    fn data_ptr(&self) -> *mut c_char {
        unsafe { (self as *const Self as *mut u8).add(std::mem::size_of::<Self>()) as *mut c_char }
    }

    fn from_parts(first: &str, second: Option<&str>) -> *mut Self {
        let second_len = second.map_or(0, str::len);
        let len = first.len() + second_len;
        let layout = Self::layout_for_len(len);

        unsafe {
            let ptr = alloc(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }

            let string = ptr as *mut Self;
            std::ptr::write(
                string,
                Self {
                    header: RcHeader::new(TypeTag::String),
                    len,
                },
            );

            let data = (*string).data_ptr() as *mut u8;
            std::ptr::copy_nonoverlapping(first.as_ptr(), data, first.len());
            if let Some(second) = second {
                std::ptr::copy_nonoverlapping(second.as_ptr(), data.add(first.len()), second.len());
            }
            *data.add(len) = 0;

            string
        }
    }

    /// 创建新字符串（strong_count = 1）
    ///
    /// 内容必须是合法 UTF-8（&str 保证），as_str 据此免校验
    pub fn new(s: &str) -> *mut Self {
        Self::from_parts(s, None)
    }

    /// 拼接两个字符串为新字符串（单次分配，无中间拷贝）
    pub fn concat(a: &str, b: &str) -> *mut Self {
        Self::from_parts(a, Some(b))
    }

    /// 拼接多个字符串为新字符串（单次分配，无中间拷贝）
    unsafe fn concat_ptrs(parts: *const *const Self, count: usize) -> *mut Self {
        if parts.is_null() || count == 0 {
            return Self::new("");
        }

        let parts = std::slice::from_raw_parts(parts, count);
        let mut len = 0usize;
        for &part in parts {
            if !part.is_null() {
                len = len
                    .checked_add((*part).len)
                    .expect("BolideString concatenation size overflow");
            }
        }

        let layout = Self::layout_for_len(len);
        let ptr = alloc(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        let string = ptr as *mut Self;
        std::ptr::write(
            string,
            Self {
                header: RcHeader::new(TypeTag::String),
                len,
            },
        );

        let data = (*string).data_ptr() as *mut u8;
        let mut offset = 0usize;
        for &part in parts {
            if part.is_null() {
                continue;
            }
            let part_ref = &*part;
            let part_len = part_ref.len;
            std::ptr::copy_nonoverlapping(
                part_ref.data_ptr() as *const u8,
                data.add(offset),
                part_len,
            );
            offset += part_len;
        }
        *data.add(len) = 0;

        string
    }

    /// 获取字符串内容
    ///
    /// O(1)：直接用 len 字段构造切片。内容在创建时已保证是合法 UTF-8，
    /// 无需每次重新 strlen + 校验
    #[inline]
    pub fn as_str(&self) -> &str {
        unsafe {
            let bytes = std::slice::from_raw_parts(self.data_ptr() as *const u8, self.len);
            std::str::from_utf8_unchecked(bytes)
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    // ==================== RC 操作 ====================

    /// 增加引用计数
    #[inline]
    pub fn retain(&self) {
        self.header.inc_strong();
    }

    /// 减少引用计数，返回是否应该释放
    #[inline]
    pub fn release(&self) -> bool {
        self.header.dec_strong()
    }

    /// 获取引用计数
    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.header.strong_count()
    }

    /// 检查是否已被 move
    #[inline]
    pub fn is_moved(&self) -> bool {
        self.header.is_moved()
    }

    /// 标记为已 move
    #[inline]
    pub fn mark_moved(&self) {
        self.header.mark_moved();
    }

    unsafe fn dealloc(ptr: *mut Self) {
        let layout = Self::layout_for_len((*ptr).len);
        std::ptr::drop_in_place(ptr);
        dealloc(ptr as *mut u8, layout);
    }
}

// ==================== FFI 导出 ====================

/// 创建新字符串
#[no_mangle]
pub extern "C" fn bolide_string_new(s: *const c_char) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let c_str = unsafe { CStr::from_ptr(s) };
    BolideString::new(c_str.to_str().unwrap_or(""))
}

/// 从切片创建字符串
#[no_mangle]
pub extern "C" fn bolide_string_from_slice(s: *const i8, len: usize) -> *mut BolideString {
    let slice = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    let s = std::str::from_utf8(slice).unwrap_or("");
    BolideString::new(s)
}

/// 获取字符串字面量（带 Interning）
#[no_mangle]
pub extern "C" fn bolide_string_literal(s: *const i8, len: usize) -> *mut BolideString {
    let slice = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    let s_str = std::str::from_utf8(slice).unwrap_or("");

    STRING_LITERALS.with(|interner| {
        let mut map = interner.borrow_mut();
        if let Some(&ptr) = map.get(s_str) {
            // Found. Retain and return a NEW reference.
            unsafe {
                (*ptr).retain();
            }
            ptr
        } else {
            // Not found. Create (RC=1).
            let ptr = BolideString::new(s_str);
            // Interner keeps the original RC=1.
            // We retain to give caller their own reference (RC=2).
            unsafe {
                (*ptr).retain();
            }
            map.insert(s_str.to_string(), ptr);
            ptr
        }
    })
}

/// 增加引用计数（浅拷贝）
#[no_mangle]
pub extern "C" fn bolide_string_retain(s: *mut BolideString) -> *mut BolideString {
    if s.is_null() {
        return s;
    }
    unsafe {
        (*s).retain();
    }
    s
}

/// 减少引用计数，归零时释放
#[no_mangle]
pub extern "C" fn bolide_string_release(s: *mut BolideString) {
    if s.is_null() {
        return;
    }
    unsafe {
        if (*s).release() {
            BolideString::dealloc(s);
        }
    }
}

/// 深拷贝字符串（创建新对象，ref_count = 1）
#[no_mangle]
pub extern "C" fn bolide_string_clone(s: *const BolideString) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let s = unsafe { &*s };
    BolideString::new(s.as_str())
}

/// 释放字符串（兼容旧 API，等同于 release）
#[no_mangle]
pub extern "C" fn bolide_string_free(s: *mut BolideString) {
    bolide_string_release(s);
}

/// 获取字符串长度
#[no_mangle]
pub extern "C" fn bolide_string_len(s: *const BolideString) -> usize {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).len() }
}

/// 获取引用计数
#[no_mangle]
pub extern "C" fn bolide_string_ref_count(s: *const BolideString) -> u32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).ref_count() }
}

/// 字符串拼接（返回新字符串，ref_count = 1）
#[no_mangle]
pub extern "C" fn bolide_string_concat(
    a: *const BolideString,
    b: *const BolideString,
) -> *mut BolideString {
    let a_str = if a.is_null() {
        ""
    } else {
        unsafe { (*a).as_str() }
    };
    let b_str = if b.is_null() {
        ""
    } else {
        unsafe { (*b).as_str() }
    };
    BolideString::concat(a_str, b_str)
}

/// 多段字符串拼接（parts 为 BolideString* 数组，返回新字符串，ref_count = 1）
#[no_mangle]
pub extern "C" fn bolide_string_concat_many(
    parts: *const *const BolideString,
    count: usize,
) -> *mut BolideString {
    unsafe { BolideString::concat_ptrs(parts, count) }
}


/// 对已转为字符串的值应用 Python 风格的格式说明符。
/// 格式说明语法: [[fill]align][0][width][.precision]
///  - fill:   任意填充字符（需后跟 align）
///  - align:  < (左对齐) | > (右对齐) | ^ (居中) | = (数字符号后填充)
///  - 0:     零填充标志（等价于 fill='0', align='>'）
///  - width: 最小字段宽度
///  - .precision: 字符串最大截断长度
/// 省略时默认: fill=' ', align='<', 无宽度限制, 无截断。
fn apply_format_spec(value: &str, spec: &str) -> String {
    if spec.is_empty() {
        return value.to_string();
    }

    let s = spec;
    let len = s.len();
    let bytes = s.as_bytes();
    let mut pos = 0usize;

    let mut fill = ' ';
    let mut align = '<'; // 字符串默认左对齐
    let mut width = 0usize;
    let mut precision: Option<usize> = None;

    // 1. 解析 [[fill]align] — 两字符 fill+align 或单字符 align
    if pos < len {
        if pos + 1 < len && matches!(bytes[pos + 1], b'<' | b'>' | b'^' | b'=') {
            fill = bytes[pos] as char;
            align = bytes[pos + 1] as char;
            pos += 2;
        } else if matches!(bytes[pos], b'<' | b'>' | b'^' | b'=') {
            align = bytes[pos] as char;
            pos += 1;
        }
    }

    // 2. 解析 0 标志（零填充，等价 fill='0', align='>'）
    let had_explicit_align = pos > 0;
    if pos < len && bytes[pos] == b'0' && !had_explicit_align {
        fill = '0';
        align = '>';
        pos += 1;
    }

    // 3. 解析 width（数字）
    while pos < len && bytes[pos].is_ascii_digit() {
        width = width * 10 + (bytes[pos] - b'0') as usize;
        pos += 1;
    }

    // 4. 解析 .precision
    if pos < len && bytes[pos] == b'.' {
        pos += 1;
        let mut prec = 0usize;
        while pos < len && bytes[pos].is_ascii_digit() {
            prec = prec * 10 + (bytes[pos] - b'0') as usize;
            pos += 1;
        }
        precision = Some(prec);
    }

    // 应用格式
    let display: String = if let Some(n) = precision {
        value.chars().take(n).collect()
    } else {
        value.to_string()
    };

    if display.len() >= width {
        return display;
    }

    let padding = width - display.len();
    let mut result = String::with_capacity(width);
    let fill_str: String = std::iter::repeat(fill).take(padding).collect();

    match align {
        '<' => {
            result.push_str(&display);
            result.push_str(&fill_str);
        }
        '>' | '=' => {
            result.push_str(&fill_str);
            result.push_str(&display);
        }
        '^' => {
            let left = padding / 2;
            let right = padding - left;
            for _ in 0..left { result.push(fill); }
            result.push_str(&display);
            for _ in 0..right { result.push(fill); }
        }
        _ => {
            result.push_str(&display);
        }
    }

    result
}

/// 格式化字符串。支持 Python 风格格式说明符。
/// `{}` / `{:spec}` 消耗位置参数，`{name}` / `{name:spec}` 使用命名参数，
/// `{{` 和 `}}` 输出字面量花括号。
/// 格式说明语法: [[fill]align][0][width][.precision]
///   例: {:.1} 截断; {:>10} 右对齐; {:0>5} 零填充; {name:^20} 居中
#[no_mangle]
pub extern "C" fn bolide_string_format(
    template: *const BolideString,
    pos_args: *const *const BolideString,
    pos_count: i64,
    names: *const *const BolideString,
    named_args: *const *const BolideString,
    named_count: i64,
) -> *mut BolideString {
    let src = if template.is_null() {
        ""
    } else {
        unsafe { (*template).as_str() }
    };
    let positional: &[*const BolideString] = if pos_args.is_null() || pos_count <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pos_args, pos_count as usize) }
    };
    let named_names: &[*const BolideString] = if names.is_null() || named_count <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(names, named_count as usize) }
    };
    let named_values: &[*const BolideString] = if named_args.is_null() || named_count <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(named_args, named_count as usize) }
    };

    let mut out = String::with_capacity(src.len());
    let mut arg_index = 0usize;
    let mut chars = src.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                out.push('{');
            } else if chars.peek() == Some(&'}') {
                // 空占位符 {} — 位置参数，无格式说明
                chars.next();
                if let Some(&value) = positional.get(arg_index) {
                    if !value.is_null() {
                        out.push_str(unsafe { (*value).as_str() });
                    }
                    arg_index += 1;
                } else {
                    out.push_str("{}");
                }
            } else {
                // 有内容的占位符: 名字/索引 或 格式说明
                let mut field = String::new();
                while let Some(&next) = chars.peek() {
                    if next == '}' || next == ':' {
                        break;
                    }
                    field.push(next);
                    chars.next();
                }

                // 解析格式说明（: 后面的部分）
                let mut format_spec = String::new();
                if chars.peek() == Some(&':') {
                    chars.next(); // skip ':'
                    while let Some(&next) = chars.peek() {
                        if next == '}' {
                            break;
                        }
                        format_spec.push(next);
                        chars.next();
                    }
                }

                if chars.peek() == Some(&'}') {
                    chars.next(); // skip '}'
                }

                if field.is_empty() {
                    // 纯格式说明 {:spec} — 位置参数
                    if let Some(&value) = positional.get(arg_index) {
                        if !value.is_null() {
                            let raw = unsafe { (*value).as_str() };
                            out.push_str(&apply_format_spec(raw, &format_spec));
                        }
                        arg_index += 1;
                    } else {
                        out.push('{');
                        if !format_spec.is_empty() {
                            out.push(':');
                            out.push_str(&format_spec);
                        }
                        out.push('}');
                    }
                } else {
                    // 命名占位符 {name} 或 {name:spec}
                    let mut replaced = false;
                    for (i, &key) in named_names.iter().enumerate() {
                        if key.is_null() {
                            continue;
                        }
                        if unsafe { (*key).as_str() } == field {
                            if let Some(&value) = named_values.get(i) {
                                if !value.is_null() {
                                    let raw = unsafe { (*value).as_str() };
                                    out.push_str(&apply_format_spec(raw, &format_spec));
                                }
                            }
                            replaced = true;
                            break;
                        }
                    }
                    if !replaced {
                        out.push('{');
                        out.push_str(&field);
                        if !format_spec.is_empty() {
                            out.push(':');
                            out.push_str(&format_spec);
                        }
                        out.push('}');
                    }
                }
            }
        } else if ch == '}' {
            if chars.peek() == Some(&'}') {
                chars.next();
                out.push('}');
            } else {
                out.push(ch);
            }
        } else {
            out.push(ch);
        }
    }
    BolideString::new(&out)
}

/// 字符串比较
#[no_mangle]
pub extern "C" fn bolide_string_eq(a: *const BolideString, b: *const BolideString) -> i64 {
    if a.is_null() && b.is_null() {
        return 1;
    }
    if a.is_null() || b.is_null() {
        return 0;
    }
    if a == b {
        return 1;
    }
    let a = unsafe { &*a };
    let b = unsafe { &*b };
    // 先比长度，再按字节比较（slice eq 内部即 memcmp）
    if a.len != b.len {
        return 0;
    }
    if a.as_str().as_bytes() == b.as_str().as_bytes() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn bolide_string_compare(a: *const BolideString, b: *const BolideString) -> i64 {
    let a = if a.is_null() {
        ""
    } else {
        unsafe { (*a).as_str() }
    };
    let b = if b.is_null() {
        ""
    } else {
        unsafe { (*b).as_str() }
    };
    match a.cmp(b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// 检查是否已被 move
#[no_mangle]
pub extern "C" fn bolide_string_is_moved(s: *const BolideString) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe {
        if (*s).is_moved() {
            1
        } else {
            0
        }
    }
}

/// 标记为已 move（spawn 使用）
#[no_mangle]
pub extern "C" fn bolide_string_mark_moved(s: *mut BolideString) {
    if !s.is_null() {
        unsafe {
            (*s).mark_moved();
        }
    }
}

// ==================== 类型转换 ====================

// --- 转为字符串 ---

#[no_mangle]
pub extern "C" fn bolide_string_from_int(value: i64) -> *mut BolideString {
    BolideString::new(&value.to_string())
}

#[no_mangle]
pub extern "C" fn bolide_string_from_float(value: f64) -> *mut BolideString {
    BolideString::new(&value.to_string())
}

#[no_mangle]
pub extern "C" fn bolide_string_from_bool(value: i64) -> *mut BolideString {
    let s = if value != 0 { "true" } else { "false" };
    BolideString::new(s)
}

/// bigint 转字符串
#[no_mangle]
pub extern "C" fn bolide_string_from_bigint(ptr: *const crate::BolideBigInt) -> *mut BolideString {
    if ptr.is_null() {
        return BolideString::new("0");
    }
    let bigint = unsafe { &*ptr };
    BolideString::new(&bigint.to_string())
}

/// decimal 转字符串
#[no_mangle]
pub extern "C" fn bolide_string_from_decimal(
    ptr: *const crate::BolideDecimal,
) -> *mut BolideString {
    if ptr.is_null() {
        return BolideString::new("0");
    }
    let decimal = unsafe { &*ptr };
    BolideString::new(&decimal.to_string())
}

// --- 从字符串转换 ---

/// 字符串转 int
#[no_mangle]
pub extern "C" fn bolide_string_to_int(s: *const BolideString) -> i64 {
    if s.is_null() {
        return 0;
    }
    let str_val = unsafe { (*s).as_str() };
    str_val.trim().parse::<i64>().unwrap_or(0)
}

/// 字符串转 float
#[no_mangle]
pub extern "C" fn bolide_string_to_float(s: *const BolideString) -> f64 {
    if s.is_null() {
        return 0.0;
    }
    let str_val = unsafe { (*s).as_str() };
    str_val.trim().parse::<f64>().unwrap_or(0.0)
}

/// 从 Rust String 创建 BolideString（内部使用）
pub fn bolide_string_from_rust(s: &str) -> *mut BolideString {
    BolideString::new(s)
}

// ==================== 切片 / 索引（按 Unicode 码点） ====================

/// 把可能为负的码点下标归一化到 [0, char_len]，正向截断到 char_len。
#[inline]
fn norm_index(idx: i64, char_len: i64) -> i64 {
    if idx < 0 {
        (char_len + idx).max(0)
    } else {
        idx.min(char_len)
    }
}

/// 字符串切片 s[start:end:step]，按 Unicode 码点索引。
///
/// flags: bit0 = 提供了 start，bit1 = 提供了 end。step 恒由调用方传入（缺省 1）。
/// 遵循 Python 语义：step<0 时默认从尾到头。结果为新串（RC=1）。
#[no_mangle]
pub extern "C" fn bolide_string_slice(
    s: *const BolideString,
    start: i64,
    end: i64,
    step: i64,
    flags: i64,
) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let src = unsafe { (*s).as_str() };
    let chars: Vec<char> = src.chars().collect();
    let char_len = chars.len() as i64;

    let step = if step == 0 { 1 } else { step };
    let has_start = flags & 1 != 0;
    let has_end = flags & 2 != 0;

    let mut out = String::new();
    if step > 0 {
        let begin = if has_start {
            norm_index(start, char_len)
        } else {
            0
        };
        let stop = if has_end {
            norm_index(end, char_len)
        } else {
            char_len
        };
        let mut i = begin;
        while i < stop {
            out.push(chars[i as usize]);
            i += step;
        }
    } else {
        // 负步长：默认从 char_len-1 走到 -1（含 0）
        let begin = if has_start {
            // 负步长起点截断到 char_len-1
            let b = if start < 0 { char_len + start } else { start };
            b.min(char_len - 1)
        } else {
            char_len - 1
        };
        let stop = if has_end {
            let e = if end < 0 { char_len + end } else { end };
            e.max(-1)
        } else {
            -1
        };
        let mut i = begin;
        while i > stop {
            if i >= 0 && i < char_len {
                out.push(chars[i as usize]);
            }
            i += step;
        }
    }
    BolideString::new(&out)
}

/// 字符串索引 s[i]，按码点；返回单码点新串。越界返回空串。
#[no_mangle]
pub extern "C" fn bolide_string_char_at(s: *const BolideString, idx: i64) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let src = unsafe { (*s).as_str() };
    let chars: Vec<char> = src.chars().collect();
    let char_len = chars.len() as i64;
    let i = if idx < 0 { char_len + idx } else { idx };
    if i < 0 || i >= char_len {
        return BolideString::new("");
    }
    let mut out = String::new();
    out.push(chars[i as usize]);
    BolideString::new(&out)
}

// ==================== 完整常用字符串方法 ====================

/// 转大写（新串，RC=1）
#[no_mangle]
pub extern "C" fn bolide_string_upper(s: *const BolideString) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let src = unsafe { (*s).as_str() };
    BolideString::new(&src.to_uppercase())
}

/// 转小写（新串，RC=1）
#[no_mangle]
pub extern "C" fn bolide_string_lower(s: *const BolideString) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let src = unsafe { (*s).as_str() };
    BolideString::new(&src.to_lowercase())
}

/// 去首尾空白（新串，RC=1）
#[no_mangle]
pub extern "C" fn bolide_string_trim(s: *const BolideString) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let src = unsafe { (*s).as_str() };
    BolideString::new(src.trim())
}

/// 替换所有 old 为 new（新串，RC=1）
#[no_mangle]
pub extern "C" fn bolide_string_replace(
    s: *const BolideString,
    old: *const BolideString,
    new: *const BolideString,
) -> *mut BolideString {
    if s.is_null() {
        return BolideString::new("");
    }
    let src = unsafe { (*s).as_str() };
    let old_str = if old.is_null() {
        ""
    } else {
        unsafe { (*old).as_str() }
    };
    let new_str = if new.is_null() {
        ""
    } else {
        unsafe { (*new).as_str() }
    };
    if old_str.is_empty() {
        return BolideString::new(src);
    }
    BolideString::new(&src.replace(old_str, new_str))
}

/// 重复 n 次（新串，RC=1）
#[no_mangle]
pub extern "C" fn bolide_string_repeat(s: *const BolideString, n: i64) -> *mut BolideString {
    if s.is_null() || n <= 0 {
        return BolideString::new("");
    }
    let src = unsafe { (*s).as_str() };
    BolideString::new(&src.repeat(n as usize))
}

/// 子串首次出现的码点下标，无则 -1
#[no_mangle]
pub extern "C" fn bolide_string_find(s: *const BolideString, sub: *const BolideString) -> i64 {
    if s.is_null() {
        return -1;
    }
    let src = unsafe { (*s).as_str() };
    let sub_str = if sub.is_null() {
        ""
    } else {
        unsafe { (*sub).as_str() }
    };
    match src.find(sub_str) {
        Some(byte_off) => src[..byte_off].chars().count() as i64,
        None => -1,
    }
}

/// 是否包含子串
#[no_mangle]
pub extern "C" fn bolide_string_contains(s: *const BolideString, sub: *const BolideString) -> i64 {
    if s.is_null() {
        return 0;
    }
    let src = unsafe { (*s).as_str() };
    let sub_str = if sub.is_null() {
        ""
    } else {
        unsafe { (*sub).as_str() }
    };
    if src.contains(sub_str) {
        1
    } else {
        0
    }
}

/// 是否以前缀开头
#[no_mangle]
pub extern "C" fn bolide_string_starts_with(
    s: *const BolideString,
    pre: *const BolideString,
) -> i64 {
    if s.is_null() {
        return 0;
    }
    let src = unsafe { (*s).as_str() };
    let pre_str = if pre.is_null() {
        ""
    } else {
        unsafe { (*pre).as_str() }
    };
    if src.starts_with(pre_str) {
        1
    } else {
        0
    }
}

/// 是否以后缀结尾
#[no_mangle]
pub extern "C" fn bolide_string_ends_with(s: *const BolideString, suf: *const BolideString) -> i64 {
    if s.is_null() {
        return 0;
    }
    let src = unsafe { (*s).as_str() };
    let suf_str = if suf.is_null() {
        ""
    } else {
        unsafe { (*suf).as_str() }
    };
    if src.ends_with(suf_str) {
        1
    } else {
        0
    }
}

/// 不重叠出现次数
#[no_mangle]
pub extern "C" fn bolide_string_count(s: *const BolideString, sub: *const BolideString) -> i64 {
    if s.is_null() {
        return 0;
    }
    let src = unsafe { (*s).as_str() };
    let sub_str = if sub.is_null() {
        ""
    } else {
        unsafe { (*sub).as_str() }
    };
    if sub_str.is_empty() {
        return 0;
    }
    src.matches(sub_str).count() as i64
}

/// 按分隔符拆分，返回 list<str>。sep 为空串时按单字符拆。
#[no_mangle]
pub extern "C" fn bolide_string_split(
    s: *const BolideString,
    sep: *const BolideString,
) -> *mut crate::list::BolideList {
    use crate::list::{BolideList, ElementType};
    let result = BolideList::new(ElementType::String);
    if s.is_null() {
        return result;
    }
    let src = unsafe { (*s).as_str() };
    let sep_str = if sep.is_null() {
        ""
    } else {
        unsafe { (*sep).as_str() }
    };

    let dst = unsafe { &mut *result };
    if sep_str.is_empty() {
        // 空分隔符：按单码点拆
        for ch in src.chars() {
            let mut tmp = String::new();
            tmp.push(ch);
            let part = BolideString::new(&tmp);
            // push 内部会 retain，这里把本地 RC=1 释放，避免泄漏
            dst.push(part as i64);
            bolide_string_release(part);
        }
    } else {
        for piece in src.split(sep_str) {
            let part = BolideString::new(piece);
            dst.push(part as i64);
            bolide_string_release(part);
        }
    }
    result
}

/// 获取 BolideString 的 C 字符串指针（用于 FFI）
#[no_mangle]
pub extern "C" fn bolide_string_as_cstr(s: *const BolideString) -> *const c_char {
    if s.is_null() {
        return std::ptr::null();
    }
    unsafe { (*s).data_ptr() }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_new() {
        let s = BolideString::new("hello");
        unsafe {
            assert_eq!((*s).as_str(), "hello");
            assert_eq!((*s).ref_count(), 1);
            bolide_string_release(s);
        }
    }

    #[test]
    fn test_string_layout_size() {
        assert_eq!(std::mem::size_of::<BolideString>(), 24);
    }

    #[test]
    fn test_string_retain_release() {
        let s = BolideString::new("test");
        unsafe {
            assert_eq!((*s).ref_count(), 1);

            bolide_string_retain(s);
            assert_eq!((*s).ref_count(), 2);

            bolide_string_retain(s);
            assert_eq!((*s).ref_count(), 3);

            bolide_string_release(s);
            assert_eq!((*s).ref_count(), 2);

            bolide_string_release(s);
            assert_eq!((*s).ref_count(), 1);

            bolide_string_release(s);
            // s 已被释放，不能再访问
        }
    }

    #[test]
    fn test_string_concat() {
        let a = BolideString::new("hello ");
        let b = BolideString::new("world");
        let c = bolide_string_concat(a, b);
        unsafe {
            assert_eq!((*c).as_str(), "hello world");
            assert_eq!((*c).ref_count(), 1);

            bolide_string_release(a);
            bolide_string_release(b);
            bolide_string_release(c);
        }
    }

    #[test]
    fn test_string_concat_many() {
        let a = BolideString::new("hello");
        let b = BolideString::new(" ");
        let c = BolideString::new("world");
        let parts = [
            a as *const BolideString,
            b as *const BolideString,
            c as *const BolideString,
        ];
        let out = bolide_string_concat_many(parts.as_ptr(), parts.len());
        unsafe {
            assert_eq!((*out).as_str(), "hello world");

            bolide_string_release(a);
            bolide_string_release(b);
            bolide_string_release(c);
            bolide_string_release(out);
        }
    }

    #[test]
    fn test_string_move_flag() {
        let s = BolideString::new("movable");
        unsafe {
            assert!(!(*s).is_moved());
            (*s).mark_moved();
            assert!((*s).is_moved());
            bolide_string_release(s);
        }
    }
}
