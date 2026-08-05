//! Frontend pipeline for LLVM backend (mirrors Cranelift preprocess, no codegen).

use bolide_parser::{
    parse_source, CType, Expr, ExternDecl, FuncDef, Param, Program, Statement, Type,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{
    desugar_generators, desugar_traits, expand_macros, inject_builtin_classes, inline_expand,
    monomorphize, std_import_candidates,
};

/// Runtime / extern C function known to LLVM codegen.
#[derive(Clone, Debug)]
pub struct ExternSig {
    pub name: String,
    pub params: Vec<&'static str>,
    pub ret: &'static str,
    /// true at the index of a `func(...)` callback param — the runtime invokes it
    /// as a raw `extern "C" fn(...)` pointer, so codegen must pass the bare
    /// function address, not a closure object.
    pub funcptr_params: Vec<bool>,
    /// parallel to params: for each `func(...)` callback param, its full
    /// `(callback param LLVM types, callback return LLVM type)` signature, used to
    /// build a bare-call trampoline when a closure value is forwarded.
    pub funcptr_sigs: Vec<Option<(Vec<&'static str>, &'static str)>>,
    /// true at the index of a `*c_char` param — a Bolide `str` must be converted
    /// to a C string pointer via `bolide_string_as_cstr` before the call.
    pub cstr_params: Vec<bool>,
    /// true at the index of a `*dynamic` param — the runtime expects a
    /// `BolideDynamic` wrapper, so the arg must be converted via
    /// `bolide_dynamic_from_*` before the call.
    pub dynamic_params: Vec<bool>,
}

/// Fully expanded, monomorphized program ready for LLVM IR emission.
pub struct PreparedProgram {
    pub program: Program,
    /// module alias → prefix used for mangled names (`time` → `@time_`)
    pub modules: HashMap<String, String>,
    /// `extern "bolide"` (+ a few built-ins) available as direct C calls
    pub externs: Vec<ExternSig>,
    /// original free-function name → distinct mangled names (only entries with >1 overload)
    pub overloads: HashMap<String, Vec<String>>,
}

pub fn prepare_program(
    program: &Program,
    base_dir: Option<&str>,
) -> Result<PreparedProgram, String> {
    let mut modules = HashMap::new();
    let mut program = process_imports(program, base_dir, &mut modules)?;
    program = expand_macros(program).map_err(|e| format!("Macro error: {}", e))?;
    program = desugar_traits(program).map_err(|e| format!("Trait error: {}", e))?;
    program = desugar_generators(program).map_err(|e| format!("Generator error: {}", e))?;
    program = inject_builtin_classes(program);
    program = monomorphize(program)?;
    program = inline_expand(program)?;
    // Rewrite `mod.fn(...)` → `@mod_fn(...)` for the whole program (main + modules)
    program = rewrite_module_calls(program, &modules);
    // Free functions overloaded by parameter type (e.g. std/math `abs(int)` / `abs(float)`)
    // would otherwise collide as duplicate LLVM symbols; give each variant a unique name
    // and let codegen pick the right one per call site based on inferred arg types.
    let (program, overloads) = disambiguate_overloaded_functions(program);
    let externs = collect_externs(&program);
    Ok(PreparedProgram {
        program,
        modules,
        externs,
        overloads,
    })
}

/// Renames duplicate top-level `fn name(...)` definitions to unique symbols
/// (`name$<suffix>`) and returns `name → [mangled names]` for overload groups.
fn disambiguate_overloaded_functions(
    mut program: Program,
) -> (Program, HashMap<String, Vec<String>>) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for stmt in &program.statements {
        if let Statement::FuncDef(f) = stmt {
            *counts.entry(f.name.clone()).or_insert(0) += 1;
        }
    }
    let mut overloads: HashMap<String, Vec<String>> = HashMap::new();
    let mut used_names: HashMap<String, usize> = HashMap::new();
    for stmt in &mut program.statements {
        if let Statement::FuncDef(f) = stmt {
            if counts.get(&f.name).copied().unwrap_or(0) <= 1 {
                continue;
            }
            let original = f.name.clone();
            let suffix = function_type_suffix(&f.params);
            let mut mangled = format!("{}${}", original, suffix);
            // Guard against two variants producing the same suffix (e.g. two `ptr` params).
            let dup_idx = used_names.entry(mangled.clone()).or_insert(0);
            if *dup_idx > 0 {
                mangled = format!("{}_{}", mangled, dup_idx);
            }
            *dup_idx += 1;
            f.name = mangled.clone();
            overloads.entry(original).or_default().push(mangled);
        }
    }
    (program, overloads)
}

