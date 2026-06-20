//! Small blocking HTTP/HTTPS client.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::raw::c_char;
use std::time::Duration;

use native_tls::TlsConnector;

use super::{bstr_to_str, cstr_to_str, find_header, parse_header_lines};
use crate::{BolideBytes, BolideString};

#[repr(C)]
pub struct BolideWebClientResponse {
    status: i64,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn parse_url(url: &str) -> Option<(bool, String, u16, String)> {
    let (https, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return None;
    };
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().ok()?),
        None => (host_port.to_string(), if https { 443 } else { 80 }),
    };
    Some((https, host, port, format!("/{}", path)))
}

fn find_crlf(buf: &[u8], from: usize) -> Option<usize> {
    buf.get(from..)?
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|pos| from + pos)
}

fn decode_chunked_body(buf: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    let mut out = Vec::new();
    loop {
        let line_end = find_crlf(buf, pos)?;
        let line = std::str::from_utf8(&buf[pos..line_end]).ok()?;
        let size = usize::from_str_radix(line.split(';').next()?.trim(), 16).ok()?;
        pos = line_end + 2;
        if size == 0 {
            return Some(out);
        }
        if buf.len() < pos + size + 2 {
            return None;
        }
        out.extend_from_slice(&buf[pos..pos + size]);
        pos += size + 2;
    }
}

fn read_response(mut io: impl Read) -> std::io::Result<BolideWebClientResponse> {
    let mut data = Vec::new();
    io.read_to_end(&mut data)?;
    let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(BolideWebClientResponse {
            status: 0,
            headers: Vec::new(),
            body: data,
        });
    };
    let head = String::from_utf8_lossy(&data[..pos + 4]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = parse_header_lines(&lines.collect::<Vec<_>>().join("\n"));
    let body = if find_header(&headers, "transfer-encoding")
        .map(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
        })
        .unwrap_or(false)
    {
        decode_chunked_body(&data[pos + 4..]).unwrap_or_else(|| data[pos + 4..].to_vec())
    } else if let Some(len) =
        find_header(&headers, "content-length").and_then(|value| value.parse::<usize>().ok())
    {
        data[pos + 4..]
            .get(..len)
            .unwrap_or(&data[pos + 4..])
            .to_vec()
    } else {
        data[pos + 4..].to_vec()
    };
    Ok(BolideWebClientResponse {
        status,
        headers,
        body,
    })
}

fn request_once(
    method: &str,
    url: &str,
    body: &[u8],
    headers: &str,
    timeout: Duration,
) -> Option<BolideWebClientResponse> {
    let (https, host, port, path) = parse_url(url)?;
    let mut stream = TcpStream::connect((host.as_str(), port)).ok()?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n",
        method,
        path,
        host,
        body.len(),
        headers
    );
    if https {
        let connector = TlsConnector::new().ok()?;
        let mut tls = connector.connect(&host, stream).ok()?;
        tls.write_all(request.as_bytes()).ok()?;
        tls.write_all(body).ok()?;
        tls.flush().ok()?;
        read_response(tls).ok()
    } else {
        stream.write_all(request.as_bytes()).ok()?;
        stream.write_all(body).ok()?;
        stream.flush().ok()?;
        read_response(stream).ok()
    }
}

fn redirect_url(current: &str, location: &str) -> Option<String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Some(location.to_string());
    }
    let (https, host, port, _path) = parse_url(current)?;
    let scheme = if https { "https" } else { "http" };
    let authority = if (https && port == 443) || (!https && port == 80) {
        host
    } else {
        format!("{}:{}", host, port)
    };
    if location.starts_with('/') {
        Some(format!("{}://{}{}", scheme, authority, location))
    } else {
        Some(format!("{}://{}/{}", scheme, authority, location))
    }
}

