//! Buffered gzip response compression.

use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;

use super::{contains_header, find_header, upsert_header, BolideWebResponse};

const MIN_COMPRESS_BYTES: usize = 256;

fn compressible(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("javascript")
        || ct.contains("xml")
        || ct.contains("svg")
        || ct.contains("wasm")
}

pub fn maybe_compress(accept_encoding: &str, res: &mut BolideWebResponse) {
    if !accept_encoding
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("gzip"))
    {
        return;
    }
    if res.body.len() < MIN_COMPRESS_BYTES
        || contains_header(&res.headers, "content-encoding")
        || res.status == 204
        || res.status == 304
    {
        return;
    }
    let content_type = find_header(&res.headers, "content-type").unwrap_or("");
    if !compressible(content_type) {
        return;
    }

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    if encoder.write_all(&res.body).is_err() {
        return;
    }
    let Ok(body) = encoder.finish() else {
        return;
    };
    res.body = body;
    upsert_header(&mut res.headers, "Content-Encoding", "gzip".to_string());
    upsert_header(&mut res.headers, "Vary", "Accept-Encoding".to_string());
}
