use clap::{Parser, Subcommand};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use bolide_compiler::{AotCompiler, JitCompiler};
use bolide_parser::{parse_source_with_diagnostics, ParseDiagnostic, Program};
use miette::{LabeledSpan, MietteDiagnostic, NamedSource, Report};

mod pkg_cmd;

const NATIVE_LIB_PREFIX: &str = "lib:";
const AUTO_LIB_PREFIX: &str = "auto:";

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
    if let Some(name) = lib.strip_prefix(AUTO_LIB_PREFIX) {
        if name.is_empty() {
            return Err(miette::miette!(
                "extern \"auto:\" is missing a library name"
            ));
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
    /// Create a new Bolide project skeleton
    New {
        /// Project name (also the new directory name)
        name: String,
    },
    /// Add a dependency to bolide.toml and install it
    Add {
        /// Dependency spec: git URL, local path, or name@version
        spec: String,
        /// Git tag or branch to use (for git dependencies)
        #[arg(long)]
        tag: Option<String>,
        /// Treat the spec as a local path dependency
        #[arg(long)]
        path: bool,
        /// Registry URL (for name@version dependencies)
        #[arg(long)]
        registry: Option<String>,
        /// Override the dependency name (defaults to the package's own name)
        #[arg(long)]
        name: Option<String>,
    },
    /// Resolve dependencies declared in bolide.toml and write bolide.lock
    Install,
    /// Validate the current package for publishing (registry upload not yet implemented)
    Publish,
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
        Some(Commands::New { name }) => {
            pkg_cmd::new_project(&name)?;
        }
        Some(Commands::Add {
            spec,
            tag,
            path,
            registry,
            name,
        }) => {
            pkg_cmd::add_dependency(
                &spec,
                tag.as_deref(),
                path,
                registry.as_deref(),
                name.as_deref(),
            )?;
        }
        Some(Commands::Install) => {
            pkg_cmd::install()?;
        }
        Some(Commands::Publish) => {
            pkg_cmd::publish()?;
        }
        None => {
            run_repl()?;
        }
    }

    Ok(())
}

/// 沿父目录向上查找包含 bolide.toml 的项目根目录。
fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() {
        Some(start.to_path_buf())
    } else {
        start.parent().map(|p| p.to_path_buf())
    };
    while let Some(current) = dir {
        if current.join("bolide.toml").exists() {
            return Some(current);
        }
        dir = current.parent().map(|p| p.to_path_buf());
    }
    None
}

/// 若源文件位于一个 Bolide 项目内，解析依赖并构造编译器使用的依赖映射。
/// 没有 bolide.toml 时返回 None（保持单文件项目的原有行为）。
fn load_dependency_manifest(
    file: &Path,
) -> miette::Result<Option<bolide_compiler::DependencyManifest>> {
    // 规范化为绝对路径，避免相对路径在父目录遍历时产生空路径。
    let abs = file
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(file));
    let Some(project_root) = find_project_root(&abs) else {
        return Ok(None);
    };
    let graph = bolide_pkg::resolve_dependencies(&project_root)
        .map_err(|e| miette::miette!("Dependency resolution error: {}", e))?;

    // 把解析出的依赖图映射成编译器使用的最小依赖映射。
    let mut manifest = bolide_compiler::DependencyManifest::new();
    for (name, dep) in &graph.packages {
        manifest.insert(
            name.clone(),
            dep.source_path.clone(),
            dep.entry_file.clone(),
        );
    }
    Ok(Some(manifest))
}

fn parse_file_source(file: &Path, source: &str) -> miette::Result<Program> {
    parse_source_with_diagnostics(source).map_err(|e| parse_error_report(file, source, e))
}