/// Short per-parameter type code used to build a unique overload symbol.
fn function_type_suffix(params: &[Param]) -> String {
    if params.is_empty() {
        return "0".to_string();
    }
    params.iter().map(|p| type_suffix_code(&p.ty)).collect()
}

fn type_suffix_code(ty: &Type) -> char {
    match ty {
        Type::Int | Type::BigInt => 'i',
        Type::Float | Type::Decimal => 'f',
        Type::Bool => 'b',
        Type::Str | Type::Bytes => 's',
        _ => 'p',
    }
}

/// `time.monotonic_ms()` → `@time_monotonic_ms()` so codegen can resolve like free functions.
fn rewrite_module_calls(program: Program, modules: &HashMap<String, String>) -> Program {
    Program {
        statements: program
            .statements
            .into_iter()
            .map(|s| rewrite_mod_stmt(s, modules))
            .collect(),
    }
}

fn rewrite_mod_stmt(stmt: Statement, modules: &HashMap<String, String>) -> Statement {
    match stmt {
        Statement::Expr(e) => Statement::Expr(rewrite_mod_expr(e, modules)),
        Statement::Return(Some(e)) => Statement::Return(Some(rewrite_mod_expr(e, modules))),
        Statement::Throw(e) => Statement::Throw(rewrite_mod_expr(e, modules)),
        Statement::VarDecl(mut d) => {
            if let Some(v) = d.value.take() {
                d.value = Some(rewrite_mod_expr(v, modules));
            }
            Statement::VarDecl(d)
        }
        Statement::Assign(mut a) => {
            a.target = rewrite_mod_expr(a.target, modules);
            a.value = rewrite_mod_expr(a.value, modules);
            Statement::Assign(a)
        }
        Statement::If(mut i) => {
            i.condition = rewrite_mod_expr(i.condition, modules);
            i.then_body = i
                .then_body
                .into_iter()
                .map(|s| rewrite_mod_stmt(s, modules))
                .collect();
            i.elif_branches = i
                .elif_branches
                .into_iter()
                .map(|(c, b)| {
                    (
                        rewrite_mod_expr(c, modules),
                        b.into_iter()
                            .map(|s| rewrite_mod_stmt(s, modules))
                            .collect(),
                    )
                })
                .collect();
            if let Some(b) = i.else_body {
                i.else_body = Some(
                    b.into_iter()
                        .map(|s| rewrite_mod_stmt(s, modules))
                        .collect(),
                );
            }
            Statement::If(i)
        }
        Statement::While(mut w) => {
            w.condition = rewrite_mod_expr(w.condition, modules);
            w.body = w
                .body
                .into_iter()
                .map(|s| rewrite_mod_stmt(s, modules))
                .collect();
            Statement::While(w)
        }
        Statement::For(mut f) => {
            f.iter = rewrite_mod_expr(f.iter, modules);
            f.body = f
                .body
                .into_iter()
                .map(|s| rewrite_mod_stmt(s, modules))
                .collect();
            Statement::For(f)
        }
        Statement::FuncDef(mut f) => {
            f.body = f
                .body
                .into_iter()
                .map(|s| rewrite_mod_stmt(s, modules))
                .collect();
            Statement::FuncDef(f)
        }
        Statement::ClassDef(mut c) => {
            c.methods = c
                .methods
                .into_iter()
                .map(|mut m| {
                    m.body = m
                        .body
                        .into_iter()
                        .map(|s| rewrite_mod_stmt(s, modules))
                        .collect();
                    m
                })
                .collect();
            Statement::ClassDef(c)
        }
        Statement::Match(mut m) => {
            m.expr = rewrite_mod_expr(m.expr, modules);
            m.arms = m
                .arms
                .into_iter()
                .map(|mut a| {
                    a.body = a
                        .body
                        .into_iter()
                        .map(|s| rewrite_mod_stmt(s, modules))
                        .collect();
                    a
                })
                .collect();
            Statement::Match(m)
        }
        Statement::Try(mut t) => {
            t.try_body = t
                .try_body
                .into_iter()
                .map(|s| rewrite_mod_stmt(s, modules))
                .collect();
            t.catch_clauses = t
                .catch_clauses
                .into_iter()
                .map(|mut c| {
                    c.body = c
                        .body
                        .into_iter()
                        .map(|s| rewrite_mod_stmt(s, modules))
                        .collect();
                    c
                })
                .collect();
            if let Some(f) = t.finally {
                t.finally = Some(
                    f.into_iter()
                        .map(|s| rewrite_mod_stmt(s, modules))
                        .collect(),
                );
            }
            Statement::Try(t)
        }
        other => other,
    }
}

