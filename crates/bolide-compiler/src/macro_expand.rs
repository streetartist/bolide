//! Bolide 宏展开：声明式宏、`!` 调用、属性 `@derive` / `@test`、内置 `assert!`/`dbg!` 等。
//!
//! 管线位置：import 合并之后、内置类注入与 monomorph 之前。

use bolide_parser::{
    AttrArg, Attribute, AttrMacroDef, BinOp, ClassDef, ClassField, ComptimeFn, EnumDef, Expr,
    FragKind, FuncDef, MacroArg, MacroArgs, MacroDef, MacroInvoke, MacroPattern, PatPiece, Program,
    SpliceMeta, Statement, Type, UnaryOp, ValueDef, ValueField, VarDecl,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static EXPAND_ID: AtomicU64 = AtomicU64::new(1);
const MAX_DEPTH: usize = 128;

/// 展开上下文（调用点文件/行，用于 `$x:file` / `$x:line`）
#[derive(Clone, Debug, Default)]
pub struct ExpandContext {
    pub file: String,
    pub line: usize,
}

#[derive(Clone, Debug)]
enum Capture {
    Expr(Expr),
    Ident(String),
    /// 重复：每次迭代的绑定表
    Rep(Vec<HashMap<String, Capture>>),
}

/// 展开程序中的宏定义/调用与属性。成功后 AST 中不应再有 MacroDef/MacroInvoke/Splice。
pub fn expand_macros(program: Program) -> Result<Program, String> {
    expand_macros_with_ctx(
        program,
        &ExpandContext {
            file: "<input>".to_string(),
            line: 1,
        },
    )
}

pub fn expand_macros_with_ctx(program: Program, ctx: &ExpandContext) -> Result<Program, String> {
    let mut macros: HashMap<String, MacroDef> = HashMap::new();
    let mut attr_macros: HashMap<String, AttrMacroDef> = HashMap::new();
    let mut comptime_fns: HashMap<String, ComptimeFn> = HashMap::new();
    register_builtins(&mut macros);

    for stmt in &program.statements {
        match stmt {
            Statement::MacroDef(m) => {
                macros.insert(m.name.clone(), m.clone());
                if let Some(rest) = m.name.strip_prefix('@') {
                    if let Some((mod_name, short)) = rest.split_once('_') {
                        macros
                            .entry(format!("{}.{}", mod_name, short))
                            .or_insert_with(|| m.clone());
                    }
                }
            }
            Statement::AttrMacroDef(m) => {
                attr_macros.insert(m.name.clone(), m.clone());
                if let Some(rest) = m.name.strip_prefix('@') {
                    if let Some((mod_name, short)) = rest.split_once('_') {
                        attr_macros
                            .entry(format!("{}.{}", mod_name, short))
                            .or_insert_with(|| m.clone());
                    }
                }
            }
            Statement::ComptimeFn(f) => {
                comptime_fns.insert(f.name.clone(), f.clone());
            }
            _ => {}
        }
    }

    let env = ExpandEnv {
        macros: &macros,
        attr_macros: &attr_macros,
        comptime_fns: &comptime_fns,
    };

    let mut out_stmts = Vec::new();
    for stmt in program.statements {
        match stmt {
            Statement::MacroDef(_) | Statement::AttrMacroDef(_) | Statement::ComptimeFn(_) => {}
            Statement::MacroRep { .. } => {
                return Err(
                    "`$(...)*` is only valid inside macro / attr-macro / quote templates"
                        .to_string(),
                );
            }
            other => {
                let expanded = expand_statement(other, &env, ctx, 0)?;
                out_stmts.extend(expanded);
            }
        }
    }

    Ok(Program {
        statements: out_stmts,
    })
}

struct ExpandEnv<'a> {
    macros: &'a HashMap<String, MacroDef>,
    attr_macros: &'a HashMap<String, AttrMacroDef>,
    comptime_fns: &'a HashMap<String, ComptimeFn>,
}

fn register_builtins(macros: &mut HashMap<String, MacroDef>) {
    // 内置宏用空 arms 占位；展开时按名字特殊处理
    for name in ["assert", "assert_eq", "dbg", "todo", "unreachable", "stringify"] {
        macros.entry(name.to_string()).or_insert(MacroDef {
            name: name.to_string(),
            is_export: true,
            arms: vec![],
            def_span_start: None,
        });
    }
}

fn expand_statement(
    stmt: Statement,
    env: &ExpandEnv,
    ctx: &ExpandContext,
    depth: usize,
) -> Result<Vec<Statement>, String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "macro expansion exceeded depth limit ({})",
            MAX_DEPTH
        ));
    }
    match stmt {
        Statement::Expr(Expr::MacroInvoke(inv)) => {
            let pieces = expand_invoke(&inv, env, ctx, depth)?;
            let mut out = Vec::new();
            for p in pieces {
                out.extend(expand_statement(p, env, ctx, depth + 1)?);
            }
            Ok(out)
        }
        Statement::Expr(e) => {
            let e = expand_expr(e, env, ctx, depth)?;
            Ok(vec![Statement::Expr(e)])
        }
        Statement::VarDecl(mut d) => {
            if let Some(v) = d.value.take() {
                d.value = Some(expand_expr(v, env, ctx, depth)?);
            }
            // name_is_splice 仅应出现在模板内，展开后若仍在则为错误
            if d.name_is_splice {
                return Err(format!(
                    "unexpanded macro splice in variable name '${}'",
                    d.name
                ));
            }
            Ok(vec![Statement::VarDecl(d)])
        }
        Statement::Assign(mut a) => {
            a.target = expand_expr(a.target, env, ctx, depth)?;
            a.value = expand_expr(a.value, env, ctx, depth)?;
            Ok(vec![Statement::Assign(a)])
        }
        Statement::Return(Some(e)) => {
            Ok(vec![Statement::Return(Some(expand_expr(e, env, ctx, depth)?))])
        }
        Statement::Return(None) => Ok(vec![Statement::Return(None)]),
        Statement::Throw(e) => Ok(vec![Statement::Throw(expand_expr(e, env, ctx, depth)?)]),
        Statement::If(mut s) => {
            s.condition = expand_expr(s.condition, env, ctx, depth)?;
            s.then_body = expand_stmts(s.then_body, env, ctx, depth)?;
            let mut elifs = Vec::new();
            for (c, b) in s.elif_branches {
                elifs.push((
                    expand_expr(c, env, ctx, depth)?,
                    expand_stmts(b, env, ctx, depth)?,
                ));
            }
            s.elif_branches = elifs;
            if let Some(eb) = s.else_body {
                s.else_body = Some(expand_stmts(eb, env, ctx, depth)?);
            }
            Ok(vec![Statement::If(s)])
        }
        Statement::While(mut s) => {
            s.condition = expand_expr(s.condition, env, ctx, depth)?;
            s.body = expand_stmts(s.body, env, ctx, depth)?;
            Ok(vec![Statement::While(s)])
        }
        Statement::For(mut s) => {
            s.iter = expand_expr(s.iter, env, ctx, depth)?;
            s.body = expand_stmts(s.body, env, ctx, depth)?;
            Ok(vec![Statement::For(s)])
        }
        Statement::FuncDef(f) => {
            let funcs = apply_fn_attrs(f, env, ctx)?;
            let mut out = Vec::new();
            for mut f in funcs {
                f.body = expand_stmts(f.body, env, ctx, depth)?;
                f.attrs.clear();
                out.push(Statement::FuncDef(f));
            }
            Ok(out)
        }
        Statement::With(w) => {
            let desugared = desugar_with(w)?;
            expand_stmts(desugared, env, ctx, depth)
        }
        Statement::ClassDef(c) => {
            // 属性只应用一次；不要再对 ClassDef 递归 expand_statement
            let stmts = apply_class_attrs(c, env, ctx)?;
            let mut out = Vec::new();
            for st in stmts {
                match st {
                    Statement::ClassDef(mut cls) => {
                        for m in &mut cls.methods {
                            m.body = expand_stmts(std::mem::take(&mut m.body), env, ctx, depth)?;
                            m.attrs.clear();
                        }
                        cls.attrs.clear();
                        out.push(Statement::ClassDef(cls));
                    }
                    other => out.extend(expand_statement(other, env, ctx, depth)?),
                }
            }
            Ok(out)
        }
        Statement::ValueDef(v) => {
            let stmts = apply_value_attrs(v, env, ctx)?;
            let mut out = Vec::new();
            for st in stmts {
                match st {
                    Statement::ValueDef(mut vd) => {
                        vd.attrs.clear();
                        out.push(Statement::ValueDef(vd));
                    }
                    Statement::FuncDef(mut f) => {
                        f.body = expand_stmts(f.body, env, ctx, depth)?;
                        f.attrs.clear();
                        out.push(Statement::FuncDef(f));
                    }
                    other => out.extend(expand_statement(other, env, ctx, depth)?),
                }
            }
            Ok(out)
        }
        Statement::EnumDef(mut e) => {
            e.attrs.clear();
            Ok(vec![Statement::EnumDef(e)])
        }
        Statement::Try(mut t) => {
            t.try_body = expand_stmts(t.try_body, env, ctx, depth)?;
            for c in &mut t.catch_clauses {
                c.body = expand_stmts(std::mem::take(&mut c.body), env, ctx, depth)?;
            }
            if let Some(f) = t.finally.take() {
                t.finally = Some(expand_stmts(f, env, ctx, depth)?);
            }
            Ok(vec![Statement::Try(t)])
        }
        Statement::Match(mut m) => {
            m.expr = expand_expr(m.expr, env, ctx, depth)?;
            for arm in &mut m.arms {
                arm.body = expand_stmts(std::mem::take(&mut arm.body), env, ctx, depth)?;
            }
            Ok(vec![Statement::Match(m)])
        }
        Statement::Pool(mut p) => {
            p.size = expand_expr(p.size, env, ctx, depth)?;
            p.body = expand_stmts(p.body, env, ctx, depth)?;
            Ok(vec![Statement::Pool(p)])
        }
        Statement::AwaitScope(mut a) => {
            a.body = expand_stmts(a.body, env, ctx, depth)?;
            Ok(vec![Statement::AwaitScope(a)])
        }
        Statement::Select(mut s) => {
            for b in &mut s.branches {
                match b {
                    bolide_parser::SelectBranch::Recv { body, .. } => {
                        *body = expand_stmts(std::mem::take(body), env, ctx, depth)?;
                    }
                    bolide_parser::SelectBranch::Timeout { duration, body } => {
                        let d = std::mem::replace(duration, Expr::Int(0));
                        *duration = expand_expr(d, env, ctx, depth)?;
                        *body = expand_stmts(std::mem::take(body), env, ctx, depth)?;
                    }
                    bolide_parser::SelectBranch::Default { body } => {
                        *body = expand_stmts(std::mem::take(body), env, ctx, depth)?;
                    }
                }
            }
            Ok(vec![Statement::Select(s)])
        }
        Statement::SpawnSelect(mut s) => {
            for b in &mut s.branches {
                match b {
                    bolide_parser::SpawnSelectBranch::Bind { expr, body, .. } => {
                        let e = std::mem::replace(expr, Expr::Int(0));
                        *expr = expand_expr(e, env, ctx, depth)?;
                        *body = expand_stmts(std::mem::take(body), env, ctx, depth)?;
                    }
                    bolide_parser::SpawnSelectBranch::Expr { expr, body } => {
                        let e = std::mem::replace(expr, Expr::Int(0));
                        *expr = expand_expr(e, env, ctx, depth)?;
                        *body = expand_stmts(std::mem::take(body), env, ctx, depth)?;
                    }
                }
            }
            Ok(vec![Statement::SpawnSelect(s)])
        }
        other => Ok(vec![other]),
    }
}

fn expand_stmts(
    stmts: Vec<Statement>,
    env: &ExpandEnv,
    ctx: &ExpandContext,
    depth: usize,
) -> Result<Vec<Statement>, String> {
    let mut out = Vec::new();
    for s in stmts {
        out.extend(expand_statement(s, env, ctx, depth)?);
    }
    Ok(out)
}

