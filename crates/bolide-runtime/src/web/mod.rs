//! Minimal HTTP runtime for the Bolide web standard library.

use crate::{BolideBytes, BolideDynamic, BolideString};
use mio::net::{TcpListener as MioTcpListener, TcpStream as MioTcpStream};
use mio::{Events, Interest, Poll, Token, Waker};
use once_cell::sync::Lazy;
use std::collections::{BTreeSet, HashMap};
use std::ffi::CStr;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::net::{
    SocketAddr, TcpListener as StdTcpListener, TcpStream as StdTcpStream, ToSocketAddrs,
};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod client;
mod compress;
mod cors;
mod crypto;
mod multipart;
mod stream;
mod tls;
mod websocket;

pub use client::*;
pub use cors::*;
pub use multipart::*;
pub use stream::*;
pub use websocket::*;

type WebHandler = unsafe extern "C" fn(*mut BolideWebRequest) -> *mut BolideWebResponse;
type WebObjectHandler = unsafe extern "C" fn(*mut u8) -> *mut u8;
type WebStreamHandler = unsafe extern "C" fn(*mut u8, *mut u8) -> i64;
type WebSocketHandler = unsafe extern "C" fn(*mut u8, *mut u8) -> i64;
type WebAfterHandler = unsafe extern "C" fn(*mut u8, *mut u8);

const MIN_AUTO_WORKERS: usize = 2;
const MAX_AUTO_WORKERS: usize = 32;
const MAX_ACCEPTORS: usize = 4;
const DEFAULT_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const LISTEN_BACKLOG: i32 = 4096;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const EVENT_CAPACITY: usize = 1024;
const SERVER_TOKEN: Token = Token(0);
const WAKER_TOKEN: Token = Token(1);
const FIRST_CONN_TOKEN: usize = 2;
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(30);
const SESSION_COOKIE: &str = "bolide_session";

#[derive(Clone)]
enum RouteTarget {
    Handler(WebHandler),
    ObjectHandler(WebObjectHandler),
    StreamHandler(WebStreamHandler),
    WebSocketHandler(WebSocketHandler),
    StaticDir { root: String },
}

#[derive(Clone)]
struct Route {
    method: String,
    path: String,
    target: RouteTarget,
}

#[derive(Clone, Default)]
struct RouteTable {
    routes: Vec<Route>,
    exact: HashMap<String, HashMap<String, usize>>,
    pattern: HashMap<String, Vec<usize>>,
}

#[derive(Clone, Default)]
pub(crate) struct AppShared {
    routes: RouteTable,
    before: Vec<WebObjectHandler>,
    after: Vec<WebAfterHandler>,
    not_found: Option<WebObjectHandler>,
    error_handler: Option<WebObjectHandler>,
    cors: Option<cors::CorsConfig>,
    compression: bool,
    max_body_bytes: usize,
}

#[derive(Clone)]
pub(crate) struct TlsSettings {
    pub(crate) identity: TlsIdentity,
}

#[derive(Clone)]
pub(crate) enum TlsIdentity {
    Pkcs12 { path: String, password: String },
    Pkcs8 { cert_path: String, key_path: String },
}

#[repr(C)]
pub struct BolideWebApp {
    routes: RouteTable,
    shared: AppShared,
    workers: usize,
    max_body_bytes: usize,
    tls: Option<TlsSettings>,
}

pub(crate) struct DetachRequest {
    kind: DetachKind,
    request: BolideWebRequest,
    leftover: Vec<u8>,
}

pub(crate) enum DetachKind {
    Stream(WebStreamHandler, Vec<(String, String)>),
    WebSocket(WebSocketHandler, Vec<(String, String)>),
}

pub(crate) enum Dispatch {
    Respond(BolideWebResponse),
    Detach(DetachRequest),
}

enum ConnOutcome {
    Keep,
    Close,
    Detach(DetachRequest),
}

#[derive(Clone)]
#[repr(C)]
pub struct BolideWebRequest {
    method: String,
    target: String,
    path: String,
    query: String,
    version: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    path_params: Vec<(String, String)>,
    multipart: std::cell::OnceCell<Vec<multipart::MultipartPart>>,
}

impl BolideWebRequest {
    pub(crate) fn multipart_parts(&self) -> &Vec<multipart::MultipartPart> {
        self.multipart
            .get_or_init(|| multipart::parse_multipart(self))
    }
}

#[repr(C)]
pub struct BolideWebResponse {
    status: i64,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[repr(C)]
pub struct BolideWebSession {
    id: String,
}

struct StoredDynamic(*mut BolideDynamic);

unsafe impl Send for StoredDynamic {}

impl Drop for StoredDynamic {
    fn drop(&mut self) {
        crate::bolide_dynamic_release(self.0);
    }
}

struct SessionData {
    values: HashMap<String, StoredDynamic>,
    last_access: SystemTime,
    expires_at: Option<SystemTime>,
}

enum StaticPathError {
    Forbidden,
    NotFound,
}

static SESSIONS: Lazy<Mutex<HashMap<String, SessionData>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
#[derive(Clone)]
struct SessionConfig {
    ttl: Option<Duration>,
    max_sessions: usize,
    dir: Option<PathBuf>,
}

static SESSION_CONFIG: Lazy<Mutex<SessionConfig>> = Lazy::new(|| {
    Mutex::new(SessionConfig {
        ttl: None,
        max_sessions: 10_000,
        dir: None,
    })
});
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

struct RequestBudget {
    max: i64,
    started: i64,
}

enum AsyncConnState {
    Reading,
    Writing,
}

struct AsyncConnection {
    stream: MioTcpStream,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
    write_pos: usize,
    state: AsyncConnState,
    registered: bool,
    keep_alive: bool,
    last_active: Instant,
}

#[derive(Clone)]
struct ReactorTarget {
    stream_tx: mpsc::Sender<StdTcpStream>,
    waker: Arc<Waker>,
}

impl RouteTable {
    fn push(&mut self, route: Route) {
        let index = self.routes.len();
        let method = route.method.clone();
        let path = route.path.clone();
        let is_exact_handler = matches!(
            &route.target,
            RouteTarget::Handler(_)
                | RouteTarget::ObjectHandler(_)
                | RouteTarget::StreamHandler(_)
                | RouteTarget::WebSocketHandler(_)
        );
        let needs_pattern_match = matches!(&route.target, RouteTarget::StaticDir { .. })
            || path.as_bytes().contains(&b'{')
            || path.as_bytes().contains(&b'*');

        self.routes.push(route);

        if is_exact_handler {
            self.exact
                .entry(method.clone())
                .or_default()
                .entry(path)
                .or_insert(index);
        }
        if needs_pattern_match {
            self.pattern.entry(method).or_default().push(index);
        }
    }
}

impl RequestBudget {
    fn reserve(&mut self) -> bool {
        if self.started >= self.max {
            return false;
        }
        self.started += 1;
        true
    }

    fn exhausted(&self) -> bool {
        self.started >= self.max
    }
}

pub(crate) fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

pub(crate) fn bstr_to_str<'a>(ptr: *const BolideString) -> &'a str {
    if ptr.is_null() {
        ""
    } else {
        unsafe { (&*ptr).as_str() }
    }
}

pub(crate) fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

pub(crate) fn contains_header(headers: &[(String, String)], name: &str) -> bool {
    headers
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case(name))
}

pub(crate) fn upsert_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some((_, old_value)) = headers
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
    {
        *old_value = value;
    } else {
        headers.push((name.to_string(), value));
    }
}

pub(crate) fn add_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    headers.push((name.to_string(), value));
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push(((hi << 4) | lo) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, name: &str) -> String {
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        if percent_decode(key) == name {
            return percent_decode(value);
        }
    }
    String::new()
}

fn cookie_value(cookie_header: &str, name: &str) -> Option<String> {
    for part in cookie_header.split(';') {
        let trimmed = part.trim();
        let (key, value) = trimmed.split_once('=').unwrap_or((trimmed, ""));
        if key == name {
            return Some(percent_decode(value));
        }
    }
    None
}

fn request_cookie(req: &BolideWebRequest, name: &str) -> String {
    find_header(&req.headers, "cookie")
        .and_then(|value| cookie_value(value, name))
        .unwrap_or_default()
}

fn request_form_param(req: &BolideWebRequest, name: &str) -> String {
    let content_type = find_header(&req.headers, "content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.starts_with("application/x-www-form-urlencoded") {
        return query_param(&String::from_utf8_lossy(&req.body), name);
    }
    if content_type.starts_with("multipart/form-data") {
        if let Some(part) = req
            .multipart_parts()
            .iter()
            .find(|part| part.name == name && part.filename.is_none())
        {
            return String::from_utf8_lossy(&part.data).into_owned();
        }
    }
    String::new()
}

fn is_cookie_value_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'0'..=b'9'
            | b'A'..=b'Z'
            | b'^'
            | b'_'
            | b'`'
            | b'a'..=b'z'
            | b'|'
            | b'~'
    )
}

fn percent_encode_cookie_value(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_cookie_value_byte(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

fn cookie_header(
    name: &str,
    value: &str,
    path: &str,
    max_age: i64,
    http_only: bool,
    same_site: &str,
) -> String {
    let mut out = format!("{}={}", name, percent_encode_cookie_value(value));
    if !path.is_empty() {
        out.push_str("; Path=");
        out.push_str(path);
    }
    if max_age >= 0 {
        out.push_str("; Max-Age=");
        out.push_str(&max_age.to_string());
    }
    if http_only {
        out.push_str("; HttpOnly");
    }
    if !same_site.is_empty() {
        out.push_str("; SameSite=");
        out.push_str(same_site);
    }
    out
}

fn cookie_pair_from_set_cookie(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn normalize_url_prefix(prefix: &str) -> String {
    let mut prefix = if prefix.starts_with('/') {
        prefix.to_string()
    } else {
        format!("/{}", prefix)
    };
    while prefix.len() > 1 && prefix.ends_with('/') {
        prefix.pop();
    }
    prefix
}

fn static_path_suffix<'a>(prefix: &str, path: &'a str) -> Option<&'a str> {
    if prefix == "/" {
        return Some(path.trim_start_matches('/'));
    }
    if path == prefix {
        return Some("");
    }
    path.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
}

fn resolve_static_file(root: &str, prefix: &str, path: &str) -> Result<PathBuf, StaticPathError> {
    let Some(suffix) = static_path_suffix(prefix, path) else {
        return Err(StaticPathError::NotFound);
    };
    let root = fs::canonicalize(root).map_err(|_| StaticPathError::NotFound)?;
    let decoded = percent_decode(suffix);
    let mut candidate = root.clone();

    for component in Path::new(&decoded).components() {
        match component {
            Component::Normal(part) => candidate.push(part),
            Component::CurDir => {}
            _ => return Err(StaticPathError::Forbidden),
        }
    }

    if decoded.is_empty() || decoded.ends_with('/') {
        candidate.push("index.html");
    }

    let metadata = fs::metadata(&candidate).map_err(|_| StaticPathError::NotFound)?;
    if metadata.is_dir() {
        candidate.push("index.html");
    }

    let candidate = fs::canonicalize(&candidate).map_err(|_| StaticPathError::NotFound)?;
    if !candidate.starts_with(&root) {
        return Err(StaticPathError::Forbidden);
    }
    if !candidate.is_file() {
        return Err(StaticPathError::NotFound);
    }
    Ok(candidate)
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn file_etag(size: u64, mtime: Option<SystemTime>) -> String {
    let secs = mtime
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("\"{:x}-{:x}\"", size, secs)
}

fn httpdate_format(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let wday = (days.rem_euclid(7) + 4) % 7;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        WDAYS[wday as usize],
        day,
        MONTHS[(month - 1) as usize],
        year,
        hour,
        min,
        sec
    )
}

fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let first = spec.split(',').next()?.trim();
    let (start_s, end_s) = first.split_once('-')?;
    let (start, end) = if start_s.is_empty() {
        let n: u64 = end_s.trim().parse().ok()?;
        if n == 0 || size == 0 {
            return None;
        }
        (size.saturating_sub(n), size.saturating_sub(1))
    } else {
        let start: u64 = start_s.trim().parse().ok()?;
        let end = if end_s.trim().is_empty() {
            size.saturating_sub(1)
        } else {
            end_s
                .trim()
                .parse::<u64>()
                .ok()?
                .min(size.saturating_sub(1))
        };
        (start, end)
    };
    if start > end || start >= size {
        return None;
    }
    Some((start, end))
}

