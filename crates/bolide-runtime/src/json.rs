//! Bolide JSON runtime.
//!
//! 提供完整的 JSON 解析与序列化能力，结果以 `BolideDynamic` 树承载：
//! - 对象 → `dict<str, dynamic>`（字符串键）
//! - 数组 → `list<dynamic>`
//! - 数字 → 整数走 `int`，含小数/指数走 `float`
//! - 字符串 / true / false / null → 对应标量
//!
//! 这些函数通过 `std/json/json.bl` 里的 `extern "bolide"` 暴露给用户：
//! `parse(text) -> dynamic`、`stringify(value)`、`get_path(value, "a.b.0")` 等。
//!
//! 内存约定与其它运行时模块一致：返回新分配的对象时引用计数为 1，
//! 入参按借用处理（不释放）。容器的 `push`/`set` 会自增元素引用，
//! 因此本模块在交出所有权后会释放本地持有的那一份引用。

use std::cell::RefCell;

use crate::dict::BolideDict;
use crate::dynamic::{BolideDynamic, DynamicType};
use crate::list::{BolideList, ElementType};
use crate::string::BolideString;

/// 防御性递归深度上限，避免恶意深嵌套导致栈溢出。
const MAX_DEPTH: usize = 256;

thread_local! {
    /// 最近一次 `bolide_json_parse` 的错误信息（成功时为空串）。
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_error(msg: String) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg);
}

fn clear_error() {
    LAST_ERROR.with(|e| e.borrow_mut().clear());
}

// ==================== 解析器 ====================

struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser {
            b: s.as_bytes(),
            pos: 0,
            depth: 0,
        }
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    #[inline]
    fn bump(&mut self) -> Option<u8> {
        let c = self.b.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    /// 解析一个 JSON 值，成功返回引用计数为 1 的 `*mut BolideDynamic`。
    fn parse_value(&mut self) -> Result<*mut BolideDynamic, String> {
        if self.depth > MAX_DEPTH {
            return Err("maximum nesting depth exceeded".to_string());
        }
        self.skip_ws();
        match self.peek() {
            None => Err("unexpected end of input".to_string()),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => {
                let s = self.parse_string()?;
                Ok(BolideDynamic::from_string(BolideString::new(&s)))
            }
            Some(b't') | Some(b'f') => self.parse_bool(),
            Some(b'n') => self.parse_null(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!(
                "unexpected character '{}' at position {}",
                c as char, self.pos
            )),
        }
    }

    fn parse_object(&mut self) -> Result<*mut BolideDynamic, String> {
        self.depth += 1;
        self.pos += 1; // 消费 '{'
        let dict = BolideDict::new(ElementType::String, ElementType::Dynamic);
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(BolideDynamic::from_dict(dict));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                unsafe { crate::bolide_dict_release(dict) };
                return Err(format!("expected string key at position {}", self.pos));
            }
            let key = match self.parse_string() {
                Ok(k) => k,
                Err(e) => {
                    unsafe { crate::bolide_dict_release(dict) };
                    return Err(e);
                }
            };
            self.skip_ws();
            if self.bump() != Some(b':') {
                unsafe { crate::bolide_dict_release(dict) };
                return Err(format!("expected ':' after key at position {}", self.pos));
            }
            let val = match self.parse_value() {
                Ok(v) => v,
                Err(e) => {
                    unsafe { crate::bolide_dict_release(dict) };
                    return Err(e);
                }
            };
            let kstr = BolideString::new(&key);
            unsafe {
                // set 会 retain 键与值；随后释放本地的那一份所有权。
                (*dict).set(kstr as i64, val as i64);
                crate::bolide_string_release(kstr);
                crate::bolide_dynamic_release(val);
            }
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => break,
                _ => {
                    unsafe { crate::bolide_dict_release(dict) };
                    return Err(format!("expected ',' or '}}' at position {}", self.pos));
                }
            }
        }
        self.depth -= 1;
        Ok(BolideDynamic::from_dict(dict))
    }

    fn parse_array(&mut self) -> Result<*mut BolideDynamic, String> {
        self.depth += 1;
        self.pos += 1; // 消费 '['
        let list = BolideList::new(ElementType::Dynamic);
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(BolideDynamic::from_list(list));
        }
        loop {
            let val = match self.parse_value() {
                Ok(v) => v,
                Err(e) => {
                    unsafe { crate::bolide_list_release(list) };
                    return Err(e);
                }
            };
            unsafe {
                // push 会 retain 元素；随后释放本地的那一份所有权。
                (*list).push(val as i64);
                crate::bolide_dynamic_release(val);
            }
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => break,
                _ => {
                    unsafe { crate::bolide_list_release(list) };
                    return Err(format!("expected ',' or ']' at position {}", self.pos));
                }
            }
        }
        self.depth -= 1;
        Ok(BolideDynamic::from_list(list))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.pos += 1; // 消费起始引号
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.bump() {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => break,
                Some(b'\\') => match self.bump() {
                    Some(b'"') => out.push(b'"'),
                    Some(b'\\') => out.push(b'\\'),
                    Some(b'/') => out.push(b'/'),
                    Some(b'b') => out.push(0x08),
                    Some(b'f') => out.push(0x0C),
                    Some(b'n') => out.push(b'\n'),
                    Some(b'r') => out.push(b'\r'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'u') => {
                        let cp = self.parse_hex4()?;
                        let ch = if (0xD800..=0xDBFF).contains(&cp) {
                            // 高代理项，必须紧跟低代理项 \uXXXX
                            if self.bump() != Some(b'\\') || self.bump() != Some(b'u') {
                                return Err("invalid surrogate pair".to_string());
                            }
                            let lo = self.parse_hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&lo) {
                                return Err("invalid low surrogate".to_string());
                            }
                            let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            char::from_u32(c).ok_or_else(|| "invalid unicode escape".to_string())?
                        } else if (0xDC00..=0xDFFF).contains(&cp) {
                            return Err("unexpected low surrogate".to_string());
                        } else {
                            char::from_u32(cp).ok_or_else(|| "invalid unicode escape".to_string())?
                        };
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    _ => return Err("invalid escape sequence".to_string()),
                },
                Some(c) => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid utf-8 in string".to_string())
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        let mut v: u32 = 0;
        for _ in 0..4 {
            let c = self.bump().ok_or_else(|| "incomplete unicode escape".to_string())?;
            let d = (c as char)
                .to_digit(16)
                .ok_or_else(|| "invalid hex digit in unicode escape".to_string())?;
            v = v * 16 + d;
        }
        Ok(v)
    }

    fn parse_bool(&mut self) -> Result<*mut BolideDynamic, String> {
        if self.b[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Ok(BolideDynamic::from_bool(true))
        } else if self.b[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Ok(BolideDynamic::from_bool(false))
        } else {
            Err(format!("invalid literal at position {}", self.pos))
        }
    }

    fn parse_null(&mut self) -> Result<*mut BolideDynamic, String> {
        if self.b[self.pos..].starts_with(b"null") {
            self.pos += 4;
            Ok(BolideDynamic::none())
        } else {
            Err(format!("invalid literal at position {}", self.pos))
        }
    }

    fn parse_number(&mut self) -> Result<*mut BolideDynamic, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        let slice = std::str::from_utf8(&self.b[start..self.pos])
            .map_err(|_| "invalid number".to_string())?;
        if is_float {
            let f: f64 = slice
                .parse()
                .map_err(|_| format!("invalid number '{}'", slice))?;
            Ok(BolideDynamic::from_float(f))
        } else {
            match slice.parse::<i64>() {
                Ok(i) => Ok(BolideDynamic::from_int(i)),
                Err(_) => {
                    // 超出 i64 范围 → 退化为浮点（仍保留量级）。
                    let f: f64 = slice
                        .parse()
                        .map_err(|_| format!("invalid number '{}'", slice))?;
                    Ok(BolideDynamic::from_float(f))
                }
            }
        }
    }
}

// ==================== FFI: 解析 ====================

