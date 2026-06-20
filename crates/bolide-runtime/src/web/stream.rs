//! Blocking chunked streams and Server-Sent Events.

use std::io::Write;
use std::os::raw::c_char;

use super::{bstr_to_str, cstr_to_str, reason_phrase, upsert_header};
use crate::{BolideBytes, BolideString};

pub trait WebWrite: Write + Send {}
impl<T: Write + Send> WebWrite for T {}

pub struct BolideWebStream {
    writer: Box<dyn WebWrite>,
    status: i64,
    headers: Vec<(String, String)>,
    head_sent: bool,
    open: bool,
}

impl BolideWebStream {
    pub fn new(writer: Box<dyn WebWrite>) -> Self {
        Self {
            writer,
            status: 200,
            headers: Vec::new(),
            head_sent: false,
            open: true,
        }
    }

    fn send_head(&mut self) -> bool {
        if self.head_sent {
            return true;
        }
        self.head_sent = true;
        if !self
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"))
        {
            self.headers
                .push(("Transfer-Encoding".to_string(), "chunked".to_string()));
        }
        if !self
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("connection"))
        {
            self.headers
                .push(("Connection".to_string(), "close".to_string()));
        }
        let mut head = Vec::new();
        let _ = write!(
            head,
            "HTTP/1.1 {} {}\r\n",
            self.status,
            reason_phrase(self.status)
        );
        for (name, value) in &self.headers {
            head.extend_from_slice(name.as_bytes());
            head.extend_from_slice(b": ");
            head.extend_from_slice(value.as_bytes());
            head.extend_from_slice(b"\r\n");
        }
        head.extend_from_slice(b"\r\n");
        self.write_raw(&head)
    }

    fn write_raw(&mut self, data: &[u8]) -> bool {
        if !self.open {
            return false;
        }
        if self.writer.write_all(data).is_err() {
            self.open = false;
            return false;
        }
        true
    }

    pub fn write_chunk(&mut self, data: &[u8]) -> bool {
        if !self.send_head() {
            return false;
        }
        let mut prefix = Vec::new();
        let _ = write!(prefix, "{:x}\r\n", data.len());
        self.write_raw(&prefix) && self.write_raw(data) && self.write_raw(b"\r\n")
    }

    pub fn flush(&mut self) -> bool {
        self.writer.flush().is_ok()
    }

    pub fn end(&mut self) {
        if self.open {
            let _ = self.send_head();
            let _ = self.write_raw(b"0\r\n\r\n");
        }
        self.open = false;
    }
}

impl Drop for BolideWebStream {
    fn drop(&mut self) {
        self.end();
    }
}

fn with_stream<R>(
    stream: *mut BolideWebStream,
    f: impl FnOnce(&mut BolideWebStream) -> R,
) -> Option<R> {
    if stream.is_null() {
        return None;
    }
    Some(f(unsafe { &mut *stream }))
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_set_status(stream: *mut BolideWebStream, status: i64) {
    let _ = with_stream(stream, |s| {
        if !s.head_sent {
            s.status = status;
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_set_header(
    stream: *mut BolideWebStream,
    name: *const c_char,
    value: *const c_char,
) {
    let Some(name) = cstr_to_str(name) else {
        return;
    };
    let value = cstr_to_str(value).unwrap_or("");
    let _ = with_stream(stream, |s| {
        if !s.head_sent {
            upsert_header(&mut s.headers, name, value.to_string());
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_set_header_str(
    stream: *mut BolideWebStream,
    name: *const BolideString,
    value: *const BolideString,
) {
    let _ = with_stream(stream, |s| {
        if !s.head_sent {
            upsert_header(
                &mut s.headers,
                bstr_to_str(name),
                bstr_to_str(value).to_string(),
            );
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_write(
    stream: *mut BolideWebStream,
    data: *const c_char,
) -> i64 {
    let data = cstr_to_str(data).unwrap_or("");
    bolide_web_stream_write_bytes_raw(stream, data.as_bytes())
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_write_str(
    stream: *mut BolideWebStream,
    data: *const BolideString,
) -> i64 {
    bolide_web_stream_write_bytes_raw(stream, bstr_to_str(data).as_bytes())
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_write_bytes(
    stream: *mut BolideWebStream,
    data: *const BolideBytes,
) -> i64 {
    if data.is_null() {
        return bolide_web_stream_write_bytes_raw(stream, &[]);
    }
    bolide_web_stream_write_bytes_raw(stream, unsafe { (&*data).as_slice() })
}

fn bolide_web_stream_write_bytes_raw(stream: *mut BolideWebStream, data: &[u8]) -> i64 {
    with_stream(stream, |s| s.write_chunk(data)).unwrap_or(false) as i64
}

fn sse_payload(event: &str, data: &str, id: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(id) = id {
        if !id.is_empty() {
            out.push_str("id: ");
            out.push_str(id);
            out.push('\n');
        }
    }
    if !event.is_empty() {
        out.push_str("event: ");
        out.push_str(event);
        out.push('\n');
    }
    for line in data.lines() {
        out.push_str("data: ");
        out.push_str(line);
        out.push('\n');
    }
    if data.is_empty() {
        out.push_str("data:\n");
    }
    out.push('\n');
    out
}

fn ensure_sse_headers(stream: *mut BolideWebStream) {
    let _ = with_stream(stream, |s| {
        if !s.head_sent {
            upsert_header(
                &mut s.headers,
                "Content-Type",
                "text/event-stream".to_string(),
            );
            upsert_header(&mut s.headers, "Cache-Control", "no-cache".to_string());
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_sse(
    stream: *mut BolideWebStream,
    event: *const c_char,
    data: *const c_char,
) -> i64 {
    let event = cstr_to_str(event).unwrap_or("");
    let data = cstr_to_str(data).unwrap_or("");
    ensure_sse_headers(stream);
    bolide_web_stream_write_bytes_raw(stream, sse_payload(event, data, None).as_bytes())
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_sse_str(
    stream: *mut BolideWebStream,
    event: *const BolideString,
    data: *const BolideString,
) -> i64 {
    let payload = sse_payload(bstr_to_str(event), bstr_to_str(data), None);
    ensure_sse_headers(stream);
    bolide_web_stream_write_bytes_raw(stream, payload.as_bytes())
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_sse_event(
    stream: *mut BolideWebStream,
    id: *const BolideString,
    event: *const BolideString,
    data: *const BolideString,
) -> i64 {
    let payload = sse_payload(bstr_to_str(event), bstr_to_str(data), Some(bstr_to_str(id)));
    ensure_sse_headers(stream);
    bolide_web_stream_write_bytes_raw(stream, payload.as_bytes())
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_sse_comment(
    stream: *mut BolideWebStream,
    text: *const BolideString,
) -> i64 {
    let payload = format!(": {}\n\n", bstr_to_str(text));
    ensure_sse_headers(stream);
    bolide_web_stream_write_bytes_raw(stream, payload.as_bytes())
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_flush(stream: *mut BolideWebStream) -> i64 {
    with_stream(stream, |s| s.flush()).unwrap_or(false) as i64
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_end(stream: *mut BolideWebStream) {
    let _ = with_stream(stream, |s| s.end());
}

#[no_mangle]
pub extern "C" fn bolide_web_stream_is_open(stream: *mut BolideWebStream) -> i64 {
    with_stream(stream, |s| s.open).unwrap_or(false) as i64
}