fn read_file_range(path: &Path, start: u64, end: u64) -> io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let len = (end - start + 1) as usize;
    let mut body = vec![0; len];
    file.read_exact(&mut body)?;
    Ok(body)
}

fn static_file_response(root: &str, prefix: &str, req: &BolideWebRequest) -> BolideWebResponse {
    let file = match resolve_static_file(root, prefix, &req.path) {
        Ok(file) => file,
        Err(StaticPathError::Forbidden) => {
            return response(403, "text/plain; charset=utf-8", "forbidden")
        }
        Err(StaticPathError::NotFound) => {
            return response(404, "text/plain; charset=utf-8", "not found")
        }
    };
    let Ok(metadata) = fs::metadata(&file) else {
        return response(404, "text/plain; charset=utf-8", "not found");
    };
    let size = metadata.len();
    let mtime = metadata.modified().ok();
    let etag = file_etag(size, mtime);
    let last_modified = mtime.map(httpdate_format);
    let content_type = content_type_for_path(&file);

    if let Some(inm) = find_header(&req.headers, "if-none-match") {
        if inm
            .split(',')
            .map(str::trim)
            .any(|token| token == etag || token == "*")
        {
            let mut res = response(304, "", Vec::new());
            upsert_header(&mut res.headers, "ETag", etag);
            return res;
        }
    } else if let (Some(ims), Some(lm)) = (
        find_header(&req.headers, "if-modified-since"),
        &last_modified,
    ) {
        if ims == lm {
            let mut res = response(304, "", Vec::new());
            upsert_header(&mut res.headers, "Last-Modified", lm.clone());
            return res;
        }
    }

    let cache_headers = |res: &mut BolideWebResponse| {
        upsert_header(&mut res.headers, "ETag", etag.clone());
        upsert_header(&mut res.headers, "Accept-Ranges", "bytes".to_string());
        upsert_header(
            &mut res.headers,
            "Cache-Control",
            "public, max-age=3600".to_string(),
        );
        if let Some(lm) = &last_modified {
            upsert_header(&mut res.headers, "Last-Modified", lm.clone());
        }
    };

    if let Some(range) = find_header(&req.headers, "range") {
        return match parse_range(range, size) {
            Some((start, end)) => match read_file_range(&file, start, end) {
                Ok(body) => {
                    let mut res = response(206, content_type, body);
                    upsert_header(
                        &mut res.headers,
                        "Content-Range",
                        format!("bytes {}-{}/{}", start, end, size),
                    );
                    cache_headers(&mut res);
                    res
                }
                Err(_) => response(404, "text/plain; charset=utf-8", "not found"),
            },
            None => {
                let mut res = response(416, "text/plain; charset=utf-8", "range not satisfiable");
                upsert_header(
                    &mut res.headers,
                    "Content-Range",
                    format!("bytes */{}", size),
                );
                res
            }
        };
    }

    match fs::read(&file) {
        Ok(body) => {
            let mut res = response(200, content_type, body);
            cache_headers(&mut res);
            res
        }
        Err(_) => response(403, "text/plain; charset=utf-8", "forbidden"),
    }
}

pub(crate) fn parse_header_lines(headers_text: &str) -> Vec<(String, String)> {
    headers_text
        .split('\n')
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn new_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let count = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let thread = thread::current();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    thread.id().hash(&mut hasher);
    now.hash(&mut hasher);
    count.hash(&mut hasher);
    let mixed = hasher.finish();
    format!("{:016x}{:016x}", mixed, count)
}

fn new_session_data(config: &SessionConfig) -> SessionData {
    let now = SystemTime::now();
    SessionData {
        values: HashMap::new(),
        last_access: now,
        expires_at: config.ttl.and_then(|ttl| now.checked_add(ttl)),
    }
}

fn session_expired(data: &SessionData) -> bool {
    data.expires_at
        .and_then(|expires| SystemTime::now().duration_since(expires).ok())
        .is_some()
}

fn touch_session(data: &mut SessionData, config: &SessionConfig) {
    let now = SystemTime::now();
    data.last_access = now;
    data.expires_at = config.ttl.and_then(|ttl| now.checked_add(ttl));
}

fn session_path(config: &SessionConfig, id: &str) -> Option<PathBuf> {
    let dir = config.dir.as_ref()?;
    let safe: String = id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    Some(dir.join(format!("{}.json", safe)))
}

fn json_escape(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn dynamic_to_json(value: *mut BolideDynamic) -> Option<String> {
    if value.is_null() {
        return Some("{\"type\":\"none\"}".to_string());
    }
    let value = unsafe { &*value };
    match value.tag {
        crate::DynamicType::None => Some("{\"type\":\"none\"}".to_string()),
        crate::DynamicType::Bool => Some(format!("{{\"type\":\"bool\",\"value\":{}}}", unsafe {
            value.data.bool_val != 0
        })),
        crate::DynamicType::Int => Some(format!("{{\"type\":\"int\",\"value\":{}}}", unsafe {
            value.data.int_val
        })),
        crate::DynamicType::Float => Some(format!("{{\"type\":\"float\",\"value\":{}}}", unsafe {
            value.data.float_val
        })),
        crate::DynamicType::String => {
            let s = unsafe {
                if value.data.string_ptr.is_null() {
                    ""
                } else {
                    (*value.data.string_ptr).as_str()
                }
            };
            Some(format!(
                "{{\"type\":\"string\",\"value\":\"{}\"}}",
                json_escape(s)
            ))
        }
        crate::DynamicType::Bytes => {
            let bytes = unsafe {
                if value.data.bytes_ptr.is_null() {
                    &[][..]
                } else {
                    (*value.data.bytes_ptr).as_slice()
                }
            };
            Some(format!(
                "{{\"type\":\"bytes\",\"value\":\"{}\"}}",
                crypto::base64_encode(bytes)
            ))
        }
        _ => None,
    }
}

fn flush_session(config: &SessionConfig, id: &str, data: &SessionData) {
    let Some(path) = session_path(config, id) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut out = String::from("{\"values\":{");
    let mut first = true;
    for (key, value) in &data.values {
        let Some(json) = dynamic_to_json(value.0) else {
            continue;
        };
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        out.push_str(&json_escape(key));
        out.push_str("\":");
        out.push_str(&json);
    }
    out.push_str("}}\n");
    let _ = fs::write(path, out);
}

fn extract_json_string(src: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":\"", key);
    let start = src.find(&needle)? + needle.len();
    let mut out = String::new();
    let mut escape = false;
    for ch in src[start..].chars() {
        if escape {
            out.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escape = false;
        } else if ch == '\\' {
            escape = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

fn dynamic_from_json(src: &str) -> Option<*mut BolideDynamic> {
    let ty = extract_json_string(src, "type")?;
    match ty.as_str() {
        "none" => Some(crate::bolide_dynamic_none()),
        "bool" => Some(crate::bolide_dynamic_from_bool(
            src.contains("\"value\":true") as i64,
        )),
        "int" => {
            let start = src.find("\"value\":")? + 8;
            let end = src[start..]
                .find(|ch: char| !ch.is_ascii_digit() && ch != '-')
                .map(|n| start + n)
                .unwrap_or(src.len());
            Some(crate::bolide_dynamic_from_int(
                src[start..end].parse().ok()?,
            ))
        }
        "float" => {
            let start = src.find("\"value\":")? + 8;
            let end = src[start..]
                .find(|ch: char| {
                    !(ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E'))
                })
                .map(|n| start + n)
                .unwrap_or(src.len());
            Some(crate::bolide_dynamic_from_float(
                src[start..end].parse().ok()?,
            ))
        }
        "string" => {
            let value = extract_json_string(src, "value")?;
            Some(crate::bolide_dynamic_from_string(BolideString::new(&value)))
        }
        "bytes" => {
            let value = extract_json_string(src, "value")?;
            let bytes = crypto::base64_decode(&value)?;
            Some(crate::bolide_dynamic_from_bytes(BolideBytes::from_slice(
                &bytes,
            )))
        }
        _ => None,
    }
}

fn load_session(config: &SessionConfig, id: &str) -> Option<SessionData> {
    let path = session_path(config, id)?;
    let text = fs::read_to_string(path).ok()?;
    let values_pos = text.find("\"values\":{")? + "\"values\":{".len();
    let mut rest = &text[values_pos..];
    let mut data = new_session_data(config);
    while let Some(key_start) = rest.find('"') {
        rest = &rest[key_start + 1..];
        let key_end = rest.find('"')?;
        let key = rest[..key_end].to_string();
        rest = &rest[key_end + 1..];
        let obj_start = rest.find('{')?;
        rest = &rest[obj_start..];
        let obj_end = rest.find('}')?;
        let obj = &rest[..=obj_end];
        if let Some(dynamic) = dynamic_from_json(obj) {
            data.values.insert(key, StoredDynamic(dynamic));
        }
        rest = &rest[obj_end + 1..];
        if rest.trim_start().starts_with('}') {
            break;
        }
    }
    Some(data)
}

fn evict_sessions(config: &SessionConfig, sessions: &mut HashMap<String, SessionData>) {
    let expired: Vec<String> = sessions
        .iter()
        .filter_map(|(id, data)| session_expired(data).then(|| id.clone()))
        .collect();
    for id in expired {
        sessions.remove(&id);
        if let Some(path) = session_path(config, &id) {
            let _ = fs::remove_file(path);
        }
    }
    while sessions.len() > config.max_sessions {
        let Some(id) = sessions
            .iter()
            .min_by_key(|(_, data)| data.last_access)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        sessions.remove(&id);
        if let Some(path) = session_path(config, &id) {
            let _ = fs::remove_file(path);
        }
    }
}

fn ensure_session<'a>(
    sessions: &'a mut HashMap<String, SessionData>,
    config: &SessionConfig,
    id: &str,
) -> &'a mut SessionData {
    if !sessions.contains_key(id) {
        if let Some(data) = load_session(config, id) {
            sessions.insert(id.to_string(), data);
        }
    }
    sessions
        .entry(id.to_string())
        .or_insert_with(|| new_session_data(config))
}

fn split_target(target: &str) -> (String, String) {
    match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.to_string(), String::new()),
    }
}

fn uppercase_method(method: &str) -> String {
    if method.bytes().all(|byte| !byte.is_ascii_lowercase()) {
        method.to_string()
    } else {
        method.to_ascii_uppercase()
    }
}

fn match_route(pattern: &str, path: &str) -> Option<Vec<(String, String)>> {
    if pattern == path {
        return Some(Vec::new());
    }

    let mut params = Vec::new();
    let mut pattern_parts = pattern.trim_matches('/').split('/');
    let mut path_parts = path.trim_matches('/').split('/');
    loop {
        let expected = pattern_parts.next();
        let actual_opt = path_parts.next();
        match (expected, actual_opt) {
            (None, None) => return Some(params),
            (None, Some(_)) => return None,
            (Some(""), Some("")) => return Some(params),
            (Some(expected), actual_opt) => {
                if expected.starts_with('*') && expected.len() > 1 {
                    let mut rest = String::new();
                    if let Some(actual) = actual_opt {
                        rest.push_str(actual);
                        for part in path_parts.by_ref() {
                            rest.push('/');
                            rest.push_str(part);
                        }
                    }
                    params.push((expected[1..].to_string(), percent_decode(&rest)));
                    return Some(params);
                }
                let Some(actual) = actual_opt else {
                    return None;
                };
                if expected.starts_with('{') && expected.ends_with('}') && expected.len() > 2 {
                    params.push((
                        expected[1..expected.len() - 1].to_string(),
                        percent_decode(actual),
                    ));
                } else if expected != actual {
                    return None;
                }
            }
        }
    }
}