/// 解析 JSON 文本为 dynamic。失败时返回 `none`，并把错误写入 `bolide_json_error`。
#[no_mangle]
pub extern "C" fn bolide_json_parse(s: *const BolideString) -> *mut BolideDynamic {
    clear_error();
    if s.is_null() {
        set_error("null input".to_string());
        return BolideDynamic::none();
    }
    let text = unsafe { (*s).as_str() };
    let mut p = Parser::new(text);
    match p.parse_value() {
        Ok(v) => {
            p.skip_ws();
            if p.pos != p.b.len() {
                unsafe { crate::bolide_dynamic_release(v) };
                set_error(format!("trailing characters at position {}", p.pos));
                BolideDynamic::none()
            } else {
                v
            }
        }
        Err(e) => {
            set_error(e);
            BolideDynamic::none()
        }
    }
}

/// 返回最近一次 `bolide_json_parse` 的错误信息（成功时为空串）。
#[no_mangle]
pub extern "C" fn bolide_json_error() -> *mut BolideString {
    LAST_ERROR.with(|e| BolideString::new(&e.borrow()))
}

// ==================== FFI: 序列化 ====================

/// 紧凑序列化。
#[no_mangle]
pub extern "C" fn bolide_json_stringify(d: *const BolideDynamic) -> *mut BolideString {
    let mut out = String::new();
    unsafe { stringify_dynamic(&mut out, d, false, 0, 0, 0) };
    BolideString::new(&out)
}

/// 带缩进的美化序列化（indent 为每级空格数，0..=16）。
#[no_mangle]
pub extern "C" fn bolide_json_stringify_pretty(
    d: *const BolideDynamic,
    indent: i64,
) -> *mut BolideString {
    let ind = indent.clamp(0, 16) as usize;
    let mut out = String::new();
    unsafe { stringify_dynamic(&mut out, d, true, ind, 0, 0) };
    BolideString::new(&out)
}

fn push_float(out: &mut String, f: f64) {
    if f.is_finite() {
        out.push_str(&f.to_string());
    } else {
        // JSON 没有 Infinity / NaN 表示。
        out.push_str("null");
    }
}