fn parse_error_report(file: &Path, source: &str, error: ParseDiagnostic) -> Report {
    let label = error
        .span
        .map(|(offset, len)| {
            source_label(
                source,
                offset,
                len,
                error.label.as_deref().unwrap_or("here"),
            )
        })
        .or_else(|| {
            locate_message_token(source, &error.message)
                .map(|(offset, len, label)| source_label(source, offset, len, &label))
        });

    diagnostic_report(
        file,
        source,
        "bolide::parse",
        format!("Parse error: {}", strip_phase_prefix(&error.message)),
        label.into_iter().collect(),
        error.help.as_deref(),
    )
}

fn compile_error_report(file: &Path, source: &str, message: String) -> Report {
    let clean = strip_phase_prefix(&message);
    let labels = locate_compile_error(source, clean)
        .map(|(offset, len, label)| vec![source_label(source, offset, len, &label)])
        .unwrap_or_default();

    diagnostic_report(
        file,
        source,
        "bolide::compile",
        format!("Compile error: {}", clean),
        labels,
        help_for_compile_error(clean).as_deref(),
    )
}

fn diagnostic_report(
    file: &Path,
    source: &str,
    code: &str,
    message: String,
    labels: Vec<LabeledSpan>,
    help: Option<&str>,
) -> Report {
    let mut diagnostic = MietteDiagnostic::new(message).with_code(code);
    if !labels.is_empty() {
        diagnostic = diagnostic.with_labels(labels);
    }
    if let Some(help) = help {
        diagnostic = diagnostic.with_help(help);
    }
    Report::from(diagnostic).with_source_code(NamedSource::new(
        file.display().to_string(),
        source.to_string(),
    ))
}

fn source_label(source: &str, offset: usize, len: usize, label: &str) -> LabeledSpan {
    let offset = offset.min(source.len());
    let len = len.min(source.len().saturating_sub(offset));
    LabeledSpan::new_primary_with_span(Some(label.to_string()), (offset, len))
}

fn strip_phase_prefix(message: &str) -> &str {
    message
        .strip_prefix("Parse error: ")
        .or_else(|| message.strip_prefix("Compile error: "))
        .unwrap_or(message)
}

fn locate_compile_error(source: &str, message: &str) -> Option<(usize, usize, String)> {
    if let Some(path) = extract_single_quoted_after(message, "Failed to parse module ") {
        return find_import_path(source, path)
            .map(|(offset, len)| (offset, len, "imported module parsed here".to_string()));
    }
    if let Some(path) = extract_single_quoted_after(message, "Failed to load module ") {
        return find_import_path(source, path)
            .map(|(offset, len)| (offset, len, "imported module loaded here".to_string()));
    }

    for prefix in [
        "Undefined variable or function: ",
        "Undefined variable for ref: ",
        "Undefined async function: ",
        "Undefined function: ",
        "Undefined variable: ",
        "Undefined channel: ",
    ] {
        if let Some(name) = message.strip_prefix(prefix) {
            let name = trim_error_name(name);
            return find_identifier(source, name)
                .map(|(offset, len)| (offset, len, format!("'{}' is not defined", name)));
        }
    }

    if let Some(name) = message.strip_prefix("Unknown method: ") {
        let name = trim_error_name(name);
        return find_member_name(source, name)
            .or_else(|| find_identifier(source, name))
            .map(|(offset, len)| (offset, len, format!("unknown method '{}'", name)));
    }

    if message.starts_with("Method '") && message.contains("' not found in class ") {
        if let Some(name) = extract_single_quoted(message) {
            return find_member_name(source, name)
                .or_else(|| find_identifier(source, name))
                .map(|(offset, len)| (offset, len, format!("unknown method '{}'", name)));
        }
    }

    if let Some(name) = message.strip_prefix("Unknown method return type: ") {
        let method = trim_error_name(name).rsplit('.').next().unwrap_or(name);
        return find_member_name(source, method)
            .or_else(|| find_identifier(source, method))
            .map(|(offset, len)| {
                (
                    offset,
                    len,
                    format!("return type for method '{}' is unknown", method),
                )
            });
    }

    if let Some((callee, arg)) = extract_missing_required_argument(message) {
        if let Some((offset, len)) = find_call_site(source, callee) {
            return Some((
                offset,
                len,
                format!("call is missing required argument '{}'", arg),
            ));
        }
        if let Some((offset, len)) = find_identifier(source, arg) {
            return Some((offset, len, format!("required argument '{}'", arg)));
        }
    }

    locate_message_token(source, message)
}