fn route_path_params(route: &Route, path: &str) -> Option<Vec<(String, String)>> {
    match &route.target {
        RouteTarget::Handler(_)
        | RouteTarget::ObjectHandler(_)
        | RouteTarget::StreamHandler(_)
        | RouteTarget::WebSocketHandler(_) => match_route(&route.path, path),
        RouteTarget::StaticDir { .. } => static_path_suffix(&route.path, path).map(|_| Vec::new()),
    }
}

fn find_route<'a>(
    routes: &'a RouteTable,
    method: &str,
    path: &str,
) -> Option<(&'a Route, Vec<(String, String)>)> {
    let head_methods = ["HEAD", "GET"];
    let single_method = [method];
    let methods: &[&str] = if method == "HEAD" {
        &head_methods
    } else {
        &single_method
    };

    for candidate in methods {
        if let Some(by_path) = routes.exact.get(*candidate) {
            if let Some(index) = by_path.get(path) {
                return Some((&routes.routes[*index], Vec::new()));
            }
        }
    }

    for candidate in methods {
        if let Some(indices) = routes.pattern.get(*candidate) {
            for index in indices {
                let route = &routes.routes[*index];
                if let Some(params) = route_path_params(route, path) {
                    return Some((route, params));
                }
            }
        }
    }

    None
}

fn allowed_methods(routes: &RouteTable, path: &str) -> BTreeSet<String> {
    let mut methods = BTreeSet::new();
    for (method, by_path) in &routes.exact {
        if by_path.contains_key(path) {
            methods.insert(method.clone());
        }
    }
    for (method, indices) in &routes.pattern {
        for index in indices {
            if route_path_params(&routes.routes[*index], path).is_some() {
                methods.insert(method.clone());
                break;
            }
        }
    }
    if methods.contains("GET") {
        methods.insert("HEAD".to_string());
    }
    if !methods.is_empty() {
        methods.insert("OPTIONS".to_string());
    }
    methods
}

fn allow_header(routes: &RouteTable, path: &str) -> String {
    allowed_methods(routes, path)
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn reason_phrase(status: i64) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

pub(crate) fn response(
    status: i64,
    content_type: &str,
    body: impl Into<Vec<u8>>,
) -> BolideWebResponse {
    let mut headers = Vec::with_capacity(8);
    if !content_type.is_empty() {
        headers.push(("Content-Type".to_string(), content_type.to_string()));
    }
    BolideWebResponse {
        status,
        headers,
        body: body.into(),
    }
}

fn boxed_response(
    status: i64,
    content_type: &str,
    body: impl Into<Vec<u8>>,
) -> *mut BolideWebResponse {
    Box::into_raw(Box::new(response(status, content_type, body)))
}

fn write_response_with_connection(
    out: &mut Vec<u8>,
    mut res: BolideWebResponse,
    include_body: bool,
    keep_alive: bool,
) {
    if res.status <= 0 {
        res.status = 200;
    }
    let has_connection = contains_header(&res.headers, "Connection");
    let needs_content_type = !res.body.is_empty() && !contains_header(&res.headers, "Content-Type");

    let mut capacity = 64 + if include_body { res.body.len() } else { 0 };
    for (name, value) in &res.headers {
        capacity += name.len() + value.len() + 4;
    }
    capacity += 32;
    if !has_connection {
        capacity += 30;
    }
    if needs_content_type {
        capacity += 43;
    }

    out.clear();
    if out.capacity() < capacity {
        out.reserve(capacity - out.capacity());
    }

    let _ = write!(
        out,
        "HTTP/1.1 {} {}\r\n",
        res.status,
        reason_phrase(res.status)
    );
    for (name, value) in &res.headers {
        if name.eq_ignore_ascii_case("Content-Length") {
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    let _ = write!(out, "Content-Length: {}\r\n", res.body.len());
    if !has_connection {
        if keep_alive {
            out.extend_from_slice(b"Connection: keep-alive\r\n");
        } else {
            out.extend_from_slice(b"Connection: close\r\n");
        }
    }
    if needs_content_type {
        out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
    }
    out.extend_from_slice(b"\r\n");
    if include_body {
        out.extend_from_slice(&res.body);
    }
}

fn should_keep_alive(req: &BolideWebRequest) -> bool {
    if let Some(connection) = find_header(&req.headers, "connection") {
        if connection
            .split(',')
            .map(str::trim)
            .any(|part| part.eq_ignore_ascii_case("close"))
        {
            return false;
        }
        if req.version.eq_ignore_ascii_case("HTTP/1.0") {
            return connection
                .split(',')
                .map(str::trim)
                .any(|part| part.eq_ignore_ascii_case("keep-alive"));
        }
        return true;
    }
    !req.version.eq_ignore_ascii_case("HTTP/1.0")
}

fn parse_http_request_buffer(
    buffer: &[u8],
    max_body_bytes: usize,
) -> io::Result<Option<(BolideWebRequest, usize)>> {
    let Some(header_pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
        if buffer.len() > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
        return Ok(None);
    };
    let header_end = header_pos + 4;
    if header_end > MAX_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request headers too large",
        ));
    }

    let head = std::str::from_utf8(&buffer[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid request headers"))?;
    let mut lines = head.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = uppercase_method(parts.next().unwrap_or(""));
    let target = parts.next().unwrap_or("/").to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    if method.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "missing method"));
    }

    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let is_chunked = find_header(&headers, "transfer-encoding")
        .map(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
        })
        .unwrap_or(false);
    let (body, consumed) = if is_chunked {
        match decode_chunked_body(buffer, header_end, max_body_bytes)? {
            Some(result) => result,
            None => return Ok(None),
        }
    } else {
        let content_length = find_header(&headers, "content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if content_length > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request body too large",
            ));
        }

        let consumed = header_end + content_length;
        if buffer.len() < consumed {
            return Ok(None);
        }
        (buffer[header_end..consumed].to_vec(), consumed)
    };
    let (path, query) = split_target(&target);
    Ok(Some((
        BolideWebRequest {
            method,
            target,
            path,
            query,
            version,
            headers,
            body,
            path_params: Vec::new(),
            multipart: std::cell::OnceCell::new(),
        },
        consumed,
    )))
}

fn find_crlf(buf: &[u8], from: usize) -> Option<usize> {
    buf.get(from..)?
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|pos| from + pos)
}

fn decode_chunked_body(
    buf: &[u8],
    start: usize,
    max_body_bytes: usize,
) -> io::Result<Option<(Vec<u8>, usize)>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid chunked body");
    let mut pos = start;
    let mut body = Vec::new();
    loop {
        let Some(line_end) = find_crlf(buf, pos) else {
            return Ok(None);
        };
        let size_line = std::str::from_utf8(&buf[pos..line_end]).map_err(|_| invalid())?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| invalid())?;
        pos = line_end + 2;
        if size == 0 {
            let Some(trailer_end) = find_crlf(buf, pos) else {
                return Ok(None);
            };
            return Ok(Some((body, trailer_end + 2)));
        }
        if body.len() + size > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request body too large",
            ));
        }
        if buf.len() < pos + size + 2 {
            return Ok(None);
        }
        body.extend_from_slice(&buf[pos..pos + size]);
        pos += size + 2;
    }
}

fn read_request_blocking<R: Read + ?Sized>(
    conn: &mut R,
    buf: &mut Vec<u8>,
    max_body_bytes: usize,
) -> io::Result<Option<(BolideWebRequest, usize)>> {
    loop {
        if let Some(parsed) = parse_http_request_buffer(buf, max_body_bytes)? {
            return Ok(Some(parsed));
        }
        let mut tmp = [0u8; 8192];
        match conn.read(&mut tmp) {
            Ok(0) if buf.is_empty() => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "eof mid-request",
                ))
            }
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
}

fn request_object(req: BolideWebRequest) -> (*mut BolideWebRequest, *mut u8) {
    let req_ptr = Box::into_raw(Box::new(req));
    let req_obj = crate::object_alloc(std::mem::size_of::<usize>());
    unsafe {
        (req_obj as *mut *mut BolideWebRequest).write(req_ptr);
    }
    (req_ptr, req_obj)
}

fn wrap_handle(ptr: *mut u8) -> *mut u8 {
    let obj = crate::object_alloc(std::mem::size_of::<usize>());
    unsafe {
        (obj as *mut *mut u8).write(ptr);
    }
    obj
}

fn response_from_object(res_obj: *mut u8) -> BolideWebResponse {
    if res_obj.is_null() {
        return response(
            500,
            "text/plain; charset=utf-8",
            "handler returned null response",
        );
    }

    let res_ptr = unsafe { *(res_obj as *mut *mut BolideWebResponse) };
    crate::object_release(res_obj);
    if res_ptr.is_null() {
        response(
            500,
            "text/plain; charset=utf-8",
            "handler returned null response",
        )
    } else {
        unsafe { *Box::from_raw(res_ptr) }
    }
}

fn handler_error_response(
    shared: &AppShared,
    req: &BolideWebRequest,
    message: &'static str,
) -> BolideWebResponse {
    if let Some(handler) = shared.error_handler {
        let req_clone = req.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let (req_ptr, req_obj) = request_object(req_clone);
            let res_obj = unsafe { handler(req_obj) };
            crate::object_release(req_obj);
            unsafe {
                drop(Box::from_raw(req_ptr));
            }
            response_from_object(res_obj)
        }));
        if let Ok(res) = result {
            return res;
        }
    }
    response(500, "text/plain; charset=utf-8", message)
}

fn is_null_handler_response(res: &BolideWebResponse) -> bool {
    res.status == 500 && res.body == b"handler returned null response"
}

fn response_ptr_from_object(res_obj: *mut u8) -> Option<BolideWebResponse> {
    if res_obj.is_null() {
        return None;
    }
    let res_ptr = unsafe { *(res_obj as *mut *mut BolideWebResponse) };
    crate::object_release(res_obj);
    if res_ptr.is_null() {
        None
    } else {
        Some(unsafe { *Box::from_raw(res_ptr) })
    }
}

fn run_before_middleware(
    handler: WebObjectHandler,
    req: &BolideWebRequest,
) -> Option<BolideWebResponse> {
    let req_obj = wrap_handle(req as *const BolideWebRequest as *mut u8);
    let res_obj = unsafe { handler(req_obj) };
    crate::object_release(req_obj);
    response_ptr_from_object(res_obj)
}

fn run_after_middleware(
    handler: WebAfterHandler,
    req: &BolideWebRequest,
    res: &mut BolideWebResponse,
) {
    let req_obj = wrap_handle(req as *const BolideWebRequest as *mut u8);
    let res_obj = wrap_handle(res as *mut BolideWebResponse as *mut u8);
    unsafe {
        handler(req_obj, res_obj);
    }
    crate::object_release(req_obj);
    crate::object_release(res_obj);
}

fn finalize_response(shared: &AppShared, req: &BolideWebRequest, res: &mut BolideWebResponse) {
    for handler in &shared.after {
        run_after_middleware(*handler, req, res);
    }
    if let Some(cors) = &shared.cors {
        cors::apply_cors(cors, req, res);
    }
    if shared.compression {
        let accept = find_header(&req.headers, "accept-encoding").unwrap_or("");
        compress::maybe_compress(accept, res);
    }
}

