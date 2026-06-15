use clap::{Parser, Subcommand};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use bolide_compiler::{AotCompiler, JitCompiler};
use bolide_parser::parse_source;

const NATIVE_LIB_PREFIX: &str = "lib:";

fn is_shared_library_path(lib: &str) -> bool {
    let lower = lib.to_ascii_lowercase();
    lower.ends_with(".dll") || lower.ends_with(".so") || lower.ends_with(".dylib")
}

fn native_lib_name(lib: &str) -> miette::Result<Option<&str>> {
    if let Some(name) = lib.strip_prefix(NATIVE_LIB_PREFIX) {
        if name.is_empty() {
            return Err(miette::miette!("extern \"lib:\" is missing a library name"));
        }
        return Ok(Some(name));
    }
    Ok(None)
}

#[cfg(target_os = "windows")]
fn native_link_arg_windows(lib: &str) -> miette::Result<String> {
    if lib.starts_with("std:") {
        return Err(miette::miette!("Unknown standard native library: {}", lib));
    }
    if is_shared_library_path(lib) {
        return Err(miette::miette!(
            "extern \"{}\" is a shared library path. Use `lib:name` or a .lib import/static library.",
            lib
        ));
    }
    if let Some(name) = native_lib_name(lib)? {
        if matches!(name, "c" | "m") {
            return Ok("msvcrt.lib".to_string());
        }
        return Ok(format!("{}.lib", name));
    }
    Ok(lib.to_string())
}

#[cfg(not(target_os = "windows"))]
fn native_link_arg_unix(lib: &str) -> miette::Result<String> {
    if lib.starts_with("std:") {
        return Err(miette::miette!("Unknown standard native library: {}", lib));
    }
    if is_shared_library_path(lib) {
        return Err(miette::miette!(
            "extern \"{}\" is a shared library path. Use `lib:name`, -lname, or a static archive path.",
            lib
        ));
    }
    if let Some(name) = native_lib_name(lib)? {
        if name == "c" {
            return Ok("-lc".to_string());
        }
        if name == "m" {
            return Ok("-lm".to_string());
        }
        return Ok(format!("-l{}", name));
    }
    Ok(lib.to_string())
}

/// REPL 状态，维护累积的代码
struct ReplState {
    /// 函数定义
    functions: Vec<String>,
    /// 全局变量声明
    globals: Vec<String>,
}

impl ReplState {
    fn new() -> Self {
        Self {
            functions: Vec::new(),
            globals: Vec::new(),
        }
    }

    /// 判断输入类型并添加到状态
    fn add_input(&mut self, input: &str) -> InputType {
        let trimmed = input.trim();

        if trimmed.starts_with("fn ") {
            self.functions.push(input.to_string());
            InputType::FuncDef
        } else if trimmed.starts_with("let ") {
            self.globals.push(input.to_string());
            InputType::VarDecl
        } else if trimmed.starts_with("class ") {
            self.functions.push(input.to_string());
            InputType::ClassDef
        } else {
            InputType::Expr
        }
    }

    /// 生成完整的程序代码
    fn build_program(&self, expr: Option<&str>) -> String {
        let mut code = String::new();

        // 添加函数/类定义
        for func in &self.functions {
            code.push_str(func);
            code.push('\n');
        }

        // 添加全局变量
        for var in &self.globals {
            code.push_str(var);
            code.push('\n');
        }

        // 添加表达式/语句
        if let Some(e) = expr {
            code.push_str(e);
            code.push('\n');
        }

        code
    }
}

#[derive(Debug, PartialEq)]
enum InputType {
    FuncDef,
    VarDecl,
    ClassDef,
    Expr,
}

#[derive(Parser)]
#[command(name = "bolide")]
#[command(about = "Bolide programming language compiler")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a Bolide source file (JIT)
    Run {
        /// Source file path
        file: PathBuf,
    },
    /// Compile a Bolide source file to executable (AOT)
    Compile {
        /// Source file path
        file: PathBuf,
        /// Output file path
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Compile to a C-linkable static library (.lib/.a) instead of an executable.
        /// Suppresses the synthetic `main` entry; only `export fn` symbols use bare names.
        #[arg(long)]
        lib: bool,
        /// Also emit a C header (.h) declaring all `export fn` functions.
        #[arg(long)]
        header: bool,
    },
}