fn locate_message_token(source: &str, message: &str) -> Option<(usize, usize, String)> {
    if let Some(name) = extract_single_quoted(message) {
        if let Some(span) = find_identifier(source, name) {
            return Some((span.0, span.1, format!("related name '{}'", name)));
        }
        if let Some(offset) = source.find(name) {
            return Some((offset, name.len(), format!("related text '{}'", name)));
        }
    }
    None
}

fn help_for_compile_error(message: &str) -> Option<String> {
    if message.starts_with("Undefined variable or function: ")
        || message.starts_with("Undefined variable: ")
    {
        Some("Define the name before using it, or check for a spelling/import mistake.".to_string())
    } else if message.starts_with("Undefined function: ")
        || message.starts_with("Undefined async function: ")
    {
        Some(
            "Define the function with 'fn', import it, or check that the call target is correct."
                .to_string(),
        )
    } else if message.starts_with("Undefined channel: ") {
        Some(
            "Create the channel before send/receive, and make sure the same name is in scope."
                .to_string(),
        )
    } else if message.starts_with("Unknown method: ")
        || (message.starts_with("Method '") && message.contains("' not found in class "))
    {
        Some("Check the receiver type and the method name. Methods must be declared inside the class.".to_string())
    } else if message.contains(" missing required argument ") {
        Some("Pass the missing argument positionally or with a named argument.".to_string())
    } else if message.contains("Type mismatch") || message.contains("type mismatch") {
        Some("Compare the declared type with the value being assigned or returned.".to_string())
    } else if message.starts_with("Failed to parse module ") {
        Some("The imported file has a syntax error. Run that file directly to see its exact source location.".to_string())
    } else if message.starts_with("Failed to load module ") {
        Some("Check the import path relative to the current file or package manifest.".to_string())
    } else {
        None
    }
}

fn repl_parse_error(source: &str, error: ParseDiagnostic) -> String {
    let clean = strip_phase_prefix(&error.message);
    let loc = error
        .span
        .map(|(offset, len)| {
            (
                offset,
                len,
                error.label.unwrap_or_else(|| "here".to_string()),
            )
        })
        .or_else(|| locate_message_token(source, clean));
    inline_diagnostic("Parse error", source, clean, loc, error.help.as_deref())
}

fn repl_compile_error(source: &str, message: String) -> String {
    let clean = strip_phase_prefix(&message);
    let loc = locate_compile_error(source, clean);
    let help = help_for_compile_error(clean);
    inline_diagnostic("Compile error", source, clean, loc, help.as_deref())
}

fn inline_diagnostic(
    phase: &str,
    source: &str,
    message: &str,
    loc: Option<(usize, usize, String)>,
    help: Option<&str>,
) -> String {
    let mut out = format!("{}: {}", phase, message);
    if let Some((offset, len, label)) = loc {
        let (line_no, col_no, line, col_chars) = source_line_at(source, offset);
        out.push_str(&format!("\n  --> <repl>:{}:{}", line_no, col_no));
        out.push_str(&format!("\n{:>4} | {}", line_no, line));
        let start = offset.min(source.len());
        let end = start.saturating_add(len).min(source.len());
        let caret_count = source[start..end].chars().count().max(1);
        out.push_str(&format!(
            "\n     | {}{} {}",
            " ".repeat(col_chars),
            "^".repeat(caret_count),
            label
        ));
    }
    if let Some(help) = help {
        out.push_str(&format!("\n  help: {}", help));
    }
    out
}