fn expand_expr(
    expr: Expr,
    env: &ExpandEnv,
    ctx: &ExpandContext,
    depth: usize,
) -> Result<Expr, String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "macro expansion exceeded depth limit ({})",
            MAX_DEPTH
        ));
    }
    match expr {
        Expr::MacroInvoke(inv) => {
            let stmts = expand_invoke(&inv, env, ctx, depth)?;
            stmts_to_expr(stmts, &inv)
        }
        Expr::Comptime(body) => {
            let body = expand_stmts(body, env, ctx, depth)?;
            eval_comptime_block(&body, env.comptime_fns)
        }
        Expr::Splice { name, .. } => Err(format!(
            "unexpanded macro splice `${}` outside macro template",
            name
        )),
        Expr::BinOp(l, op, r) => Ok(Expr::BinOp(
            Box::new(expand_expr(*l, env, ctx, depth)?),
            op,
            Box::new(expand_expr(*r, env, ctx, depth)?),
        )),
        Expr::UnaryOp(op, e) => Ok(Expr::UnaryOp(
            op,
            Box::new(expand_expr(*e, env, ctx, depth)?),
        )),
        Expr::Call(c, args) => {
            let c = expand_expr(*c, env, ctx, depth)?;
            let mut new_args = Vec::new();
            for a in args {
                new_args.push(expand_expr(a, env, ctx, depth)?);
            }
            Ok(Expr::Call(Box::new(c), new_args))
        }
        Expr::Index(b, i) => Ok(Expr::Index(
            Box::new(expand_expr(*b, env, ctx, depth)?),
            Box::new(expand_expr(*i, env, ctx, depth)?),
        )),
        Expr::Slice(b, s, e, st) => Ok(Expr::Slice(
            Box::new(expand_expr(*b, env, ctx, depth)?),
            map_opt_box(s, env, ctx, depth)?,
            map_opt_box(e, env, ctx, depth)?,
            map_opt_box(st, env, ctx, depth)?,
        )),
        Expr::Member(b, m) => Ok(Expr::Member(
            Box::new(expand_expr(*b, env, ctx, depth)?),
            m,
        )),
        Expr::List(items) => {
            let mut v = Vec::new();
            for i in items {
                v.push(expand_expr(i, env, ctx, depth)?);
            }
            Ok(Expr::List(v))
        }
        Expr::Tuple(items) => {
            let mut v = Vec::new();
            for i in items {
                v.push(expand_expr(i, env, ctx, depth)?);
            }
            Ok(Expr::Tuple(v))
        }
        Expr::Dict(entries) => {
            let mut v = Vec::new();
            for (k, val) in entries {
                v.push((
                    expand_expr(k, env, ctx, depth)?,
                    expand_expr(val, env, ctx, depth)?,
                ));
            }
            Ok(Expr::Dict(v))
        }
        Expr::Await(e) => Ok(Expr::Await(Box::new(expand_expr(*e, env, ctx, depth)?))),
        Expr::Propagate(e) => Ok(Expr::Propagate(Box::new(expand_expr(
            *e, env, ctx, depth,
        )?))),
        Expr::Raise(e) => Ok(Expr::Raise(Box::new(expand_expr(*e, env, ctx, depth)?))),
        Expr::NamedArg(n, e) => Ok(Expr::NamedArg(
            n,
            Box::new(expand_expr(*e, env, ctx, depth)?),
        )),
        Expr::SpreadArg(e) => Ok(Expr::SpreadArg(Box::new(expand_expr(
            *e, env, ctx, depth,
        )?))),
        Expr::KwSpreadArg(e) => Ok(Expr::KwSpreadArg(Box::new(expand_expr(
            *e, env, ctx, depth,
        )?))),
        Expr::Spawn(n, args) => {
            let mut v = Vec::new();
            for a in args {
                v.push(expand_expr(a, env, ctx, depth)?);
            }
            Ok(Expr::Spawn(n, v))
        }
        Expr::SpawnThread(n, args) => {
            let mut v = Vec::new();
            for a in args {
                v.push(expand_expr(a, env, ctx, depth)?);
            }
            Ok(Expr::SpawnThread(n, v))
        }
        Expr::SpawnAll(args) => {
            let mut v = Vec::new();
            for a in args {
                v.push(expand_expr(a, env, ctx, depth)?);
            }
            Ok(Expr::SpawnAll(v))
        }
        Expr::ValueConstruct(n, fields) => {
            let mut v = Vec::new();
            for (f, e) in fields {
                v.push((f, expand_expr(e, env, ctx, depth)?));
            }
            Ok(Expr::ValueConstruct(n, v))
        }
        Expr::TryExpr(body) => Ok(Expr::TryExpr(expand_stmts(body, env, ctx, depth)?)),
        Expr::Closure {
            params,
            return_type,
            body,
        } => Ok(Expr::Closure {
            params,
            return_type,
            body: expand_stmts(body, env, ctx, depth)?,
        }),
        Expr::ListComprehension {
            expr,
            vars,
            iter,
            filter,
        } => Ok(Expr::ListComprehension {
            expr: Box::new(expand_expr(*expr, env, ctx, depth)?),
            vars,
            iter: Box::new(expand_expr(*iter, env, ctx, depth)?),
            filter: match filter {
                Some(f) => Some(Box::new(expand_expr(*f, env, ctx, depth)?)),
                None => None,
            },
        }),
        other => Ok(other),
    }
}

fn map_opt_box(
    o: Option<Box<Expr>>,
    env: &ExpandEnv,
    ctx: &ExpandContext,
    depth: usize,
) -> Result<Option<Box<Expr>>, String> {
    match o {
        Some(e) => Ok(Some(Box::new(expand_expr(*e, env, ctx, depth)?))),
        None => Ok(None),
    }
}

fn expand_invoke(
    inv: &MacroInvoke,
    env: &ExpandEnv,
    ctx: &ExpandContext,
    depth: usize,
) -> Result<Vec<Statement>, String> {
    let name = inv.path.last().cloned().unwrap_or_default();
    let full = inv.path.join(".");

    // 内置优先（仅短名）
    if inv.path.len() == 1 {
        if let Some(stmts) = try_expand_builtin(&name, inv, ctx)? {
            return Ok(stmts);
        }
    }

    let def = resolve_macro_def(inv, env.macros).ok_or_else(|| {
        format!(
            "unknown macro `{}!` (macro calls require `!`; define with `macro {}` or `export macro {}`)",
            full, name, name
        )
    })?;

    if def.arms.is_empty() && is_builtin_name(&name) {
        return Err(format!("builtin macro `{}!` failed to expand", name));
    }

    let mut last_err = String::new();
    for arm in &def.arms {
        match match_pattern(&arm.pattern, &inv.args) {
            Ok(captures) => {
                let id = EXPAND_ID.fetch_add(1, Ordering::Relaxed);
                let body = substitute_stmts(&arm.body, &captures, ctx, id)?;
                // 卫生：重命名宏内引入的绑定（非 splice / 非 capture ident）
                let body = apply_hygiene(body, id, &captures);
                // 递归展开嵌套宏
                return expand_stmts(body, env, ctx, depth + 1);
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!(
        "no arm of macro `{}!` matched arguments: {}",
        name, last_err
    ))
}

fn is_builtin_name(name: &str) -> bool {
    matches!(
        name,
        "assert" | "assert_eq" | "dbg" | "todo" | "unreachable" | "stringify"
    )
}

fn resolve_macro_def<'a>(
    inv: &MacroInvoke,
    macros: &'a HashMap<String, MacroDef>,
) -> Option<&'a MacroDef> {
    let name = inv.path.last()?;
    let full = inv.path.join(".");
    if let Some(d) = macros.get(&full) {
        return Some(d);
    }
    if let Some(d) = macros.get(name) {
        return Some(d);
    }
    // path [mod, name] → @mod_name
    if inv.path.len() >= 2 {
        let module = &inv.path[inv.path.len() - 2];
        let key = format!("@{}_{}", module, name);
        if let Some(d) = macros.get(&key) {
            return Some(d);
        }
        let dotted = format!("{}.{}", module, name);
        if let Some(d) = macros.get(&dotted) {
            return Some(d);
        }
    }
    // 后缀匹配 @anything_name
    macros
        .iter()
        .find(|(k, _)| k.ends_with(&format!("_{}", name)))
        .map(|(_, v)| v)
}

fn try_expand_builtin(
    name: &str,
    inv: &MacroInvoke,
    ctx: &ExpandContext,
) -> Result<Option<Vec<Statement>>, String> {
    let args = match &inv.args {
        MacroArgs::Paren(a) => a,
        MacroArgs::Brace(_) if name == "assert" || name == "dbg" => {
            return Err(format!(
                "`{}!` expects parentheses: {}!(...)",
                name, name
            ));
        }
        MacroArgs::Brace(_) => return Ok(None),
    };

    match name {
        "assert" => {
            let cond = one_expr_arg(args, "assert")?;
            let src = expr_to_src(&cond);
            // if not (cond) { throw Error("assertion failed: ..."); }
            Ok(Some(vec![Statement::If(bolide_parser::IfStmt {
                condition: Expr::UnaryOp(UnaryOp::Not, Box::new(cond)),
                then_body: vec![Statement::Throw(Expr::Call(
                    Box::new(Expr::Ident("Error".to_string())),
                    vec![Expr::String(format!("assertion failed: {}", src))],
                ))],
                elif_branches: vec![],
                else_body: None,
            })]))
        }
        "assert_eq" => {
            if args.len() != 2 {
                return Err("assert_eq! expects two arguments".to_string());
            }
            let a = arg_as_expr(&args[0])?;
            let b = arg_as_expr(&args[1])?;
            let sa = expr_to_src(&a);
            let sb = expr_to_src(&b);
            Ok(Some(vec![Statement::If(bolide_parser::IfStmt {
                condition: Expr::UnaryOp(
                    UnaryOp::Not,
                    Box::new(Expr::BinOp(
                        Box::new(a),
                        BinOp::Eq,
                        Box::new(b),
                    )),
                ),
                then_body: vec![Statement::Throw(Expr::Call(
                    Box::new(Expr::Ident("Error".to_string())),
                    vec![Expr::String(format!(
                        "assert_eq! failed: {} != {}",
                        sa, sb
                    ))],
                ))],
                elif_branches: vec![],
                else_body: None,
            })]))
        }
        "dbg" => {
            // let tmp = e; print("[file:line] src => (dbg)"); tmp
            let e = one_expr_arg(args, "dbg")?;
            let src = expr_to_src(&e);
            let id = EXPAND_ID.fetch_add(1, Ordering::Relaxed);
            let tmp = format!("__dbg_{}", id);
            Ok(Some(vec![
                Statement::VarDecl(VarDecl {
                    name: tmp.clone(),
                    mutable: false,
                    ty: None,
                    value: Some(e),
                    name_is_splice: false,
                }),
                Statement::Expr(Expr::Call(
                    Box::new(Expr::Ident("print".to_string())),
                    vec![Expr::String(format!(
                        "[{}:{}] {} => (dbg)",
                        ctx.file, ctx.line, src
                    ))],
                )),
                Statement::Expr(Expr::Ident(tmp)),
            ]))
        }
        "todo" => {
            let msg = if args.is_empty() {
                "not yet implemented".to_string()
            } else {
                match &args[0] {
                    MacroArg::Expr(Expr::String(s)) => s.clone(),
                    MacroArg::Expr(e) => expr_to_src(e),
                    MacroArg::Named { value, .. } => expr_to_src(value),
                }
            };
            Ok(Some(vec![Statement::Throw(Expr::Call(
                Box::new(Expr::Ident("Error".to_string())),
                vec![Expr::String(format!(
                    "TODO at {}:{}: {}",
                    ctx.file, ctx.line, msg
                ))],
            ))]))
        }
        "unreachable" => Ok(Some(vec![Statement::Throw(Expr::Call(
            Box::new(Expr::Ident("Error".to_string())),
            vec![Expr::String(format!(
                "unreachable at {}:{}",
                ctx.file, ctx.line
            ))],
        ))])),
        "stringify" => {
            let e = one_expr_arg(args, "stringify")?;
            Ok(Some(vec![Statement::Expr(Expr::String(expr_to_src(&e)))]))
        }
        _ => Ok(None),
    }
}