fn main() -> miette::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run { file }) => {
            run_file(&file)?;
        }
        Some(Commands::Compile {
            file,
            output,
            lib,
            header,
        }) => {
            if lib {
                let out = output.unwrap_or_else(|| {
                    #[cfg(target_os = "windows")]
                    let ext = "lib";
                    #[cfg(not(target_os = "windows"))]
                    let ext = "a";
                    file.with_extension(ext)
                });
                compile_lib(&file, &out, header)?;
            } else {
                let out = output.unwrap_or_else(|| file.with_extension("exe"));
                compile_file(&file, &out, header)?;
            }
        }
        None => {
            run_repl()?;
        }
    }

    Ok(())
}

fn run_file(file: &PathBuf) -> miette::Result<()> {
    println!("Running: {}", file.display());
    let source =
        fs::read_to_string(file).map_err(|e| miette::miette!("Failed to read file: {}", e))?;

    let ast = parse_source(&source).map_err(|e| miette::miette!("Parse error: {}", e))?;

    let mut compiler = JitCompiler::new();
    // import 相对路径基于源文件所在目录解析
    if let Some(parent) = file.parent() {
        compiler.set_base_dir(&parent.to_string_lossy());
    }
    let main_ptr = compiler
        .compile(&ast)
        .map_err(|e| miette::miette!("Compile error: {}", e))?;

    let main_fn: fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
    let result = main_fn();
    println!("Result: {}", result);
    Ok(())
}

/// 写出 C 头文件（如果编译结果包含 export 函数）
fn write_header_if_present(result: &bolide_compiler::AotCompileResult, output: &PathBuf) {
    if let Some(ref header) = result.c_header {
        let header_path = output.with_extension("h");
        match fs::write(&header_path, header) {
            Ok(_) => println!("Generated C header: {}", header_path.display()),
            Err(e) => eprintln!(
                "Warning: failed to write header {}: {}",
                header_path.display(),
                e
            ),
        }
    }
}

/// AOT 编译文件
fn compile_file(file: &PathBuf, output: &PathBuf, header: bool) -> miette::Result<()> {
    println!("Compiling: {} -> {}", file.display(), output.display());

    // 读取源文件
    let source =
        fs::read_to_string(file).map_err(|e| miette::miette!("Failed to read file: {}", e))?;

    // 解析
    let ast = parse_source(&source).map_err(|e| miette::miette!("Parse error: {}", e))?;

    // AOT 编译
    let mut compiler =
        AotCompiler::new().map_err(|e| miette::miette!("Compiler init error: {}", e))?;

    // import 相对路径基于源文件所在目录解析
    if let Some(parent) = file.parent() {
        compiler.set_base_dir(&parent.to_string_lossy());
    }

    let result = compiler
        .compile(&ast)
        .map_err(|e| miette::miette!("Compile error: {}", e))?;

    // 打印外部库信息
    if !result.extern_libs.is_empty() {
        println!("External libraries: {:?}", result.extern_libs);
    }

    if header {
        write_header_if_present(&result, output);
    }

    // 写入目标文件
    let obj_path = output.with_extension("o");
    fs::write(&obj_path, &result.object_code)
        .map_err(|e| miette::miette!("Failed to write object file: {}", e))?;

    println!("Generated object file: {}", obj_path.display());

    // 链接
    link_executable(&obj_path, output, &result.extern_libs)?;

    // 清理目标文件
    let _ = fs::remove_file(&obj_path);

    println!("Successfully compiled: {}", output.display());
    Ok(())
}