fn source_line_at(source: &str, offset: usize) -> (usize, usize, &str, usize) {
    let offset = offset.min(source.len());
    let line_start = source[..offset].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    let line_end = source[offset..]
        .find('\n')
        .map(|idx| offset + idx)
        .unwrap_or(source.len());
    let line_no = source[..line_start].bytes().filter(|b| *b == b'\n').count() + 1;
    let col_chars = source[line_start..offset].chars().count();
    (
        line_no,
        col_chars + 1,
        &source[line_start..line_end],
        col_chars,
    )
}

fn extract_single_quoted(message: &str) -> Option<&str> {
    let start = message.find('\'')? + 1;
    let end = message[start..].find('\'')? + start;
    Some(&message[start..end])
}

fn extract_single_quoted_after<'a>(message: &'a str, prefix: &str) -> Option<&'a str> {
    message.strip_prefix(prefix).and_then(extract_single_quoted)
}

fn extract_missing_required_argument(message: &str) -> Option<(&str, &str)> {
    let marker = " missing required argument ";
    let idx = message.find(marker)?;
    let callee = trim_error_name(&message[..idx]);
    let arg = extract_single_quoted(&message[idx + marker.len()..])?;
    Some((callee, arg))
}

fn trim_error_name(name: &str) -> &str {
    name.trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches('.')
}

fn find_import_path(source: &str, path: &str) -> Option<(usize, usize)> {
    source
        .find(path)
        .map(|offset| (offset, path.len()))
        .or_else(|| find_identifier(source, path))
}

fn find_call_site(source: &str, callee: &str) -> Option<(usize, usize)> {
    let callee = trim_error_name(callee);
    if callee.is_empty() {
        return None;
    }
    let needle = format!("{}(", callee);
    for (offset, _) in source.match_indices(&needle) {
        if !looks_like_function_definition(source, offset) {
            return Some((offset, callee.len()));
        }
    }
    let short = callee.rsplit('.').next().unwrap_or(callee);
    find_member_name(source, short).or_else(|| find_identifier(source, short))
}

fn looks_like_function_definition(source: &str, name_offset: usize) -> bool {
    let line_start = source[..name_offset]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    source[line_start..name_offset]
        .trim_start()
        .starts_with("fn ")
}

fn find_member_name(source: &str, name: &str) -> Option<(usize, usize)> {
    let needle = format!(".{}", name);
    source.match_indices(&needle).find_map(|(offset, _)| {
        let name_offset = offset + 1;
        let end = name_offset + name.len();
        if is_identifier_boundary(source, name_offset, end) {
            Some((name_offset, name.len()))
        } else {
            None
        }
    })
}

fn find_identifier(source: &str, ident: &str) -> Option<(usize, usize)> {
    if ident.is_empty()
        || !ident
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    source.match_indices(ident).find_map(|(offset, _)| {
        let end = offset + ident.len();
        if is_identifier_boundary(source, offset, end) {
            Some((offset, ident.len()))
        } else {
            None
        }
    })
}

fn is_identifier_boundary(source: &str, start: usize, end: usize) -> bool {
    let before = source[..start]
        .chars()
        .next_back()
        .map(|c| c.is_ascii_alphanumeric() || c == '_')
        .unwrap_or(false);
    let after = source[end..]
        .chars()
        .next()
        .map(|c| c.is_ascii_alphanumeric() || c == '_')
        .unwrap_or(false);
    !before && !after
}