fn call_route_target(
    target: &RouteTarget,
    route_path: &str,
    mut req: BolideWebRequest,
    path_params: Vec<(String, String)>,
) -> (BolideWebResponse, BolideWebRequest) {
    match target {
        RouteTarget::Handler(handler) => {
            req.path_params = path_params;
            let req_ptr = Box::into_raw(Box::new(req));
            let res_ptr = unsafe { (*handler)(req_ptr) };
            let req = unsafe { *Box::from_raw(req_ptr) };
            let res = if res_ptr.is_null() {
                response(
                    500,
                    "text/plain; charset=utf-8",
                    "handler returned null response",
                )
            } else {
                unsafe { *Box::from_raw(res_ptr) }
            };
            (res, req)
        }
        RouteTarget::ObjectHandler(handler) => {
            req.path_params = path_params;
            let (req_ptr, req_obj) = request_object(req);
            let res_obj = unsafe { (*handler)(req_obj) };
            crate::object_release(req_obj);
            let req = unsafe { *Box::from_raw(req_ptr) };
            (response_from_object(res_obj), req)
        }
        RouteTarget::StaticDir { root } => {
            let res = static_file_response(root, route_path, &req);
            (res, req)
        }
        RouteTarget::StreamHandler(_) | RouteTarget::WebSocketHandler(_) => (
            response(500, "text/plain; charset=utf-8", "internal detaching route"),
            req,
        ),
    }
}

fn dispatch(shared: &AppShared, req: BolideWebRequest) -> Dispatch {
    if let Some(cors) = &shared.cors {
        if cors::is_preflight(&req) {
            return Dispatch::Respond(cors::preflight_response(cors, &req));
        }
    }

    for handler in &shared.before {
        if let Some(mut res) = run_before_middleware(*handler, &req) {
            finalize_response(shared, &req, &mut res);
            return Dispatch::Respond(res);
        }
    }

    if req.method == "OPTIONS" && find_route(&shared.routes, "OPTIONS", &req.path).is_none() {
        let allow = allow_header(&shared.routes, &req.path);
        if !allow.is_empty() {
            let mut res = response(204, "", Vec::new());
            upsert_header(&mut res.headers, "Allow", allow);
            finalize_response(shared, &req, &mut res);
            return Dispatch::Respond(res);
        }
    }

    let route = find_route(&shared.routes, &req.method, &req.path)
        .map(|(route, params)| (route.target.clone(), route.path.clone(), params));
    match route {
        Some((RouteTarget::StreamHandler(handler), _, params)) => Dispatch::Detach(DetachRequest {
            kind: DetachKind::Stream(handler, params),
            request: req,
            leftover: Vec::new(),
        }),
        Some((RouteTarget::WebSocketHandler(handler), _, params)) => {
            Dispatch::Detach(DetachRequest {
                kind: DetachKind::WebSocket(handler, params),
                request: req,
                leftover: Vec::new(),
            })
        }
        Some((target, route_path, params)) => {
            let error_req = req.clone();
            let result = catch_unwind(AssertUnwindSafe(|| {
                call_route_target(&target, &route_path, req, params)
            }));
            let (mut res, req) = match result {
                Ok((res, req)) if is_null_handler_response(&res) => {
                    let res =
                        handler_error_response(shared, &req, "handler returned null response");
                    (res, req)
                }
                Ok(result) => result,
                Err(_) => {
                    let res = handler_error_response(shared, &error_req, "handler panicked");
                    (res, error_req)
                }
            };
            finalize_response(shared, &req, &mut res);
            Dispatch::Respond(res)
        }
        None => {
            let (mut res, req) = if let Some(handler) = shared.not_found {
                let error_req = req.clone();
                match catch_unwind(AssertUnwindSafe(|| {
                    let (req_ptr, req_obj) = request_object(req);
                    let res_obj = unsafe { handler(req_obj) };
                    crate::object_release(req_obj);
                    let req = unsafe { *Box::from_raw(req_ptr) };
                    (response_from_object(res_obj), req)
                })) {
                    Ok((res, req)) if is_null_handler_response(&res) => {
                        let res = handler_error_response(
                            shared,
                            &req,
                            "not_found handler returned null response",
                        );
                        (res, req)
                    }
                    Ok(result) => result,
                    Err(_) => {
                        let res = handler_error_response(
                            shared,
                            &error_req,
                            "not_found handler panicked",
                        );
                        (res, error_req)
                    }
                }
            } else {
                let allow = allow_header(&shared.routes, &req.path);
                let res = if allow.is_empty() {
                    response(404, "text/plain; charset=utf-8", "not found")
                } else {
                    let mut res = response(405, "text/plain; charset=utf-8", "method not allowed");
                    upsert_header(&mut res.headers, "Allow", allow);
                    res
                };
                (res, req)
            };
            finalize_response(shared, &req, &mut res);
            Dispatch::Respond(res)
        }
    }
}

fn dispatch_to_response(shared: &AppShared, req: BolideWebRequest) -> BolideWebResponse {
    match dispatch(shared, req) {
        Dispatch::Respond(res) => res,
        Dispatch::Detach(_) => response(
            503,
            "text/plain; charset=utf-8",
            "streaming route requires a live connection",
        ),
    }
}

fn register_conn(
    poll: &Poll,
    token: Token,
    conn: &mut AsyncConnection,
    interest: Interest,
) -> io::Result<()> {
    if conn.registered {
        poll.registry()
            .reregister(&mut conn.stream, token, interest)
    } else {
        poll.registry()
            .register(&mut conn.stream, token, interest)?;
        conn.registered = true;
        Ok(())
    }
}

fn deregister_conn(poll: &Poll, conn: &mut AsyncConnection) {
    if conn.registered {
        let _ = poll.registry().deregister(&mut conn.stream);
        conn.registered = false;
    }
}

fn prepare_error_write(
    poll: &Poll,
    token: Token,
    conn: &mut AsyncConnection,
    status: i64,
    body: &'static str,
) -> ConnOutcome {
    conn.read_buf.clear();
    write_response_with_connection(
        &mut conn.write_buf,
        response(status, "text/plain; charset=utf-8", body),
        true,
        false,
    );
    conn.write_pos = 0;
    conn.keep_alive = false;
    conn.state = AsyncConnState::Writing;
    conn.last_active = Instant::now();
    if register_conn(poll, token, conn, Interest::WRITABLE).is_ok() {
        ConnOutcome::Keep
    } else {
        ConnOutcome::Close
    }
}

fn start_request_inline(
    poll: &Poll,
    token: Token,
    conn: &mut AsyncConnection,
    shared: &AppShared,
    request_budget: Option<&mut RequestBudget>,
) -> ConnOutcome {
    match parse_http_request_buffer(&conn.read_buf, shared.max_body_bytes) {
        Ok(Some((req, consumed))) => {
            let include_body = req.method != "HEAD";
            let mut keep_alive = should_keep_alive(&req);
            if let Some(budget) = request_budget {
                if !budget.reserve() {
                    return ConnOutcome::Close;
                }
                if budget.exhausted() {
                    keep_alive = false;
                }
            }

            let leftover = conn.read_buf.split_off(consumed);
            match dispatch(shared, req) {
                Dispatch::Detach(mut detach) => {
                    detach.leftover = leftover;
                    ConnOutcome::Detach(detach)
                }
                Dispatch::Respond(res) => {
                    conn.read_buf = leftover;
                    write_response_with_connection(
                        &mut conn.write_buf,
                        res,
                        include_body,
                        keep_alive,
                    );
                    conn.write_pos = 0;
                    conn.keep_alive = keep_alive;
                    conn.state = AsyncConnState::Writing;
                    conn.last_active = Instant::now();
                    if register_conn(poll, token, conn, Interest::WRITABLE).is_ok() {
                        ConnOutcome::Keep
                    } else {
                        ConnOutcome::Close
                    }
                }
            }
        }
        Ok(None) => {
            conn.state = AsyncConnState::Reading;
            if register_conn(poll, token, conn, Interest::READABLE).is_ok() {
                ConnOutcome::Keep
            } else {
                ConnOutcome::Close
            }
        }
        Err(_) => prepare_error_write(poll, token, conn, 400, "bad request"),
    }
}

fn read_inline_connection(
    poll: &Poll,
    token: Token,
    conn: &mut AsyncConnection,
    shared: &AppShared,
    request_budget: Option<&mut RequestBudget>,
) -> ConnOutcome {
    let mut temp = [0u8; 8192];
    loop {
        match conn.stream.read(&mut temp) {
            Ok(0) => return ConnOutcome::Close,
            Ok(n) => {
                conn.read_buf.extend_from_slice(&temp[..n]);
                conn.last_active = Instant::now();
                if conn.read_buf.len() > MAX_HEADER_BYTES + shared.max_body_bytes {
                    return prepare_error_write(poll, token, conn, 400, "bad request");
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return ConnOutcome::Close,
        }
    }
    start_request_inline(poll, token, conn, shared, request_budget)
}

fn write_inline_connection(
    poll: &Poll,
    token: Token,
    conn: &mut AsyncConnection,
    shared: &AppShared,
    request_budget: Option<&mut RequestBudget>,
) -> ConnOutcome {
    while conn.write_pos < conn.write_buf.len() {
        match conn.stream.write(&conn.write_buf[conn.write_pos..]) {
            Ok(0) => return ConnOutcome::Close,
            Ok(n) => {
                conn.write_pos += n;
                conn.last_active = Instant::now();
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                return if register_conn(poll, token, conn, Interest::WRITABLE).is_ok() {
                    ConnOutcome::Keep
                } else {
                    ConnOutcome::Close
                };
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return ConnOutcome::Close,
        }
    }

    conn.write_buf.clear();
    conn.write_pos = 0;
    if !conn.keep_alive {
        return ConnOutcome::Close;
    }
    start_request_inline(poll, token, conn, shared, request_budget)
}

fn close_async_connection(
    poll: &Poll,
    connections: &mut HashMap<Token, AsyncConnection>,
    token: Token,
) {
    if let Some(mut conn) = connections.remove(&token) {
        deregister_conn(poll, &mut conn);
    }
}

fn call_stream_handler<C: Write + Send + 'static>(
    conn: C,
    mut req: BolideWebRequest,
    handler: WebStreamHandler,
    params: Vec<(String, String)>,
) {
    req.path_params = params;
    let (req_ptr, req_obj) = request_object(req);
    let stream = Box::into_raw(Box::new(stream::BolideWebStream::new(Box::new(conn))));
    let stream_obj = wrap_handle(stream as *mut u8);
    unsafe {
        handler(req_obj, stream_obj);
        drop(Box::from_raw(stream));
        drop(Box::from_raw(req_ptr));
    }
    crate::object_release(req_obj);
    crate::object_release(stream_obj);
}

fn call_websocket_handler<C: websocket::WebSocketIo + 'static>(
    mut conn: C,
    mut req: BolideWebRequest,
    handler: WebSocketHandler,
    params: Vec<(String, String)>,
) {
    req.path_params = params;
    if websocket::handshake(&mut conn, &req).is_err() {
        return;
    }
    let (req_ptr, req_obj) = request_object(req);
    let ws = Box::into_raw(Box::new(websocket::BolideWebSocket::new(Box::new(conn))));
    let ws_obj = wrap_handle(ws as *mut u8);
    unsafe {
        handler(req_obj, ws_obj);
        drop(Box::from_raw(ws));
        drop(Box::from_raw(req_ptr));
    }
    crate::object_release(req_obj);
    crate::object_release(ws_obj);
}

fn run_detached_connection<C: websocket::WebSocketIo + 'static>(conn: C, detach: DetachRequest) {
    let DetachRequest { kind, request, .. } = detach;
    match kind {
        DetachKind::Stream(handler, params) => call_stream_handler(conn, request, handler, params),
        DetachKind::WebSocket(handler, params) => {
            call_websocket_handler(conn, request, handler, params)
        }
    }
}

