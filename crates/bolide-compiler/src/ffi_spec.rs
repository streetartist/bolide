pub(crate) const LINK_LIB_PREFIX: &str = "lib:";
pub(crate) const DYNAMIC_LIB_PREFIX: &str = "dyn:";

pub(crate) fn is_dynamic_lib_spec(lib: &str) -> bool {
    lib.starts_with(DYNAMIC_LIB_PREFIX)
}

pub(crate) fn validate_extern_lib_spec(lib: &str) -> Result<(), String> {
    if lib == "bolide" || lib.starts_with("std:") {
        return Ok(());
    }

    if let Some(name) = lib.strip_prefix(LINK_LIB_PREFIX) {
        return validate_logical_lib_name(lib, name);
    }

    if let Some(name) = lib.strip_prefix(DYNAMIC_LIB_PREFIX) {
        return validate_logical_lib_name(lib, name);
    }

    if is_shared_library_path(lib) {
        return Err(format!(
            "extern \"{}\" is a platform shared library path. Use `dyn:name` for dynamic loading or `lib:name` for native linking.",
            lib
        ));
    }

    Ok(())
}

pub(crate) fn resolve_dynamic_lib_spec(lib: &str) -> Result<String, String> {
    let name = lib.strip_prefix(DYNAMIC_LIB_PREFIX).ok_or_else(|| {
        format!(
            "extern \"{}\" is not dynamic. Use `dyn:name` for runtime dynamic loading.",
            lib
        )
    })?;
    validate_logical_lib_name(lib, name)?;

    Ok(match name {
        "c" => platform_c_dynamic_lib().to_string(),
        "m" => platform_math_dynamic_lib().to_string(),
        other => platform_dynamic_lib_name(other),
    })
}

fn validate_logical_lib_name(spec: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("extern \"{}\" is missing a library name", spec));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(format!(
            "extern \"{}\" contains a path separator. Use a logical library name such as `lib:m` or `dyn:sqlite3`.",
            spec
        ));
    }
    if is_shared_library_path(name) {
        return Err(format!(
            "extern \"{}\" contains a platform shared library suffix. Use the logical name without .dll/.so/.dylib.",
            spec
        ));
    }
    Ok(())
}

fn is_shared_library_path(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".dll") || lower.ends_with(".so") || lower.ends_with(".dylib")
}

fn platform_dynamic_lib_name(name: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("{}.dll", name)
    }

    #[cfg(target_os = "macos")]
    {
        format!("lib{}.dylib", name)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        format!("lib{}.so", name)
    }

    #[cfg(not(any(target_os = "windows", unix)))]
    {
        name.to_string()
    }
}

fn platform_c_dynamic_lib() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "msvcrt.dll"
    }

    #[cfg(target_os = "linux")]
    {
        "libc.so.6"
    }

    #[cfg(target_os = "macos")]
    {
        "libSystem.B.dylib"
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        "libc.so"
    }
}

fn platform_math_dynamic_lib() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "msvcrt.dll"
    }

    #[cfg(target_os = "linux")]
    {
        "libm.so.6"
    }

    #[cfg(target_os = "macos")]
    {
        "libSystem.B.dylib"
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        "libm.so"
    }
}
