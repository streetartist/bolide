//! Small runtime-backed template engine for the Bolide standard library.

use crate::list::ElementType;
use crate::{BolideBytes, BolideDict, BolideDynamic, BolideList, BolideString, DynamicType};
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Var {
        expr: String,
        escaped: bool,
    },
    If {
        cond: String,
        then_nodes: Vec<Node>,
        else_nodes: Vec<Node>,
    },
    For {
        var: String,
        iter: String,
        body: Vec<Node>,
    },
}

#[derive(Clone, Copy)]
enum ValueRef {
    None,
    Dynamic(*const BolideDynamic),
    Dict(*const BolideDict),
    List(*const BolideList),
    Str(*const BolideString),
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(*const BolideBytes),
}

struct RenderState<'a> {
    root: *const BolideDict,
    scopes: Vec<HashMap<String, ValueRef>>,
    _marker: std::marker::PhantomData<&'a ()>,
}

struct CachedTemplate {
    modified: Option<SystemTime>,
    len: u64,
    checked_at: Instant,
    nodes: Arc<Vec<Node>>,
    static_len: usize,
}

fn template_cache() -> &'static Mutex<HashMap<String, CachedTemplate>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedTemplate>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const TEMPLATE_RECHECK_INTERVAL: Duration = Duration::from_secs(1);

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