fn rewrite_mod_expr(expr: Expr, modules: &HashMap<String, String>) -> Expr {
    match expr {
        Expr::Call(callee, args) => {
            let args: Vec<Expr> = args
                .into_iter()
                .map(|a| rewrite_mod_expr(a, modules))
                .collect();
            match *callee {
                Expr::Member(base, method) => {
                    if let Expr::Ident(mod_name) = base.as_ref() {
                        if let Some(prefix) = modules.get(mod_name) {
                            return Expr::Call(
                                Box::new(Expr::Ident(format!("{}{}", prefix, method))),
                                args,
                            );
                        }
                    }
                    Expr::Call(
                        Box::new(Expr::Member(
                            Box::new(rewrite_mod_expr(*base, modules)),
                            method,
                        )),
                        args,
                    )
                }
                other => Expr::Call(Box::new(rewrite_mod_expr(other, modules)), args),
            }
        }
        Expr::Member(base, m) => Expr::Member(Box::new(rewrite_mod_expr(*base, modules)), m),
        Expr::BinOp(l, op, r) => Expr::BinOp(
            Box::new(rewrite_mod_expr(*l, modules)),
            op,
            Box::new(rewrite_mod_expr(*r, modules)),
        ),
        Expr::UnaryOp(op, e) => Expr::UnaryOp(op, Box::new(rewrite_mod_expr(*e, modules))),
        Expr::Index(b, i) => Expr::Index(
            Box::new(rewrite_mod_expr(*b, modules)),
            Box::new(rewrite_mod_expr(*i, modules)),
        ),
        Expr::List(items) => Expr::List(
            items
                .into_iter()
                .map(|e| rewrite_mod_expr(e, modules))
                .collect(),
        ),
        Expr::Dict(pairs) => Expr::Dict(
            pairs
                .into_iter()
                .map(|(k, v)| (rewrite_mod_expr(k, modules), rewrite_mod_expr(v, modules)))
                .collect(),
        ),
        other => other,
    }
}