fn fetch_with_options(
    method: &str,
    url: &str,
    body: &[u8],
    headers: &str,
    timeout_ms: i64,
    max_redirects: i64,
) -> Option<BolideWebClientResponse> {
    let timeout = if timeout_ms > 0 {
        Duration::from_millis(timeout_ms as u64)
    } else {
        Duration::from_secs(30)
    };
    let mut url = url.to_string();
    let mut method = method.to_ascii_uppercase();
    let mut body = body.to_vec();
    let max_redirects = max_redirects.clamp(0, 20);
    for redirect_count in 0..=max_redirects {
        let res = request_once(&method, &url, &body, headers, timeout)?;
        if !matches!(res.status, 301 | 302 | 303 | 307 | 308) || redirect_count == max_redirects {
            return Some(res);
        }
        let Some(location) = find_header(&res.headers, "location") else {
            return Some(res);
        };
        let Some(next_url) = redirect_url(&url, location) else {
            return Some(res);
        };
        if matches!(res.status, 301 | 302 | 303) && method != "GET" && method != "HEAD" {
            method = "GET".to_string();
            body.clear();
        }
        url = next_url;
    }
    None
}

fn fetch(method: &str, url: &str, body: &[u8], headers: &str) -> Option<BolideWebClientResponse> {
    fetch_with_options(method, url, body, headers, 30_000, 5)
}

#[no_mangle]
pub extern "C" fn bolide_web_fetch(
    method: *const c_char,
    url: *const c_char,
    body: *const c_char,
    headers: *const c_char,
) -> *mut BolideWebClientResponse {
    let method = cstr_to_str(method).unwrap_or("GET");
    let url = cstr_to_str(url).unwrap_or("");
    let body = cstr_to_str(body).unwrap_or("").as_bytes().to_vec();
    let headers = cstr_to_str(headers).unwrap_or("");
    fetch(method, url, &body, headers)
        .map(Box::new)
        .map(Box::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn bolide_web_fetch_str(
    method: *const BolideString,
    url: *const BolideString,
    body: *const BolideString,
    headers: *const BolideString,
) -> *mut BolideWebClientResponse {
    fetch(
        bstr_to_str(method),
        bstr_to_str(url),
        bstr_to_str(body).as_bytes(),
        bstr_to_str(headers),
    )
    .map(Box::new)
    .map(Box::into_raw)
    .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn bolide_web_fetch_with_options_str(
    method: *const BolideString,
    url: *const BolideString,
    body: *const BolideString,
    headers: *const BolideString,
    timeout_ms: i64,
    max_redirects: i64,
) -> *mut BolideWebClientResponse {
    fetch_with_options(
        bstr_to_str(method),
        bstr_to_str(url),
        bstr_to_str(body).as_bytes(),
        bstr_to_str(headers),
        timeout_ms,
        max_redirects,
    )
    .map(Box::new)
    .map(Box::into_raw)
    .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn bolide_web_client_response_status(res: *const BolideWebClientResponse) -> i64 {
    if res.is_null() {
        return 0;
    }
    unsafe { (*res).status }
}

#[no_mangle]
pub extern "C" fn bolide_web_client_response_header(
    res: *const BolideWebClientResponse,
    name: *const BolideString,
) -> *mut BolideString {
    if res.is_null() {
        return BolideString::new("");
    }
    BolideString::new(find_header(unsafe { &(*res).headers }, bstr_to_str(name)).unwrap_or(""))
}

#[no_mangle]
pub extern "C" fn bolide_web_client_response_body_text(
    res: *const BolideWebClientResponse,
) -> *mut BolideString {
    if res.is_null() {
        return BolideString::new("");
    }
    let text = unsafe { String::from_utf8_lossy(&(*res).body).into_owned() };
    BolideString::new(&text)
}

#[no_mangle]
pub extern "C" fn bolide_web_client_response_body_bytes(
    res: *const BolideWebClientResponse,
) -> *mut BolideBytes {
    if res.is_null() {
        return BolideBytes::new();
    }
    unsafe { BolideBytes::from_slice(&(*res).body) }
}

#[no_mangle]
pub extern "C" fn bolide_web_client_response_free(res: *mut BolideWebClientResponse) {
    if !res.is_null() {
        unsafe {
            drop(Box::from_raw(res));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let res = read_response(&raw[..]).unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.body, b"hello world");
    }

    #[test]
    fn resolves_relative_redirects() {
        assert_eq!(
            redirect_url("https://example.com/a/b", "/next").unwrap(),
            "https://example.com/next"
        );
        assert_eq!(
            redirect_url("http://example.com:8080/a", "next").unwrap(),
            "http://example.com:8080/next"
        );
    }
}
