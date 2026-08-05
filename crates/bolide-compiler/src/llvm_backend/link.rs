//! Invoke system clang/lld to compile LLVM IR and link bolide_runtime.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn require_clang() -> Result<PathBuf, String> {
    which("clang")
        .or_else(|_| which("clang.exe"))
        .map_err(|_| {
            "LLVM backend requires `clang` on PATH (e.g. LLVM install)".into()
        })
}

fn which(name: &str) -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("BOLIDE_CLANG") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Ok(pb);
        }
    }
    #[cfg(target_os = "windows")]
    {
        for c in [
            r"D:\Program Files\LLVM\bin\clang.exe",
            r"C:\Program Files\LLVM\bin\clang.exe",
        ] {
            let p = PathBuf::from(c);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return Ok(p);
            }
            #[cfg(target_os = "windows")]
            {
                let p2 = dir.join(format!("{}.exe", name.trim_end_matches(".exe")));
                if p2.is_file() {
                    return Ok(p2);
                }
            }
        }
    }
    Err(format!("`{}` not found on PATH", name))
}

fn find_runtime_lib() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let names = ["bolide_runtime.lib"];
    #[cfg(not(target_os = "windows"))]
    let names = ["libbolide_runtime.a"];

    // Same priority as the Cranelift path (CLI find_runtime_lib): the exe's own
    // directory and its parent/deps first, then BOLIDE_HOME, then the CWD tree
    // as a fallback. Searching the CWD first meant a bolide.exe copied elsewhere
    // failed to find the runtime next to itself.
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
            roots.push(dir.join(".."));
            roots.push(dir.join("deps"));
            roots.push(dir.join("..").join("deps"));
        }
    }
    if let Ok(home) = std::env::var("BOLIDE_HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join("target").join("release"));
        roots.push(home.join("target").join("debug"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut p = cwd;
        for _ in 0..5 {
            roots.push(p.join("target").join("release"));
            roots.push(p.join("target").join("debug"));
            if !p.pop() {
                break;
            }
        }
    }

    for root in &roots {
        for n in &names {
            let p = root.join(n);
            if p.is_file() {
                return Ok(p);
            }
        }
        for sub in [root.clone(), root.join("deps")] {
            if let Ok(rd) = std::fs::read_dir(&sub) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    #[cfg(target_os = "windows")]
                    let ok = name.starts_with("bolide_runtime-") && name.ends_with(".lib");
                    #[cfg(not(target_os = "windows"))]
                    let ok = name.starts_with("libbolide_runtime-") && name.ends_with(".a");
                    if ok {
                        return Ok(e.path());
                    }
                }
            }
        }
    }
    Err(
        "bolide_runtime library not found; build with `cargo build -p bolide-runtime --release` and set BOLIDE_HOME"
            .into(),
    )
}

pub fn compile_ir_to_object(ir: &str) -> Result<Vec<u8>, String> {
    let clang = require_clang()?;
    let tmp = std::env::temp_dir().join(format!("bolide_llvm_{}.ll", std::process::id()));
    let obj = tmp.with_extension("o");
    std::fs::write(&tmp, ir).map_err(|e| e.to_string())?;
    let status = Command::new(&clang)
        .args(["-O2", "-c", "-Wno-override-module"])
        .arg(&tmp)
        .arg("-o")
        .arg(&obj)
        .status()
        .map_err(|e| format!("failed to run clang: {}", e))?;
    if !status.success() {
        let keep = std::env::temp_dir().join("bolide_llvm_last_fail.ll");
        let _ = std::fs::copy(&tmp, &keep);
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "clang failed to compile generated LLVM IR (IR saved to {})",
            keep.display()
        ));
    }
    let _ = std::fs::remove_file(&tmp);
    let bytes = std::fs::read(&obj).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&obj);
    Ok(bytes)
}