fn run_file(file: &PathBuf) -> miette::Result<()> {
    println!("Running: {}", file.display());
    let source =
        fs::read_to_string(file).map_err(|e| miette::miette!("Failed to read file: {}", e))?;

    let ast = parse_file_source(file, &source)?;

    let mut compiler = JitCompiler::new();
    // import 相对路径基于源文件所在目录解析
    if let Some(parent) = file.parent() {
        compiler.set_base_dir(&parent.to_string_lossy());
    }
    // 若存在项目 manifest，加载依赖映射注入编译器
    if let Some(manifest) = load_dependency_manifest(file)? {
        compiler.set_dependency_manifest(manifest);
    }
    let main_ptr = compiler
        .compile(&ast)
        .map_err(|e| compile_error_report(file, &source, e))?;

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
    let ast = parse_file_source(file, &source)?;

    // AOT 编译
    let mut compiler =
        AotCompiler::new().map_err(|e| miette::miette!("Compiler init error: {}", e))?;

    // import 相对路径基于源文件所在目录解析
    if let Some(parent) = file.parent() {
        compiler.set_base_dir(&parent.to_string_lossy());
    }
    if let Some(manifest) = load_dependency_manifest(file)? {
        compiler.set_dependency_manifest(manifest);
    }

    let result = compiler
        .compile(&ast)
        .map_err(|e| compile_error_report(file, &source, e))?;

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
    let ast = parse_file_source(file, &source)?;

    let mut compiler =
        AotCompiler::new().map_err(|e| miette::miette!("Compiler init error: {}", e))?;
    compiler.set_lib_mode(true);
    if let Some(parent) = file.parent() {
        compiler.set_base_dir(&parent.to_string_lossy());
    }
    if let Some(manifest) = load_dependency_manifest(file)? {
        compiler.set_dependency_manifest(manifest);
    }

    let result = compiler
        .compile(&ast)
        .map_err(|e| compile_error_report(file, &source, e))?;

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
            let ast = parse_source_with_diagnostics(&code).map_err(|e| {
                state.functions.pop();
                repl_parse_error(&code, e)
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.functions.pop();
                repl_compile_error(&code, e)
            })?;
            Ok("Function defined.".to_string())
        }
        InputType::ClassDef => {
            let code = state.build_program(None);
            let ast = parse_source_with_diagnostics(&code).map_err(|e| {
                state.functions.pop();
                repl_parse_error(&code, e)
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.functions.pop();
                repl_compile_error(&code, e)
            })?;
            Ok("Class defined.".to_string())
        }
        InputType::VarDecl => {
            // 验证变量声明是否有效
            let code = state.build_program(None);
            let ast = parse_source_with_diagnostics(&code).map_err(|e| {
                state.globals.pop();
                repl_parse_error(&code, e)
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.globals.pop();
                repl_compile_error(&code, e)
            })?;
            Ok("Variable declared.".to_string())
        }
        InputType::Expr => {
            let code = state.build_program(Some(input));
            let ast =
                parse_source_with_diagnostics(&code).map_err(|e| repl_parse_error(&code, e))?;
            let mut compiler = JitCompiler::new();
            let main_ptr = compiler
                .compile(&ast)
                .map_err(|e| repl_compile_error(&code, e))?;
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

#[cfg(test)]
mod diagnostics_tests {
    use super::*;

    #[test]
    fn missing_required_argument_points_to_call_site() {
        let source = r#"fn add(a: int, b: int) -> int {
    return a + b;
}

let x = add(1);
"#;
        let (offset, len, label) =
            locate_compile_error(source, "add missing required argument 'b'").unwrap();
        assert_eq!(&source[offset..offset + len], "add");
        assert!(label.contains("missing required argument 'b'"));
        assert!(source[..offset].contains("let x = "));
    }

    #[test]
    fn method_not_found_points_to_member_name() {
        let source = r#"class Box {
    value: int;
}

let b = Box(1);
b.nope();
"#;
        let (offset, len, label) = locate_compile_error(
            source,
            "Method 'nope' not found in class 'Box' or its parents",
        )
        .unwrap();
        assert_eq!(&source[offset..offset + len], "nope");
        assert_eq!(label, "unknown method 'nope'");
    }

    #[test]
    fn undefined_name_points_to_identifier() {
        let source = "let x = missing_name + 1;\n";
        let (offset, len, label) =
            locate_compile_error(source, "Undefined variable or function: missing_name").unwrap();
        assert_eq!(&source[offset..offset + len], "missing_name");
        assert_eq!(label, "'missing_name' is not defined");
    }
}