fn collect_externs(program: &Program) -> Vec<ExternSig> {
    let mut out = Vec::new();
    // Always available helpers used by benches / format
    for (name, params, ret) in [
        ("bolide_string_format", vec!["ptr", "ptr", "i64", "ptr", "ptr", "i64"], "ptr"),
        ("bolide_string_to_int", vec!["ptr"], "i64"),
        ("bolide_string_to_float", vec!["ptr"], "double"),
        ("bolide_string_from_int", vec!["i64"], "ptr"),
        ("bolide_string_from_float", vec!["double"], "ptr"),
        ("bolide_string_concat", vec!["ptr", "ptr"], "ptr"),
        ("bolide_string_len", vec!["ptr"], "i64"),
        ("bolide_string_new", vec!["ptr"], "ptr"),
        ("bolide_env_args", vec![], "ptr"),
        ("bolide_time_monotonic_ms", vec![], "i64"),
        ("bolide_time_now_ms", vec![], "i64"),
        ("bolide_time_now", vec![], "i64"),
        ("bolide_math_sqrt", vec!["double"], "double"),
        ("bolide_math_sin", vec!["double"], "double"),
        ("bolide_math_cos", vec!["double"], "double"),
        ("bolide_math_pow", vec!["double", "double"], "double"),
        ("bolide_math_abs_f64", vec!["double"], "double"),
        ("bolide_math_floor", vec!["double"], "double"),
        ("bolide_math_ceil", vec!["double"], "double"),
    ] {
        let n_params = params.len();
        out.push(ExternSig {
            name: name.into(),
            params: params.into_iter().map(|s| s).collect(),
            ret,
            funcptr_params: vec![false; n_params],
            funcptr_sigs: vec![None; n_params],
            cstr_params: vec![false; n_params],
            dynamic_params: vec![false; n_params],
        });
    }
    for stmt in &program.statements {
        if let Statement::ExternBlock(eb) = stmt {
            // bolide / empty → runtime
            for d in &eb.declarations {
                if let ExternDecl::Function(f) = d {
                    let params: Vec<&'static str> =
                        f.params.iter().map(|p| ctype_llvm(&p.ty)).collect();
                    let funcptr_params: Vec<bool> = f
                        .params
                        .iter()
                        .map(|p| matches!(p.ty, CType::FuncPtr { .. }))
                        .collect();
                    let funcptr_sigs: Vec<Option<(Vec<&'static str>, &'static str)>> = f
                        .params
                        .iter()
                        .map(|p| match &p.ty {
                            CType::FuncPtr { params, return_type } => Some((
                                params.iter().map(ctype_llvm).collect(),
                                ctype_llvm(return_type),
                            )),
                            _ => None,
                        })
                        .collect();
                    let cstr_params: Vec<bool> = f
                        .params
                        .iter()
                        .map(|p| {
                            matches!(&p.ty, CType::Ptr(inner) if matches!(inner.as_ref(), CType::Char))
                        })
                        .collect();
                    let dynamic_params: Vec<bool> = f
                        .params
                        .iter()
                        .map(|p| {
                            matches!(&p.ty, CType::Ptr(inner) if matches!(inner.as_ref(), CType::Struct(n) if n == "dynamic"))
                        })
                        .collect();
                    let ret = f
                        .return_type
                        .as_ref()
                        .map(ctype_llvm)
                        .unwrap_or("void");
                    // avoid dup
                    if !out.iter().any(|e| e.name == f.name) {
                        out.push(ExternSig {
                            name: f.name.clone(),
                            params,
                            ret,
                            funcptr_params,
                            funcptr_sigs,
                            cstr_params,
                            dynamic_params,
                        });
                    }
                }
            }
        }
    }
    out
}

fn is_builtin_name(name: &str) -> bool {
    matches!(
        name,
        "print"
            | "int"
            | "float"
            | "str"
            | "bool"
            | "range"
            | "len"
            | "assert"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
    )
}

fn ctype_llvm(ty: &CType) -> &'static str {
    match ty {
        CType::Float | CType::Double => "double",
        CType::Void => "void",
        CType::FuncPtr { .. } => "ptr", // callback: raw extern "C" fn pointer
        CType::Ptr(_) | CType::Struct(_) => "ptr", // str, list, *c_char, …
        _ => "i64",
    }
}

fn process_imports(
    program: &Program,
    base_dir: Option<&str>,
    modules: &mut HashMap<String, String>,
) -> Result<Program, String> {
    let mut out = Vec::new();
    for stmt in &program.statements {
        match stmt {
            Statement::Import(imp) => {
                let file_path = resolve_import_path(imp, base_dir)?;
                let alias = imp
                    .alias
                    .clone()
                    .or_else(|| {
                        Path::new(&file_path)
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                    })
                    .unwrap_or_else(|| "mod".to_string());
                let prefix = format!("@{}_", alias);
                modules.insert(alias.clone(), prefix.clone());

                let src = std::fs::read_to_string(&file_path)
                    .map_err(|e| format!("Failed to read import '{}': {}", file_path, e))?;
                let mut mod_prog =
                    parse_source(&src).map_err(|e| format!("Parse import '{}': {}", file_path, e))?;
                // Nested imports relative to this module
                let parent = Path::new(&file_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string());
                mod_prog = process_imports(&mod_prog, parent.as_deref(), modules)?;
                // Collect module-level globals for PI-style name mangling
                let mut mod_globals: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let mut mod_funcs: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for s in &mod_prog.statements {
                    match s {
                        Statement::VarDecl(d) => {
                            mod_globals.insert(d.name.clone());
                        }
                        Statement::FuncDef(f) => {
                            mod_funcs.insert(f.name.clone());
                        }
                        _ => {}
                    }
                }
                for s in mod_prog.statements {
                    out.push(mangle_module_stmt(
                        s,
                        &prefix,
                        &alias,
                        &mod_globals,
                        &mod_funcs,
                    )?);
                }
            }
            other => out.push(other.clone()),
        }
    }
    Ok(Program { statements: out })
}