pub fn compile_and_link_exe(ir: &str, output_exe: &Path) -> Result<(), String> {
    let clang = require_clang()?;
    let runtime = find_runtime_lib()?;
    let tmp_ll = std::env::temp_dir().join(format!("bolide_llvm_{}.ll", std::process::id()));
    let tmp_o = tmp_ll.with_extension("o");
    std::fs::write(&tmp_ll, ir).map_err(|e| e.to_string())?;
    if let Ok(dump) = std::env::var("BOLIDE_LLVM_DUMP") {
        let _ = std::fs::write(&dump, ir);
    }

    // 1) IR → object
    let status = Command::new(&clang)
        .args(["-O2", "-c", "-Wno-override-module"])
        .arg(&tmp_ll)
        .arg("-o")
        .arg(&tmp_o)
        .status()
        .map_err(|e| format!("failed to run clang -c: {}", e))?;
    if !status.success() {
        let keep = std::env::temp_dir().join("bolide_llvm_last_fail.ll");
        let _ = std::fs::copy(&tmp_ll, &keep);
        let _ = std::fs::remove_file(&tmp_ll);
        return Err(format!(
            "clang failed to compile generated LLVM IR to object (IR saved to {})",
            keep.display()
        ));
    }
    let _ = std::fs::remove_file(&tmp_ll);

    // 2) link (match Cranelift AOT Windows system libs so bolide_runtime resolves)
    #[cfg(target_os = "windows")]
    {
        let status = Command::new("lld-link")
            .arg("/ENTRY:main")
            .arg("/SUBSYSTEM:CONSOLE")
            .arg(format!("/OUT:{}", output_exe.display()))
            .arg(&tmp_o)
            .arg(&runtime)
            .arg("kernel32.lib")
            .arg("msvcrt.lib")
            .arg("ucrt.lib")
            .arg("vcruntime.lib")
            .arg("libcmt.lib")
            .arg("ws2_32.lib")
            .arg("userenv.lib")
            .arg("advapi32.lib")
            .arg("bcrypt.lib")
            .arg("user32.lib")
            .arg("shell32.lib")
            .arg("gdi32.lib")
            .arg("opengl32.lib")
            .arg("shlwapi.lib")
            .arg("msimg32.lib")
            .arg("winspool.lib")
            .arg("dbghelp.lib")
            .arg("ole32.lib")
            .arg("dwmapi.lib")
            .arg("imm32.lib")
            .arg("winmm.lib")
            .arg("uxtheme.lib")
            .arg("shcore.lib")
            .arg("pathcch.lib")
            .arg("ntdll.lib")
            .arg("legacy_stdio_definitions.lib")
            // GUI / clipboard bits pulled by runtime
            .arg("oleaut32.lib")
            .arg("uuid.lib")
            .arg("comdlg32.lib")
            .arg("propsys.lib")
            .arg("runtimeobject.lib")
            .status()
            .map_err(|e| format!("failed to run lld-link: {} (is lld-link on PATH?)", e))?;
        let _ = std::fs::remove_file(&tmp_o);
        if !status.success() {
            return Err(format!(
                "lld-link failed for LLVM AOT (runtime={})",
                runtime.display()
            ));
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let status = Command::new(&clang)
            .arg(&tmp_o)
            .arg(&runtime)
            .arg("-o")
            .arg(output_exe)
            .arg("-lm")
            .arg("-lpthread")
            .arg("-ldl")
            .status()
            .map_err(|e| format!("failed to link: {}", e))?;
        let _ = std::fs::remove_file(&tmp_o);
        if !status.success() {
            return Err("clang link failed for LLVM AOT".into());
        }
    }

    Ok(())
}

/// Compile to a temp exe, run it, return process exit code as i64.
///
/// Runs the child with inherited stdio so its output streams to the terminal
/// incrementally — same observable behavior as the in-process Cranelift JIT.
/// (`.output()` would capture everything into memory and only flush after the
/// process exits, making LLVM output appear batched.)
pub fn compile_run_temp(ir: &str) -> Result<i64, String> {
    let exe = std::env::temp_dir().join(format!(
        "bolide_llvm_jit_{}.{}",
        std::process::id(),
        if cfg!(windows) { "exe" } else { "bin" }
    ));
    compile_and_link_exe(ir, &exe)?;
    let status = Command::new(&exe)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to run LLVM JIT binary: {}", e))?;
    let code = status.code().unwrap_or(-1) as i64;
    let _ = std::fs::remove_file(&exe);
    Ok(code)
}