fn html_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn next_marker(source: &str, pos: usize) -> Option<(usize, &'static str)> {
    let tail = &source[pos..];
    let mut best: Option<(usize, &'static str)> = None;
    for marker in ["{{", "{!!", "{%"] {
        if let Some(rel) = tail.find(marker) {
            let abs = pos + rel;
            if best.map_or(true, |(idx, _)| abs < idx) {
                best = Some((abs, marker));
            }
        }
    }
    best
}

fn parse_nodes(source: &str, pos: &mut usize, stops: &[&str]) -> (Vec<Node>, Option<String>) {
    let mut nodes = Vec::new();
    while *pos < source.len() {
        let Some((idx, marker)) = next_marker(source, *pos) else {
            nodes.push(Node::Text(source[*pos..].to_string()));
            *pos = source.len();
            break;
        };

        if idx > *pos {
            nodes.push(Node::Text(source[*pos..idx].to_string()));
        }

        match marker {
            "{{" => {
                let body_start = idx + 2;
                let Some(close_rel) = source[body_start..].find("}}") else {
                    nodes.push(Node::Text(source[idx..].to_string()));
                    *pos = source.len();
                    break;
                };
                let body_end = body_start + close_rel;
                nodes.push(Node::Var {
                    expr: source[body_start..body_end].trim().to_string(),
                    escaped: true,
                });
                *pos = body_end + 2;
            }
            "{!!" => {
                let body_start = idx + 3;
                let Some(close_rel) = source[body_start..].find("!!}") else {
                    nodes.push(Node::Text(source[idx..].to_string()));
                    *pos = source.len();
                    break;
                };
                let body_end = body_start + close_rel;
                nodes.push(Node::Var {
                    expr: source[body_start..body_end].trim().to_string(),
                    escaped: false,
                });
                *pos = body_end + 3;
            }
            "{%" => {
                let body_start = idx + 2;
                let Some(close_rel) = source[body_start..].find("%}") else {
                    nodes.push(Node::Text(source[idx..].to_string()));
                    *pos = source.len();
                    break;
                };
                let body_end = body_start + close_rel;
                let tag = source[body_start..body_end].trim().to_string();
                *pos = body_end + 2;

                if stops.iter().any(|stop| *stop == tag) {
                    return (nodes, Some(tag));
                }

                if let Some(cond) = tag.strip_prefix("if ") {
                    let (then_nodes, stop) = parse_nodes(source, pos, &["else", "endif"]);
                    let else_nodes = if stop.as_deref() == Some("else") {
                        let (else_nodes, _) = parse_nodes(source, pos, &["endif"]);
                        else_nodes
                    } else {
                        Vec::new()
                    };
                    nodes.push(Node::If {
                        cond: cond.trim().to_string(),
                        then_nodes,
                        else_nodes,
                    });
                } else if let Some(rest) = tag.strip_prefix("for ") {
                    if let Some((var, iter)) = rest.split_once(" in ") {
                        let (body, _) = parse_nodes(source, pos, &["endfor"]);
                        nodes.push(Node::For {
                            var: var.trim().to_string(),
                            iter: iter.trim().to_string(),
                            body,
                        });
                    }
                }
            }
            _ => unreachable!(),
        }
    }
    (nodes, None)
}

fn parse_template(source: &str) -> Vec<Node> {
    let mut pos = 0;
    let (nodes, _) = parse_nodes(source, &mut pos, &[]);
    nodes
}

fn static_len(nodes: &[Node]) -> usize {
    let mut len = 0usize;
    for node in nodes {
        match node {
            Node::Text(text) => len += text.len(),
            Node::If {
                then_nodes,
                else_nodes,
                ..
            } => {
                len += static_len(then_nodes).max(static_len(else_nodes));
            }
            Node::For { body, .. } => {
                len += static_len(body);
            }
            Node::Var { .. } => {}
        }
    }
    len
}

fn value_from_dynamic(value: *const BolideDynamic) -> ValueRef {
    if value.is_null() {
        return ValueRef::None;
    }
    let dyn_value = unsafe { &*value };
    match dyn_value.tag {
        DynamicType::None => ValueRef::None,
        DynamicType::Bool => ValueRef::Bool(unsafe { dyn_value.data.bool_val != 0 }),
        DynamicType::Int => ValueRef::Int(unsafe { dyn_value.data.int_val }),
        DynamicType::Float => ValueRef::Float(unsafe { dyn_value.data.float_val }),
        DynamicType::String => ValueRef::Str(unsafe { dyn_value.data.string_ptr }),
        DynamicType::List => ValueRef::List(unsafe { dyn_value.data.list_ptr }),
        DynamicType::Bytes => ValueRef::Bytes(unsafe { dyn_value.data.bytes_ptr }),
        DynamicType::Dict => ValueRef::Dict(unsafe { dyn_value.data.dict_ptr }),
        DynamicType::BigInt | DynamicType::Decimal => ValueRef::Dynamic(value),
    }
}

fn lookup_in_dict(dict: *const BolideDict, key: &str) -> ValueRef {
    if dict.is_null() {
        return ValueRef::None;
    }

    let dict_ref = unsafe { &*dict };
    let raw = dict_ref.get_str(key);
    let Some(raw) = raw else {
        return ValueRef::None;
    };

    match dict_ref.value_type() {
        ElementType::Int => ValueRef::Int(raw),
        ElementType::Float => ValueRef::Float(f64::from_bits(raw as u64)),
        ElementType::Bool => ValueRef::Bool(raw != 0),
        ElementType::String => ValueRef::Str(raw as *const BolideString),
        ElementType::List => ValueRef::List(raw as *const BolideList),
        ElementType::Dict => ValueRef::Dict(raw as *const BolideDict),
        ElementType::Dynamic => ValueRef::Dynamic(raw as *const BolideDynamic),
        ElementType::Bytes => ValueRef::Bytes(raw as *const BolideBytes),
        ElementType::BigInt
        | ElementType::Decimal
        | ElementType::Ptr
        | ElementType::Closure
        | ElementType::Object => ValueRef::None,
    }
}

fn lookup_path(expr: &str, state: &RenderState<'_>) -> ValueRef {
    let mut parts = expr
        .split('.')
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let Some(first) = parts.next() else {
        return ValueRef::None;
    };

    let mut current = state
        .scopes
        .iter()
        .rev()
        .find_map(|scope| scope.get(first).copied())
        .unwrap_or_else(|| lookup_in_dict(state.root, first));

    for part in parts {
        current = match value_from_dynamic_if_needed(current) {
            ValueRef::Dict(dict) => lookup_in_dict(dict, part),
            ValueRef::List(list) if part == "len" => {
                if list.is_null() {
                    ValueRef::Int(0)
                } else {
                    ValueRef::Int(unsafe { (&*list).len() as i64 })
                }
            }
            ValueRef::Str(s) if part == "len" => {
                if s.is_null() {
                    ValueRef::Int(0)
                } else {
                    ValueRef::Int(unsafe { (&*s).len() as i64 })
                }
            }
            _ => ValueRef::None,
        };
    }

    current
}

fn value_from_dynamic_if_needed(value: ValueRef) -> ValueRef {
    if let ValueRef::Dynamic(ptr) = value {
        value_from_dynamic(ptr)
    } else {
        value
    }
}

fn value_to_string(value: ValueRef) -> String {
    match value_from_dynamic_if_needed(value) {
        ValueRef::None => String::new(),
        ValueRef::Bool(v) => {
            if v {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        ValueRef::Int(v) => v.to_string(),
        ValueRef::Float(v) => v.to_string(),
        ValueRef::Str(s) => {
            if s.is_null() {
                String::new()
            } else {
                unsafe { (&*s).as_str().to_string() }
            }
        }
        ValueRef::Bytes(bytes) => {
            if bytes.is_null() {
                String::new()
            } else {
                let bytes = unsafe { &*bytes };
                String::from_utf8_lossy(bytes.as_slice()).into_owned()
            }
        }
        ValueRef::Dynamic(d) => {
            if d.is_null() {
                String::new()
            } else {
                unsafe { (&*d).to_string_repr() }
            }
        }
        ValueRef::List(_) => "[...]".to_string(),
        ValueRef::Dict(_) => "{...}".to_string(),
    }
}

fn is_truthy(value: ValueRef) -> bool {
    match value_from_dynamic_if_needed(value) {
        ValueRef::None => false,
        ValueRef::Bool(v) => v,
        ValueRef::Int(v) => v != 0,
        ValueRef::Float(v) => v != 0.0,
        ValueRef::Str(s) => !s.is_null() && unsafe { !(&*s).is_empty() },
        ValueRef::Bytes(bytes) => !bytes.is_null() && unsafe { !(&*bytes).as_slice().is_empty() },
        ValueRef::List(list) => !list.is_null() && unsafe { !(&*list).is_empty() },
        ValueRef::Dict(dict) => !dict.is_null() && unsafe { !(&*dict).is_empty() },
        ValueRef::Dynamic(d) => !d.is_null() && unsafe { (&*d).is_truthy() },
    }
}

fn list_items(value: ValueRef) -> Vec<ValueRef> {
    let value = value_from_dynamic_if_needed(value);
    let ValueRef::List(list) = value else {
        return Vec::new();
    };
    if list.is_null() {
        return Vec::new();
    }

    let list_ref = unsafe { &*list };
    let mut items = Vec::with_capacity(list_ref.len());
    for index in 0..list_ref.len() {
        let Some(raw) = list_ref.get(index) else {
            continue;
        };
        let item = match list_ref.elem_type() {
            ElementType::Int => ValueRef::Int(raw),
            ElementType::Float => ValueRef::Float(f64::from_bits(raw as u64)),
            ElementType::Bool => ValueRef::Bool(raw != 0),
            ElementType::String => ValueRef::Str(raw as *const BolideString),
            ElementType::List => ValueRef::List(raw as *const BolideList),
            ElementType::Dict => ValueRef::Dict(raw as *const BolideDict),
            ElementType::Dynamic => ValueRef::Dynamic(raw as *const BolideDynamic),
            ElementType::Bytes => ValueRef::Bytes(raw as *const BolideBytes),
            ElementType::BigInt
            | ElementType::Decimal
            | ElementType::Ptr
            | ElementType::Closure
            | ElementType::Object => ValueRef::None,
        };
        items.push(item);
    }
    items
}

fn render_nodes(nodes: &[Node], state: &mut RenderState<'_>, out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Var { expr, escaped } => {
                let value = value_to_string(lookup_path(expr, state));
                if *escaped {
                    out.push_str(&html_escape(&value));
                } else {
                    out.push_str(&value);
                }
            }
            Node::If {
                cond,
                then_nodes,
                else_nodes,
            } => {
                if is_truthy(lookup_path(cond, state)) {
                    render_nodes(then_nodes, state, out);
                } else {
                    render_nodes(else_nodes, state, out);
                }
            }
            Node::For { var, iter, body } => {
                let items = list_items(lookup_path(iter, state));
                for (index, item) in items.into_iter().enumerate() {
                    let mut scope = HashMap::new();
                    scope.insert(var.clone(), item);
                    scope.insert(format!("{}_index", var), ValueRef::Int(index as i64));
                    scope.insert(format!("{}_first", var), ValueRef::Bool(index == 0));
                    state.scopes.push(scope);
                    render_nodes(body, state, out);
                    state.scopes.pop();
                }
            }
        }
    }
}