fn resolve_import_path(
    imp: &bolide_parser::Import,
    base_dir: Option<&str>,
) -> Result<String, String> {
    let rel = if let Some(ref fp) = imp.file_path {
        fp.clone()
    } else if !imp.path.is_empty() {
        // dotted path → join
        let mut p = imp.path.join("/");
        if !p.ends_with(".bl") {
            p.push_str(".bl");
        }
        p
    } else {
        return Err("import missing path".into());
    };

    let candidates = std_import_candidates(&rel);
    let mut search_roots = Vec::new();
    if let Some(b) = base_dir {
        search_roots.push(PathBuf::from(b));
    }
    if let Ok(home) = std::env::var("BOLIDE_HOME") {
        search_roots.push(PathBuf::from(home));
    }
    // 按 exe 位置找依赖（与 Cranelift JIT 一致）：bolide.exe 同目录下的 std/、
    // std 子模块等。这样从任意 CWD 运行都能解析 import。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            search_roots.push(exe_dir.to_path_buf());
            // 常见布局：exe 在 target/release/，std 在仓库根
            search_roots.push(exe_dir.join(".."));
        }
    }
    // CWD
    if let Ok(cwd) = std::env::current_dir() {
        search_roots.push(cwd);
    }

    for root in &search_roots {
        for c in &candidates {
            let full = root.join(c);
            if full.is_file() {
                return Ok(full.to_string_lossy().to_string());
            }
        }
        // also try as-is under root
        let full = root.join(&rel);
        if full.is_file() {
            return Ok(full.to_string_lossy().to_string());
        }
    }
    Err(format!(
        "Cannot resolve import '{}' (tried under base_dir/BOLIDE_HOME/exe-dir/cwd)",
        rel
    ))
}

fn mangle_module_stmt(
    stmt: Statement,
    prefix: &str,
    _alias: &str,
    globals: &std::collections::HashSet<String>,
    funcs: &std::collections::HashSet<String>,
) -> Result<Statement, String> {
    match stmt {
        Statement::FuncDef(mut f) => {
            if !f.name.starts_with('@') {
                f.name = format!("{}{}", prefix, f.name);
            }
            f.body = mangle_body(f.body, prefix, globals, funcs)?;
            Ok(Statement::FuncDef(f))
        }
        Statement::VarDecl(mut d) => {
            if !d.name.starts_with('@') {
                d.name = format!("{}{}", prefix, d.name);
            }
            if let Some(v) = d.value.take() {
                d.value = Some(mangle_expr(v, prefix, globals, funcs)?);
            }
            Ok(Statement::VarDecl(d))
        }
        Statement::ClassDef(mut c) => {
            let mut methods = Vec::new();
            for mut m in c.methods.drain(..) {
                let body = std::mem::take(&mut m.body);
                m.body = mangle_body(body, prefix, globals, funcs)?;
                methods.push(m);
            }
            c.methods = methods;
            Ok(Statement::ClassDef(c))
        }
        other => Ok(other),
    }
}

fn mangle_body(
    body: Vec<Statement>,
    prefix: &str,
    globals: &std::collections::HashSet<String>,
    funcs: &std::collections::HashSet<String>,
) -> Result<Vec<Statement>, String> {
    body.into_iter()
        .map(|s| mangle_stmt_in_fn(s, prefix, globals, funcs))
        .collect()
}