fn write_escaped(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn newline_indent(out: &mut String, indent: usize, level: usize) {
    out.push('\n');
    for _ in 0..(indent * level) {
        out.push(' ');
    }
}

unsafe fn stringify_dynamic(
    out: &mut String,
    d: *const BolideDynamic,
    pretty: bool,
    indent: usize,
    level: usize,
    depth: usize,
) {
    if d.is_null() || depth > MAX_DEPTH {
        out.push_str("null");
        return;
    }
    let dy = &*d;
    match dy.tag {
        DynamicType::None => out.push_str("null"),
        DynamicType::Bool => out.push_str(if dy.data.bool_val != 0 { "true" } else { "false" }),
        DynamicType::Int => out.push_str(&dy.data.int_val.to_string()),
        DynamicType::Float => push_float(out, dy.data.float_val),
        DynamicType::BigInt | DynamicType::Decimal => out.push_str(&dy.to_string_repr()),
        DynamicType::String => {
            let sp = dy.data.string_ptr;
            if sp.is_null() {
                out.push_str("\"\"");
            } else {
                write_escaped(out, (*sp).as_str());
            }
        }
        DynamicType::List => stringify_list(out, dy.data.list_ptr, pretty, indent, level, depth),
        DynamicType::Dict => stringify_dict(out, dy.data.dict_ptr, pretty, indent, level, depth),
        DynamicType::Bytes => out.push_str("null"),
        DynamicType::Tuple => out.push_str("null"),
    }
}

unsafe fn stringify_elem(
    out: &mut String,
    et: ElementType,
    raw: i64,
    pretty: bool,
    indent: usize,
    level: usize,
    depth: usize,
) {
    match et {
        ElementType::Dynamic => {
            stringify_dynamic(out, raw as *const BolideDynamic, pretty, indent, level, depth)
        }
        ElementType::Int => out.push_str(&raw.to_string()),
        ElementType::Float => push_float(out, f64::from_bits(raw as u64)),
        ElementType::Bool => out.push_str(if raw != 0 { "true" } else { "false" }),
        ElementType::String => {
            let p = raw as *const BolideString;
            if p.is_null() {
                out.push_str("\"\"");
            } else {
                write_escaped(out, (*p).as_str());
            }
        }
        ElementType::BigInt => {
            let p = raw as *const crate::BolideBigInt;
            if p.is_null() {
                out.push_str("null");
            } else {
                out.push_str(&(*p).to_string());
            }
        }
        ElementType::Decimal => {
            let p = raw as *const crate::BolideDecimal;
            if p.is_null() {
                out.push_str("null");
            } else {
                out.push_str(&(*p).to_string());
            }
        }
        ElementType::List => stringify_list(out, raw as *const BolideList, pretty, indent, level, depth),
        ElementType::Dict => stringify_dict(out, raw as *const BolideDict, pretty, indent, level, depth),
        _ => out.push_str("null"),
    }
}

unsafe fn stringify_list(
    out: &mut String,
    lp: *const BolideList,
    pretty: bool,
    indent: usize,
    level: usize,
    depth: usize,
) {
    if lp.is_null() || depth > MAX_DEPTH {
        out.push_str("[]");
        return;
    }
    let n = (*lp).len();
    if n == 0 {
        out.push_str("[]");
        return;
    }
    let et = (*lp).elem_type();
    out.push('[');
    for i in 0..n {
        if i > 0 {
            out.push(',');
        }
        if pretty {
            newline_indent(out, indent, level + 1);
        }
        match (*lp).get(i) {
            Some(raw) => stringify_elem(out, et, raw, pretty, indent, level + 1, depth + 1),
            None => out.push_str("null"),
        }
    }
    if pretty {
        newline_indent(out, indent, level);
    }
    out.push(']');
}

unsafe fn stringify_dict(
    out: &mut String,
    dp: *const BolideDict,
    pretty: bool,
    indent: usize,
    level: usize,
    depth: usize,
) {
    if dp.is_null() || depth > MAX_DEPTH {
        out.push_str("{}");
        return;
    }
    let keys = (*dp).keys();
    if keys.is_empty() {
        out.push_str("{}");
        return;
    }
    let kt = (*dp).key_type();
    let vt = (*dp).value_type();
    out.push('{');
    let mut first = true;
    for k in keys {
        if !first {
            out.push(',');
        }
        first = false;
        if pretty {
            newline_indent(out, indent, level + 1);
        }
        if kt == ElementType::String {
            let ks = k as *const BolideString;
            if ks.is_null() {
                out.push_str("\"\"");
            } else {
                write_escaped(out, (*ks).as_str());
            }
        } else {
            // 非字符串键：渲染后强制加引号（JSON 键必须是字符串）。
            let mut tmp = String::new();
            stringify_elem(&mut tmp, kt, k, false, 0, 0, depth + 1);
            write_escaped(out, tmp.trim_matches('"'));
        }
        out.push(':');
        if pretty {
            out.push(' ');
        }
        match (*dp).get(k) {
            Some(v) => stringify_elem(out, vt, v, pretty, indent, level + 1, depth + 1),
            None => out.push_str("null"),
        }
    }
    if pretty {
        newline_indent(out, indent, level);
    }
    out.push('}');
}

// ==================== FFI: 导航与自省 ====================

/// 把容器元素（按 ElementType 标记的 raw i64）装箱为 dynamic（引用计数 1）。
unsafe fn elem_to_dynamic(et: ElementType, raw: i64) -> *mut BolideDynamic {
    match et {
        ElementType::Dynamic => {
            let p = raw as *mut BolideDynamic;
            if p.is_null() {
                BolideDynamic::none()
            } else {
                crate::bolide_dynamic_retain(p);
                p
            }
        }
        ElementType::Int => BolideDynamic::from_int(raw),
        ElementType::Float => BolideDynamic::from_float(f64::from_bits(raw as u64)),
        ElementType::Bool => BolideDynamic::from_bool(raw != 0),
        ElementType::String => {
            let p = raw as *mut BolideString;
            if p.is_null() {
                BolideDynamic::from_string(BolideString::new(""))
            } else {
                crate::bolide_string_retain(p);
                BolideDynamic::from_string(p)
            }
        }
        ElementType::BigInt => {
            let p = raw as *mut crate::BolideBigInt;
            if p.is_null() {
                BolideDynamic::none()
            } else {
                crate::bolide_bigint_retain(p);
                BolideDynamic::from_bigint(p)
            }
        }
        ElementType::Decimal => {
            let p = raw as *mut crate::BolideDecimal;
            if p.is_null() {
                BolideDynamic::none()
            } else {
                crate::bolide_decimal_retain(p);
                BolideDynamic::from_decimal(p)
            }
        }
        ElementType::List => {
            let p = raw as *mut BolideList;
            if p.is_null() {
                BolideDynamic::none()
            } else {
                crate::bolide_list_retain(p);
                BolideDynamic::from_list(p)
            }
        }
        ElementType::Dict => {
            let p = raw as *mut BolideDict;
            if p.is_null() {
                BolideDynamic::none()
            } else {
                crate::bolide_dict_retain(p);
                BolideDynamic::from_dict(p)
            }
        }
        ElementType::Bytes => {
            let p = raw as *mut crate::BolideBytes;
            if p.is_null() {
                BolideDynamic::none()
            } else {
                crate::bolide_bytes_retain(p);
                BolideDynamic::from_bytes(p)
            }
        }
        _ => BolideDynamic::none(),
    }
}

/// JSON 语义类型名："null"/"bool"/"number"/"string"/"array"/"object"。
#[no_mangle]
pub extern "C" fn bolide_json_type(d: *const BolideDynamic) -> *mut BolideString {
    let name = if d.is_null() {
        "null"
    } else {
        unsafe {
            match (*d).tag {
                DynamicType::None => "null",
                DynamicType::Bool => "bool",
                DynamicType::Int
                | DynamicType::Float
                | DynamicType::BigInt
                | DynamicType::Decimal => "number",
                DynamicType::String => "string",
                DynamicType::List => "array",
                DynamicType::Dict => "object",
                DynamicType::Bytes => "bytes",
                DynamicType::Tuple => "tuple",
            }
        }
    };
    BolideString::new(name)
}

/// 数组 / 对象 / 字符串的长度，其它类型返回 0。
#[no_mangle]
pub extern "C" fn bolide_json_length(d: *const BolideDynamic) -> i64 {
    if d.is_null() {
        return 0;
    }
    unsafe {
        match (*d).tag {
            DynamicType::List => {
                let lp = (*d).data.list_ptr;
                if lp.is_null() {
                    0
                } else {
                    (*lp).len() as i64
                }
            }
            DynamicType::Dict => {
                let dp = (*d).data.dict_ptr;
                if dp.is_null() {
                    0
                } else {
                    (*dp).len() as i64
                }
            }
            DynamicType::String => {
                let sp = (*d).data.string_ptr;
                if sp.is_null() {
                    0
                } else {
                    (*sp).as_str().chars().count() as i64
                }
            }
            _ => 0,
        }
    }
}

/// 对象按键取值；非对象或键不存在返回 `none`。
#[no_mangle]
pub extern "C" fn bolide_json_get(
    d: *const BolideDynamic,
    key: *const BolideString,
) -> *mut BolideDynamic {
    unsafe {
        if d.is_null() || key.is_null() || (*d).tag != DynamicType::Dict {
            return BolideDynamic::none();
        }
        let dp = (*d).data.dict_ptr;
        if dp.is_null() {
            return BolideDynamic::none();
        }
        match (*dp).get_str((*key).as_str()) {
            Some(raw) => elem_to_dynamic((*dp).value_type(), raw),
            None => BolideDynamic::none(),
        }
    }
}

/// 数组按下标取值；非数组或越界返回 `none`。
#[no_mangle]
pub extern "C" fn bolide_json_at(d: *const BolideDynamic, index: i64) -> *mut BolideDynamic {
    unsafe {
        if d.is_null() || index < 0 || (*d).tag != DynamicType::List {
            return BolideDynamic::none();
        }
        let lp = (*d).data.list_ptr;
        if lp.is_null() {
            return BolideDynamic::none();
        }
        match (*lp).get(index as usize) {
            Some(raw) => elem_to_dynamic((*lp).elem_type(), raw),
            None => BolideDynamic::none(),
        }
    }
}

/// 对象键列表，返回 `list<dynamic>`（每个元素为字符串 dynamic）。非对象返回空列表。
#[no_mangle]
pub extern "C" fn bolide_json_keys(d: *const BolideDynamic) -> *mut BolideList {
    unsafe {
        let out = BolideList::new(ElementType::Dynamic);
        if d.is_null() || (*d).tag != DynamicType::Dict {
            return out;
        }
        let dp = (*d).data.dict_ptr;
        if dp.is_null() {
            return out;
        }
        let kt = (*dp).key_type();
        for k in (*dp).keys() {
            let dv = if kt == ElementType::String {
                let ks = k as *const BolideString;
                let s = if ks.is_null() { "" } else { (*ks).as_str() };
                BolideDynamic::from_string(BolideString::new(s))
            } else {
                elem_to_dynamic(kt, k)
            };
            (*out).push(dv as i64);
            crate::bolide_dynamic_release(dv);
        }
        out
    }
}

/// 按点分路径取值，数组下标用数字段：`"users.0.name"`。任一步缺失返回 `none`。
#[no_mangle]
pub extern "C" fn bolide_json_get_path(
    d: *const BolideDynamic,
    path: *const BolideString,
) -> *mut BolideDynamic {
    unsafe {
        if d.is_null() {
            return BolideDynamic::none();
        }
        let p = if path.is_null() { "" } else { (*path).as_str() };
        if p.is_empty() {
            crate::bolide_dynamic_retain(d as *mut BolideDynamic);
            return d as *mut BolideDynamic;
        }
        let mut cur: *const BolideDynamic = d;
        for seg in p.split('.') {
            if cur.is_null() {
                return BolideDynamic::none();
            }
            match (*cur).tag {
                DynamicType::Dict => {
                    let dp = (*cur).data.dict_ptr;
                    if dp.is_null() {
                        return BolideDynamic::none();
                    }
                    match (*dp).get_str(seg) {
                        Some(raw) => {
                            if (*dp).value_type() == ElementType::Dynamic {
                                cur = raw as *const BolideDynamic;
                            } else {
                                return elem_to_dynamic((*dp).value_type(), raw);
                            }
                        }
                        None => return BolideDynamic::none(),
                    }
                }
                DynamicType::List => {
                    let lp = (*cur).data.list_ptr;
                    if lp.is_null() {
                        return BolideDynamic::none();
                    }
                    match seg.parse::<usize>() {
                        Ok(idx) => match (*lp).get(idx) {
                            Some(raw) => {
                                if (*lp).elem_type() == ElementType::Dynamic {
                                    cur = raw as *const BolideDynamic;
                                } else {
                                    return elem_to_dynamic((*lp).elem_type(), raw);
                                }
                            }
                            None => return BolideDynamic::none(),
                        },
                        Err(_) => return BolideDynamic::none(),
                    }
                }
                _ => return BolideDynamic::none(),
            }
        }
        if cur.is_null() {
            return BolideDynamic::none();
        }
        crate::bolide_dynamic_retain(cur as *mut BolideDynamic);
        cur as *mut BolideDynamic
    }
}

/// 把数组 dynamic 取为 `list<dynamic>`（引用计数 1）；非数组返回空列表。
/// 若内部列表已是 dynamic 元素则直接 retain 返回，否则逐元素装箱。
#[no_mangle]
pub extern "C" fn bolide_dynamic_as_list(d: *const BolideDynamic) -> *mut BolideList {
    unsafe {
        if !d.is_null() && (*d).tag == DynamicType::List {
            let lp = (*d).data.list_ptr;
            if !lp.is_null() {
                let et = (*lp).elem_type();
                if et == ElementType::Dynamic {
                    crate::bolide_list_retain(lp);
                    return lp;
                }
                let out = BolideList::new(ElementType::Dynamic);
                let n = (*lp).len();
                for i in 0..n {
                    if let Some(raw) = (*lp).get(i) {
                        let dv = elem_to_dynamic(et, raw);
                        (*out).push(dv as i64);
                        crate::bolide_dynamic_release(dv);
                    }
                }
                return out;
            }
        }
        BolideList::new(ElementType::Dynamic)
    }
}

/// 把 dynamic 取为布尔（JSON 真值语义）。
#[no_mangle]
pub extern "C" fn bolide_dynamic_as_bool(d: *const BolideDynamic) -> i64 {
    if d.is_null() {
        return 0;
    }
    unsafe { i64::from((*d).is_truthy()) }
}
