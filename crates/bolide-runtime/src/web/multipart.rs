//! Lazy `multipart/form-data` parsing for uploads.

use std::fs;
use std::os::raw::c_char;

use super::{bstr_to_str, cstr_to_str, find_header, BolideWebRequest};
use crate::{BolideBytes, BolideString};

#[derive(Clone, Debug)]
pub struct MultipartPart {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: String,
    pub data: Vec<u8>,
}

fn boundary(content_type: &str) -> Option<String> {
    for part in content_type.split(';').skip(1) {
        let part = part.trim();
        let Some(value) = part.strip_prefix("boundary=") else {
            continue;
        };
        return Some(value.trim_matches('"').to_string());
    }
    None
}

fn header_value(headers: &[(String, String)], name: &str) -> String {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

fn disposition_attr(disposition: &str, attr: &str) -> Option<String> {
    for part in disposition.split(';').skip(1) {
        let (key, value) = part.trim().split_once('=')?;
        if key.trim().eq_ignore_ascii_case(attr) {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|pos| from + pos)
}

pub fn parse_multipart(req: &BolideWebRequest) -> Vec<MultipartPart> {
    let content_type = find_header(&req.headers, "content-type").unwrap_or("");
    if !content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    {
        return Vec::new();
    }
    let Some(boundary) = boundary(content_type) else {
        return Vec::new();
    };

    let marker = format!("--{}", boundary).into_bytes();
    let end_marker = format!("--{}--", boundary).into_bytes();
    let mut parts = Vec::new();
    let mut pos = 0usize;

    while let Some(start) = find_bytes(&req.body, &marker, pos) {
        if req.body[start..].starts_with(&end_marker) {
            break;
        }
        let mut part_start = start + marker.len();
        if req.body.get(part_start..part_start + 2) == Some(b"\r\n") {
            part_start += 2;
        }
        let Some(header_end) = find_bytes(&req.body, b"\r\n\r\n", part_start) else {
            break;
        };
        let headers_text = String::from_utf8_lossy(&req.body[part_start..header_end]);
        let mut headers = Vec::new();
        for line in headers_text.split("\r\n") {
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }

        let data_start = header_end + 4;
        let next_marker = find_bytes(&req.body, &marker, data_start).unwrap_or(req.body.len());
        let data_end = next_marker.saturating_sub(2);
        let disposition = header_value(&headers, "content-disposition");
        let Some(name) = disposition_attr(&disposition, "name") else {
            pos = next_marker;
            continue;
        };
        let filename = disposition_attr(&disposition, "filename").filter(|s| !s.is_empty());
        let content_type = header_value(&headers, "content-type");
        parts.push(MultipartPart {
            name,
            filename,
            content_type,
            data: req.body[data_start..data_end.min(req.body.len())].to_vec(),
        });
        pos = next_marker;
    }
    parts
}

fn part<'a>(req: *const BolideWebRequest, index: i64) -> Option<&'a MultipartPart> {
    if req.is_null() || index < 0 {
        return None;
    }
    let req = unsafe { &*req };
    req.multipart_parts().get(index as usize)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_count(req: *const BolideWebRequest) -> i64 {
    if req.is_null() {
        return 0;
    }
    unsafe { (&*req).multipart_parts().len() as i64 }
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_name(
    req: *const BolideWebRequest,
    index: i64,
) -> *mut BolideString {
    BolideString::new(part(req, index).map(|p| p.name.as_str()).unwrap_or(""))
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_filename(
    req: *const BolideWebRequest,
    index: i64,
) -> *mut BolideString {
    BolideString::new(
        part(req, index)
            .and_then(|p| p.filename.as_deref())
            .unwrap_or(""),
    )
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_content_type(
    req: *const BolideWebRequest,
    index: i64,
) -> *mut BolideString {
    BolideString::new(
        part(req, index)
            .map(|p| p.content_type.as_str())
            .unwrap_or(""),
    )
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_text(
    req: *const BolideWebRequest,
    index: i64,
) -> *mut BolideString {
    let text = part(req, index)
        .map(|p| String::from_utf8_lossy(&p.data).into_owned())
        .unwrap_or_default();
    BolideString::new(&text)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_bytes(
    req: *const BolideWebRequest,
    index: i64,
) -> *mut BolideBytes {
    part(req, index)
        .map(|p| BolideBytes::from_slice(&p.data))
        .unwrap_or_else(BolideBytes::new)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_len(
    req: *const BolideWebRequest,
    index: i64,
) -> i64 {
    part(req, index).map(|p| p.data.len() as i64).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_index(
    req: *const BolideWebRequest,
    field: *const BolideString,
) -> i64 {
    if req.is_null() {
        return -1;
    }
    let field = bstr_to_str(field);
    unsafe { &*req }
        .multipart_parts()
        .iter()
        .position(|p| p.name == field)
        .map(|i| i as i64)
        .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn bolide_web_request_multipart_save(
    req: *const BolideWebRequest,
    index: i64,
    path: *const c_char,
) -> i64 {
    let Some(path) = cstr_to_str(path) else {
        return 0;
    };
    let Some(part) = part(req, index) else {
        return 0;
    };
    fs::write(path, &part.data).is_ok() as i64
}