fn handle_blocking_connection<C: websocket::WebSocketIo + 'static>(
    mut conn: C,
    shared: Arc<AppShared>,
) {
    let mut buf = Vec::with_capacity(4096);
    loop {
        let parsed = match read_request_blocking(&mut conn, &mut buf, shared.max_body_bytes) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => return,
            Err(_) => return,
        };
        let (req, consumed) = parsed;
        let include_body = req.method != "HEAD";
        let keep_alive = should_keep_alive(&req);
        if consumed == buf.len() {
            buf.clear();
        } else {
            buf.drain(..consumed);
        }
        match dispatch(&shared, req) {
            Dispatch::Respond(res) => {
                let mut out = Vec::new();
                write_response_with_connection(&mut out, res, include_body, keep_alive);
                if conn.write_all(&out).is_err() || conn.flush().is_err() || !keep_alive {
                    return;
                }
            }
            Dispatch::Detach(detach) => {
                run_detached_connection(conn, detach);
                return;
            }
        }
    }
}

fn reclaim_blocking_stream(poll: &Poll, mut conn: AsyncConnection) -> StdTcpStream {
    deregister_conn(poll, &mut conn);
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawSocket, IntoRawSocket};
        let raw = conn.stream.into_raw_socket();
        let stream = unsafe { StdTcpStream::from_raw_socket(raw) };
        let _ = stream.set_nonblocking(false);
        stream
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        let raw = conn.stream.into_raw_fd();
        let stream = unsafe { StdTcpStream::from_raw_fd(raw) };
        let _ = stream.set_nonblocking(false);
        stream
    }
}

fn detach_connection(
    poll: &Poll,
    connections: &mut HashMap<Token, AsyncConnection>,
    token: Token,
    detach: DetachRequest,
) {
    if let Some(conn) = connections.remove(&token) {
        let stream = reclaim_blocking_stream(poll, conn);
        let _ = thread::Builder::new()
            .name("bolide-web-detached".to_string())
            .spawn(move || run_detached_connection(stream, detach));
    }
}

fn run_tls_server(
    listener: StdTcpListener,
    shared: Arc<AppShared>,
    settings: &TlsSettings,
    max_connections: Option<i64>,
) -> i64 {
    let Some(acceptor) = tls::build_acceptor(settings) else {
        return 0;
    };
    let acceptor = Arc::new(acceptor);
    let max_connections = max_connections.filter(|value| *value > 0);
    let mut handled = 0i64;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if max_connections.is_some() {
                    if let Ok(tls_stream) = acceptor.accept(stream) {
                        handle_blocking_connection(tls_stream, shared.clone());
                    }
                    handled += 1;
                    if max_connections.is_some_and(|max| handled >= max) {
                        return 1;
                    }
                } else {
                    let acceptor = acceptor.clone();
                    let shared = shared.clone();
                    let _ = thread::Builder::new()
                        .name("bolide-web-tls".to_string())
                        .spawn(move || {
                            if let Ok(tls_stream) = acceptor.accept(stream) {
                                handle_blocking_connection(tls_stream, shared);
                            }
                        });
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return 0,
        }
    }
}

fn resolve_listen_addr(host: &str, port: i64) -> Option<std::net::SocketAddr> {
    (host, port as u16)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
}

fn default_worker_count() -> usize {
    thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(8)
        .clamp(MIN_AUTO_WORKERS, MAX_AUTO_WORKERS)
}

fn acceptor_count(workers: usize) -> usize {
    workers.clamp(1, MAX_ACCEPTORS)
}

fn bind_tcp_listener(addr: SocketAddr) -> io::Result<StdTcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let domain = match addr {
        SocketAddr::V4(_) => Domain::IPV4,
        SocketAddr::V6(_) => Domain::IPV6,
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(LISTEN_BACKLOG)?;
    Ok(socket.into())
}

fn register_mio_stream(
    poll: &Poll,
    token: Token,
    stream: StdTcpStream,
    connections: &mut HashMap<Token, AsyncConnection>,
    now: Instant,
) {
    let _ = stream.set_nonblocking(true);
    let mut conn = AsyncConnection {
        stream: MioTcpStream::from_std(stream),
        read_buf: Vec::with_capacity(4096),
        write_buf: Vec::new(),
        write_pos: 0,
        state: AsyncConnState::Reading,
        registered: false,
        keep_alive: true,
        last_active: now,
    };
    if register_conn(poll, token, &mut conn, Interest::READABLE).is_ok() {
        connections.insert(token, conn);
    }
}

fn run_inline_reactor(
    stream_rx: mpsc::Receiver<StdTcpStream>,
    waker_tx: mpsc::Sender<Arc<Waker>>,
    shared: Arc<AppShared>,
) {
    let Ok(mut poll) = Poll::new() else {
        return;
    };
    let Ok(waker) = Waker::new(poll.registry(), WAKER_TOKEN) else {
        return;
    };
    let waker = Arc::new(waker);
    if waker_tx.send(waker).is_err() {
        return;
    }

    let mut next_token = FIRST_CONN_TOKEN;
    let mut events = Events::with_capacity(EVENT_CAPACITY);
    let mut connections: HashMap<Token, AsyncConnection> = HashMap::new();

    loop {
        if poll
            .poll(&mut events, Some(Duration::from_millis(100)))
            .is_err()
        {
            break;
        }

        let now = Instant::now();
        let mut to_close = Vec::new();
        let mut to_detach = Vec::new();

        for event in events.iter() {
            match event.token() {
                WAKER_TOKEN => {
                    while let Ok(stream) = stream_rx.try_recv() {
                        let token = Token(next_token);
                        next_token = next_token.wrapping_add(1).max(FIRST_CONN_TOKEN);
                        register_mio_stream(&poll, token, stream, &mut connections, now);
                    }
                }
                token => {
                    if let Some(conn) = connections.get_mut(&token) {
                        let outcome = match conn.state {
                            AsyncConnState::Reading => {
                                if event.is_readable() {
                                    read_inline_connection(&poll, token, conn, &shared, None)
                                } else {
                                    ConnOutcome::Keep
                                }
                            }
                            AsyncConnState::Writing => {
                                if event.is_writable() {
                                    write_inline_connection(&poll, token, conn, &shared, None)
                                } else {
                                    ConnOutcome::Keep
                                }
                            }
                        };
                        match outcome {
                            ConnOutcome::Keep => {}
                            ConnOutcome::Close => to_close.push(token),
                            ConnOutcome::Detach(detach) => to_detach.push((token, detach)),
                        }
                    }
                }
            }
        }

        for (token, detach) in to_detach {
            detach_connection(&poll, &mut connections, token, detach);
        }

        for token in to_close {
            close_async_connection(&poll, &mut connections, token);
        }

        let idle: Vec<Token> = connections
            .iter()
            .filter_map(|(token, conn)| {
                if matches!(conn.state, AsyncConnState::Reading)
                    && now.duration_since(conn.last_active) > KEEP_ALIVE_TIMEOUT
                {
                    Some(*token)
                } else {
                    None
                }
            })
            .collect();
        for token in idle {
            close_async_connection(&poll, &mut connections, token);
        }
    }
}

fn dispatch_accepted_stream(
    reactors: &[ReactorTarget],
    next: &AtomicUsize,
    stream: StdTcpStream,
) -> bool {
    if reactors.is_empty() {
        return false;
    }

    let _ = stream.set_nodelay(true);
    let start = next.fetch_add(1, Ordering::Relaxed);
    let mut pending = Some(stream);
    for offset in 0..reactors.len() {
        let index = (start + offset) % reactors.len();
        let stream = pending.take().unwrap();
        match reactors[index].stream_tx.send(stream) {
            Ok(()) => {
                let _ = reactors[index].waker.wake();
                return true;
            }
            Err(err) => {
                pending = Some(err.0);
            }
        }
    }
    false
}

fn run_accept_loop(
    listener: StdTcpListener,
    reactors: Arc<Vec<ReactorTarget>>,
    next: Arc<AtomicUsize>,
) -> bool {
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if !dispatch_accepted_stream(&reactors, &next, stream) {
                    return false;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(err) => {
                eprintln!("bolide web: accept failed: {}", err);
                return false;
            }
        }
    }
}

fn run_multi_inline_server(app: *const BolideWebApp, host: *const c_char, port: i64) -> i64 {
    if app.is_null() || port < 0 || port > u16::MAX as i64 {
        return 0;
    }
    let Some(host) = cstr_to_str(host) else {
        return 0;
    };
    let app_ref = unsafe { &*app };
    let Some(addr) = resolve_listen_addr(host, port) else {
        eprintln!("bolide web: failed to resolve listen address {}:{}", host, port);
        return 0;
    };
    let listener = match bind_tcp_listener(addr) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("bolide web: failed to bind {}: {}", addr, err);
            return 0;
        }
    };

    let shared = Arc::new(app_ref.shared.clone());
    if let Some(settings) = &app_ref.tls {
        return run_tls_server(listener, shared, settings, None);
    }
    let workers = app_ref.workers.clamp(1, 1024);
    let mut reactors = Vec::with_capacity(workers);
    let mut _reactor_handles = Vec::with_capacity(workers);

    for _ in 0..workers {
        let (stream_tx, stream_rx) = mpsc::channel::<StdTcpStream>();
        let (waker_tx, waker_rx) = mpsc::channel::<Arc<Waker>>();
        let reactor_shared = shared.clone();
        let handle = thread::spawn(move || {
            run_inline_reactor(stream_rx, waker_tx, reactor_shared);
        });
        let Ok(waker) = waker_rx.recv_timeout(Duration::from_secs(2)) else {
            return 0;
        };
        reactors.push(ReactorTarget { stream_tx, waker });
        _reactor_handles.push(handle);
    }

    let reactors = Arc::new(reactors);
    let next = Arc::new(AtomicUsize::new(0));
    let acceptors = acceptor_count(workers);
    let mut _acceptor_handles = Vec::with_capacity(acceptors.saturating_sub(1));
    for _ in 1..acceptors {
        let Ok(listener_clone) = listener.try_clone() else {
            break;
        };
        let accept_reactors = reactors.clone();
        let accept_next = next.clone();
        _acceptor_handles.push(thread::spawn(move || {
            let _ = run_accept_loop(listener_clone, accept_reactors, accept_next);
        }));
    }

    if run_accept_loop(listener, reactors, next) {
        1
    } else {
        0
    }
}

