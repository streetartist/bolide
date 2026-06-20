//! Math helpers for the Bolide standard library.

#[no_mangle]
pub extern "C" fn bolide_math_abs_i64(value: i64) -> i64 {
    value.saturating_abs()
}

#[no_mangle]
pub extern "C" fn bolide_math_min_i64(a: i64, b: i64) -> i64 {
    a.min(b)
}

#[no_mangle]
pub extern "C" fn bolide_math_max_i64(a: i64, b: i64) -> i64 {
    a.max(b)
}

#[no_mangle]
pub extern "C" fn bolide_math_clamp_i64(value: i64, min: i64, max: i64) -> i64 {
    if min > max {
        return value.clamp(max, min);
    }
    value.clamp(min, max)
}

#[no_mangle]
pub extern "C" fn bolide_math_abs_f64(value: f64) -> f64 {
    value.abs()
}

#[no_mangle]
pub extern "C" fn bolide_math_min_f64(a: f64, b: f64) -> f64 {
    a.min(b)
}

#[no_mangle]
pub extern "C" fn bolide_math_max_f64(a: f64, b: f64) -> f64 {
    a.max(b)
}

#[no_mangle]
pub extern "C" fn bolide_math_clamp_f64(value: f64, min: f64, max: f64) -> f64 {
    if min > max {
        return value.clamp(max, min);
    }
    value.clamp(min, max)
}

#[no_mangle]
pub extern "C" fn bolide_math_floor(value: f64) -> f64 {
    value.floor()
}

#[no_mangle]
pub extern "C" fn bolide_math_ceil(value: f64) -> f64 {
    value.ceil()
}

#[no_mangle]
pub extern "C" fn bolide_math_round(value: f64) -> f64 {
    value.round()
}

#[no_mangle]
pub extern "C" fn bolide_math_trunc(value: f64) -> f64 {
    value.trunc()
}

#[no_mangle]
pub extern "C" fn bolide_math_sqrt(value: f64) -> f64 {
    value.sqrt()
}

#[no_mangle]
pub extern "C" fn bolide_math_pow(value: f64, exp: f64) -> f64 {
    value.powf(exp)
}

#[no_mangle]
pub extern "C" fn bolide_math_sin(value: f64) -> f64 {
    value.sin()
}

#[no_mangle]
pub extern "C" fn bolide_math_cos(value: f64) -> f64 {
    value.cos()
}

#[no_mangle]
pub extern "C" fn bolide_math_tan(value: f64) -> f64 {
    value.tan()
}

#[no_mangle]
pub extern "C" fn bolide_math_log(value: f64, base: f64) -> f64 {
    value.log(base)
}

#[no_mangle]
pub extern "C" fn bolide_math_ln(value: f64) -> f64 {
    value.ln()
}

#[no_mangle]
pub extern "C" fn bolide_math_exp(value: f64) -> f64 {
    value.exp()
}