/// AOT 编译为 C 可链接静态库（.lib/.a），抑制合成入口 main。
/// C 端最终链接时仍需带上 bolide_runtime 静态库以解析 bolide_* 运行时符号。
fn compile_lib(file: &PathBuf, output: &PathBuf, header: bool) -> miette::Result<()> {
    println!(
        "Compiling library: {} -> {}",
        file.display(),
        output.display()
    );

    let source =
        fs::read_to_string(file).map_err(|e| miette::miette!("Failed to read file: {}", e))?;
    let ast = parse_source(&source).map_err(|e| miette::miette!("Parse error: {}", e))?;

    let mut compiler =
        AotCompiler::new().map_err(|e| miette::miette!("Compiler init error: {}", e))?;
    compiler.set_lib_mode(true);
    if let Some(parent) = file.parent() {
        compiler.set_base_dir(&parent.to_string_lossy());
    }

    let result = compiler
        .compile(&ast)
        .map_err(|e| miette::miette!("Compile error: {}", e))?;

    if header {
        write_header_if_present(&result, output);
    }

    // 写入目标文件，再打包成静态库
    let obj_path = output.with_extension("o");
    fs::write(&obj_path, &result.object_code)
        .map_err(|e| miette::miette!("Failed to write object file: {}", e))?;

    archive_static_lib(&obj_path, output)?;
    let _ = fs::remove_file(&obj_path);

    println!("Successfully built static library: {}", output.display());
    println!(
        "Note: link your C program with both {} and the bolide runtime static library.",
        output.display()
    );
    Ok(())
}

/// 把单个 .o 打包成静态库：Windows 用 llvm-lib，Unix 用 ar。
fn archive_static_lib(obj_path: &PathBuf, output: &PathBuf) -> miette::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let out_arg = format!("/OUT:{}", output.display());
        // llvm-lib 与 MSVC lib.exe 命令行兼容；优先 llvm-lib（随 LLVM 提供）
        let candidates = ["llvm-lib", "lib"];
        let mut last_err = String::new();
        for tool in candidates {
            match Command::new(tool)
                .arg(&out_arg)
                .arg(obj_path.display().to_string())
                .status()
            {
                Ok(status) if status.success() => return Ok(()),
                Ok(status) => last_err = format!("{} exited with {}", tool, status),
                Err(e) => last_err = format!("{} not found: {}", tool, e),
            }
        }
        Err(miette::miette!(
            "Failed to archive static library: {}",
            last_err
        ))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let status = Command::new("ar")
            .arg("rcs")
            .arg(output.display().to_string())
            .arg(obj_path.display().to_string())
            .status()
            .map_err(|e| miette::miette!("ar not found: {}", e))?;
        if status.success() {
            Ok(())
        } else {
            Err(miette::miette!("ar failed to archive static library"))
        }
    }
}

/// 查找运行时库路径
fn find_runtime_lib() -> miette::Result<String> {
    // 获取当前可执行文件路径
    let exe_path = std::env::current_exe()
        .map_err(|e| miette::miette!("Failed to get executable path: {}", e))?;

    // 尝试在可执行文件同目录下查找
    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));

    #[cfg(target_os = "windows")]
    let lib_name = "bolide_runtime.lib";
    #[cfg(not(target_os = "windows"))]
    let lib_name = "libbolide_runtime.a";

    for path in [exe_dir.join(lib_name), exe_dir.join("..").join(lib_name)] {
        if path.exists() {
            let path = path.canonicalize().unwrap_or(path);
            println!("Found runtime library: {}", path.display());
            return Ok(path.display().to_string());
        }
    }

    for dir in [exe_dir.join("deps"), exe_dir.join("..").join("deps")] {
        if let Some(path) = find_hashed_runtime_lib(&dir) {
            println!("Found runtime library: {}", path.display());
            return Ok(path.display().to_string());
        }
    }

    let cwd_path = PathBuf::from("target/debug").join(lib_name);
    if cwd_path.exists() {
        let path = cwd_path.canonicalize().unwrap_or(cwd_path);
        println!("Found runtime library: {}", path.display());
        return Ok(path.display().to_string());
    }

    if let Some(path) = find_hashed_runtime_lib(Path::new("target/debug/deps")) {
        println!("Found runtime library: {}", path.display());
        return Ok(path.display().to_string());
    }

    Err(miette::miette!("Runtime library not found: {}", lib_name))
}