fn run_single_inline_server(
    app: *const BolideWebApp,
    host: *const c_char,
    port: i64,
    max_requests: i64,
) -> i64 {
    if app.is_null() || port < 0 || port > u16::MAX as i64 {
        return 0;
    }
    let Some(host) = cstr_to_str(host) else {
        return 0;
    };
    let app_ref = unsafe { &*app };
    let Some(addr) = resolve_listen_addr(host, port) else {
        eprintln!("bolide web: failed to resolve listen address {}:{}", host, port);
        return 0;
    };
    let listener = match bind_tcp_listener(addr) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("bolide web: failed to bind {}: {}", addr, err);
            return 0;
        }
    };
    let shared = Arc::new(app_ref.shared.clone());
    if let Some(settings) = &app_ref.tls {
        return run_tls_server(listener, shared, settings, Some(max_requests));
    }
    let _ = listener.set_nonblocking(true);
    let mut listener = MioTcpListener::from_std(listener);
    let mut poll = match Poll::new() {
        Ok(poll) => poll,
        Err(_) => return 0,
    };
    if poll
        .registry()
        .register(&mut listener, SERVER_TOKEN, Interest::READABLE)
        .is_err()
    {
        return 0;
    }

    let finite = max_requests > 0;
    let mut request_budget = finite.then_some(RequestBudget {
        max: max_requests,
        started: 0,
    });
    let mut accepting = true;
    let mut next_token = FIRST_CONN_TOKEN;
    let mut events = Events::with_capacity(EVENT_CAPACITY);
    let mut connections: HashMap<Token, AsyncConnection> = HashMap::new();

    loop {
        if finite && !accepting && connections.is_empty() {
            break;
        }

        if poll
            .poll(&mut events, Some(Duration::from_millis(100)))
            .is_err()
        {
            break;
        }

        if accepting
            && request_budget
                .as_ref()
                .is_some_and(|budget| budget.exhausted())
        {
            let _ = poll.registry().deregister(&mut listener);
            accepting = false;
        }
        let now = Instant::now();
        let mut to_close = Vec::new();
        let mut to_detach = Vec::new();

        for event in events.iter() {
            match event.token() {
                SERVER_TOKEN if accepting => loop {
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            if request_budget
                                .as_ref()
                                .is_some_and(|budget| budget.exhausted())
                            {
                                break;
                            }
                            let _ = stream.set_nodelay(true);
                            let token = Token(next_token);
                            next_token = next_token.wrapping_add(1).max(FIRST_CONN_TOKEN);
                            let mut conn = AsyncConnection {
                                stream,
                                read_buf: Vec::with_capacity(4096),
                                write_buf: Vec::new(),
                                write_pos: 0,
                                state: AsyncConnState::Reading,
                                registered: false,
                                keep_alive: true,
                                last_active: now,
                            };
                            if register_conn(&poll, token, &mut conn, Interest::READABLE).is_ok() {
                                connections.insert(token, conn);
                            }
                        }
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            accepting = false;
                            break;
                        }
                    }
                },
                token => {
                    if let Some(conn) = connections.get_mut(&token) {
                        let outcome = match conn.state {
                            AsyncConnState::Reading => {
                                if event.is_readable() {
                                    read_inline_connection(
                                        &poll,
                                        token,
                                        conn,
                                        &shared,
                                        request_budget.as_mut(),
                                    )
                                } else {
                                    ConnOutcome::Keep
                                }
                            }
                            AsyncConnState::Writing => {
                                if event.is_writable() {
                                    write_inline_connection(
                                        &poll,
                                        token,
                                        conn,
                                        &shared,
                                        request_budget.as_mut(),
                                    )
                                } else {
                                    ConnOutcome::Keep
                                }
                            }
                        };
                        match outcome {
                            ConnOutcome::Keep => {}
                            ConnOutcome::Close => to_close.push(token),
                            ConnOutcome::Detach(detach) => to_detach.push((token, detach)),
                        }
                    }
                }
            }
        }

        if accepting
            && request_budget
                .as_ref()
                .is_some_and(|budget| budget.exhausted())
        {
            let _ = poll.registry().deregister(&mut listener);
            accepting = false;
        }

        for (token, detach) in to_detach {
            detach_connection(&poll, &mut connections, token, detach);
        }

        for token in to_close {
            close_async_connection(&poll, &mut connections, token);
        }

        let idle: Vec<Token> = connections
            .iter()
            .filter_map(|(token, conn)| {
                if matches!(conn.state, AsyncConnState::Reading)
                    && now.duration_since(conn.last_active) > KEEP_ALIVE_TIMEOUT
                {
                    Some(*token)
                } else {
                    None
                }
            })
            .collect();
        for token in idle {
            close_async_connection(&poll, &mut connections, token);
        }
    }

    request_budget.map_or(0, |budget| budget.started)
}