fn mangle_stmt_in_fn(
    stmt: Statement,
    prefix: &str,
    globals: &std::collections::HashSet<String>,
    funcs: &std::collections::HashSet<String>,
) -> Result<Statement, String> {
    match stmt {
        Statement::Expr(e) => Ok(Statement::Expr(mangle_expr(e, prefix, globals, funcs)?)),
        Statement::Return(Some(e)) => {
            Ok(Statement::Return(Some(mangle_expr(e, prefix, globals, funcs)?)))
        }
        Statement::Return(None) => Ok(Statement::Return(None)),
        Statement::VarDecl(mut d) => {
            if let Some(v) = d.value.take() {
                d.value = Some(mangle_expr(v, prefix, globals, funcs)?);
            }
            Ok(Statement::VarDecl(d))
        }
        Statement::Assign(mut a) => {
            a.value = mangle_expr(a.value, prefix, globals, funcs)?;
            a.target = mangle_expr(a.target, prefix, globals, funcs)?;
            Ok(Statement::Assign(a))
        }
        Statement::If(mut i) => {
            i.condition = mangle_expr(i.condition, prefix, globals, funcs)?;
            i.then_body = mangle_body(i.then_body, prefix, globals, funcs)?;
            let mut elifs = Vec::new();
            for (c, b) in i.elif_branches {
                elifs.push((
                    mangle_expr(c, prefix, globals, funcs)?,
                    mangle_body(b, prefix, globals, funcs)?,
                ));
            }
            i.elif_branches = elifs;
            if let Some(b) = i.else_body {
                i.else_body = Some(mangle_body(b, prefix, globals, funcs)?);
            }
            Ok(Statement::If(i))
        }
        Statement::While(mut w) => {
            w.condition = mangle_expr(w.condition, prefix, globals, funcs)?;
            w.body = mangle_body(w.body, prefix, globals, funcs)?;
            Ok(Statement::While(w))
        }
        Statement::For(mut f) => {
            f.iter = mangle_expr(f.iter, prefix, globals, funcs)?;
            f.body = mangle_body(f.body, prefix, globals, funcs)?;
            Ok(Statement::For(f))
        }
        other => Ok(other),
    }
}

fn mangle_expr(
    expr: Expr,
    prefix: &str,
    globals: &std::collections::HashSet<String>,
    funcs: &std::collections::HashSet<String>,
) -> Result<Expr, String> {
    match expr {
        Expr::Call(callee, args) => {
            let callee = mangle_expr(*callee, prefix, globals, funcs)?;
            let mut new_args = Vec::new();
            for a in args {
                new_args.push(mangle_expr(a, prefix, globals, funcs)?);
            }
            let callee = if let Expr::Ident(name) = &callee {
                if !name.starts_with('@')
                    && !name.starts_with("bolide_")
                    && !is_builtin_name(name)
                    && (funcs.contains(name) || globals.contains(name))
                {
                    Expr::Ident(format!("{}{}", prefix, name))
                } else if !name.starts_with('@')
                    && !name.starts_with("bolide_")
                    && !is_builtin_name(name)
                    && !funcs.contains(name)
                    && !globals.contains(name)
                {
                    // bare call that looks like local module fn (e.g. helper in same file)
                    // only prefix if it matches a known module function
                    callee
                } else {
                    callee
                }
            } else {
                callee
            };
            // still prefix bare module function calls not in funcs set (recursive helpers)
            let callee = if let Expr::Ident(name) = &callee {
                if !name.starts_with('@')
                    && !name.starts_with("bolide_")
                    && !is_builtin_name(name)
                    && funcs.contains(name)
                {
                    Expr::Ident(format!("{}{}", prefix, name))
                } else {
                    callee
                }
            } else {
                callee
            };
            Ok(Expr::Call(Box::new(callee), new_args))
        }
        Expr::Ident(name) => {
            if globals.contains(&name) && !name.starts_with('@') {
                Ok(Expr::Ident(format!("{}{}", prefix, name)))
            } else {
                Ok(Expr::Ident(name))
            }
        }
        Expr::Member(base, member) => Ok(Expr::Member(
            Box::new(mangle_expr(*base, prefix, globals, funcs)?),
            member,
        )),
        Expr::BinOp(l, op, r) => Ok(Expr::BinOp(
            Box::new(mangle_expr(*l, prefix, globals, funcs)?),
            op,
            Box::new(mangle_expr(*r, prefix, globals, funcs)?),
        )),
        Expr::UnaryOp(op, e) => Ok(Expr::UnaryOp(
            op,
            Box::new(mangle_expr(*e, prefix, globals, funcs)?),
        )),
        Expr::Index(b, i) => Ok(Expr::Index(
            Box::new(mangle_expr(*b, prefix, globals, funcs)?),
            Box::new(mangle_expr(*i, prefix, globals, funcs)?),
        )),
        other => Ok(other),
    }
}

#[allow(dead_code)]
pub fn type_is_float(ty: &Option<Type>) -> bool {
    matches!(ty, Some(Type::Float))
}

#[allow(dead_code)]
pub fn func_return_is_float(f: &FuncDef) -> bool {
    matches!(f.return_type, Some(Type::Float))
}
