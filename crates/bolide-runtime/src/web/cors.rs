//! CORS helpers and app-level preflight support.

use std::os::raw::c_char;

use super::{
    cstr_to_str, find_header, response, upsert_header, BolideWebRequest, BolideWebResponse,
};

#[derive(Clone, Default)]
pub struct CorsConfig {
    pub origins: String,
    pub methods: String,
    pub headers: String,
    pub credentials: bool,
    pub max_age: i64,
}

fn origin_allowed(config: &CorsConfig, origin: &str) -> bool {
    config.origins == "*"
        || config
            .origins
            .split(',')
            .map(str::trim)
            .any(|allowed| allowed == origin)
}

fn response_origin(config: &CorsConfig, req: &BolideWebRequest) -> Option<String> {
    let origin = find_header(&req.headers, "origin").unwrap_or("");
    if origin.is_empty() {
        return None;
    }
    if config.origins == "*" && !config.credentials {
        Some("*".to_string())
    } else if origin_allowed(config, origin) {
        Some(origin.to_string())
    } else {
        None
    }
}

pub fn is_preflight(req: &BolideWebRequest) -> bool {
    req.method == "OPTIONS"
        && find_header(&req.headers, "origin").is_some()
        && find_header(&req.headers, "access-control-request-method").is_some()
}

pub fn apply_cors(config: &CorsConfig, req: &BolideWebRequest, res: &mut BolideWebResponse) {
    let Some(origin) = response_origin(config, req) else {
        return;
    };
    upsert_header(&mut res.headers, "Access-Control-Allow-Origin", origin);
    if config.credentials {
        upsert_header(
            &mut res.headers,
            "Access-Control-Allow-Credentials",
            "true".to_string(),
        );
    }
    upsert_header(&mut res.headers, "Vary", "Origin".to_string());
}

pub fn preflight_response(config: &CorsConfig, req: &BolideWebRequest) -> BolideWebResponse {
    let mut res = response(204, "", Vec::new());
    if let Some(origin) = response_origin(config, req) {
        upsert_header(&mut res.headers, "Access-Control-Allow-Origin", origin);
    }
    upsert_header(
        &mut res.headers,
        "Access-Control-Allow-Methods",
        config.methods.clone(),
    );
    let headers = if config.headers == "*" {
        find_header(&req.headers, "access-control-request-headers")
            .unwrap_or("*")
            .to_string()
    } else {
        config.headers.clone()
    };
    upsert_header(&mut res.headers, "Access-Control-Allow-Headers", headers);
    if config.credentials {
        upsert_header(
            &mut res.headers,
            "Access-Control-Allow-Credentials",
            "true".to_string(),
        );
    }
    if config.max_age >= 0 {
        upsert_header(
            &mut res.headers,
            "Access-Control-Max-Age",
            config.max_age.to_string(),
        );
    }
    res
}

#[no_mangle]
pub extern "C" fn bolide_web_response_cors(
    res: *mut BolideWebResponse,
    origin: *const c_char,
    methods: *const c_char,
    headers: *const c_char,
    credentials: i64,
) {
    if res.is_null() {
        return;
    }
    unsafe {
        upsert_header(
            &mut (*res).headers,
            "Access-Control-Allow-Origin",
            cstr_to_str(origin).unwrap_or("*").to_string(),
        );
        upsert_header(
            &mut (*res).headers,
            "Access-Control-Allow-Methods",
            cstr_to_str(methods)
                .unwrap_or("GET, POST, OPTIONS")
                .to_string(),
        );
        upsert_header(
            &mut (*res).headers,
            "Access-Control-Allow-Headers",
            cstr_to_str(headers).unwrap_or("*").to_string(),
        );
        if credentials != 0 {
            upsert_header(
                &mut (*res).headers,
                "Access-Control-Allow-Credentials",
                "true".to_string(),
            );
        }
    }
}