fn find_hashed_runtime_lib(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy();

        #[cfg(target_os = "windows")]
        let matches = name.starts_with("bolide_runtime-") && name.ends_with(".lib");
        #[cfg(not(target_os = "windows"))]
        let matches = name.starts_with("libbolide_runtime-") && name.ends_with(".a");

        if !matches {
            continue;
        }

        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

        match &best {
            Some((best_modified, _)) if *best_modified >= modified => {}
            _ => best = Some((modified, path)),
        }
    }

    best.map(|(_, path)| path.canonicalize().unwrap_or(path))
}

/// 链接可执行文件
fn link_executable(
    obj_path: &PathBuf,
    output: &PathBuf,
    extern_libs: &[String],
) -> miette::Result<()> {
    #[cfg(target_os = "windows")]
    {
        link_windows(obj_path, output, extern_libs)
    }

    #[cfg(not(target_os = "windows"))]
    {
        link_unix(obj_path, output, extern_libs)
    }
}

#[cfg(target_os = "windows")]
fn link_windows(
    obj_path: &PathBuf,
    output: &PathBuf,
    extern_libs: &[String],
) -> miette::Result<()> {
    // 查找运行时库
    let runtime_lib_path = PathBuf::from(find_runtime_lib()?);
    let runtime_lib_dir = runtime_lib_path.parent().unwrap().display().to_string();
    let runtime_lib_name = runtime_lib_path.file_name().unwrap().to_str().unwrap();
    let runtime_lib_arg = runtime_lib_path.display().to_string();

    println!("Runtime lib dir: {}", runtime_lib_dir);
    println!("Runtime lib name: {}", runtime_lib_name);
    println!("Runtime lib path: {}", runtime_lib_arg);

    // 构建链接参数
    let libpath_arg = format!("/LIBPATH:{}", runtime_lib_dir);
    let out_arg = format!("/OUT:{}", output.display());

    let mut args = vec![
        "/ENTRY:main".to_string(),
        "/SUBSYSTEM:CONSOLE".to_string(),
        out_arg,
        obj_path.display().to_string(),
        runtime_lib_arg,
        libpath_arg,
        "kernel32.lib".to_string(),
        "msvcrt.lib".to_string(),
        "ucrt.lib".to_string(),
        "vcruntime.lib".to_string(),
        "libcmt.lib".to_string(),
        "ws2_32.lib".to_string(),
        "userenv.lib".to_string(),
        "advapi32.lib".to_string(),
        "bcrypt.lib".to_string(),
        "user32.lib".to_string(),
        "shell32.lib".to_string(),
        "gdi32.lib".to_string(),
        "opengl32.lib".to_string(),
        "shlwapi.lib".to_string(),
        "msimg32.lib".to_string(),
        "winspool.lib".to_string(),
        "dbghelp.lib".to_string(),
        "ole32.lib".to_string(),
        "dwmapi.lib".to_string(),
        "imm32.lib".to_string(),
        "winmm.lib".to_string(),
        "uxtheme.lib".to_string(),
        "shcore.lib".to_string(),
        "pathcch.lib".to_string(),
        "ntdll.lib".to_string(),
        "legacy_stdio_definitions.lib".to_string(),
    ];

    let mut user_lib_args = Vec::new();
    for lib in extern_libs {
        user_lib_args.push(native_link_arg_windows(lib)?);
    }

    for lib in user_lib_args {
        println!("Adding external library: {}", lib);
        args.push(lib);
    }

    println!("Running lld-link...");
    let status = Command::new("lld-link")
        .args(&args)
        .status()
        .map_err(|e| miette::miette!("Linker not found: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!("Linking failed"))
    }
}

#[cfg(not(target_os = "windows"))]
fn link_unix(obj_path: &PathBuf, output: &PathBuf, extern_libs: &[String]) -> miette::Result<()> {
    let runtime_lib = find_runtime_lib()?;

    let mut args = vec![
        "-o".to_string(),
        output.display().to_string(),
        obj_path.display().to_string(),
        runtime_lib,
        "-lm".to_string(),
        "-lpthread".to_string(),
        "-ldl".to_string(),
    ];

    for lib in extern_libs {
        let arg = native_link_arg_unix(lib)?;
        println!("Adding external library: {}", arg);
        args.push(arg);
    }

    let status = Command::new("cc")
        .args(&args)
        .status()
        .map_err(|e| miette::miette!("Linker not found: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!("Linking failed"))
    }
}

fn run_repl() -> miette::Result<()> {
    println!("Bolide {} - Interactive Mode", env!("CARGO_PKG_VERSION"));
    println!("Type 'exit' or 'quit' to exit, 'help' for help.");
    println!();

    let stdin = io::stdin();
    let mut state = ReplState::new();
    let mut input_buffer = String::new();
    let mut in_multiline = false;

    loop {
        if in_multiline {
            print!("... ");
        } else {
            print!(">>> ");
        }
        io::stdout().flush().unwrap();

        let mut line = String::new();
        if stdin.read_line(&mut line).is_err() {
            break;
        }

        let line = line.trim_end_matches('\n').trim_end_matches('\r');

        // 处理多行输入（函数/类定义、嵌套字典等）
        if in_multiline {
            input_buffer.push_str(line);
            input_buffer.push('\n');

            // 用括号深度判断是否结束
            let depth = count_brace_depth(&input_buffer);
            if depth == 0 {
                in_multiline = false;
                let input = input_buffer.trim().to_string();
                input_buffer.clear();

                match eval_input(&mut state, &input) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
            continue;
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        match input {
            "exit" | "quit" => break,
            "help" => {
                print_help();
                continue;
            }
            "clear" => {
                state = ReplState::new();
                println!("State cleared.");
                continue;
            }
            _ => {}
        }

        // 检查是否是多行输入——按括号深度判断
        let depth = count_brace_depth(input);
        if depth > 0 {
            in_multiline = true;
            input_buffer = input.to_string();
            input_buffer.push('\n');
            continue;
        }

        match eval_input(&mut state, input) {
            Ok(msg) => println!("{}", msg),
            Err(e) => eprintln!("Error: {}", e),
        }
    }

    println!("Goodbye!");
    Ok(())
}

/// 计算未闭合的 { } 括号深度。>0 表示还有未闭合的 {
fn count_brace_depth(s: &str) -> usize {
    let mut depth = 0usize;
    for ch in s.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

fn print_help() {
    println!("Bolide Interactive Mode Commands:");
    println!("  exit, quit  - Exit the REPL");
    println!("  help        - Show this help message");
    println!("  clear       - Clear all defined variables and functions");
    println!();
    println!("Enter Bolide code:");
    println!("  - Variables:   let x: int = 10;");
    println!("  - Functions:   fn add(a: int, b: int) -> int {{ return a + b }}");
}

fn eval_input(state: &mut ReplState, input: &str) -> Result<String, String> {
    let input_type = state.add_input(input);

    match input_type {
        InputType::FuncDef => {
            // 验证函数定义是否有效
            let code = state.build_program(None);
            let ast = parse_source(&code).map_err(|e| {
                state.functions.pop();
                e.to_string()
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.functions.pop();
                e.to_string()
            })?;
            Ok("Function defined.".to_string())
        }
        InputType::ClassDef => {
            let code = state.build_program(None);
            let ast = parse_source(&code).map_err(|e| {
                state.functions.pop();
                e.to_string()
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.functions.pop();
                e.to_string()
            })?;
            Ok("Class defined.".to_string())
        }
        InputType::VarDecl => {
            // 验证变量声明是否有效
            let code = state.build_program(None);
            let ast = parse_source(&code).map_err(|e| {
                state.globals.pop();
                e.to_string()
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.globals.pop();
                e.to_string()
            })?;
            Ok("Variable declared.".to_string())
        }
        InputType::Expr => {
            let code = state.build_program(Some(input));
            let ast = parse_source(&code).map_err(|e| e.to_string())?;
            let mut compiler = JitCompiler::new();
            let main_ptr = compiler.compile(&ast).map_err(|e| e.to_string())?;
            let main_fn: fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
            let result = main_fn();
            // 只有非零结果才显示（print等语句返回0）
            if result != 0 {
                Ok(result.to_string())
            } else {
                Ok(String::new())
            }
        }
    }
}
