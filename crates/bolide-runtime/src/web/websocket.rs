//! Blocking server-side WebSocket support (RFC 6455).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::raw::c_char;
use std::time::Duration;

use native_tls::TlsStream;

use super::{find_header, BolideWebRequest};
use crate::{BolideBytes, BolideString};

pub trait WebSocketIo: Read + Write + Send {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()>;
}

impl WebSocketIo for TcpStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
}

impl WebSocketIo for TlsStream<TcpStream> {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.get_ref().set_read_timeout(timeout)
    }
}

const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[repr(C)]
pub struct BolideWebSocketMessage {
    kind: i64,
    data: Vec<u8>,
}

pub struct BolideWebSocket {
    io: Box<dyn WebSocketIo>,
    open: bool,
}

impl BolideWebSocket {
    pub fn new(io: Box<dyn WebSocketIo>) -> Self {
        Self { io, open: true }
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> bool {
        if self.io.read_exact(buf).is_err() {
            self.open = false;
            return false;
        }
        true
    }

    fn read_message(&mut self) -> Option<BolideWebSocketMessage> {
        loop {
            let mut head = [0u8; 2];
            if !self.read_exact(&mut head) {
                return None;
            }
            let fin = head[0] & 0x80 != 0;
            let opcode = head[0] & 0x0f;
            let masked = head[1] & 0x80 != 0;
            let mut len = (head[1] & 0x7f) as u64;
            if len == 126 {
                let mut ext = [0u8; 2];
                if !self.read_exact(&mut ext) {
                    return None;
                }
                len = u16::from_be_bytes(ext) as u64;
            } else if len == 127 {
                let mut ext = [0u8; 8];
                if !self.read_exact(&mut ext) {
                    return None;
                }
                len = u64::from_be_bytes(ext);
            }
            if len > 64 * 1024 * 1024 {
                self.open = false;
                return None;
            }
            let mut mask = [0u8; 4];
            if masked && !self.read_exact(&mut mask) {
                return None;
            }
            let mut data = vec![0u8; len as usize];
            if !data.is_empty() && !self.read_exact(&mut data) {
                return None;
            }
            if masked {
                for (i, byte) in data.iter_mut().enumerate() {
                    *byte ^= mask[i % 4];
                }
            }
            if !fin && !matches!(opcode, 0x0 | 0x1 | 0x2) {
                self.open = false;
                return None;
            }
            match opcode {
                0x1 => return Some(BolideWebSocketMessage { kind: 1, data }),
                0x2 => return Some(BolideWebSocketMessage { kind: 2, data }),
                0x8 => {
                    self.open = false;
                    let _ = self.send_frame(0x8, &data);
                    return Some(BolideWebSocketMessage { kind: 8, data });
                }
                0x9 => {
                    let _ = self.send_frame(0xA, &data);
                    continue;
                }
                0xA => continue,
                _ => {
                    self.open = false;
                    return None;
                }
            }
        }
    }

    fn send_frame(&mut self, opcode: u8, data: &[u8]) -> bool {
        if !self.open && opcode != 0x8 {
            return false;
        }
        let mut frame = Vec::with_capacity(data.len() + 14);
        frame.push(0x80 | (opcode & 0x0f));
        if data.len() < 126 {
            frame.push(data.len() as u8);
        } else if data.len() <= u16::MAX as usize {
            frame.push(126);
            frame.extend_from_slice(&(data.len() as u16).to_be_bytes());
        } else {
            frame.push(127);
            frame.extend_from_slice(&(data.len() as u64).to_be_bytes());
        }
        frame.extend_from_slice(data);
        if self.io.write_all(&frame).is_err() || self.io.flush().is_err() {
            self.open = false;
            return false;
        }
        true
    }
}

pub fn handshake<T: Read + Write>(io: &mut T, req: &BolideWebRequest) -> std::io::Result<()> {
    let key = find_header(&req.headers, "sec-websocket-key").unwrap_or("");
    if key.is_empty()
        || !find_header(&req.headers, "upgrade")
            .unwrap_or("")
            .eq_ignore_ascii_case("websocket")
    {
        io.write_all(
            b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        )?;
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid websocket upgrade",
        ));
    }
    let mut accept_src = key.as_bytes().to_vec();
    accept_src.extend_from_slice(WS_GUID);
    let accept = super::crypto::base64_encode(&super::crypto::sha1(&accept_src));
    write!(
        io,
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept
    )?;
    io.flush()
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_recv(ws: *mut BolideWebSocket) -> *mut BolideWebSocketMessage {
    if ws.is_null() {
        return std::ptr::null_mut();
    }
    match unsafe { &mut *ws }.read_message() {
        Some(msg) => Box::into_raw(Box::new(msg)),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_recv_text(ws: *mut BolideWebSocket) -> *mut BolideString {
    let msg = bolide_web_ws_recv(ws);
    if msg.is_null() {
        return BolideString::new("");
    }
    let text = unsafe { String::from_utf8_lossy(&(*msg).data).into_owned() };
    bolide_web_ws_message_free(msg);
    BolideString::new(&text)
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_message_kind(msg: *const BolideWebSocketMessage) -> i64 {
    if msg.is_null() {
        return 0;
    }
    unsafe { (*msg).kind }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_message_text(
    msg: *const BolideWebSocketMessage,
) -> *mut BolideString {
    if msg.is_null() {
        return BolideString::new("");
    }
    let text = unsafe { String::from_utf8_lossy(&(*msg).data).into_owned() };
    BolideString::new(&text)
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_message_bytes(
    msg: *const BolideWebSocketMessage,
) -> *mut BolideBytes {
    if msg.is_null() {
        return BolideBytes::new();
    }
    unsafe { BolideBytes::from_slice(&(*msg).data) }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_message_free(msg: *mut BolideWebSocketMessage) {
    if !msg.is_null() {
        unsafe {
            drop(Box::from_raw(msg));
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_send_text(ws: *mut BolideWebSocket, text: *const c_char) -> i64 {
    let text = if text.is_null() {
        ""
    } else {
        unsafe { std::ffi::CStr::from_ptr(text).to_str().unwrap_or("") }
    };
    bolide_web_ws_send_text_raw(ws, text.as_bytes())
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_send_text_str(
    ws: *mut BolideWebSocket,
    text: *const BolideString,
) -> i64 {
    let text = if text.is_null() {
        ""
    } else {
        unsafe { (&*text).as_str() }
    };
    bolide_web_ws_send_text_raw(ws, text.as_bytes())
}

fn bolide_web_ws_send_text_raw(ws: *mut BolideWebSocket, data: &[u8]) -> i64 {
    if ws.is_null() {
        return 0;
    }
    unsafe { (&mut *ws).send_frame(0x1, data) as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_send_binary(
    ws: *mut BolideWebSocket,
    data: *const BolideBytes,
) -> i64 {
    if ws.is_null() {
        return 0;
    }
    let data = if data.is_null() {
        &[][..]
    } else {
        unsafe { (&*data).as_slice() }
    };
    unsafe { (&mut *ws).send_frame(0x2, data) as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_ping(ws: *mut BolideWebSocket) -> i64 {
    if ws.is_null() {
        return 0;
    }
    unsafe { (&mut *ws).send_frame(0x9, &[]) as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_close(
    ws: *mut BolideWebSocket,
    code: i64,
    reason: *const c_char,
) -> i64 {
    if ws.is_null() {
        return 0;
    }
    let reason = if reason.is_null() {
        ""
    } else {
        unsafe { std::ffi::CStr::from_ptr(reason).to_str().unwrap_or("") }
    };
    let mut data = Vec::new();
    if code > 0 {
        data.extend_from_slice(&(code as u16).to_be_bytes());
    }
    data.extend_from_slice(reason.as_bytes());
    unsafe {
        let ok = (&mut *ws).send_frame(0x8, &data);
        (&mut *ws).open = false;
        ok as i64
    }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_is_open(ws: *const BolideWebSocket) -> i64 {
    if ws.is_null() {
        return 0;
    }
    unsafe { (*ws).open as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_web_ws_set_read_timeout(ws: *mut BolideWebSocket, millis: i64) {
    if ws.is_null() {
        return;
    }
    let timeout = if millis > 0 {
        Some(Duration::from_millis(millis as u64))
    } else {
        None
    };
    let _ = unsafe { (&mut *ws).io.set_read_timeout(timeout) };
}
