use clap::{Parser, Subcommand};
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{ValidationResult, Validator};
use rustyline::{Cmd, Context, Helper, KeyCode, KeyEvent, Modifiers};
use std::borrow::Cow;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use bolide_compiler::{
    expand_macros_with_ctx, pretty_print, AotCompiler, ExpandContext, JitCompiler, LlvmAotCompiler,
    LlvmJitCompiler,
};
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
    /// 持久化 JitCompiler：跨输入保留已编译的全局变量/函数状态。
    compiler: JitCompiler,
    /// 每次输入分配唯一的顶层函数名（增量编译）。
    input_counter: usize,
}

impl ReplState {
    fn new() -> Self {
        let mut compiler = JitCompiler::new();
        compiler.set_repl_mode(true);
        Self {
            compiler,
            input_counter: 0,
        }
    }

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
        /// Codegen backend: `cranelift` (default) or `llvm`
        #[arg(long, default_value = "cranelift")]
        backend: String,
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
        /// Codegen backend: `cranelift` (default) or `llvm`
        #[arg(long, default_value = "cranelift")]
        backend: String,
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
    /// Expand macros and print the resulting Bolide-like AST as text
    Expand {
        /// Source file path
        file: PathBuf,
    },
}

fn main() -> miette::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run { file, backend }) => {
            run_file(&file, &backend)?;
        }
        Some(Commands::Compile {
            file,
            output,
            lib,
            header,
            backend,
        }) => {
            if lib {
                if backend.to_ascii_lowercase() == "llvm" {
                    return Err(miette::miette!(
                        "LLVM backend does not support --lib yet; use --backend cranelift"
                    ));
                }
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
                compile_file(&file, &out, header, &backend)?;
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
        Some(Commands::Expand { file }) => {
            expand_file(&file)?;
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

    // `let`-immutability errors: point at the ACTUAL mutation site (`.method(`
    // call or `= ` assignment) rather than the first binding with that name,
    // which may live in an unrelated function (e.g. a param named `status`).
    if let Some(name) = message
        .strip_prefix("Cannot call mutating method on immutable binding '")
        .and_then(|rest| rest.split("';").next())
    {
        let name = trim_error_name(name);
        if let Some((offset, len)) = find_mutation_site(source, name, true) {
            return Some((
                offset,
                len,
                format!("mutating method call on immutable '{}'", name),
            ));
        }
    }
    if let Some(name) = message
        .strip_prefix("Cannot assign to immutable binding '")
        .and_then(|rest| rest.split("';").next())
    {
        let name = trim_error_name(name);
        if let Some((offset, len)) = find_mutation_site(source, name, false) {
            return Some((
                offset,
                len,
                format!("assignment to immutable '{}'", name),
            ));
        }
    }

    locate_message_token(source, message)
}

/// Find the occurrence of `name` that is a mutation target: followed by `.` when
/// `method` is true (a `.push(...)`-style call), or by `=` when false (an
/// assignment LHS). Declarations (`var x:`/`let x:`) and reads (`return x`)
/// are skipped.
fn find_mutation_site(source: &str, name: &str, method: bool) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(rel) = source[from..].find(name) {
        let offset = from + rel;
        let end = offset + name.len();
        if is_identifier_boundary(source, offset, end) {
            let after = source[end..].trim_start();
            let is_mutation = if method {
                after.starts_with('.')
            } else {
                after.starts_with('=')
            };
            if is_mutation {
                return Some((offset, name.len()));
            }
        }
        from = end;
    }
    None
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

fn expand_file(file: &PathBuf) -> miette::Result<()> {
    let source =
        fs::read_to_string(file).map_err(|e| miette::miette!("Failed to read file: {}", e))?;
    let ast = parse_file_source(file, &source)?;
    let ctx = ExpandContext {
        file: file.display().to_string(),
        line: 1,
    };
    let expanded = expand_macros_with_ctx(ast, &ctx)
        .map_err(|e| miette::miette!("Macro expand error: {}", e))?;
    print!("{}", pretty_print(&expanded));
    Ok(())
}

fn run_file(file: &PathBuf, backend: &str) -> miette::Result<()> {
    println!("Running: {} (backend={})", file.display(), backend);
    let source =
        fs::read_to_string(file).map_err(|e| miette::miette!("Failed to read file: {}", e))?;

    let ast = parse_file_source(file, &source)?;

    match backend.to_ascii_lowercase().as_str() {
        "llvm" => {
            let mut compiler = LlvmJitCompiler::new()
                .map_err(|e| miette::miette!("LLVM backend init error: {}", e))?;
            if let Some(parent) = file.parent() {
                compiler.set_base_dir(&parent.to_string_lossy());
            }
            let result = compiler
                .compile_and_run(&ast)
                .map_err(|e| compile_error_report(file, &source, e))?;
            println!("Result: {}", result);
        }
        "cranelift" | "clif" => {
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
        }
        other => {
            return Err(miette::miette!(
                "Unknown backend '{}'; use cranelift or llvm",
                other
            ));
        }
    }
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
fn compile_file(
    file: &PathBuf,
    output: &PathBuf,
    header: bool,
    backend: &str,
) -> miette::Result<()> {
    println!(
        "Compiling: {} -> {} (backend={})",
        file.display(),
        output.display(),
        backend
    );

    // 读取源文件
    let source =
        fs::read_to_string(file).map_err(|e| miette::miette!("Failed to read file: {}", e))?;

    // 解析
    let ast = parse_file_source(file, &source)?;

    match backend.to_ascii_lowercase().as_str() {
        "llvm" => {
            let mut compiler = LlvmAotCompiler::new()
                .map_err(|e| miette::miette!("LLVM backend init error: {}", e))?;
            if let Some(parent) = file.parent() {
                compiler.set_base_dir(&parent.to_string_lossy());
            }
            // Direct link path (clang + bolide_runtime)
            compiler
                .compile_and_link(&ast, output)
                .map_err(|e| compile_error_report(file, &source, e))?;
            if header {
                eprintln!("Warning: LLVM backend ignores --header for now");
            }
            println!("Successfully compiled (LLVM): {}", output.display());
        }
        "cranelift" | "clif" => {
            // AOT 编译（Cranelift — 原有路径，未改动）
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
        }
        other => {
            return Err(miette::miette!(
                "Unknown backend '{}'; use cranelift or llvm",
                other
            ));
        }
    }
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

    // rustyline：方向键编辑、历史记录、多行块内上下移动编辑、Tab 缩进、语法高亮。
    // Validator 在块未闭合时返回 Incomplete → Enter 插入换行，整个块作为可编辑缓冲区。
    let mut editor = rustyline::Editor::<ReplHelper, _>::with_config(rustyline::Config::default())
        .map_err(|e| miette::miette!("REPL init error: {}", e))?;
    editor.set_helper(Some(ReplHelper));
    // Tab → 插入 4 个空格（缩进）。直接绑定 Cmd::Insert，避免 rustyline
    // 补全的 Circular 循环导致第二次 Tab 取消缩进。
    editor.bind_sequence(
        KeyEvent(KeyCode::Tab, Modifiers::NONE),
        Cmd::Insert(1, "    ".to_string()),
    );
    let _ = editor.load_history("bolide_repl_history.txt");

    let mut state = ReplState::new();

    loop {
        let input = match editor.readline(">>> ") {
            Ok(l) => l.trim().to_string(),
            // Ctrl+C：取消当前输入（多行块可放弃重来），不退出 REPL。
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("^C");
                continue;
            }
            // Ctrl+D：退出。
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        };

        if input.is_empty() {
            continue;
        }
        match input.as_str() {
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

        let _ = editor.add_history_entry(&input);
        match eval_input(&mut state, &input) {
            Ok(msg) => {
                if !msg.is_empty() {
                    println!("{}", msg);
                }
            }
            Err(e) => eprintln!("Error: {}", e),
        }
    }

    let _ = editor.save_history("bolide_repl_history.txt");
    println!("Goodbye!");
    Ok(())
}

/// 计算未闭合的 { } 括号深度（跳过字符串字面量、// 注释）。>0 表示还有未闭合的 {
fn count_brace_depth(s: &str) -> usize {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut in_comment = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_comment {
            if c == '\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if c == '\\' {
                chars.next();
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '/' if chars.peek() == Some(&'/') => in_comment = true,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

const RESET: &str = "\x1b[0m";
const HL_KEYWORD: &str = "\x1b[1;34m"; // 粗蓝：关键字 / 控制流
const HL_TYPE: &str = "\x1b[35m"; // 品红：类型名
const HL_BUILTIN: &str = "\x1b[33m"; // 黄：内置函数 / 常量
const HL_STRING: &str = "\x1b[32m"; // 绿：字符串
const HL_STRING_ESC: &str = "\x1b[36m"; // 青：字符串转义
const HL_NUMBER: &str = "\x1b[36m"; // 青：数字
const HL_COMMENT: &str = "\x1b[90m"; // 灰：注释
const HL_CONST: &str = "\x1b[1;36m"; // 粗青：常量（大写）

const HL_KEYWORDS: &[&str] = &[
    "let", "var", "fn", "for", "while", "if", "else", "return", "class", "import", "match",
    "enum", "trait", "true", "false", "and", "or", "not", "break", "continue", "yield", "async",
    "await", "throw", "try", "catch", "finally", "as", "in", "is", "pub", "mut", "self",
    "None", "defer", "with", "struct",
];

const HL_TYPES: &[&str] = &[
    "int", "float", "bigint", "decimal", "str", "string", "bool", "bytes", "byte", "char", "list",
    "dict", "tuple", "set", "dynamic", "Option", "Error", "Future", "Result",
];

const HL_BUILTINS: &[&str] = &[
    "print", "input", "len", "range", "repr", "type", "isinstance", "sorted", "min", "max",
    "sum", "abs", "enumerate", "zip", "map", "filter", "exit", "quit",
];

/// 给单个标识符上色（关键字 / 类型 / 内置 / 大写常量 / 其他）。
fn classify_word(w: &str) -> String {
    if HL_KEYWORDS.contains(&w) {
        format!("{}{}{}", HL_KEYWORD, w, RESET)
    } else if HL_TYPES.contains(&w) {
        format!("{}{}{}", HL_TYPE, w, RESET)
    } else if HL_BUILTINS.contains(&w) {
        format!("{}{}{}", HL_BUILTIN, w, RESET)
    } else if w == w.to_uppercase() && w.len() > 1 && w.chars().all(|c| c.is_uppercase() || c == '_')
    {
        // 全大写 → 常量
        format!("{}{}{}", HL_CONST, w, RESET)
    } else if w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
        // 首字母大写 → 类/类型名
        format!("{}{}{}", HL_TYPE, w, RESET)
    } else {
        w.to_string()
    }
}

/// 语法高亮。输入为整个（多行）缓冲区，跨行保持字符串 / 注释状态。
fn highlight_line(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    let mut in_comment = false;

    while let Some(c) = chars.next() {
        if in_comment {
            out.push(c);
            if c == '\n' {
                in_comment = false;
                out.push_str(RESET);
            }
            continue;
        }
        if in_string {
            match c {
                '\\' => {
                    out.push_str(HL_STRING_ESC);
                    out.push('\\');
                    if let Some(n) = chars.next() {
                        out.push(n);
                    }
                    out.push_str(HL_STRING);
                }
                '"' => {
                    in_string = false;
                    out.push('"');
                    out.push_str(RESET);
                }
                _ => out.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push_str(HL_STRING);
                out.push('"');
            }
            '/' if chars.peek() == Some(&'/') => {
                in_comment = true;
                out.push_str(HL_COMMENT);
                out.push('/');
                // 吃掉第二个 '/'，避免重复输出成 '///'
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            // 标识符 / 关键字 / 类型 / 内置
            c if c.is_alphabetic() || c == '_' => {
                let mut word = String::new();
                word.push(c);
                while let Some(&n) = chars.peek() {
                    if n.is_alphanumeric() || n == '_' {
                        word.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push_str(&classify_word(&word));
            }
            // 数字（含 0x/0b、下划线、b/d 后缀、浮点）
            c if c.is_ascii_digit()
                || (c == '.' && chars.peek().map(|n| n.is_ascii_digit()).unwrap_or(false)) =>
            {
                let mut tok = String::new();
                tok.push(c);
                while let Some(&n) = chars.peek() {
                    if n.is_alphanumeric() || n == '_' || n == '.' {
                        tok.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push_str(HL_NUMBER);
                out.push_str(&tok);
                out.push_str(RESET);
            }
            // f"..." / r"..." 前缀：字符串前缀并入字符串色
            c if (c == 'f' || c == 'r') && chars.peek() == Some(&'"') => {
                out.push_str(HL_STRING);
                out.push(c);
                chars.next(); // 吃掉 "
                out.push('"');
                in_string = true;
            }
            _ => out.push(c),
        }
    }
    if in_string || in_comment {
        out.push_str(RESET);
    }
    out
}

/// rustyline Helper：Tab 插入缩进、语法高亮、Enter 在块未闭合时继续多行编辑。
struct ReplHelper;

impl Completer for ReplHelper {
    type Candidate = Pair;
    fn complete(
        &self,
        _line: &str,
        _pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Tab 已绑定为插入缩进（见 run_repl），这里不做补全。
        Ok((0, Vec::new()))
    }
}

impl Hinter for ReplHelper {
    type Hint = String;
    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> {
        None
    }
}

impl Highlighter for ReplHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        Cow::Owned(highlight_line(line))
    }
    // 只在光标位于着色区域（字符串/注释/数字/关键字等）时整行重绘，
    // 普通文本走快路径 → 减少闪烁；forced(Enter) 时始终重绘。
    fn highlight_char(&self, line: &str, pos: usize, forced: bool) -> bool {
        forced || char_is_highlighted(line, pos)
    }
}

/// 判断缓冲区中 pos（字节下标）处的字符是否处于着色区域。
fn char_is_highlighted(line: &str, pos: usize) -> bool {
    let mut byte = 0usize;
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    let mut in_comment = false;
    while let Some(c) = chars.next() {
        let start = byte;
        byte += c.len_utf8();
        if in_comment {
            if pos >= start && pos <= byte {
                return true;
            }
            if c == '\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if pos >= start && pos <= byte {
                return true;
            }
            if c == '\\' {
                if let Some(n) = chars.next() {
                    byte += n.len_utf8();
                }
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                if pos >= start && pos <= byte {
                    return true;
                }
                in_string = true;
            }
            '/' if chars.peek() == Some(&'/') => {
                if pos >= start && pos <= byte {
                    return true;
                }
                in_comment = true;
            }
            // f"..." / r"..." 前缀
            c if (c == 'f' || c == 'r') && chars.peek() == Some(&'"') => {
                if pos >= start && pos <= byte {
                    return true;
                }
                chars.next();
                byte += 1;
                in_string = true;
            }
            // 标识符：若为着色词（关键字/类型/内置/常量）则命中。
            // 用 pos <= byte：光标停在词末（刚打完关键字）时也应触发重绘。
            c if c.is_alphabetic() || c == '_' => {
                let mut word = String::new();
                word.push(c);
                while let Some(&n) = chars.peek() {
                    if n.is_alphanumeric() || n == '_' {
                        word.push(n);
                        chars.next();
                        byte += n.len_utf8();
                    } else {
                        break;
                    }
                }
                if classify_word(&word) != word && pos >= start && pos <= byte {
                    return true;
                }
            }
            // 数字
            c if c.is_ascii_digit() => {
                while let Some(&n) = chars.peek() {
                    if n.is_alphanumeric() || n == '_' || n == '.' {
                        chars.next();
                        byte += n.len_utf8();
                    } else {
                        break;
                    }
                }
                if pos >= start && pos <= byte {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

impl Validator for ReplHelper {
    fn validate(&self, ctx: &mut rustyline::validate::ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
        if count_brace_depth(ctx.input()) > 0 {
            Ok(ValidationResult::Incomplete)
        } else {
            Ok(ValidationResult::Valid(None))
        }
    }
}

impl Helper for ReplHelper {}

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
    // 解析当前输入（单独解析，不重放之前的声明）。
    let ast = parse_source_with_diagnostics(input).map_err(|e| repl_parse_error(input, e))?;
    // 每次输入编译成一个全新的顶层函数（唯一名），复用同一个 JitCompiler，
    // 从而保留已编译的全局变量/函数状态：声明只执行一次，input() 即时读取。
    let main_name = format!("__repl_{}", state.input_counter);
    state.input_counter += 1;
    let main_ptr = state
        .compiler
        .compile_with_main(&ast, &main_name)
        .map_err(|e| repl_compile_error(input, e))?;
    let main_fn: fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
    let result = main_fn();
    if result != 0 {
        Ok(result.to_string())
    } else {
        Ok(String::new())
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;

    /// 高亮输出应包含 ANSI 颜色码，且字符串/注释状态跨行保持。
    #[test]
    fn highlight_line_emits_ansi_codes() {
        let src = "fn fib(n: int) -> int {\n    // comment\n    let x = \"a\\tb\"; // c\n    return 0x1F + 1_000;\n}\n";
        let out = highlight_line(src);
        assert!(out.contains("\x1b[1;34mfn\x1b[0m"), "fn keyword; got: {:?}", out);
        assert!(out.contains("\x1b[35mint\x1b[0m"), "int type; got: {:?}", out);
        assert!(
            out.contains("\x1b[90m// comment\n\x1b[0m"),
            "comment reset; got: {:?}",
            out
        );
        assert!(out.contains("\x1b[90m// c\n\x1b[0m"), "comment c; got: {:?}", out);
        assert!(
            out.contains("\x1b[32m\"a\x1b[36m\\t\x1b[32mb\"\x1b[0m"),
            "string+escape; got: {:?}",
            out
        );
        assert!(out.contains("\x1b[36m0x1F\x1b[0m"), "hex; got: {:?}", out);
        assert!(out.contains("\x1b[36m1_000\x1b[0m"), "underscore; got: {:?}", out);
        assert!(
            out.contains("\x1b[1;34mreturn\x1b[0m"),
            "return keyword; got: {:?}",
            out
        );
    }

    /// 光标位于着色区域（关键字/字符串/注释/数字）时应触发重绘。
    #[test]
    fn char_is_highlighted_detects_colored_regions() {
        // "print(x)" 中，光标在 "print" 末尾（pos=5）应命中
        assert!(char_is_highlighted("print(x)", 5));
        // 光标在普通标识符 "x" 处不应命中
        assert!(!char_is_highlighted("print(x)", 7));
        // 字符串内部应命中
        assert!(char_is_highlighted("let s = \"hi\";", 10));
        // 注释内应命中
        assert!(char_is_highlighted("let a = 1; // comment", 15));
        // 数字应命中
        assert!(char_is_highlighted("let n = 1234;", 11));
    }

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