fn render_template(source: &str, context: *const BolideDict) -> String {
    let nodes = parse_template(source);
    render_parsed_template(&nodes, static_len(&nodes), context)
}

fn render_parsed_template(nodes: &[Node], static_len: usize, context: *const BolideDict) -> String {
    let mut state = RenderState {
        root: context,
        scopes: Vec::new(),
        _marker: std::marker::PhantomData,
    };
    let mut out = String::with_capacity(static_len.saturating_add(128));
    render_nodes(nodes, &mut state, &mut out);
    out
}

#[no_mangle]
pub extern "C" fn bolide_template_escape_html(value: *const c_char) -> *mut BolideString {
    let Some(value) = cstr_to_str(value) else {
        return BolideString::new("");
    };
    BolideString::new(&html_escape(value))
}

#[no_mangle]
pub extern "C" fn bolide_template_render(
    source: *const c_char,
    context: *const BolideDict,
) -> *mut BolideString {
    let Some(source) = cstr_to_str(source) else {
        return BolideString::new("");
    };
    BolideString::new(&render_template(source, context))
}

#[no_mangle]
pub extern "C" fn bolide_template_render_file(
    path: *const c_char,
    context: *const BolideDict,
) -> *mut BolideString {
    let Some(path) = cstr_to_str(path) else {
        return BolideString::new("");
    };

    let now = Instant::now();
    if let Ok(cache) = template_cache().lock() {
        if let Some(cached) = cache.get(path) {
            if now.duration_since(cached.checked_at) < TEMPLATE_RECHECK_INTERVAL {
                return BolideString::new(&render_parsed_template(
                    &cached.nodes,
                    cached.static_len,
                    context,
                ));
            }
        }
    }

    let metadata = std::fs::metadata(path).ok();
    let modified = metadata.as_ref().and_then(|m| m.modified().ok());
    let len = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

    if let Ok(mut cache) = template_cache().lock() {
        if let Some(cached) = cache.get_mut(path) {
            if cached.modified == modified && cached.len == len {
                cached.checked_at = now;
                return BolideString::new(&render_parsed_template(
                    &cached.nodes,
                    cached.static_len,
                    context,
                ));
            }
        }
    }

    let Ok(source) = std::fs::read_to_string(path) else {
        return BolideString::new("");
    };
    let nodes = Arc::new(parse_template(&source));
    let static_len = static_len(&nodes);
    if let Ok(mut cache) = template_cache().lock() {
        cache.insert(
            path.to_string(),
            CachedTemplate {
                modified,
                len,
                checked_at: now,
                nodes: Arc::clone(&nodes),
                static_len,
            },
        );
    }
    BolideString::new(&render_parsed_template(&nodes, static_len, context))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::ElementType;

    #[test]
    fn renders_vars_and_escapes_html() {
        let ctx = BolideDict::new(ElementType::String, ElementType::Dynamic);
        unsafe {
            let key = BolideString::new("title");
            let value = BolideString::new("<hello>");
            let dyn_value = BolideDynamic::from_string(value);
            (*ctx).set(key as i64, dyn_value as i64);
            crate::bolide_string_release(key);
            crate::bolide_dynamic_release(dyn_value);

            let out = render_template("{{ title }} {!! title !!}", ctx);
            assert_eq!(out, "&lt;hello&gt; <hello>");
            crate::bolide_dict_release(ctx);
        }
    }
}