fn one_expr_arg(args: &[MacroArg], name: &str) -> Result<Expr, String> {
    if args.len() != 1 {
        return Err(format!("`{}!` expects exactly one argument", name));
    }
    arg_as_expr(&args[0])
}

fn arg_as_expr(arg: &MacroArg) -> Result<Expr, String> {
    match arg {
        MacroArg::Expr(e) => Ok(e.clone()),
        MacroArg::Named { name, value } => Ok(Expr::BinOp(
            // named alone not valid for assert - but allow as binding form elsewhere
            Box::new(Expr::Ident(name.clone())),
            BinOp::Eq,
            Box::new(value.clone()),
        )),
    }
}

fn match_pattern(pattern: &MacroPattern, args: &MacroArgs) -> Result<HashMap<String, Capture>, String> {
    let args = match args {
        MacroArgs::Paren(a) => a,
        MacroArgs::Brace(stmts) => {
            // brace form: bind as single $body:block if pattern is one block bind
            if pattern.pieces.len() == 1 {
                if let PatPiece::Bind {
                    name,
                    kind: FragKind::Block | FragKind::Stmt | FragKind::Tt,
                } = &pattern.pieces[0]
                {
                    let mut map = HashMap::new();
                    map.insert(name.clone(), Capture::Expr(Expr::TryExpr(stmts.clone()))); // hack store
                    // Use a special approach: store stmts via Capture - add Block variant usage
                    let _ = map;
                    let mut map = HashMap::new();
                    map.insert(
                        name.clone(),
                        Capture::Rep(
                            stmts
                                .iter()
                                .map(|s| {
                                    let mut m = HashMap::new();
                                    m.insert(
                                        "__stmt__".to_string(),
                                        Capture::Expr(match s {
                                            Statement::Expr(e) => e.clone(),
                                            _ => Expr::Int(0),
                                        }),
                                    );
                                    m
                                })
                                .collect(),
                        ),
                    );
                    // Simpler: only support paren args for user macros in v1
                    return Err(
                        "brace-form macro arguments are only supported for limited builtins"
                            .to_string(),
                    );
                }
            }
            return Err("macro pattern does not match brace arguments".to_string());
        }
    };

    let mut captures = HashMap::new();
    let mut ai = 0usize;
    let mut pi = 0usize;
    while pi < pattern.pieces.len() {
        match &pattern.pieces[pi] {
            PatPiece::Bind { name, kind } => {
                if ai >= args.len() {
                    return Err(format!("missing argument for `${}`", name));
                }
                let cap = match_bind(kind, &args[ai], name)?;
                captures.insert(name.clone(), cap);
                ai += 1;
                pi += 1;
            }
            PatPiece::EqBind {
                ident_name,
                expr_name,
                expr_kind,
            } => {
                if ai >= args.len() {
                    return Err("missing named argument (ident = expr)".to_string());
                }
                match &args[ai] {
                    MacroArg::Named { name, value } => {
                        captures.insert(ident_name.clone(), Capture::Ident(name.clone()));
                        let cap = match_bind(expr_kind, &MacroArg::Expr(value.clone()), expr_name)?;
                        captures.insert(expr_name.clone(), cap);
                    }
                    MacroArg::Expr(Expr::BinOp(l, BinOp::Eq, r)) => {
                        // fallback if named parsed as expr
                        if let Expr::Ident(n) = l.as_ref() {
                            captures.insert(ident_name.clone(), Capture::Ident(n.clone()));
                            captures.insert(expr_name.clone(), Capture::Expr(*r.clone()));
                        } else {
                            return Err("expected `name = expr` argument".to_string());
                        }
                    }
                    _ => return Err("expected `name = expr` argument".to_string()),
                }
                ai += 1;
                pi += 1;
            }
            PatPiece::Rep {
                pieces,
                leading_sep: _,
                inter_sep: _,
                min,
            } => {
                // 重复：消费剩余参数，每次匹配 pieces
                let mut reps = Vec::new();
                while ai < args.len() {
                    let mut local = HashMap::new();
                    let start_ai = ai;
                    let mut ok = true;
                    for piece in pieces {
                        match piece {
                            PatPiece::Bind { name, kind } => {
                                if ai >= args.len() {
                                    ok = false;
                                    break;
                                }
                                match match_bind(kind, &args[ai], name) {
                                    Ok(c) => {
                                        local.insert(name.clone(), c);
                                        ai += 1;
                                    }
                                    Err(_) => {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        ai = start_ai;
                        break;
                    }
                    reps.push(local);
                }
                if reps.len() < *min {
                    return Err(format!(
                        "repetition matched {} times, need at least {}",
                        reps.len(),
                        min
                    ));
                }
                // 把重复捕获并入：对每个 bind 名，存 Rep
                if let Some(first) = reps.first() {
                    for key in first.keys() {
                        let series: Vec<HashMap<String, Capture>> = reps
                            .iter()
                            .map(|m| {
                                let mut one = HashMap::new();
                                if let Some(c) = m.get(key) {
                                    one.insert(key.clone(), c.clone());
                                }
                                one
                            })
                            .collect();
                        captures.insert(key.clone(), Capture::Rep(series));
                    }
                } else {
                    // empty rep: still insert empty rep for bind names in pieces
                    for piece in pieces {
                        if let PatPiece::Bind { name, .. } = piece {
                            captures.insert(name.clone(), Capture::Rep(vec![]));
                        }
                    }
                }
                // Also store full reps under a synthetic key for template $(...)* expansion
                captures.insert("__rep__".to_string(), Capture::Rep(reps));
                pi += 1;
            }
        }
    }
    if ai != args.len() {
        return Err(format!(
            "too many arguments: {} unused",
            args.len() - ai
        ));
    }
    Ok(captures)
}

fn match_bind(kind: &FragKind, arg: &MacroArg, name: &str) -> Result<Capture, String> {
    match arg {
        MacroArg::Named { name: n, .. } if *kind == FragKind::Ident => {
            return Ok(Capture::Ident(n.clone()));
        }
        MacroArg::Named { .. } => {
            return Err(format!(
                "`${}:{:?}` does not accept `name = expr` argument here",
                name, kind
            ));
        }
        MacroArg::Expr(expr) => match kind {
            FragKind::Expr | FragKind::Tt | FragKind::Stmt => Ok(Capture::Expr(expr.clone())),
            FragKind::Ident => match expr {
                Expr::Ident(s) => Ok(Capture::Ident(s.clone())),
                _ => Err(format!("`${}:ident` expects identifier", name)),
            },
            FragKind::Lit => match expr {
                Expr::Int(_)
                | Expr::Float(_)
                | Expr::Bool(_)
                | Expr::String(_)
                | Expr::None
                | Expr::BigInt(_)
                | Expr::Decimal(_) => Ok(Capture::Expr(expr.clone())),
                _ => Err(format!("`${}:lit` expects literal", name)),
            },
            FragKind::Block
            | FragKind::Type
            | FragKind::Path
            | FragKind::Item => Ok(Capture::Expr(expr.clone())),
        },
    }
}

fn substitute_stmts(
    body: &[Statement],
    captures: &HashMap<String, Capture>,
    ctx: &ExpandContext,
    expand_id: u64,
) -> Result<Vec<Statement>, String> {
    let mut out = Vec::new();
    for stmt in body {
        match stmt {
            Statement::MacroRep {
                body: rep_body,
                min,
            } => {
                out.extend(expand_rep_block(
                    rep_body, *min, captures, ctx, expand_id,
                )?);
            }
            other => {
                if let Some(reps) = stmt_is_rep_template(other, captures) {
                    for rep_map in reps {
                        let mut merged = captures.clone();
                        for (k, v) in rep_map {
                            merged.insert(k, v);
                        }
                        // 单次迭代内把 Rep 键替换成标量，避免嵌套 MacroRep 误用
                        strip_rep_keys(&mut merged);
                        out.push(substitute_stmt(other, &merged, ctx, expand_id)?);
                    }
                } else {
                    out.push(substitute_stmt(other, captures, ctx, expand_id)?);
                }
            }
        }
    }
    Ok(out)
}

fn strip_rep_keys(map: &mut HashMap<String, Capture>) {
    let keys: Vec<_> = map
        .iter()
        .filter(|(_, v)| matches!(v, Capture::Rep(_)))
        .map(|(k, _)| k.clone())
        .collect();
    for k in keys {
        map.remove(&k);
    }
    map.remove("__rep__");
}

/// 展开 `$( ... )*` 块
fn expand_rep_block(
    body: &[Statement],
    min: usize,
    captures: &HashMap<String, Capture>,
    ctx: &ExpandContext,
    expand_id: u64,
) -> Result<Vec<Statement>, String> {
    let iterations = rep_iterations(captures, body)?;
    if iterations.len() < min {
        return Err(format!(
            "macro repetition `$(...)+` needs at least {} iteration(s), got {}",
            min,
            iterations.len()
        ));
    }
    let mut out = Vec::new();
    for (i, iter_caps) in iterations.iter().enumerate() {
        let mut merged = captures.clone();
        for (k, v) in iter_caps {
            merged.insert(k.clone(), v.clone());
        }
        strip_rep_keys(&mut merged);
        merged.insert("i".to_string(), Capture::Expr(Expr::Int(i as i64)));
        merged.insert("index".to_string(), Capture::Expr(Expr::Int(i as i64)));
        out.extend(substitute_stmts(body, &merged, ctx, expand_id)?);
    }
    Ok(out)
}

/// 从捕获中构造重复迭代表
fn rep_iterations(
    captures: &HashMap<String, Capture>,
    body: &[Statement],
) -> Result<Vec<HashMap<String, Capture>>, String> {
    // 1) 优先：body 中用到的 Capture::Rep 键
    let mut splice_names = Vec::new();
    for s in body {
        splice_names.extend(collect_splice_names_stmt(s));
        if let Statement::MacroRep { body: inner, .. } = s {
            for t in inner {
                splice_names.extend(collect_splice_names_stmt(t));
            }
        }
    }
    splice_names.sort();
    splice_names.dedup();

    let mut rep_keys: Vec<String> = splice_names
        .iter()
        .filter(|n| matches!(captures.get(*n), Some(Capture::Rep(_))))
        .cloned()
        .collect();

    // 2) body 未引用任何 rep 键时，使用所有 Capture::Rep（含 __rep__ 以外）
    if rep_keys.is_empty() {
        rep_keys = captures
            .iter()
            .filter(|(k, v)| *k != "__rep__" && matches!(v, Capture::Rep(_)))
            .map(|(k, _)| k.clone())
            .collect();
    }

    if !rep_keys.is_empty() {
        let mut len = None;
        for k in &rep_keys {
            if let Some(Capture::Rep(r)) = captures.get(k) {
                match len {
                    None => len = Some(r.len()),
                    Some(n) if n != r.len() => {
                        return Err("macro repetition captures have inconsistent lengths".into());
                    }
                    _ => {}
                }
            }
        }
        let n = len.unwrap_or(0);
        let mut result = Vec::new();
        for i in 0..n {
            let mut map = HashMap::new();
            for k in &rep_keys {
                if let Some(Capture::Rep(r)) = captures.get(k) {
                    if let Some(inner) = r.get(i) {
                        if let Some(c) = inner.get(k) {
                            map.insert(k.clone(), c.clone());
                        }
                        // 也合并该轮全部键（field+ty 等同轮）
                        for (ik, iv) in inner {
                            map.insert(ik.clone(), iv.clone());
                        }
                    }
                }
            }
            if let Some(Capture::Rep(full)) = captures.get("__rep__") {
                if let Some(inner) = full.get(i) {
                    for (ik, iv) in inner {
                        map.entry(ik.clone()).or_insert_with(|| iv.clone());
                    }
                }
            }
            result.push(map);
        }
        return Ok(result);
    }

    // 3) 无 Rep：用单个 int 字面捕获作为重复次数（$n:lit）
    let mut count: Option<i64> = None;
    for (k, v) in captures {
        if k == "i" || k == "index" {
            continue;
        }
        if let Capture::Expr(Expr::Int(n)) = v {
            if *n < 0 {
                return Err(format!("repetition count `${}` cannot be negative", k));
            }
            if count.is_some() {
                return Err(
                    "ambiguous repetition count: multiple integer captures; use `$(...)*` with fragment reps or a single `$n:lit`"
                        .to_string(),
                );
            }
            count = Some(*n);
        }
    }
    if let Some(n) = count {
        let mut result = Vec::new();
        for i in 0..n {
            let mut map = HashMap::new();
            map.insert("i".to_string(), Capture::Expr(Expr::Int(i)));
            map.insert("index".to_string(), Capture::Expr(Expr::Int(i)));
            result.push(map);
        }
        return Ok(result);
    }

    // 4) 零次重复（仅 *）
    Ok(vec![])
}

/// If statement uses only keys that are Capture::Rep with same length, expand per iteration.
fn stmt_is_rep_template(
    stmt: &Statement,
    captures: &HashMap<String, Capture>,
) -> Option<Vec<HashMap<String, Capture>>> {
    let splices = collect_splice_names_stmt(stmt);
    if splices.is_empty() {
        return None;
    }
    let mut rep_len = None;
    let mut rep_keys = Vec::new();
    for s in &splices {
        match captures.get(s) {
            Some(Capture::Rep(r)) => {
                rep_keys.push(s.clone());
                let len = r.len();
                if let Some(n) = rep_len {
                    if n != len {
                        return None;
                    }
                } else {
                    rep_len = Some(len);
                }
            }
            Some(_) => {
                // mixed non-rep splice: not a pure rep template
            }
            None => return None,
        }
    }
    if rep_keys.is_empty() {
        return None;
    }
    // only treat as rep if ALL splices are rep keys
    if !splices.iter().all(|s| rep_keys.contains(s)) {
        return None;
    }
    let n = rep_len.unwrap_or(0);
    let mut result = Vec::new();
    for i in 0..n {
        let mut map = HashMap::new();
        for k in &rep_keys {
            if let Some(Capture::Rep(r)) = captures.get(k) {
                if let Some(inner) = r.get(i) {
                    if let Some(c) = inner.get(k) {
                        map.insert(k.clone(), c.clone());
                    } else if let Some((_, c)) = inner.iter().next() {
                        map.insert(k.clone(), c.clone());
                    }
                }
            }
        }
        result.push(map);
    }
    Some(result)
}

fn collect_splice_names_stmt(stmt: &Statement) -> Vec<String> {
    let mut v = Vec::new();
    match stmt {
        Statement::Expr(e) | Statement::Throw(e) => collect_splice_names_expr(e, &mut v),
        Statement::VarDecl(d) => {
            if d.name_is_splice {
                v.push(d.name.clone());
            }
            if let Some(ref e) = d.value {
                collect_splice_names_expr(e, &mut v);
            }
        }
        Statement::Assign(a) => {
            collect_splice_names_expr(&a.target, &mut v);
            collect_splice_names_expr(&a.value, &mut v);
        }
        Statement::If(s) => {
            collect_splice_names_expr(&s.condition, &mut v);
            for st in &s.then_body {
                v.extend(collect_splice_names_stmt(st));
            }
        }
        Statement::Return(Some(e)) => collect_splice_names_expr(e, &mut v),
        Statement::FuncDef(f) => {
            if f.name.starts_with('$') {
                v.push(f.name.trim_start_matches('$').to_string());
            }
            // 名称若是 splice 形式：解析时 name 无 $，用 name_is 不存在；约定 $name 经 splice 绑定
            // 方法名用 Ident splice：我们用 special - if name looks like capture key in body templates
            // actually parse: `fn $field()` is invalid; use MacroRep with `fn get_x` after sub
            // Support: name stored as capture key when from template `fn $field` via custom parse
            // For now scan body and params
            for p in &f.params {
                if p.name.starts_with('$') {
                    v.push(p.name.trim_start_matches('$').to_string());
                }
            }
            for st in &f.body {
                v.extend(collect_splice_names_stmt(st));
            }
        }
        Statement::MacroRep { body, .. } => {
            for st in body {
                v.extend(collect_splice_names_stmt(st));
            }
        }
        _ => {}
    }
    v.sort();
    v.dedup();
    v
}

fn collect_splice_names_expr(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Splice { name, .. } => out.push(name.clone()),
        Expr::BinOp(l, _, r) => {
            collect_splice_names_expr(l, out);
            collect_splice_names_expr(r, out);
        }
        Expr::UnaryOp(_, e)
        | Expr::Await(e)
        | Expr::Propagate(e)
        | Expr::Raise(e)
        | Expr::Member(e, _)
        | Expr::NamedArg(_, e)
        | Expr::SpreadArg(e)
        | Expr::KwSpreadArg(e) => collect_splice_names_expr(e, out),
        Expr::Call(c, args) => {
            collect_splice_names_expr(c, out);
            for a in args {
                collect_splice_names_expr(a, out);
            }
        }
        Expr::Index(a, b) => {
            collect_splice_names_expr(a, out);
            collect_splice_names_expr(b, out);
        }
        Expr::List(xs) | Expr::Tuple(xs) | Expr::SpawnAll(xs) => {
            for x in xs {
                collect_splice_names_expr(x, out);
            }
        }
        Expr::Dict(es) => {
            for (k, v) in es {
                collect_splice_names_expr(k, out);
                collect_splice_names_expr(v, out);
            }
        }
        _ => {}
    }
}

fn substitute_stmt(
    stmt: &Statement,
    captures: &HashMap<String, Capture>,
    ctx: &ExpandContext,
    expand_id: u64,
) -> Result<Statement, String> {
    Ok(match stmt {
        Statement::Expr(e) => Statement::Expr(substitute_expr(e, captures, ctx)?),
        Statement::Throw(e) => Statement::Throw(substitute_expr(e, captures, ctx)?),
        Statement::Return(Some(e)) => Statement::Return(Some(substitute_expr(e, captures, ctx)?)),
        Statement::Return(None) => Statement::Return(None),
        Statement::VarDecl(d) => {
            let mut name = d.name.clone();
            let mut name_is_splice = d.name_is_splice;
            if d.name_is_splice {
                match captures.get(&d.name) {
                    Some(Capture::Ident(n)) => {
                        name = n.clone();
                        name_is_splice = false;
                    }
                    Some(Capture::Expr(Expr::Ident(n))) => {
                        name = n.clone();
                        name_is_splice = false;
                    }
                    _ => {
                        return Err(format!(
                            "macro splice `${}` in let binding is not an ident capture",
                            d.name
                        ));
                    }
                }
            }
            Statement::VarDecl(VarDecl {
                name,
                mutable: d.mutable,
                ty: d.ty.clone(),
                value: match &d.value {
                    Some(e) => Some(substitute_expr(e, captures, ctx)?),
                    None => None,
                },
                name_is_splice,
            })
        }
        Statement::Assign(a) => Statement::Assign(bolide_parser::Assign {
            target: substitute_expr(&a.target, captures, ctx)?,
            value: substitute_expr(&a.value, captures, ctx)?,
        }),
        Statement::If(s) => Statement::If(bolide_parser::IfStmt {
            condition: substitute_expr(&s.condition, captures, ctx)?,
            then_body: substitute_stmts(&s.then_body, captures, ctx, expand_id)?,
            elif_branches: s
                .elif_branches
                .iter()
                .map(|(c, b)| {
                    Ok((
                        substitute_expr(c, captures, ctx)?,
                        substitute_stmts(b, captures, ctx, expand_id)?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?,
            else_body: match &s.else_body {
                Some(b) => Some(substitute_stmts(b, captures, ctx, expand_id)?),
                None => None,
            },
        }),
        Statement::While(s) => Statement::While(bolide_parser::WhileStmt {
            condition: substitute_expr(&s.condition, captures, ctx)?,
            body: substitute_stmts(&s.body, captures, ctx, expand_id)?,
        }),
        Statement::For(s) => Statement::For(bolide_parser::ForStmt {
            vars: s.vars.clone(),
            iter: substitute_expr(&s.iter, captures, ctx)?,
            body: substitute_stmts(&s.body, captures, ctx, expand_id)?,
        }),
        Statement::FuncDef(f) => {
            let mut name = f.name.clone();
            // 支持模板 `fn $field`：name 为 capture 键（无 $ 前缀时若 captures 有同名 Ident 也替换）
            if let Some(Capture::Ident(n)) = captures.get(&name) {
                name = n.clone();
            } else if let Some(rest) = name.strip_prefix('$') {
                if let Some(Capture::Ident(n)) = captures.get(rest) {
                    name = n.clone();
                }
            }
            // 前缀拼接：get_$field → captures field
            if name.contains('$') {
                name = expand_name_template(&name, captures)?;
            }
            let mut params = Vec::new();
            for p in &f.params {
                let mut pname = p.name.clone();
                if let Some(Capture::Ident(n)) = captures.get(&pname) {
                    pname = n.clone();
                }
                params.push(bolide_parser::Param {
                    name: pname,
                    ty: p.ty.clone(),
                    mode: p.mode,
                    default_value: match &p.default_value {
                        Some(e) => Some(substitute_expr(e, captures, ctx)?),
                        None => None,
                    },
                    is_variadic: p.is_variadic,
                    is_kw_variadic: p.is_kw_variadic,
                });
            }
            Statement::FuncDef(FuncDef {
                name,
                is_async: f.is_async,
                is_export: f.is_export,
                is_inline: f.is_inline,
                type_params: f.type_params.clone(),
                trait_bounds: f.trait_bounds.clone(),
                params,
                throws: f.throws.clone(),
                return_type: f.return_type.clone(),
                lifetime_deps: f.lifetime_deps.clone(),
                body: substitute_stmts(&f.body, captures, ctx, expand_id)?,
                def_span_start: f.def_span_start,
                attrs: vec![],
            })
        }
        Statement::MacroRep { body, min } => {
            // 嵌套 rep 在 substitute_stmts 已处理；若落到此处则再展一次
            let expanded = expand_rep_block(body, *min, captures, ctx, expand_id)?;
            // 无法返回多句：包成无副作用的占位（调用方应走 substitute_stmts）
            if expanded.len() == 1 {
                expanded.into_iter().next().unwrap()
            } else {
                Statement::Expr(Expr::Int(0))
            }
        }
        other => other.clone(),
    })
}

/// `get_$field` / `set_$field` 名称模板
fn expand_name_template(name: &str, captures: &HashMap<String, Capture>) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            let mut key = String::new();
            while let Some(ch) = chars.peek() {
                if ch.is_ascii_alphanumeric() || *ch == '_' {
                    key.push(chars.next().unwrap());
                } else {
                    break;
                }
            }
            match captures.get(&key) {
                Some(Capture::Ident(n)) => out.push_str(n),
                Some(Capture::Expr(Expr::Ident(n))) => out.push_str(n),
                Some(Capture::Expr(Expr::String(s))) => out.push_str(s),
                Some(Capture::Expr(Expr::Int(n))) => out.push_str(&n.to_string()),
                _ => {
                    return Err(format!(
                        "name template `${}` not found in macro captures",
                        key
                    ));
                }
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

fn substitute_expr(
    expr: &Expr,
    captures: &HashMap<String, Capture>,
    ctx: &ExpandContext,
) -> Result<Expr, String> {
    match expr {
        Expr::Splice { name, meta } => match captures.get(name) {
            Some(Capture::Expr(e)) => match meta {
                None => Ok(e.clone()),
                Some(SpliceMeta::Src) | Some(SpliceMeta::Stringify) => {
                    Ok(Expr::String(expr_to_src(e)))
                }
                Some(SpliceMeta::Line) => Ok(Expr::Int(ctx.line as i64)),
                Some(SpliceMeta::File) => Ok(Expr::String(ctx.file.clone())),
            },
            Some(Capture::Ident(n)) => match meta {
                None => Ok(Expr::Ident(n.clone())),
                Some(SpliceMeta::Src) | Some(SpliceMeta::Stringify) => {
                    Ok(Expr::String(n.clone()))
                }
                Some(SpliceMeta::Line) => Ok(Expr::Int(ctx.line as i64)),
                Some(SpliceMeta::File) => Ok(Expr::String(ctx.file.clone())),
            },
            Some(Capture::Rep(_)) => Err(format!(
                "splice `${}` is a repetition; use it only in a repeated template statement",
                name
            )),
            None => Err(format!("unknown macro capture `${}`", name)),
        },
        Expr::BinOp(l, op, r) => Ok(Expr::BinOp(
            Box::new(substitute_expr(l, captures, ctx)?),
            *op,
            Box::new(substitute_expr(r, captures, ctx)?),
        )),
        Expr::UnaryOp(op, e) => Ok(Expr::UnaryOp(
            *op,
            Box::new(substitute_expr(e, captures, ctx)?),
        )),
        Expr::Call(c, args) => {
            let mut na = Vec::new();
            for a in args {
                na.push(substitute_expr(a, captures, ctx)?);
            }
            Ok(Expr::Call(
                Box::new(substitute_expr(c, captures, ctx)?),
                na,
            ))
        }
        Expr::Member(b, m) => {
            let mut member = m.clone();
            if let Some(Capture::Ident(n)) = captures.get(m) {
                member = n.clone();
            } else if let Some(rest) = m.strip_prefix('$') {
                if let Some(Capture::Ident(n)) = captures.get(rest) {
                    member = n.clone();
                }
            } else if m.contains('$') {
                member = expand_name_template(m, captures)?;
            }
            Ok(Expr::Member(
                Box::new(substitute_expr(b, captures, ctx)?),
                member,
            ))
        }
        Expr::Index(b, i) => Ok(Expr::Index(
            Box::new(substitute_expr(b, captures, ctx)?),
            Box::new(substitute_expr(i, captures, ctx)?),
        )),
        Expr::List(xs) => Ok(Expr::List(
            xs.iter()
                .map(|x| substitute_expr(x, captures, ctx))
                .collect::<Result<_, _>>()?,
        )),
        Expr::Tuple(xs) => Ok(Expr::Tuple(
            xs.iter()
                .map(|x| substitute_expr(x, captures, ctx))
                .collect::<Result<_, _>>()?,
        )),
        Expr::Dict(es) => {
            let mut v = Vec::new();
            for (k, val) in es {
                v.push((
                    substitute_expr(k, captures, ctx)?,
                    substitute_expr(val, captures, ctx)?,
                ));
            }
            Ok(Expr::Dict(v))
        }
        Expr::ValueConstruct(n, fields) => {
            let mut v = Vec::new();
            for (f, e) in fields {
                v.push((f.clone(), substitute_expr(e, captures, ctx)?));
            }
            Ok(Expr::ValueConstruct(n.clone(), v))
        }
        Expr::Closure {
            params,
            return_type,
            body,
        } => Ok(Expr::Closure {
            params: params.clone(),
            return_type: return_type.clone(),
            body: substitute_stmts(body, captures, ctx, 0)?,
        }),
        Expr::TryExpr(body) => Ok(Expr::TryExpr(substitute_stmts(body, captures, ctx, 0)?)),
        Expr::Comptime(body) => Ok(Expr::Comptime(substitute_stmts(body, captures, ctx, 0)?)),
        Expr::Await(e) => Ok(Expr::Await(Box::new(substitute_expr(e, captures, ctx)?))),
        Expr::Propagate(e) => Ok(Expr::Propagate(Box::new(substitute_expr(e, captures, ctx)?))),
        Expr::Raise(e) => Ok(Expr::Raise(Box::new(substitute_expr(e, captures, ctx)?))),
        Expr::NamedArg(n, e) => Ok(Expr::NamedArg(
            n.clone(),
            Box::new(substitute_expr(e, captures, ctx)?),
        )),
        Expr::Slice(b, s, e, st) => Ok(Expr::Slice(
            Box::new(substitute_expr(b, captures, ctx)?),
            match s {
                Some(x) => Some(Box::new(substitute_expr(x, captures, ctx)?)),
                None => None,
            },
            match e {
                Some(x) => Some(Box::new(substitute_expr(x, captures, ctx)?)),
                None => None,
            },
            match st {
                Some(x) => Some(Box::new(substitute_expr(x, captures, ctx)?)),
                None => None,
            },
        )),
        other => Ok(other.clone()),
    }
}

/// 卫生：将宏内 `let/var` 引入的名字加上 `__m{id}_` 前缀（ident 捕获的名字除外）。
fn apply_hygiene(
    stmts: Vec<Statement>,
    expand_id: u64,
    captures: &HashMap<String, Capture>,
) -> Vec<Statement> {
    let mut protected = std::collections::HashSet::new();
    for c in captures.values() {
        if let Capture::Ident(n) = c {
            protected.insert(n.clone());
        }
    }
    let mut renames = HashMap::new();
    hygiene_collect(&stmts, expand_id, &protected, &mut renames);
    if renames.is_empty() {
        return stmts;
    }
    hygiene_apply_stmts(stmts, &renames)
}

fn hygiene_collect(
    stmts: &[Statement],
    expand_id: u64,
    protected: &std::collections::HashSet<String>,
    renames: &mut HashMap<String, String>,
) {
    for s in stmts {
        match s {
            Statement::VarDecl(d) if !d.name_is_splice && !protected.contains(&d.name) => {
                if !d.name.starts_with("__m") && !d.name.starts_with("__dbg_") {
                    renames
                        .entry(d.name.clone())
                        .or_insert_with(|| format!("__m{}_{}", expand_id, d.name));
                }
            }
            Statement::If(i) => {
                hygiene_collect(&i.then_body, expand_id, protected, renames);
                for (_, b) in &i.elif_branches {
                    hygiene_collect(b, expand_id, protected, renames);
                }
                if let Some(b) = &i.else_body {
                    hygiene_collect(b, expand_id, protected, renames);
                }
            }
            Statement::While(w) => hygiene_collect(&w.body, expand_id, protected, renames),
            Statement::For(f) => hygiene_collect(&f.body, expand_id, protected, renames),
            _ => {}
        }
    }
}

fn hygiene_apply_stmts(stmts: Vec<Statement>, renames: &HashMap<String, String>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|s| hygiene_apply_stmt(s, renames))
        .collect()
}

fn hygiene_apply_stmt(stmt: Statement, renames: &HashMap<String, String>) -> Statement {
    match stmt {
        Statement::VarDecl(mut d) => {
            if let Some(n) = renames.get(&d.name) {
                d.name = n.clone();
            }
            if let Some(v) = d.value {
                d.value = Some(hygiene_apply_expr(v, renames));
            }
            Statement::VarDecl(d)
        }
        Statement::Expr(e) => Statement::Expr(hygiene_apply_expr(e, renames)),
        Statement::Assign(mut a) => {
            a.target = hygiene_apply_expr(a.target, renames);
            a.value = hygiene_apply_expr(a.value, renames);
            Statement::Assign(a)
        }
        Statement::If(mut i) => {
            i.condition = hygiene_apply_expr(i.condition, renames);
            i.then_body = hygiene_apply_stmts(i.then_body, renames);
            i.elif_branches = i
                .elif_branches
                .into_iter()
                .map(|(c, b)| (hygiene_apply_expr(c, renames), hygiene_apply_stmts(b, renames)))
                .collect();
            i.else_body = i.else_body.map(|b| hygiene_apply_stmts(b, renames));
            Statement::If(i)
        }
        Statement::Return(Some(e)) => Statement::Return(Some(hygiene_apply_expr(e, renames))),
        Statement::Throw(e) => Statement::Throw(hygiene_apply_expr(e, renames)),
        other => other,
    }
}

fn hygiene_apply_expr(expr: Expr, renames: &HashMap<String, String>) -> Expr {
    match expr {
        Expr::Ident(n) => Expr::Ident(renames.get(&n).cloned().unwrap_or(n)),
        Expr::BinOp(l, op, r) => Expr::BinOp(
            Box::new(hygiene_apply_expr(*l, renames)),
            op,
            Box::new(hygiene_apply_expr(*r, renames)),
        ),
        Expr::UnaryOp(op, e) => Expr::UnaryOp(op, Box::new(hygiene_apply_expr(*e, renames))),
        Expr::Call(c, args) => Expr::Call(
            Box::new(hygiene_apply_expr(*c, renames)),
            args.into_iter()
                .map(|a| hygiene_apply_expr(a, renames))
                .collect(),
        ),
        Expr::Member(b, m) => Expr::Member(Box::new(hygiene_apply_expr(*b, renames)), m),
        other => other,
    }
}

fn stmts_to_expr(stmts: Vec<Statement>, inv: &MacroInvoke) -> Result<Expr, String> {
    if stmts.is_empty() {
        return Err(format!(
            "macro `{}!` expanded to empty body in expression position",
            inv.path.join(".")
        ));
    }
    // 单条表达式语句
    if stmts.len() == 1 {
        if let Statement::Expr(e) = &stmts[0] {
            return Ok(e.clone());
        }
    }
    // 多语句：若最后是表达式，用 IIFE；闭包标注返回 dynamic 避免 verifier 类型冲突
    let mut body = Vec::new();
    let last_idx = stmts.len() - 1;
    for (i, s) in stmts.into_iter().enumerate() {
        if i == last_idx {
            match s {
                Statement::Expr(e) => body.push(Statement::Return(Some(e))),
                Statement::Return(_) => body.push(s),
                other => {
                    body.push(other);
                    body.push(Statement::Return(Some(Expr::Int(0))));
                }
            }
        } else {
            body.push(s);
        }
    }
    Ok(Expr::Call(
        Box::new(Expr::Closure {
            params: vec![],
            return_type: Some(Type::Int),
            body,
        }),
        vec![],
    ))
}

/// 处理函数属性：
/// 1. 编译期：`@test` / `@inline` / `@export` / `attr macro` prologue
/// 2. 运行时装饰器：其余 `@name` / `@name(...)` 按 Python 语义
///    `@a @b fn f` ⇒ `f = a(b(__raw_f))`，生成 `__raw_f` + 包装 `f`
fn apply_fn_attrs(
    mut f: FuncDef,
    env: &ExpandEnv,
    ctx: &ExpandContext,
) -> Result<Vec<FuncDef>, String> {
    let attrs = std::mem::take(&mut f.attrs);
    let mut prologues: Vec<Statement> = Vec::new();
    let mut runtime_decos: Vec<Attribute> = Vec::new();
    for attr in attrs {
        match attr.name.as_str() {
            "test" => {
                if f.name.is_empty() {
                    return Err("@test function must have a name".to_string());
                }
            }
            "inline" => {
                f.is_inline = true;
            }
            "export" => {
                f.is_export = true;
            }
            other => {
                if let Some(am) = resolve_attr_macro(other, env.attr_macros) {
                    let captures = match_attr_args(&am.pattern, &attr.args)?;
                    let id = EXPAND_ID.fetch_add(1, Ordering::Relaxed);
                    let body = substitute_stmts(&am.body, &captures, ctx, id)?;
                    let body = apply_hygiene(body, id, &captures);
                    prologues.extend(body);
                } else {
                    // 运行时装饰器（Python 风格）
                    runtime_decos.push(attr);
                }
            }
        }
    }
    if !prologues.is_empty() {
        prologues.append(&mut f.body);
        f.body = prologues;
    }

    if runtime_decos.is_empty() {
        return Ok(vec![f]);
    }

    // Python 风格：@a @b fn foo(...) { body }
    //   foo = a(b(__raw_foo))
    // 脱糖为：
    //   fn __raw_…_foo(params) { body }
    //   fn foo(params) { return a(b(__raw_…_foo))(params...); }
    // 装饰器：fn deco(f: func(...)->R) -> func(...)->R
    // 工厂：@deco(args) ⇒ deco(args)(raw)(...)
    let original_name = f.name.clone();
    let id = EXPAND_ID.fetch_add(1, Ordering::Relaxed);
    let raw_name = format!("__raw_{}_{}", id, original_name);
    f.name = raw_name.clone();
    let was_export = f.is_export;
    f.is_export = false;

    let wrapper = make_python_decorator_wrapper(
        &original_name,
        &raw_name,
        &f,
        &runtime_decos,
        was_export,
    )?;
    Ok(vec![f, wrapper])
}

/// Python：`@a @b fn name` ⇒ `name = a(b(raw))`，每次调用走包装函数。
/// 使用中间 `let` 绑定，避免 `a(b(raw))(args)` 链式临时闭包生命周期问题。
fn make_python_decorator_wrapper(
    name: &str,
    raw_name: &str,
    raw: &FuncDef,
    decos: &[Attribute],
    is_export: bool,
) -> Result<FuncDef, String> {
    let mut body: Vec<Statement> = Vec::new();
    let mut current = Expr::Ident(raw_name.to_string());
    // 从内到外
    for (i, deco) in decos.iter().rev().enumerate() {
        let deco_expr = if deco.args.is_empty() {
            Expr::Ident(deco.name.clone())
        } else {
            // @factory(args) → let __f = factory(args);
            let call_args: Vec<Expr> = deco.args.iter().map(attr_arg_to_expr).collect();
            let factory_call =
                Expr::Call(Box::new(Expr::Ident(deco.name.clone())), call_args);
            let ftmp = format!("__deco_f_{}_{}", i, name);
            body.push(Statement::VarDecl(VarDecl {
                name: ftmp.clone(),
                mutable: false,
                ty: None,
                value: Some(factory_call),
                name_is_splice: false,
            }));
            Expr::Ident(ftmp)
        };
        // let __w = deco_expr(current);
        let applied = Expr::Call(Box::new(deco_expr), vec![current]);
        let wtmp = format!("__deco_w_{}_{}", i, name);
        body.push(Statement::VarDecl(VarDecl {
            name: wtmp.clone(),
            mutable: false,
            ty: None,
            value: Some(applied),
            name_is_splice: false,
        }));
        current = Expr::Ident(wtmp);
    }
    let call_args: Vec<Expr> = raw
        .params
        .iter()
        .map(|p| Expr::Ident(p.name.clone()))
        .collect();
    let invoke = Expr::Call(Box::new(current), call_args);
    body.push(Statement::Return(Some(invoke)));
    Ok(FuncDef {
        name: name.to_string(),
        is_async: raw.is_async,
        is_export,
        is_inline: false,
        type_params: raw.type_params.clone(),
        trait_bounds: raw.trait_bounds.clone(),
        params: raw.params.clone(),
        throws: raw.throws.clone(),
        return_type: raw.return_type.clone(),
        lifetime_deps: raw.lifetime_deps.clone(),
        body,
        def_span_start: raw.def_span_start,
        attrs: vec![],
    })
}

fn attr_arg_to_expr(a: &AttrArg) -> Expr {
    match a {
        AttrArg::Ident(s) => Expr::Ident(s.clone()),
        AttrArg::Str(s) => Expr::String(s.clone()),
        AttrArg::Int(n) => Expr::Int(*n),
    }
}

/// `with m as x { body }` →
/// ```text
/// let __cm = m;
/// let x = __cm.enter();
/// try { body } finally { __cm.exit(); }
/// ```
/// 多个 item 自外向内嵌套（与 Python 一致）。
fn desugar_with(w: bolide_parser::WithStmt) -> Result<Vec<Statement>, String> {
    desugar_with_items(&w.items, w.body, 0)
}

fn desugar_with_items(
    items: &[bolide_parser::WithItem],
    body: Vec<Statement>,
    depth: usize,
) -> Result<Vec<Statement>, String> {
    if items.is_empty() {
        return Ok(body);
    }
    let item = &items[0];
    let rest = &items[1..];
    // 全局唯一，避免多个 with 在顶层冲突
    let uid = EXPAND_ID.fetch_add(1, Ordering::Relaxed);
    let cm = format!("__cm_{}_{}", uid, depth);
    let mut stmts = Vec::new();
    stmts.push(Statement::VarDecl(VarDecl {
        name: cm.clone(),
        mutable: false,
        ty: None,
        value: Some(item.expr.clone()),
        name_is_splice: false,
    }));
    // enter
    let enter_call = Expr::Call(
        Box::new(Expr::Member(
            Box::new(Expr::Ident(cm.clone())),
            "enter".to_string(),
        )),
        vec![],
    );
    if let Some(ref bind) = item.binding {
        stmts.push(Statement::VarDecl(VarDecl {
            name: bind.clone(),
            mutable: false,
            ty: None,
            value: Some(enter_call),
            name_is_splice: false,
        }));
    } else {
        stmts.push(Statement::Expr(enter_call));
    }
    let inner_body = desugar_with_items(rest, body, depth + 1)?;
    let exit_call = Expr::Call(
        Box::new(Expr::Member(
            Box::new(Expr::Ident(cm)),
            "exit".to_string(),
        )),
        vec![],
    );
    stmts.push(Statement::Try(bolide_parser::TryStmt {
        try_body: inner_body,
        catch_clauses: vec![],
        finally: Some(vec![Statement::Expr(exit_call)]),
    }));
    Ok(stmts)
}

fn resolve_attr_macro<'a>(
    name: &str,
    attr_macros: &'a HashMap<String, AttrMacroDef>,
) -> Option<&'a AttrMacroDef> {
    attr_macros
        .get(name)
        .or_else(|| {
            attr_macros
                .iter()
                .find(|(k, _)| k.ends_with(&format!("_{}", name)))
                .map(|(_, v)| v)
        })
}

/// 将 `@name(args)` 的参数按 attr macro 模式绑定
fn match_attr_args(
    pattern: &MacroPattern,
    args: &[AttrArg],
) -> Result<HashMap<String, Capture>, String> {
    let mut captures = HashMap::new();
    let mut ai = 0usize;
    for piece in &pattern.pieces {
        match piece {
            PatPiece::Bind { name, kind } => {
                // `$item:item` 表示被标注项本身，属性参数里不占位
                if *kind == FragKind::Item {
                    continue;
                }
                if ai >= args.len() {
                    return Err(format!("attribute missing argument for `${}`", name));
                }
                let cap = attr_arg_to_capture(&args[ai], kind, name)?;
                captures.insert(name.clone(), cap);
                ai += 1;
            }
            PatPiece::EqBind { .. } | PatPiece::Rep { .. } => {
                return Err("attr macro pattern does not support eq/rep fragments yet".to_string());
            }
        }
    }
    if ai < args.len() {
        return Err(format!(
            "too many attribute arguments ({} unused)",
            args.len() - ai
        ));
    }
    Ok(captures)
}

fn attr_arg_to_capture(arg: &AttrArg, kind: &FragKind, name: &str) -> Result<Capture, String> {
    match (kind, arg) {
        (FragKind::Ident, AttrArg::Ident(s)) => Ok(Capture::Ident(s.clone())),
        (FragKind::Lit | FragKind::Expr, AttrArg::Str(s)) => Ok(Capture::Expr(Expr::String(s.clone()))),
        (FragKind::Lit | FragKind::Expr, AttrArg::Int(n)) => Ok(Capture::Expr(Expr::Int(*n))),
        (FragKind::Lit | FragKind::Expr, AttrArg::Ident(s)) => {
            Ok(Capture::Expr(Expr::Ident(s.clone())))
        }
        (FragKind::Ident, AttrArg::Str(s)) => Ok(Capture::Ident(s.clone())),
        _ => Err(format!(
            "attribute argument does not match `${}:{:?}`",
            name, kind
        )),
    }
}

fn apply_class_attrs(
    mut c: ClassDef,
    env: &ExpandEnv,
    ctx: &ExpandContext,
) -> Result<Vec<Statement>, String> {
    let attrs = std::mem::take(&mut c.attrs);
    let mut extra_methods = Vec::new();
    let mut extra_stmts = Vec::new();
    for attr in &attrs {
        match attr.name.as_str() {
            "derive" => {
                for arg in &attr.args {
                    let trait_name = match arg {
                        AttrArg::Ident(s) => s.as_str(),
                        AttrArg::Str(s) => s.as_str(),
                        AttrArg::Int(_) => {
                            return Err("@derive expects trait names".to_string());
                        }
                    };
                    match trait_name {
                        "Debug" => extra_methods.push(gen_debug_method_class(&c)?),
                        "Eq" => extra_methods.push(gen_eq_method_class(&c)?),
                        "Clone" => extra_methods.push(gen_clone_method_class(&c)?),
                        "Default" => {
                            // 游离函数，不进 methods
                            extra_stmts.push(Statement::FuncDef(gen_default_method_class(&c)?));
                        }
                        other => {
                            return Err(format!(
                                "unsupported @derive({}) for class (supported: Debug, Eq, Clone, Default)",
                                other
                            ));
                        }
                    }
                }
            }
            // 内置：为每个字段生成 get_$field()
            "getters" => {
                for f in &c.fields {
                    extra_methods.push(gen_getter_method(&c.name, f)?);
                }
            }
            other => {
                if let Some(am) = resolve_attr_macro(other, env.attr_macros) {
                    let mut captures = match_attr_args(&am.pattern, &attr.args)?;
                    inject_class_field_captures(&c, &mut captures);
                    let id = EXPAND_ID.fetch_add(1, Ordering::Relaxed);
                    let body = substitute_stmts(&am.body, &captures, ctx, id)?;
                    let body = apply_hygiene(body, id, &captures);
                    for st in body {
                        match st {
                            Statement::FuncDef(f) => extra_methods.push(f),
                            other => extra_stmts.push(other),
                        }
                    }
                } else {
                    return Err(format!(
                        "unknown attribute `@{}` on class `{}` (use @derive/@getters or `attr macro {}`)",
                        other, c.name, other
                    ));
                }
            }
        }
    }
    c.methods.extend(extra_methods);
    let mut out = vec![Statement::ClassDef(c)];
    out.extend(extra_stmts);
    Ok(out)
}

/// 为类属性宏注入 `field` / `ty` 重复捕获（供 `$( ... $field ... )*`）
fn inject_class_field_captures(c: &ClassDef, captures: &mut HashMap<String, Capture>) {
    let mut field_series = Vec::new();
    let mut ty_series = Vec::new();
    let mut full = Vec::new();
    for f in &c.fields {
        let mut one = HashMap::new();
        one.insert("field".to_string(), Capture::Ident(f.name.clone()));
        one.insert(
            "ty".to_string(),
            Capture::Ident(type_to_simple_name(&f.ty)),
        );
        field_series.push({
            let mut m = HashMap::new();
            m.insert("field".to_string(), Capture::Ident(f.name.clone()));
            m
        });
        ty_series.push({
            let mut m = HashMap::new();
            m.insert("ty".to_string(), Capture::Ident(type_to_simple_name(&f.ty)));
            m
        });
        full.push(one);
    }
    captures.insert("field".to_string(), Capture::Rep(field_series));
    captures.insert("ty".to_string(), Capture::Rep(ty_series));
    captures.insert("__rep__".to_string(), Capture::Rep(full));
    captures.insert(
        "class".to_string(),
        Capture::Ident(c.name.clone()),
    );
}

fn type_to_simple_name(ty: &Type) -> String {
    match ty {
        Type::Int => "int".into(),
        Type::Float => "float".into(),
        Type::Bool => "bool".into(),
        Type::Str => "str".into(),
        Type::Custom(s) => s.clone(),
        Type::Dyn(s) => format!("dyn {}", s),
        Type::Generic(s) => s.clone(),
        Type::List(inner) => format!("list<{}>", type_to_simple_name(inner)),
        other => format!("{:?}", other),
    }
}

fn gen_getter_method(class_name: &str, f: &ClassField) -> Result<FuncDef, String> {
    let _ = class_name;
    Ok(FuncDef {
        name: format!("get_{}", f.name),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params: vec![],
        throws: vec![],
        return_type: Some(f.ty.clone()),
        lifetime_deps: None,
        body: vec![Statement::Return(Some(Expr::Member(
            Box::new(Expr::Ident("self".to_string())),
            f.name.clone(),
        )))],
        def_span_start: None,
        attrs: vec![],
    })
}

fn gen_clone_method_class(c: &ClassDef) -> Result<FuncDef, String> {
    // fn clone() -> Class { return Class(self.f1, self.f2, ...); }
    let args: Vec<Expr> = c
        .fields
        .iter()
        .map(|f| {
            Expr::Member(
                Box::new(Expr::Ident("self".to_string())),
                f.name.clone(),
            )
        })
        .collect();
    Ok(FuncDef {
        name: "clone".to_string(),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params: vec![],
        throws: vec![],
        return_type: Some(Type::Custom(c.name.clone())),
        lifetime_deps: None,
        body: vec![Statement::Return(Some(Expr::Call(
            Box::new(Expr::Ident(c.name.clone())),
            args,
        )))],
        def_span_start: None,
        attrs: vec![],
    })
}

fn gen_default_method_class(c: &ClassDef) -> Result<FuncDef, String> {
    // 游离函数 ClassName_default()，因类型名上的 `.default()` 会与构造函数 FuncSig 冲突
    let args: Vec<Expr> = c.fields.iter().map(|f| zero_expr_for_type(&f.ty)).collect();
    Ok(FuncDef {
        name: format!("{}_default", c.name),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params: vec![],
        throws: vec![],
        return_type: Some(Type::Custom(c.name.clone())),
        lifetime_deps: None,
        body: vec![Statement::Return(Some(Expr::Call(
            Box::new(Expr::Ident(c.name.clone())),
            args,
        )))],
        def_span_start: None,
        attrs: vec![],
    })
}

fn zero_expr_for_type(ty: &Type) -> Expr {
    match ty {
        Type::Int | Type::BigInt => Expr::Int(0),
        Type::Float | Type::Decimal => Expr::Float(0.0),
        Type::Bool => Expr::Bool(false),
        Type::Str => Expr::String(String::new()),
        Type::List(_) => Expr::List(vec![]),
        _ => Expr::Int(0),
    }
}

fn apply_value_attrs(
    mut v: ValueDef,
    _env: &ExpandEnv,
    _ctx: &ExpandContext,
) -> Result<Vec<Statement>, String> {
    let attrs = std::mem::take(&mut v.attrs);
    let mut extra = Vec::new();
    for attr in &attrs {
        match attr.name.as_str() {
            "derive" => {
                for arg in &attr.args {
                    let trait_name = match arg {
                        AttrArg::Ident(s) => s.as_str(),
                        AttrArg::Str(s) => s.as_str(),
                        AttrArg::Int(_) => {
                            return Err("@derive expects trait names".to_string());
                        }
                    };
                    match trait_name {
                        "Debug" => extra.push(Statement::FuncDef(gen_debug_fn_value(&v)?)),
                        "Eq" => extra.push(Statement::FuncDef(gen_eq_fn_value(&v)?)),
                        other => {
                            return Err(format!(
                                "unsupported @derive({}) for value (supported: Debug, Eq)",
                                other
                            ));
                        }
                    }
                }
            }
            other => {
                return Err(format!(
                    "unknown attribute `@{}` on value `{}`",
                    other, v.name
                ));
            }
        }
    }
    let mut out = vec![Statement::ValueDef(v)];
    out.extend(extra);
    Ok(out)
}

fn str_call(expr: Expr) -> Expr {
    Expr::Call(Box::new(Expr::Ident("str".to_string())), vec![expr])
}

/// 编译期折叠：字面量、算术/逻辑、以及 `comptime fn` 调用。
fn eval_comptime_block(
    body: &[Statement],
    fns: &HashMap<String, ComptimeFn>,
) -> Result<Expr, String> {
    let mut bindings: HashMap<String, Expr> = HashMap::new();
    let mut last: Option<Expr> = None;
    for stmt in body {
        match stmt {
            Statement::VarDecl(d) => {
                let v = d
                    .value
                    .as_ref()
                    .ok_or("comptime let requires initializer")?;
                let v = eval_comptime_expr(v, &bindings, fns)?;
                bindings.insert(d.name.clone(), v);
                last = None;
            }
            Statement::Expr(e) => {
                last = Some(eval_comptime_expr(e, &bindings, fns)?);
            }
            Statement::Return(Some(e)) => {
                return eval_comptime_expr(e, &bindings, fns);
            }
            Statement::Return(None) => {
                return Ok(Expr::Int(0));
            }
            Statement::If(i) => {
                let cond = eval_comptime_expr(&i.condition, &bindings, fns)?;
                let take = match cond {
                    Expr::Bool(b) => b,
                    Expr::Int(n) => n != 0,
                    _ => return Err("comptime if condition must be bool/int".into()),
                };
                if take {
                    last = Some(eval_comptime_block(&i.then_body, fns)?);
                } else if let Some(ref eb) = i.else_body {
                    last = Some(eval_comptime_block(eb, fns)?);
                }
            }
            other => {
                return Err(format!(
                    "unsupported statement in comptime block: {:?}",
                    std::mem::discriminant(other)
                ));
            }
        }
    }
    last.ok_or_else(|| "comptime block must end with an expression".to_string())
}

fn eval_comptime_expr(
    expr: &Expr,
    env: &HashMap<String, Expr>,
    fns: &HashMap<String, ComptimeFn>,
) -> Result<Expr, String> {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::String(_) | Expr::None => {
            Ok(expr.clone())
        }
        Expr::Ident(n) => env
            .get(n)
            .cloned()
            .ok_or_else(|| format!("comptime unknown name `{}`", n)),
        Expr::Call(callee, args) => {
            let fname = match callee.as_ref() {
                Expr::Ident(n) => n.clone(),
                _ => return Err("comptime only supports direct function calls".into()),
            };
            // 内置 comptime 工具
            if fname == "len" && args.len() == 1 {
                match eval_comptime_expr(&args[0], env, fns)? {
                    Expr::String(s) => return Ok(Expr::Int(s.chars().count() as i64)),
                    Expr::List(xs) => return Ok(Expr::Int(xs.len() as i64)),
                    _ => return Err("comptime len() expects str or list".into()),
                }
            }
            if fname == "str" && args.len() == 1 {
                let v = eval_comptime_expr(&args[0], env, fns)?;
                return Ok(Expr::String(match v {
                    Expr::Int(n) => n.to_string(),
                    Expr::Float(f) => f.to_string(),
                    Expr::Bool(b) => b.to_string(),
                    Expr::String(s) => s,
                    other => format!("{:?}", other),
                }));
            }
            let fdef = fns.get(&fname).ok_or_else(|| {
                format!(
                    "comptime unknown function `{}` (define with `comptime fn {}`)",
                    fname, fname
                )
            })?;
            if fdef.params.len() != args.len() {
                return Err(format!(
                    "comptime fn `{}` expects {} args, got {}",
                    fname,
                    fdef.params.len(),
                    args.len()
                ));
            }
            let mut local = HashMap::new();
            for (i, (pname, _)) in fdef.params.iter().enumerate() {
                local.insert(
                    pname.clone(),
                    eval_comptime_expr(&args[i], env, fns)?,
                );
            }
            // 在函数体上求值（参数作绑定）
            let mut body_bindings = local;
            let mut last = None;
            for stmt in &fdef.body {
                match stmt {
                    Statement::VarDecl(d) => {
                        let v = d
                            .value
                            .as_ref()
                            .ok_or("comptime fn let needs init")?;
                        let v = eval_comptime_expr(v, &body_bindings, fns)?;
                        body_bindings.insert(d.name.clone(), v);
                    }
                    Statement::Return(Some(e)) => {
                        return eval_comptime_expr(e, &body_bindings, fns);
                    }
                    Statement::Expr(e) => {
                        last = Some(eval_comptime_expr(e, &body_bindings, fns)?);
                    }
                    Statement::If(i) => {
                        let cond = eval_comptime_expr(&i.condition, &body_bindings, fns)?;
                        let take = match cond {
                            Expr::Bool(b) => b,
                            Expr::Int(n) => n != 0,
                            _ => false,
                        };
                        if take {
                            return eval_comptime_block_with(&i.then_body, &body_bindings, fns);
                        } else if let Some(ref eb) = i.else_body {
                            return eval_comptime_block_with(eb, &body_bindings, fns);
                        }
                    }
                    _ => {
                        return Err(format!(
                            "unsupported stmt in comptime fn `{}`",
                            fname
                        ));
                    }
                }
            }
            last.ok_or_else(|| format!("comptime fn `{}` produced no value", fname))
        }
        Expr::UnaryOp(op, e) => {
            let v = eval_comptime_expr(e, env, fns)?;
            match (op, v) {
                (UnaryOp::Neg, Expr::Int(n)) => Ok(Expr::Int(-n)),
                (UnaryOp::Neg, Expr::Float(f)) => Ok(Expr::Float(-f)),
                (UnaryOp::Not, Expr::Bool(b)) => Ok(Expr::Bool(!b)),
                _ => Err("invalid comptime unary".to_string()),
            }
        }
        Expr::BinOp(l, op, r) => {
            let lv = eval_comptime_expr(l, env, fns)?;
            let rv = eval_comptime_expr(r, env, fns)?;
            match (lv, op, rv) {
                (Expr::Int(a), BinOp::Add, Expr::Int(b)) => Ok(Expr::Int(a + b)),
                (Expr::Int(a), BinOp::Sub, Expr::Int(b)) => Ok(Expr::Int(a - b)),
                (Expr::Int(a), BinOp::Mul, Expr::Int(b)) => Ok(Expr::Int(a * b)),
                (Expr::Int(a), BinOp::Div, Expr::Int(b)) => {
                    if b == 0 {
                        Err("comptime division by zero".to_string())
                    } else {
                        Ok(Expr::Int(a / b))
                    }
                }
                (Expr::Int(a), BinOp::Mod, Expr::Int(b)) => Ok(Expr::Int(a % b)),
                (Expr::Int(a), BinOp::Eq, Expr::Int(b)) => Ok(Expr::Bool(a == b)),
                (Expr::Int(a), BinOp::Ne, Expr::Int(b)) => Ok(Expr::Bool(a != b)),
                (Expr::Int(a), BinOp::Lt, Expr::Int(b)) => Ok(Expr::Bool(a < b)),
                (Expr::Int(a), BinOp::Le, Expr::Int(b)) => Ok(Expr::Bool(a <= b)),
                (Expr::Int(a), BinOp::Gt, Expr::Int(b)) => Ok(Expr::Bool(a > b)),
                (Expr::Int(a), BinOp::Ge, Expr::Int(b)) => Ok(Expr::Bool(a >= b)),
                (Expr::Bool(a), BinOp::And, Expr::Bool(b)) => Ok(Expr::Bool(a && b)),
                (Expr::Bool(a), BinOp::Or, Expr::Bool(b)) => Ok(Expr::Bool(a || b)),
                (Expr::Float(a), BinOp::Add, Expr::Float(b)) => Ok(Expr::Float(a + b)),
                (Expr::Float(a), BinOp::Sub, Expr::Float(b)) => Ok(Expr::Float(a - b)),
                (Expr::Float(a), BinOp::Mul, Expr::Float(b)) => Ok(Expr::Float(a * b)),
                (Expr::Float(a), BinOp::Div, Expr::Float(b)) => Ok(Expr::Float(a / b)),
                (Expr::String(a), BinOp::Add, Expr::String(b)) => {
                    Ok(Expr::String(format!("{}{}", a, b)))
                }
                _ => Err("unsupported comptime binary operation".to_string()),
            }
        }
        Expr::List(xs) => {
            let mut out = Vec::new();
            for x in xs {
                out.push(eval_comptime_expr(x, env, fns)?);
            }
            Ok(Expr::List(out))
        }
        _ => Err("unsupported expression in comptime block".to_string()),
    }
}

fn eval_comptime_block_with(
    body: &[Statement],
    outer: &HashMap<String, Expr>,
    fns: &HashMap<String, ComptimeFn>,
) -> Result<Expr, String> {
    let mut bindings = outer.clone();
    let mut last = None;
    for stmt in body {
        match stmt {
            Statement::VarDecl(d) => {
                let v = d.value.as_ref().ok_or("comptime let needs init")?;
                let v = eval_comptime_expr(v, &bindings, fns)?;
                bindings.insert(d.name.clone(), v);
            }
            Statement::Return(Some(e)) => return eval_comptime_expr(e, &bindings, fns),
            Statement::Expr(e) => last = Some(eval_comptime_expr(e, &bindings, fns)?),
            Statement::If(i) => {
                let cond = eval_comptime_expr(&i.condition, &bindings, fns)?;
                let take = match cond {
                    Expr::Bool(b) => b,
                    Expr::Int(n) => n != 0,
                    _ => false,
                };
                if take {
                    return eval_comptime_block_with(&i.then_body, &bindings, fns);
                } else if let Some(ref eb) = i.else_body {
                    return eval_comptime_block_with(eb, &bindings, fns);
                }
            }
            _ => return Err("unsupported stmt in nested comptime".into()),
        }
    }
    last.ok_or_else(|| "comptime block produced no value".to_string())
}

fn gen_debug_method_class(c: &ClassDef) -> Result<FuncDef, String> {
    // fn debug() -> str { return "Point { x: " + str(self.x) + ", y: " + str(self.y) + " }" }
    let mut acc = Expr::String(format!("{} {{ ", c.name));
    for (i, f) in c.fields.iter().enumerate() {
        if i > 0 {
            acc = Expr::BinOp(
                Box::new(acc),
                BinOp::Add,
                Box::new(Expr::String(", ".to_string())),
            );
        }
        acc = Expr::BinOp(
            Box::new(acc),
            BinOp::Add,
            Box::new(Expr::String(format!("{}: ", f.name))),
        );
        let field = Expr::Member(Box::new(Expr::Ident("self".to_string())), f.name.clone());
        acc = Expr::BinOp(Box::new(acc), BinOp::Add, Box::new(str_call(field)));
    }
    acc = Expr::BinOp(
        Box::new(acc),
        BinOp::Add,
        Box::new(Expr::String(" }".to_string())),
    );
    Ok(FuncDef {
        name: "debug".to_string(),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params: vec![],
        throws: vec![],
        return_type: Some(Type::Str),
        lifetime_deps: None,
        body: vec![Statement::Return(Some(acc))],
        def_span_start: None,
        attrs: vec![],
    })
}

fn gen_eq_method_class(c: &ClassDef) -> Result<FuncDef, String> {
    // fn eq(other: Point) -> bool { return self.f == other.f and ... }
    let cond = if c.fields.is_empty() {
        Expr::Bool(true)
    } else {
        let mut acc: Option<Expr> = None;
        for f in &c.fields {
            let cmp = Expr::BinOp(
                Box::new(Expr::Member(
                    Box::new(Expr::Ident("self".to_string())),
                    f.name.clone(),
                )),
                BinOp::Eq,
                Box::new(Expr::Member(
                    Box::new(Expr::Ident("other".to_string())),
                    f.name.clone(),
                )),
            );
            acc = Some(match acc {
                None => cmp,
                Some(prev) => Expr::BinOp(Box::new(prev), BinOp::And, Box::new(cmp)),
            });
        }
        acc.unwrap()
    };
    Ok(FuncDef {
        name: "eq".to_string(),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params: vec![bolide_parser::Param {
            name: "other".to_string(),
            ty: Type::Custom(c.name.clone()),
            mode: bolide_parser::ParamMode::Borrow,
            default_value: None,
            is_variadic: false,
            is_kw_variadic: false,
        }],
        throws: vec![],
        return_type: Some(Type::Bool),
        lifetime_deps: None,
        body: vec![Statement::Return(Some(cond))],
        def_span_start: None,
        attrs: vec![],
    })
}

fn gen_debug_fn_value(v: &ValueDef) -> Result<FuncDef, String> {
    let mut acc = Expr::String(format!("{} {{ ", v.name));
    for (i, f) in v.fields.iter().enumerate() {
        if i > 0 {
            acc = Expr::BinOp(
                Box::new(acc),
                BinOp::Add,
                Box::new(Expr::String(", ".to_string())),
            );
        }
        acc = Expr::BinOp(
            Box::new(acc),
            BinOp::Add,
            Box::new(Expr::String(format!("{}: ", f.name))),
        );
        let field = Expr::Member(Box::new(Expr::Ident("self".to_string())), f.name.clone());
        acc = Expr::BinOp(Box::new(acc), BinOp::Add, Box::new(str_call(field)));
    }
    acc = Expr::BinOp(
        Box::new(acc),
        BinOp::Add,
        Box::new(Expr::String(" }".to_string())),
    );
    Ok(FuncDef {
        name: format!("{}_debug", v.name),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params: vec![bolide_parser::Param {
            name: "self".to_string(),
            ty: Type::Custom(v.name.clone()),
            mode: bolide_parser::ParamMode::Borrow,
            default_value: None,
            is_variadic: false,
            is_kw_variadic: false,
        }],
        throws: vec![],
        return_type: Some(Type::Str),
        lifetime_deps: None,
        body: vec![Statement::Return(Some(acc))],
        def_span_start: None,
        attrs: vec![],
    })
}

fn gen_eq_fn_value(v: &ValueDef) -> Result<FuncDef, String> {
    let cond = if v.fields.is_empty() {
        Expr::Bool(true)
    } else {
        let mut acc: Option<Expr> = None;
        for f in &v.fields {
            let cmp = Expr::BinOp(
                Box::new(Expr::Member(
                    Box::new(Expr::Ident("a".to_string())),
                    f.name.clone(),
                )),
                BinOp::Eq,
                Box::new(Expr::Member(
                    Box::new(Expr::Ident("b".to_string())),
                    f.name.clone(),
                )),
            );
            acc = Some(match acc {
                None => cmp,
                Some(prev) => Expr::BinOp(Box::new(prev), BinOp::And, Box::new(cmp)),
            });
        }
        acc.unwrap()
    };
    Ok(FuncDef {
        name: format!("{}_eq", v.name),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params: vec![
            bolide_parser::Param {
                name: "a".to_string(),
                ty: Type::Custom(v.name.clone()),
                mode: bolide_parser::ParamMode::Borrow,
                default_value: None,
                is_variadic: false,
                is_kw_variadic: false,
            },
            bolide_parser::Param {
                name: "b".to_string(),
                ty: Type::Custom(v.name.clone()),
                mode: bolide_parser::ParamMode::Borrow,
                default_value: None,
                is_variadic: false,
                is_kw_variadic: false,
            },
        ],
        throws: vec![],
        return_type: Some(Type::Bool),
        lifetime_deps: None,
        body: vec![Statement::Return(Some(cond))],
        def_span_start: None,
        attrs: vec![],
    })
}

/// 粗粒度表达式源码还原（用于 assert!/dbg! 消息）
pub fn expr_to_src(expr: &Expr) -> String {
    match expr {
        Expr::Int(n) => n.to_string(),
        Expr::Float(f) => f.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::String(s) => format!("\"{}\"", s),
        Expr::Ident(s) => s.clone(),
        Expr::None => "none".to_string(),
        Expr::BinOp(l, op, r) => {
            let os = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::Eq => "==",
                BinOp::Ne => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::And => "and",
                BinOp::Or => "or",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::Xor => "^",
            };
            format!("({} {} {})", expr_to_src(l), os, expr_to_src(r))
        }
        Expr::UnaryOp(op, e) => {
            let os = match op {
                UnaryOp::Neg => "-",
                UnaryOp::Not => "not ",
            };
            format!("({}{})", os, expr_to_src(e))
        }
        Expr::Call(c, args) => {
            let a: Vec<_> = args.iter().map(expr_to_src).collect();
            format!("{}({})", expr_to_src(c), a.join(", "))
        }
        Expr::Member(b, m) => format!("{}.{}", expr_to_src(b), m),
        Expr::Index(b, i) => format!("{}[{}]", expr_to_src(b), expr_to_src(i)),
        Expr::List(xs) => {
            let a: Vec<_> = xs.iter().map(expr_to_src).collect();
            format!("[{}]", a.join(", "))
        }
        Expr::MacroInvoke(m) => format!("{}!(...)", m.path.join(".")),
        Expr::Splice { name, .. } => format!("${}", name),
        _ => "<expr>".to_string(),
    }
}

/// 将展开后的 AST 打印为可读 Bolide 子集（`bolide expand`）
pub fn pretty_print(program: &Program) -> String {
    let mut out = String::new();
    for stmt in &program.statements {
        pretty_stmt(stmt, 0, &mut out);
    }
    out
}

fn indent(level: usize) -> String {
    "    ".repeat(level)
}

fn pretty_stmt(stmt: &Statement, level: usize, out: &mut String) {
    let ind = indent(level);
    match stmt {
        Statement::Expr(e) => {
            out.push_str(&ind);
            out.push_str(&expr_to_src(e));
            out.push_str(";\n");
        }
        Statement::VarDecl(d) => {
            out.push_str(&ind);
            out.push_str(if d.mutable { "var " } else { "let " });
            out.push_str(&d.name);
            if let Some(v) = &d.value {
                out.push_str(" = ");
                out.push_str(&expr_to_src(v));
            }
            out.push_str(";\n");
        }
        Statement::FuncDef(f) => {
            out.push_str(&ind);
            if f.is_export {
                out.push_str("export ");
            }
            if f.is_inline {
                out.push_str("inline ");
            }
            out.push_str("fn ");
            out.push_str(&f.name);
            out.push_str("(...) {\n");
            for s in &f.body {
                pretty_stmt(s, level + 1, out);
            }
            out.push_str(&ind);
            out.push_str("}\n");
        }
        Statement::ClassDef(c) => {
            out.push_str(&ind);
            out.push_str("class ");
            out.push_str(&c.name);
            out.push_str(" {\n");
            for f in &c.fields {
                out.push_str(&indent(level + 1));
                out.push_str(&f.name);
                out.push_str(": ...;\n");
            }
            for m in &c.methods {
                pretty_stmt(&Statement::FuncDef(m.clone()), level + 1, out);
            }
            out.push_str(&ind);
            out.push_str("}\n");
        }
        Statement::If(i) => {
            out.push_str(&ind);
            out.push_str("if ");
            out.push_str(&expr_to_src(&i.condition));
            out.push_str(" {\n");
            for s in &i.then_body {
                pretty_stmt(s, level + 1, out);
            }
            out.push_str(&ind);
            out.push_str("}\n");
        }
        Statement::Throw(e) => {
            out.push_str(&ind);
            out.push_str("throw ");
            out.push_str(&expr_to_src(e));
            out.push_str(";\n");
        }
        Statement::Return(Some(e)) => {
            out.push_str(&ind);
            out.push_str("return ");
            out.push_str(&expr_to_src(e));
            out.push_str(";\n");
        }
        Statement::Return(None) => {
            out.push_str(&ind);
            out.push_str("return;\n");
        }
        Statement::MacroDef(m) => {
            out.push_str(&ind);
            out.push_str("// macro ");
            out.push_str(&m.name);
            out.push('\n');
        }
        _ => {
            out.push_str(&ind);
            out.push_str("/* ... */\n");
        }
    }
}

// silence unused import warnings for types used only in docs
#[allow(dead_code)]
fn _keep_types(_: ClassField, _: ValueField, _: EnumDef, _: Attribute) {}