fn run_server(app: *const BolideWebApp, host: *const c_char, port: i64, max_requests: i64) -> i64 {
    if max_requests > 0 {
        run_single_inline_server(app, host, port, max_requests)
    } else {
        run_multi_inline_server(app, host, port)
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_app_new() -> *mut BolideWebApp {
    Box::into_raw(Box::new(BolideWebApp {
        routes: RouteTable::default(),
        shared: AppShared {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            ..AppShared::default()
        },
        workers: default_worker_count(),
        max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        tls: None,
    }))
}

#[no_mangle]
pub extern "C" fn bolide_web_app_free(app: *mut BolideWebApp) {
    if app.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(app));
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_app_set_workers(app: *mut BolideWebApp, workers: i64) {
    if app.is_null() {
        return;
    }
    unsafe {
        (*app).workers = workers.clamp(1, 1024) as usize;
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_app_set_max_body(app: *mut BolideWebApp, max_body_bytes: i64) {
    if app.is_null() {
        return;
    }
    unsafe {
        (*app).max_body_bytes = max_body_bytes.clamp(0, i64::MAX) as usize;
        (*app).shared.max_body_bytes = max_body_bytes.clamp(0, i64::MAX) as usize;
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_route(
    app: *mut BolideWebApp,
    method: *const c_char,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    let Some(method) = cstr_to_str(method) else {
        return 0;
    };
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    let Some(handler) = handler else {
        return 0;
    };

    unsafe {
        (*app).shared.routes.push(Route {
            method: method.to_ascii_uppercase(),
            path: path.to_string(),
            target: RouteTarget::Handler(handler),
        });
    }
    1
}

#[no_mangle]
pub extern "C" fn bolide_web_route_handler(
    app: *mut BolideWebApp,
    method: *const c_char,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    let Some(method) = cstr_to_str(method) else {
        return 0;
    };
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    let Some(handler) = handler else {
        return 0;
    };

    unsafe {
        (*app).shared.routes.push(Route {
            method: method.to_ascii_uppercase(),
            path: path.to_string(),
            target: RouteTarget::ObjectHandler(handler),
        });
    }
    1
}

#[no_mangle]
pub extern "C" fn bolide_web_route_async_handler(
    app: *mut BolideWebApp,
    method: *const c_char,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    bolide_web_route_handler(app, method, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_static(
    app: *mut BolideWebApp,
    url_prefix: *const c_char,
    dir: *const c_char,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    let Some(url_prefix) = cstr_to_str(url_prefix) else {
        return 0;
    };
    let Some(dir) = cstr_to_str(dir) else {
        return 0;
    };
    unsafe {
        (*app).shared.routes.push(Route {
            method: "GET".to_string(),
            path: normalize_url_prefix(url_prefix),
            target: RouteTarget::StaticDir {
                root: dir.to_string(),
            },
        });
    }
    1
}

fn push_route(
    app: *mut BolideWebApp,
    method: &str,
    path: *const c_char,
    target: RouteTarget,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    unsafe {
        (*app).shared.routes.push(Route {
            method: method.to_ascii_uppercase(),
            path: path.to_string(),
            target,
        });
    }
    1
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_route(
    app: *mut BolideWebApp,
    method: *const c_char,
    path: *const c_char,
    handler: Option<WebStreamHandler>,
) -> i64 {
    let method = cstr_to_str(method).unwrap_or("GET");
    match handler {
        Some(handler) => push_route(app, method, path, RouteTarget::StreamHandler(handler)),
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_sse(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebStreamHandler>,
) -> i64 {
    match handler {
        Some(handler) => push_route(app, "GET", path, RouteTarget::StreamHandler(handler)),
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_websocket(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebSocketHandler>,
) -> i64 {
    match handler {
        Some(handler) => push_route(app, "GET", path, RouteTarget::WebSocketHandler(handler)),
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_app_use_before(
    app: *mut BolideWebApp,
    handler: Option<WebObjectHandler>,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    match handler {
        Some(handler) => unsafe {
            (*app).shared.before.push(handler);
            1
        },
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_app_use_after(
    app: *mut BolideWebApp,
    handler: Option<WebAfterHandler>,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    match handler {
        Some(handler) => unsafe {
            (*app).shared.after.push(handler);
            1
        },
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_app_not_found(
    app: *mut BolideWebApp,
    handler: Option<WebObjectHandler>,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    unsafe {
        (*app).shared.not_found = handler;
    }
    1
}

#[no_mangle]
pub extern "C" fn bolide_web_app_error_handler(
    app: *mut BolideWebApp,
    handler: Option<WebObjectHandler>,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    unsafe {
        (*app).shared.error_handler = handler;
    }
    1
}

#[no_mangle]
pub extern "C" fn bolide_web_app_enable_compression(app: *mut BolideWebApp, enabled: i64) {
    if !app.is_null() {
        unsafe {
            (*app).shared.compression = enabled != 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_app_enable_cors(
    app: *mut BolideWebApp,
    origins: *const c_char,
    methods: *const c_char,
    headers: *const c_char,
    credentials: i64,
    max_age: i64,
) {
    if app.is_null() {
        return;
    }
    let config = cors::CorsConfig {
        origins: cstr_to_str(origins).unwrap_or("*").to_string(),
        methods: cstr_to_str(methods)
            .filter(|value| !value.is_empty())
            .unwrap_or("GET, POST, PUT, PATCH, DELETE, OPTIONS")
            .to_string(),
        headers: cstr_to_str(headers)
            .filter(|value| !value.is_empty())
            .unwrap_or("*")
            .to_string(),
        credentials: credentials != 0,
        max_age,
    };
    unsafe {
        (*app).shared.cors = Some(config);
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_app_tls(
    app: *mut BolideWebApp,
    pkcs12_path: *const c_char,
    password: *const c_char,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    let Some(path) = cstr_to_str(pkcs12_path) else {
        return 0;
    };
    let settings = TlsSettings {
        identity: TlsIdentity::Pkcs12 {
            path: path.to_string(),
            password: cstr_to_str(password).unwrap_or("").to_string(),
        },
    };
    if tls::build_acceptor(&settings).is_none() {
        return 0;
    }
    unsafe {
        (*app).tls = Some(settings);
    }
    1
}

#[no_mangle]
pub extern "C" fn bolide_web_app_tls_pkcs8(
    app: *mut BolideWebApp,
    cert_path: *const c_char,
    key_path: *const c_char,
) -> i64 {
    if app.is_null() {
        return 0;
    }
    let Some(cert_path) = cstr_to_str(cert_path) else {
        return 0;
    };
    let Some(key_path) = cstr_to_str(key_path) else {
        return 0;
    };
    let settings = TlsSettings {
        identity: TlsIdentity::Pkcs8 {
            cert_path: cert_path.to_string(),
            key_path: key_path.to_string(),
        },
    };
    if tls::build_acceptor(&settings).is_none() {
        return 0;
    }
    unsafe {
        (*app).tls = Some(settings);
    }
    1
}

#[no_mangle]
pub extern "C" fn bolide_web_get(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static GET: &[u8] = b"GET\0";
    bolide_web_route(app, GET.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_get_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static GET: &[u8] = b"GET\0";
    bolide_web_route_handler(app, GET.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_get_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static GET: &[u8] = b"GET\0";
    bolide_web_route_async_handler(app, GET.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_post(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static POST: &[u8] = b"POST\0";
    bolide_web_route(app, POST.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_post_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static POST: &[u8] = b"POST\0";
    bolide_web_route_handler(app, POST.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_post_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static POST: &[u8] = b"POST\0";
    bolide_web_route_async_handler(app, POST.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_put(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static PUT: &[u8] = b"PUT\0";
    bolide_web_route(app, PUT.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_put_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static PUT: &[u8] = b"PUT\0";
    bolide_web_route_handler(app, PUT.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_put_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static PUT: &[u8] = b"PUT\0";
    bolide_web_route_async_handler(app, PUT.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_patch(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static PATCH: &[u8] = b"PATCH\0";
    bolide_web_route(app, PATCH.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_patch_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static PATCH: &[u8] = b"PATCH\0";
    bolide_web_route_handler(app, PATCH.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_patch_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static PATCH: &[u8] = b"PATCH\0";
    bolide_web_route_async_handler(app, PATCH.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_delete(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static DELETE: &[u8] = b"DELETE\0";
    bolide_web_route(app, DELETE.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_delete_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static DELETE: &[u8] = b"DELETE\0";
    bolide_web_route_handler(app, DELETE.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_delete_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static DELETE: &[u8] = b"DELETE\0";
    bolide_web_route_async_handler(app, DELETE.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_head(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static HEAD: &[u8] = b"HEAD\0";
    bolide_web_route(app, HEAD.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_head_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static HEAD: &[u8] = b"HEAD\0";
    bolide_web_route_handler(app, HEAD.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_head_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static HEAD: &[u8] = b"HEAD\0";
    bolide_web_route_async_handler(app, HEAD.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_options(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static OPTIONS: &[u8] = b"OPTIONS\0";
    bolide_web_route(app, OPTIONS.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_options_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static OPTIONS: &[u8] = b"OPTIONS\0";
    bolide_web_route_handler(app, OPTIONS.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_options_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static OPTIONS: &[u8] = b"OPTIONS\0";
    bolide_web_route_async_handler(app, OPTIONS.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_trace(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static TRACE: &[u8] = b"TRACE\0";
    bolide_web_route(app, TRACE.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_trace_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static TRACE: &[u8] = b"TRACE\0";
    bolide_web_route_handler(app, TRACE.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_trace_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static TRACE: &[u8] = b"TRACE\0";
    bolide_web_route_async_handler(app, TRACE.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_connect(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebHandler>,
) -> i64 {
    static CONNECT: &[u8] = b"CONNECT\0";
    bolide_web_route(app, CONNECT.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_connect_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static CONNECT: &[u8] = b"CONNECT\0";
    bolide_web_route_handler(app, CONNECT.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_connect_async_handler(
    app: *mut BolideWebApp,
    path: *const c_char,
    handler: Option<WebObjectHandler>,
) -> i64 {
    static CONNECT: &[u8] = b"CONNECT\0";
    bolide_web_route_async_handler(app, CONNECT.as_ptr() as *const c_char, path, handler)
}

#[no_mangle]
pub extern "C" fn bolide_web_run(app: *const BolideWebApp, host: *const c_char, port: i64) -> i64 {
    run_server(app, host, port, 0)
}

#[no_mangle]
pub extern "C" fn bolide_web_serve(
    app: *const BolideWebApp,
    host: *const c_char,
    port: i64,
    max_requests: i64,
) -> i64 {
    run_server(app, host, port, max_requests)
}

#[no_mangle]
pub extern "C" fn bolide_web_app_handle(
    app: *const BolideWebApp,
    method: *const c_char,
    target: *const c_char,
    body: *const c_char,
) -> *mut BolideWebResponse {
    bolide_web_app_handle_with_headers(app, method, target, body, std::ptr::null())
}

#[no_mangle]
pub extern "C" fn bolide_web_app_handle_with_headers(
    app: *const BolideWebApp,
    method: *const c_char,
    target: *const c_char,
    body: *const c_char,
    headers: *const c_char,
) -> *mut BolideWebResponse {
    if app.is_null() {
        return boxed_response(500, "text/plain; charset=utf-8", "null app");
    }
    let method = cstr_to_str(method).unwrap_or("GET").to_ascii_uppercase();
    let target = cstr_to_str(target).unwrap_or("/").to_string();
    let body = cstr_to_str(body).unwrap_or("").as_bytes().to_vec();
    let headers = cstr_to_str(headers)
        .map(parse_header_lines)
        .unwrap_or_default();
    let (path, query) = split_target(&target);
    let req = BolideWebRequest {
        method,
        target,
        path,
        query,
        version: "HTTP/1.1".to_string(),
        headers,
        body,
        path_params: Vec::new(),
        multipart: std::cell::OnceCell::new(),
    };
    let res = dispatch_to_response(&unsafe { &*app }.shared, req);
    Box::into_raw(Box::new(res))
}

#[no_mangle]
pub extern "C" fn bolide_web_cookie_pair(set_cookie: *const c_char) -> *mut BolideString {
    let set_cookie = cstr_to_str(set_cookie).unwrap_or("");
    BolideString::new(&cookie_pair_from_set_cookie(set_cookie))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_method(req: *const BolideWebRequest) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    BolideString::new(&unsafe { &*req }.method)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_target(req: *const BolideWebRequest) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    BolideString::new(&unsafe { &*req }.target)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_path(req: *const BolideWebRequest) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    BolideString::new(&unsafe { &*req }.path)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_query(req: *const BolideWebRequest) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    BolideString::new(&unsafe { &*req }.query)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_version(req: *const BolideWebRequest) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    BolideString::new(&unsafe { &*req }.version)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_header(
    req: *const BolideWebRequest,
    name: *const c_char,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let Some(name) = cstr_to_str(name) else {
        return BolideString::new("");
    };
    let req = unsafe { &*req };
    BolideString::new(find_header(&req.headers, name).unwrap_or(""))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_header_str(
    req: *const BolideWebRequest,
    name: *const BolideString,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let req = unsafe { &*req };
    BolideString::new(find_header(&req.headers, bstr_to_str(name)).unwrap_or(""))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_cookie(
    req: *const BolideWebRequest,
    name: *const c_char,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let Some(name) = cstr_to_str(name) else {
        return BolideString::new("");
    };
    let req = unsafe { &*req };
    BolideString::new(&request_cookie(req, name))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_cookie_str(
    req: *const BolideWebRequest,
    name: *const BolideString,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let req = unsafe { &*req };
    BolideString::new(&request_cookie(req, bstr_to_str(name)))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_query_param(
    req: *const BolideWebRequest,
    name: *const c_char,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let Some(name) = cstr_to_str(name) else {
        return BolideString::new("");
    };
    let req = unsafe { &*req };
    BolideString::new(&query_param(&req.query, name))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_query_param_str(
    req: *const BolideWebRequest,
    name: *const BolideString,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let req = unsafe { &*req };
    BolideString::new(&query_param(&req.query, bstr_to_str(name)))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_form_param(
    req: *const BolideWebRequest,
    name: *const c_char,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let Some(name) = cstr_to_str(name) else {
        return BolideString::new("");
    };
    let req = unsafe { &*req };
    BolideString::new(&request_form_param(req, name))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_form_param_str(
    req: *const BolideWebRequest,
    name: *const BolideString,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let req = unsafe { &*req };
    BolideString::new(&request_form_param(req, bstr_to_str(name)))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_path_param(
    req: *const BolideWebRequest,
    name: *const c_char,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let Some(name) = cstr_to_str(name) else {
        return BolideString::new("");
    };
    let req = unsafe { &*req };
    let value = req
        .path_params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    BolideString::new(value)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_path_param_str(
    req: *const BolideWebRequest,
    name: *const BolideString,
) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let req = unsafe { &*req };
    let name = bstr_to_str(name);
    let value = req
        .path_params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    BolideString::new(value)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_body_text(req: *const BolideWebRequest) -> *mut BolideString {
    if req.is_null() {
        return BolideString::new("");
    }
    let req = unsafe { &*req };
    BolideString::new(&String::from_utf8_lossy(&req.body))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_body_bytes(req: *const BolideWebRequest) -> *mut BolideBytes {
    if req.is_null() {
        return BolideBytes::new();
    }
    let req = unsafe { &*req };
    BolideBytes::from_slice(&req.body)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_body_len(req: *const BolideWebRequest) -> i64 {
    if req.is_null() {
        return 0;
    }
    unsafe { (&*req).body.len() as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_web_response_new(
    status: i64,
    content_type: *const c_char,
    body: *const c_char,
) -> *mut BolideWebResponse {
    let content_type = cstr_to_str(content_type).unwrap_or("text/plain; charset=utf-8");
    let body = cstr_to_str(body).unwrap_or("");
    boxed_response(status, content_type, body.as_bytes().to_vec())
}

#[no_mangle]
pub extern "C" fn bolide_web_response_new_str(
    status: i64,
    content_type: *const BolideString,
    body: *const BolideString,
) -> *mut BolideWebResponse {
    boxed_response(
        status,
        bstr_to_str(content_type),
        bstr_to_str(body).as_bytes().to_vec(),
    )
}

#[no_mangle]
pub extern "C" fn bolide_web_text(body: *const c_char) -> *mut BolideWebResponse {
    let body = cstr_to_str(body).unwrap_or("");
    boxed_response(200, "text/plain; charset=utf-8", body.as_bytes().to_vec())
}

#[no_mangle]
pub extern "C" fn bolide_web_text_str(body: *const BolideString) -> *mut BolideWebResponse {
    boxed_response(
        200,
        "text/plain; charset=utf-8",
        bstr_to_str(body).as_bytes().to_vec(),
    )
}

#[no_mangle]
pub extern "C" fn bolide_web_html(body: *const c_char) -> *mut BolideWebResponse {
    let body = cstr_to_str(body).unwrap_or("");
    boxed_response(200, "text/html; charset=utf-8", body.as_bytes().to_vec())
}

#[no_mangle]
pub extern "C" fn bolide_web_html_str(body: *const BolideString) -> *mut BolideWebResponse {
    boxed_response(
        200,
        "text/html; charset=utf-8",
        bstr_to_str(body).as_bytes().to_vec(),
    )
}

#[no_mangle]
pub extern "C" fn bolide_web_json(body: *const c_char) -> *mut BolideWebResponse {
    let body = cstr_to_str(body).unwrap_or("");
    boxed_response(
        200,
        "application/json; charset=utf-8",
        body.as_bytes().to_vec(),
    )
}

#[no_mangle]
pub extern "C" fn bolide_web_json_str(body: *const BolideString) -> *mut BolideWebResponse {
    boxed_response(
        200,
        "application/json; charset=utf-8",
        bstr_to_str(body).as_bytes().to_vec(),
    )
}

#[no_mangle]
pub extern "C" fn bolide_web_bytes(
    body: *const BolideBytes,
    content_type: *const c_char,
) -> *mut BolideWebResponse {
    let content_type = cstr_to_str(content_type).unwrap_or("application/octet-stream");
    let body = if body.is_null() {
        Vec::new()
    } else {
        unsafe { (&*body).as_slice().to_vec() }
    };
    boxed_response(200, content_type, body)
}

#[no_mangle]
pub extern "C" fn bolide_web_empty(status: i64) -> *mut BolideWebResponse {
    boxed_response(status, "", Vec::new())
}

#[no_mangle]
pub extern "C" fn bolide_web_redirect(
    location: *const c_char,
    status: i64,
) -> *mut BolideWebResponse {
    let location = cstr_to_str(location).unwrap_or("/");
    let mut res = response(if status == 0 { 302 } else { status }, "", Vec::new());
    res.headers
        .push(("Location".to_string(), location.to_string()));
    Box::into_raw(Box::new(res))
}

#[no_mangle]
pub extern "C" fn bolide_web_redirect_str(
    location: *const BolideString,
    status: i64,
) -> *mut BolideWebResponse {
    let location = bstr_to_str(location);
    let mut res = response(if status == 0 { 302 } else { status }, "", Vec::new());
    res.headers
        .push(("Location".to_string(), location.to_string()));
    Box::into_raw(Box::new(res))
}

#[no_mangle]
pub extern "C" fn bolide_web_response_set_status(res: *mut BolideWebResponse, status: i64) {
    if res.is_null() {
        return;
    }
    unsafe {
        (*res).status = status;
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_response_set_header(
    res: *mut BolideWebResponse,
    name: *const c_char,
    value: *const c_char,
) {
    if res.is_null() {
        return;
    }
    let Some(name) = cstr_to_str(name) else {
        return;
    };
    let Some(value) = cstr_to_str(value) else {
        return;
    };
    unsafe {
        upsert_header(&mut (*res).headers, name, value.to_string());
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_response_set_header_str(
    res: *mut BolideWebResponse,
    name: *const BolideString,
    value: *const BolideString,
) {
    if res.is_null() {
        return;
    }
    unsafe {
        upsert_header(
            &mut (*res).headers,
            bstr_to_str(name),
            bstr_to_str(value).to_string(),
        );
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_response_set_cookie(
    res: *mut BolideWebResponse,
    name: *const c_char,
    value: *const c_char,
    path: *const c_char,
    max_age: i64,
    http_only: i64,
    same_site: *const c_char,
) {
    if res.is_null() {
        return;
    }
    let Some(name) = cstr_to_str(name) else {
        return;
    };
    let value = cstr_to_str(value).unwrap_or("");
    let path = cstr_to_str(path).unwrap_or("/");
    let same_site = cstr_to_str(same_site).unwrap_or("Lax");
    let header = cookie_header(name, value, path, max_age, http_only != 0, same_site);
    unsafe {
        add_header(&mut (*res).headers, "Set-Cookie", header);
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_response_delete_cookie(
    res: *mut BolideWebResponse,
    name: *const c_char,
    path: *const c_char,
) {
    if res.is_null() {
        return;
    }
    let Some(name) = cstr_to_str(name) else {
        return;
    };
    let path = cstr_to_str(path).unwrap_or("/");
    let header = cookie_header(name, "", path, 0, true, "Lax");
    unsafe {
        add_header(&mut (*res).headers, "Set-Cookie", header);
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_response_status(res: *const BolideWebResponse) -> i64 {
    if res.is_null() {
        return 0;
    }
    unsafe { (&*res).status }
}

#[no_mangle]
pub extern "C" fn bolide_web_response_header(
    res: *const BolideWebResponse,
    name: *const c_char,
) -> *mut BolideString {
    if res.is_null() {
        return BolideString::new("");
    }
    let Some(name) = cstr_to_str(name) else {
        return BolideString::new("");
    };
    let res = unsafe { &*res };
    BolideString::new(find_header(&res.headers, name).unwrap_or(""))
}

#[no_mangle]
pub extern "C" fn bolide_web_response_header_str(
    res: *const BolideWebResponse,
    name: *const BolideString,
) -> *mut BolideString {
    if res.is_null() {
        return BolideString::new("");
    }
    let res = unsafe { &*res };
    BolideString::new(find_header(&res.headers, bstr_to_str(name)).unwrap_or(""))
}

#[no_mangle]
pub extern "C" fn bolide_web_response_cookie_pair(
    res: *const BolideWebResponse,
    name: *const c_char,
) -> *mut BolideString {
    if res.is_null() {
        return BolideString::new("");
    }
    let Some(name) = cstr_to_str(name) else {
        return BolideString::new("");
    };
    let res = unsafe { &*res };
    for (header_name, header_value) in &res.headers {
        if !header_name.eq_ignore_ascii_case("Set-Cookie") {
            continue;
        }
        let pair = cookie_pair_from_set_cookie(header_value);
        let cookie_name = pair.split_once('=').map(|(key, _)| key).unwrap_or("");
        if cookie_name == name {
            return BolideString::new(&pair);
        }
    }
    BolideString::new("")
}

#[no_mangle]
pub extern "C" fn bolide_web_response_body_text(
    res: *const BolideWebResponse,
) -> *mut BolideString {
    if res.is_null() {
        return BolideString::new("");
    }
    let res = unsafe { &*res };
    BolideString::new(&String::from_utf8_lossy(&res.body))
}

#[no_mangle]
pub extern "C" fn bolide_web_response_body_bytes(
    res: *const BolideWebResponse,
) -> *mut BolideBytes {
    if res.is_null() {
        return BolideBytes::new();
    }
    let res = unsafe { &*res };
    BolideBytes::from_slice(&res.body)
}

#[no_mangle]
pub extern "C" fn bolide_web_response_free(res: *mut BolideWebResponse) {
    if res.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(res));
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_session(
    req: *const BolideWebRequest,
    res: *mut BolideWebResponse,
) -> *mut BolideWebSession {
    if req.is_null() {
        return std::ptr::null_mut();
    }
    let req = unsafe { &*req };
    let mut id = request_cookie(req, SESSION_COOKIE);
    let mut is_new = false;
    if id.is_empty() {
        id = new_session_id();
        is_new = true;
    }

    {
        let config = SESSION_CONFIG.lock().unwrap().clone();
        let mut sessions = SESSIONS.lock().unwrap();
        evict_sessions(&config, &mut sessions);
        let data = ensure_session(&mut sessions, &config, &id);
        touch_session(data, &config);
    }

    if is_new && !res.is_null() {
        let name = std::ffi::CString::new(SESSION_COOKIE).unwrap();
        let value = std::ffi::CString::new(id.as_str()).unwrap();
        let path = std::ffi::CString::new("/").unwrap();
        let same_site = std::ffi::CString::new("Lax").unwrap();
        bolide_web_response_set_cookie(
            res,
            name.as_ptr(),
            value.as_ptr(),
            path.as_ptr(),
            SESSION_CONFIG
                .lock()
                .unwrap()
                .ttl
                .map(|ttl| ttl.as_secs() as i64)
                .unwrap_or(-1),
            1,
            same_site.as_ptr(),
        );
    }

    Box::into_raw(Box::new(BolideWebSession { id }))
}

#[no_mangle]
pub extern "C" fn bolide_web_session_id(session: *const BolideWebSession) -> *mut BolideString {
    if session.is_null() {
        return BolideString::new("");
    }
    BolideString::new(&unsafe { &*session }.id)
}

#[no_mangle]
pub extern "C" fn bolide_web_session_get(
    session: *const BolideWebSession,
    key: *const c_char,
) -> *mut BolideDynamic {
    if session.is_null() {
        return crate::bolide_dynamic_none();
    }
    let Some(key) = cstr_to_str(key) else {
        return crate::bolide_dynamic_none();
    };
    let session = unsafe { &*session };
    let config = SESSION_CONFIG.lock().unwrap().clone();
    let mut sessions = SESSIONS.lock().unwrap();
    let data = ensure_session(&mut sessions, &config, &session.id);
    touch_session(data, &config);
    data.values
        .get(key)
        .map(|value| crate::bolide_dynamic_clone(value.0))
        .unwrap_or_else(|| crate::bolide_dynamic_none())
}

#[no_mangle]
pub extern "C" fn bolide_web_session_set(
    session: *mut BolideWebSession,
    key: *const c_char,
    value: *mut BolideDynamic,
) {
    if session.is_null() {
        return;
    }
    let Some(key) = cstr_to_str(key) else {
        return;
    };
    let session = unsafe { &*session };
    let cloned = crate::bolide_dynamic_clone(value);
    let config = SESSION_CONFIG.lock().unwrap().clone();
    let mut sessions = SESSIONS.lock().unwrap();
    let data = ensure_session(&mut sessions, &config, &session.id);
    touch_session(data, &config);
    data.values.insert(key.to_string(), StoredDynamic(cloned));
    flush_session(&config, &session.id, data);
}

#[no_mangle]
pub extern "C" fn bolide_web_session_contains(
    session: *const BolideWebSession,
    key: *const c_char,
) -> i64 {
    if session.is_null() {
        return 0;
    }
    let Some(key) = cstr_to_str(key) else {
        return 0;
    };
    let session = unsafe { &*session };
    let config = SESSION_CONFIG.lock().unwrap().clone();
    let mut sessions = SESSIONS.lock().unwrap();
    let data = ensure_session(&mut sessions, &config, &session.id);
    touch_session(data, &config);
    data.values.contains_key(key) as i64
}

#[no_mangle]
pub extern "C" fn bolide_web_session_remove(
    session: *mut BolideWebSession,
    key: *const c_char,
) -> i64 {
    if session.is_null() {
        return 0;
    }
    let Some(key) = cstr_to_str(key) else {
        return 0;
    };
    let session = unsafe { &*session };
    let config = SESSION_CONFIG.lock().unwrap().clone();
    let mut sessions = SESSIONS.lock().unwrap();
    let data = ensure_session(&mut sessions, &config, &session.id);
    touch_session(data, &config);
    let removed = data.values.remove(key).is_some();
    flush_session(&config, &session.id, data);
    removed as i64
}

#[no_mangle]
pub extern "C" fn bolide_web_session_clear(session: *mut BolideWebSession) {
    if session.is_null() {
        return;
    }
    let session = unsafe { &*session };
    let config = SESSION_CONFIG.lock().unwrap().clone();
    let mut sessions = SESSIONS.lock().unwrap();
    let data = ensure_session(&mut sessions, &config, &session.id);
    data.values.clear();
    touch_session(data, &config);
    flush_session(&config, &session.id, data);
}

#[no_mangle]
pub extern "C" fn bolide_web_session_destroy(
    session: *mut BolideWebSession,
    res: *mut BolideWebResponse,
) -> i64 {
    if session.is_null() {
        return 0;
    }
    let session = unsafe { &mut *session };
    let old_id = std::mem::take(&mut session.id);
    if old_id.is_empty() {
        return 0;
    }
    let config = SESSION_CONFIG.lock().unwrap().clone();
    let removed = SESSIONS.lock().unwrap().remove(&old_id).is_some();
    if let Some(path) = session_path(&config, &old_id) {
        let _ = fs::remove_file(path);
    }
    if !res.is_null() {
        let name = std::ffi::CString::new(SESSION_COOKIE).unwrap();
        let path = std::ffi::CString::new("/").unwrap();
        bolide_web_response_delete_cookie(res, name.as_ptr(), path.as_ptr());
    }
    removed as i64
}

#[no_mangle]
pub extern "C" fn bolide_web_session_regenerate(
    session: *mut BolideWebSession,
    res: *mut BolideWebResponse,
) -> *mut BolideString {
    if session.is_null() {
        return BolideString::new("");
    }
    let session = unsafe { &mut *session };
    let old_id = std::mem::replace(&mut session.id, new_session_id());
    let new_id = session.id.clone();

    {
        let config = SESSION_CONFIG.lock().unwrap().clone();
        let mut sessions = SESSIONS.lock().unwrap();
        let mut data = sessions
            .remove(&old_id)
            .or_else(|| load_session(&config, &old_id))
            .unwrap_or_else(|| new_session_data(&config));
        touch_session(&mut data, &config);
        if let Some(path) = session_path(&config, &old_id) {
            let _ = fs::remove_file(path);
        }
        flush_session(&config, &new_id, &data);
        sessions.insert(new_id.clone(), data);
    }

    if !res.is_null() {
        let name = std::ffi::CString::new(SESSION_COOKIE).unwrap();
        let value = std::ffi::CString::new(new_id.as_str()).unwrap();
        let path = std::ffi::CString::new("/").unwrap();
        let same_site = std::ffi::CString::new("Lax").unwrap();
        bolide_web_response_set_cookie(
            res,
            name.as_ptr(),
            value.as_ptr(),
            path.as_ptr(),
            SESSION_CONFIG
                .lock()
                .unwrap()
                .ttl
                .map(|ttl| ttl.as_secs() as i64)
                .unwrap_or(-1),
            1,
            same_site.as_ptr(),
        );
    }

    BolideString::new(&new_id)
}

#[no_mangle]
pub extern "C" fn bolide_web_session_set_ttl(ttl_seconds: i64) {
    let mut config = SESSION_CONFIG.lock().unwrap();
    config.ttl = if ttl_seconds > 0 {
        Some(Duration::from_secs(ttl_seconds as u64))
    } else {
        None
    };
}

#[no_mangle]
pub extern "C" fn bolide_web_session_config(
    ttl_seconds: i64,
    max_sessions: i64,
    dir: *const c_char,
) {
    let mut config = SESSION_CONFIG.lock().unwrap();
    config.ttl = if ttl_seconds > 0 {
        Some(Duration::from_secs(ttl_seconds as u64))
    } else {
        None
    };
    if max_sessions > 0 {
        config.max_sessions = max_sessions as usize;
    }
    let dir = cstr_to_str(dir).unwrap_or("");
    config.dir = if dir.is_empty() {
        None
    } else {
        Some(PathBuf::from(dir))
    };
}

#[no_mangle]
pub extern "C" fn bolide_web_session_free(session: *mut BolideWebSession) {
    if session.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(session));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_params_match() {
        let params = match_route("/users/{id}/posts/{post}", "/users/42/posts/abc").unwrap();
        assert_eq!(params[0], ("id".to_string(), "42".to_string()));
        assert_eq!(params[1], ("post".to_string(), "abc".to_string()));
        assert!(match_route("/users/{id}", "/users/42/posts").is_none());
    }

    #[test]
    fn wildcard_route_captures_rest() {
        let params = match_route("/files/*path", "/files/a/b/c.txt").unwrap();
        assert_eq!(params[0], ("path".to_string(), "a/b/c.txt".to_string()));
    }

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(query_param("name=Bolide+Web&x=1", "name"), "Bolide Web");
        assert_eq!(query_param("encoded=a%2Fb", "encoded"), "a/b");
    }
}
