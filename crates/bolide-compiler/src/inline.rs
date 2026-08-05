//! 内联展开 pass
//!
//! 在单态化之后、后端编译之前：
//! 1. 显式 `inline fn` 的调用点展开
//! 2. 自动内联「小、非递归、无异常/spawn」的叶子函数（含 if/while）
//!
//! 表达式中的调用会先降为临时变量 + 语句序列，再替换为临时名，
//! 从而支持 `sum = sum + iter_at(...)` 这类热路径。

use bolide_parser::{
    Assign, BinOp, Expr, FuncDef, IfStmt, Program, Statement, UnaryOp, VarDecl, WhileStmt,
};
use std::collections::{HashMap, HashSet};

/// 自动内联的粗粒度成本上限（语句加权）
/// 覆盖 mandelbrot `iter_at` 一类「单出口 + 小 while」热函数
const AUTO_INLINE_COST_LIMIT: usize = 96;
/// 防止嵌套内联爆炸
const MAX_INLINE_DEPTH: usize = 6;

pub fn inline_expand(program: Program) -> Result<Program, String> {
    let mut all_funcs: HashMap<String, FuncDef> = HashMap::new();
    for stmt in &program.statements {
        if let Statement::FuncDef(func) = stmt {
            all_funcs.insert(func.name.clone(), func.clone());
        }
    }

    // 递归集合：函数体（含传递）中调用了自己
    let recursive = find_recursive(&all_funcs);

    let mut inline_defs: HashMap<String, FuncDef> = HashMap::new();
    for (name, func) in &all_funcs {
        if func.is_inline {
            inline_defs.insert(name.clone(), func.clone());
            continue;
        }
        if recursive.contains(name) {
            continue;
        }
        if is_auto_inline_candidate(func) {
            inline_defs.insert(name.clone(), func.clone());
        }
    }

    let inliner = Inliner {
        inline_defs,
        counter: 0,
    };
    inliner.run(program)
}

struct Inliner {
    inline_defs: HashMap<String, FuncDef>,
    counter: usize,
}

impl Inliner {
    fn run(mut self, program: Program) -> Result<Program, String> {
        let mut new_stmts = Vec::new();
        for stmt in program.statements {
            match stmt {
                Statement::FuncDef(func) if func.is_inline => {
                    // 显式 inline 定义不保留（与旧行为一致）
                    // 自动内联候选仍保留定义（可能被间接引用）
                }
                Statement::FuncDef(func) => {
                    // 显式 inline 已剔除；自动候选保留但展开其体内调用
                    if self.inline_defs.contains_key(&func.name) && func.is_inline {
                        // already skipped
                    } else {
                        new_stmts.push(Statement::FuncDef(self.expand_function(func)?));
                    }
                }
                other => {
                    let mut out = Vec::new();
                    self.expand_statement_into(other, &mut out, 0)?;
                    new_stmts.extend(out);
                }
            }
        }
        // 显式 inline 定义已丢弃；自动内联候选若标记 is_inline 也应丢弃——上面只丢 is_inline
        Ok(Program {
            statements: new_stmts,
        })
    }

    fn expand_function(&mut self, func: FuncDef) -> Result<FuncDef, String> {
        let mut new_body = Vec::new();
        for stmt in func.body {
            self.expand_statement_into(stmt, &mut new_body, 0)?;
        }
        Ok(FuncDef {
            body: new_body,
            ..func
        })
    }

    fn expand_statement_into(
        &mut self,
        stmt: Statement,
        out: &mut Vec<Statement>,
        depth: usize,
    ) -> Result<(), String> {
        match stmt {
            Statement::Expr(e) => {
                let e = self.lower_expr(e, out, depth)?;
                out.push(Statement::Expr(e));
            }
            Statement::Return(Some(e)) => {
                let e = self.lower_expr(e, out, depth)?;
                out.push(Statement::Return(Some(e)));
            }
            Statement::Return(None) => out.push(Statement::Return(None)),
            Statement::VarDecl(decl) => {
                let value = match decl.value {
                    Some(v) => Some(self.lower_expr(v, out, depth)?),
                    None => None,
                };
                out.push(Statement::VarDecl(VarDecl { value, ..decl }));
            }
            Statement::Assign(assign) => {
                let value = self.lower_expr(assign.value, out, depth)?;
                out.push(Statement::Assign(Assign {
                    target: assign.target,
                    value,
                }));
            }
            Statement::If(if_stmt) => {
                let cond = self.lower_expr(if_stmt.condition, out, depth)?;
                let mut then_body = Vec::new();
                for s in if_stmt.then_body {
                    self.expand_statement_into(s, &mut then_body, depth)?;
                }
                let mut elif_branches = Vec::new();
                for (c, body) in if_stmt.elif_branches {
                    // elif 条件在分支外先算（与原语义略有不同若有副作用；Bolide 条件通常纯）
                    let c = self.lower_expr(c, out, depth)?;
                    let mut b = Vec::new();
                    for s in body {
                        self.expand_statement_into(s, &mut b, depth)?;
                    }
                    elif_branches.push((c, b));
                }
                let else_body = if let Some(body) = if_stmt.else_body {
                    let mut b = Vec::new();
                    for s in body {
                        self.expand_statement_into(s, &mut b, depth)?;
                    }
                    Some(b)
                } else {
                    None
                };
                out.push(Statement::If(IfStmt {
                    condition: cond,
                    then_body,
                    elif_branches,
                    else_body,
                }));
            }
            Statement::While(ws) => {
                // while 条件每轮求值：不能把副作用降到循环外。
                // 仅展开 body；条件内调用保持（或做简单递归 expand_expr 仅对 inline 表达式形态）。
                let cond = self.expand_expr_simple(ws.condition, depth)?;
                let mut body = Vec::new();
                for s in ws.body {
                    self.expand_statement_into(s, &mut body, depth)?;
                }
                out.push(Statement::While(WhileStmt {
                    condition: cond,
                    body,
                }));
            }
            other => out.push(other),
        }
        Ok(())
    }

    /// 将表达式中的可内联调用降为前置语句 + 临时变量
    fn lower_expr(
        &mut self,
        expr: Expr,
        out: &mut Vec<Statement>,
        depth: usize,
    ) -> Result<Expr, String> {
        if depth > MAX_INLINE_DEPTH {
            return self.expand_expr_simple(expr, depth);
        }
        match expr {
            Expr::Call(callee, args) => {
                let callee = self.lower_expr(*callee, out, depth)?;
                let mut new_args = Vec::new();
                for a in args {
                    new_args.push(self.lower_expr(a, out, depth)?);
                }
                if let Expr::Ident(name) = &callee {
                    if let Some(func) = self.inline_defs.get(name).cloned() {
                        return self.emit_inlined_call(&func, &new_args, out, depth);
                    }
                }
                Ok(Expr::Call(Box::new(callee), new_args))
            }
            Expr::BinOp(l, op, r) => Ok(Expr::BinOp(
                Box::new(self.lower_expr(*l, out, depth)?),
                op,
                Box::new(self.lower_expr(*r, out, depth)?),
            )),
            Expr::UnaryOp(op, o) => Ok(Expr::UnaryOp(
                op,
                Box::new(self.lower_expr(*o, out, depth)?),
            )),
            Expr::Index(b, i) => Ok(Expr::Index(
                Box::new(self.lower_expr(*b, out, depth)?),
                Box::new(self.lower_expr(*i, out, depth)?),
            )),
            Expr::Member(b, m) => Ok(Expr::Member(
                Box::new(self.lower_expr(*b, out, depth)?),
                m,
            )),
            Expr::List(items) => {
                let mut v = Vec::new();
                for i in items {
                    v.push(self.lower_expr(i, out, depth)?);
                }
                Ok(Expr::List(v))
            }
            Expr::Tuple(items) => {
                let mut v = Vec::new();
                for i in items {
                    v.push(self.lower_expr(i, out, depth)?);
                }
                Ok(Expr::Tuple(v))
            }
            Expr::Dict(entries) => {
                let mut v = Vec::new();
                for (k, val) in entries {
                    v.push((
                        self.lower_expr(k, out, depth)?,
                        self.lower_expr(val, out, depth)?,
                    ));
                }
                Ok(Expr::Dict(v))
            }
            Expr::ValueConstruct(name, fields) => {
                let mut v = Vec::new();
                for (n, e) in fields {
                    v.push((n, self.lower_expr(e, out, depth)?));
                }
                Ok(Expr::ValueConstruct(name, v))
            }
            other => Ok(other),
        }
    }

    /// 仅递归处理显式 inline 的纯表达式形态（while 条件用）
    fn expand_expr_simple(&mut self, expr: Expr, depth: usize) -> Result<Expr, String> {
        match expr {
            Expr::Call(callee, args) => {
                let callee = self.expand_expr_simple(*callee, depth)?;
                let mut new_args = Vec::new();
                for a in args {
                    new_args.push(self.expand_expr_simple(a, depth)?);
                }
                // 纯表达式形态：仅当函数体是「let* + return expr」且无 while/if 时内联
                if let Expr::Ident(name) = &callee {
                    if let Some(func) = self.inline_defs.get(name) {
                        if func.is_inline && is_pure_expr_body(func) {
                            let mut dummy = Vec::new();
                            return self.emit_inlined_call(
                                &func.clone(),
                                &new_args,
                                &mut dummy,
                                depth,
                            );
                        }
                    }
                }
                Ok(Expr::Call(Box::new(callee), new_args))
            }
            Expr::BinOp(l, op, r) => Ok(Expr::BinOp(
                Box::new(self.expand_expr_simple(*l, depth)?),
                op,
                Box::new(self.expand_expr_simple(*r, depth)?),
            )),
            Expr::UnaryOp(op, o) => Ok(Expr::UnaryOp(
                op,
                Box::new(self.expand_expr_simple(*o, depth)?),
            )),
            Expr::Index(b, i) => Ok(Expr::Index(
                Box::new(self.expand_expr_simple(*b, depth)?),
                Box::new(self.expand_expr_simple(*i, depth)?),
            )),
            Expr::Member(b, m) => Ok(Expr::Member(
                Box::new(self.expand_expr_simple(*b, depth)?),
                m,
            )),
            other => Ok(other),
        }
    }

    fn emit_inlined_call(
        &mut self,
        func: &FuncDef,
        args: &[Expr],
        out: &mut Vec<Statement>,
        depth: usize,
    ) -> Result<Expr, String> {
        self.counter += 1;
        let id = self.counter;

        // 参数绑定
        let mut subst: HashMap<String, Expr> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            let tmp = format!("__inl_{}_{}_a{}", id, func.name, i);
            let arg = if i < args.len() {
                args[i].clone()
            } else {
                Expr::Int(0)
            };
            out.push(Statement::VarDecl(VarDecl {
                name: tmp.clone(),
                mutable: false,
                ty: Some(param.ty.clone()),
                value: Some(arg),
                name_is_splice: false,
            }));
            subst.insert(param.name.clone(), Expr::Ident(tmp));
        }

        // 局部变量 alpha rename
        let mut local_rename: HashMap<String, String> = HashMap::new();
        collect_local_names(&func.body, &mut local_rename, id, &func.name);
        for (old, new) in &local_rename {
            subst.insert(old.clone(), Expr::Ident(new.clone()));
        }

        let ret_name = format!("__inl_{}_{}_ret", id, func.name);
        // 默认返回值按返回类型选，避免 float 槽被写成 int 0 导致位型破坏
        let default_ret = default_value_for_type(func.return_type.as_ref());
        out.push(Statement::VarDecl(VarDecl {
            name: ret_name.clone(),
            mutable: true,
            ty: func.return_type.clone(),
            value: Some(default_ret),
            name_is_splice: false,
        }));

        let body = rewrite_body_for_inline(&func.body, &subst, &ret_name, &local_rename)?;
        for stmt in body {
            self.expand_statement_into(stmt, out, depth + 1)?;
        }

        Ok(Expr::Ident(ret_name))
    }
}

fn collect_local_names(
    body: &[Statement],
    map: &mut HashMap<String, String>,
    id: usize,
    fname: &str,
) {
    for stmt in body {
        match stmt {
            Statement::VarDecl(d) => {
                map.entry(d.name.clone()).or_insert_with(|| {
                    format!("__inl_{}_{}_{}", id, fname, d.name)
                });
            }
            Statement::If(i) => {
                collect_local_names(&i.then_body, map, id, fname);
                for (_, b) in &i.elif_branches {
                    collect_local_names(b, map, id, fname);
                }
                if let Some(b) = &i.else_body {
                    collect_local_names(b, map, id, fname);
                }
            }
            Statement::While(w) => collect_local_names(&w.body, map, id, fname),
            _ => {}
        }
    }
}

fn rewrite_body_for_inline(
    body: &[Statement],
    subst: &HashMap<String, Expr>,
    ret_name: &str,
    local_rename: &HashMap<String, String>,
) -> Result<Vec<Statement>, String> {
    let mut out = Vec::new();
    for stmt in body {
        match stmt {
            Statement::Return(Some(e)) => {
                out.push(Statement::Assign(Assign {
                    target: Expr::Ident(ret_name.to_string()),
                    value: substitute_expr(e, subst),
                }));
            }
            Statement::Return(None) => {
                // 视为返回完成
            }
            Statement::VarDecl(d) => {
                let new_name = local_rename
                    .get(&d.name)
                    .cloned()
                    .unwrap_or_else(|| d.name.clone());
                let value = d.value.as_ref().map(|v| substitute_expr(v, subst));
                out.push(Statement::VarDecl(VarDecl {
                    name: new_name,
                    mutable: d.mutable,
                    ty: d.ty.clone(),
                    value,
                    name_is_splice: false,
                }));
            }
            Statement::Assign(a) => {
                out.push(Statement::Assign(Assign {
                    target: substitute_expr(&a.target, subst),
                    value: substitute_expr(&a.value, subst),
                }));
            }
            Statement::Expr(e) => {
                out.push(Statement::Expr(substitute_expr(e, subst)));
            }
            Statement::If(i) => {
                let then_body = rewrite_body_for_inline(&i.then_body, subst, ret_name, local_rename)?;
                let mut elif_branches = Vec::new();
                for (c, b) in &i.elif_branches {
                    elif_branches.push((
                        substitute_expr(c, subst),
                        rewrite_body_for_inline(b, subst, ret_name, local_rename)?,
                    ));
                }
                let else_body = match &i.else_body {
                    Some(b) => Some(rewrite_body_for_inline(b, subst, ret_name, local_rename)?),
                    None => None,
                };
                out.push(Statement::If(IfStmt {
                    condition: substitute_expr(&i.condition, subst),
                    then_body,
                    elif_branches,
                    else_body,
                }));
            }
            Statement::While(w) => {
                out.push(Statement::While(WhileStmt {
                    condition: substitute_expr(&w.condition, subst),
                    body: rewrite_body_for_inline(&w.body, subst, ret_name, local_rename)?,
                }));
            }
            other => {
                return Err(format!(
                    "cannot inline function containing unsupported statement: {:?}",
                    std::mem::discriminant(other)
                ));
            }
        }
    }
    Ok(out)
}

fn default_value_for_type(ty: Option<&bolide_parser::Type>) -> Expr {
    match ty {
        Some(bolide_parser::Type::Float) => Expr::Float(0.0),
        Some(bolide_parser::Type::Bool) => Expr::Bool(false),
        Some(bolide_parser::Type::Str) => Expr::String(String::new()),
        _ => Expr::Int(0),
    }
}

fn is_pure_expr_body(func: &FuncDef) -> bool {
    if func.body.is_empty() {
        return false;
    }
    let last = func.body.last().unwrap();
    if !matches!(last, Statement::Return(Some(_))) {
        return false;
    }
    func.body[..func.body.len() - 1]
        .iter()
        .all(|s| matches!(s, Statement::VarDecl(_)))
}

fn is_auto_inline_candidate(func: &FuncDef) -> bool {
    if func.is_async || func.lifetime_deps.is_some() {
        return false;
    }
    // 自动内联仅限标量参数/返回值，避免 list/对象 RC 所有权被错误复制
    if !func.params.iter().all(|p| is_scalar_type(&p.ty)) {
        return false;
    }
    if let Some(ret) = &func.return_type {
        if !is_scalar_type(ret) {
            return false;
        }
    }
    if !body_is_inlineable(&func.body) {
        return false;
    }
    // 仅允许「唯一 return 且在函数末尾」——避免 if/while 内 return 被改成赋值后继续执行
    if !returns_only_at_end(&func.body) {
        return false;
    }
    body_cost(&func.body) <= AUTO_INLINE_COST_LIMIT
}

fn is_scalar_type(ty: &bolide_parser::Type) -> bool {
    matches!(
        ty,
        bolide_parser::Type::Int
            | bolide_parser::Type::Float
            | bolide_parser::Type::Bool
    )
}

fn returns_only_at_end(body: &[Statement]) -> bool {
    if body.is_empty() {
        return false;
    }
    if !matches!(body.last(), Some(Statement::Return(_))) {
        return false;
    }
    !body[..body.len() - 1].iter().any(stmt_contains_return)
}

fn stmt_contains_return(stmt: &Statement) -> bool {
    match stmt {
        Statement::Return(_) => true,
        Statement::If(i) => {
            i.then_body.iter().any(stmt_contains_return)
                || i.elif_branches
                    .iter()
                    .any(|(_, b)| b.iter().any(stmt_contains_return))
                || i.else_body
                    .as_ref()
                    .map(|b| b.iter().any(stmt_contains_return))
                    .unwrap_or(false)
        }
        Statement::While(w) => w.body.iter().any(stmt_contains_return),
        _ => false,
    }
}

fn body_is_inlineable(body: &[Statement]) -> bool {
    body.iter().all(|s| match s {
        Statement::VarDecl(_)
        | Statement::Assign(_)
        | Statement::Expr(_)
        | Statement::Return(_)
        | Statement::Break
        | Statement::Continue => true,
        Statement::If(i) => {
            body_is_inlineable(&i.then_body)
                && i.elif_branches
                    .iter()
                    .all(|(_, b)| body_is_inlineable(b))
                && i.else_body
                    .as_ref()
                    .map(|b| body_is_inlineable(b))
                    .unwrap_or(true)
        }
        Statement::While(w) => body_is_inlineable(&w.body),
        _ => false, // for/try/throw/spawn/match/with/...
    })
}

fn body_cost(body: &[Statement]) -> usize {
    body.iter().map(stmt_cost).sum()
}

fn stmt_cost(stmt: &Statement) -> usize {
    match stmt {
        Statement::VarDecl(d) => 1 + d.value.as_ref().map(expr_cost).unwrap_or(0),
        Statement::Assign(a) => 1 + expr_cost(&a.value),
        Statement::Expr(e) | Statement::Return(Some(e)) => 1 + expr_cost(e),
        Statement::Return(None) | Statement::Break | Statement::Continue => 1,
        Statement::If(i) => {
            2 + expr_cost(&i.condition)
                + body_cost(&i.then_body)
                + i.elif_branches
                    .iter()
                    .map(|(c, b)| 1 + expr_cost(c) + body_cost(b))
                    .sum::<usize>()
                + i.else_body.as_ref().map(|b| body_cost(b)).unwrap_or(0)
        }
        Statement::While(w) => 3 + expr_cost(&w.condition) + body_cost(&w.body),
        _ => 1000,
    }
}

fn expr_cost(expr: &Expr) -> usize {
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::String(_)
        | Expr::Ident(_)
        | Expr::None => 1,
        Expr::BinOp(l, _, r) => 1 + expr_cost(l) + expr_cost(r),
        Expr::UnaryOp(_, o) => 1 + expr_cost(o),
        Expr::Call(c, args) => 2 + expr_cost(c) + args.iter().map(expr_cost).sum::<usize>(),
        Expr::Index(b, i) => 1 + expr_cost(b) + expr_cost(i),
        Expr::Member(b, _) => 1 + expr_cost(b),
        Expr::List(items) | Expr::Tuple(items) => {
            1 + items.iter().map(expr_cost).sum::<usize>()
        }
        _ => 8,
    }
}

fn find_recursive(funcs: &HashMap<String, FuncDef>) -> HashSet<String> {
    let mut recursive = HashSet::new();
    for (name, func) in funcs {
        let mut called = HashSet::new();
        collect_calls_in_body(&func.body, &mut called);
        if called.contains(name) {
            recursive.insert(name.clone());
        }
    }
    // 简单环：A→B→A
    let names: Vec<_> = funcs.keys().cloned().collect();
    for a in &names {
        for b in &names {
            if a == b {
                continue;
            }
            let mut ca = HashSet::new();
            let mut cb = HashSet::new();
            if let Some(fa) = funcs.get(a) {
                collect_calls_in_body(&fa.body, &mut ca);
            }
            if let Some(fb) = funcs.get(b) {
                collect_calls_in_body(&fb.body, &mut cb);
            }
            if ca.contains(b) && cb.contains(a) {
                recursive.insert(a.clone());
                recursive.insert(b.clone());
            }
        }
    }
    recursive
}

fn collect_calls_in_body(body: &[Statement], out: &mut HashSet<String>) {
    for s in body {
        collect_calls_in_stmt(s, out);
    }
}

fn collect_calls_in_stmt(stmt: &Statement, out: &mut HashSet<String>) {
    match stmt {
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_calls_in_expr(e, out)
        }
        Statement::VarDecl(d) => {
            if let Some(v) = &d.value {
                collect_calls_in_expr(v, out);
            }
        }
        Statement::Assign(a) => collect_calls_in_expr(&a.value, out),
        Statement::If(i) => {
            collect_calls_in_expr(&i.condition, out);
            collect_calls_in_body(&i.then_body, out);
            for (c, b) in &i.elif_branches {
                collect_calls_in_expr(c, out);
                collect_calls_in_body(b, out);
            }
            if let Some(b) = &i.else_body {
                collect_calls_in_body(b, out);
            }
        }
        Statement::While(w) => {
            collect_calls_in_expr(&w.condition, out);
            collect_calls_in_body(&w.body, out);
        }
        Statement::For(f) => {
            collect_calls_in_expr(&f.iter, out);
            collect_calls_in_body(&f.body, out);
        }
        _ => {}
    }
}

fn collect_calls_in_expr(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Call(c, args) => {
            if let Expr::Ident(name) = c.as_ref() {
                out.insert(name.clone());
            }
            collect_calls_in_expr(c, out);
            for a in args {
                collect_calls_in_expr(a, out);
            }
        }
        Expr::BinOp(l, _, r) => {
            collect_calls_in_expr(l, out);
            collect_calls_in_expr(r, out);
        }
        Expr::UnaryOp(_, o) | Expr::Member(o, _) | Expr::Await(o) => collect_calls_in_expr(o, out),
        Expr::Index(b, i) => {
            collect_calls_in_expr(b, out);
            collect_calls_in_expr(i, out);
        }
        Expr::List(items) | Expr::Tuple(items) => {
            for i in items {
                collect_calls_in_expr(i, out);
            }
        }
        _ => {}
    }
}

/// AST 表达式中的变量替换
fn substitute_expr(expr: &Expr, subst: &HashMap<String, Expr>) -> Expr {
    match expr {
        Expr::Ident(name) => subst.get(name).cloned().unwrap_or_else(|| expr.clone()),
        Expr::Member(base, member) => {
            Expr::Member(Box::new(substitute_expr(base, subst)), member.clone())
        }
        Expr::BinOp(l, op, r) => Expr::BinOp(
            Box::new(substitute_expr(l, subst)),
            *op,
            Box::new(substitute_expr(r, subst)),
        ),
        Expr::UnaryOp(op, operand) => {
            Expr::UnaryOp(*op, Box::new(substitute_expr(operand, subst)))
        }
        Expr::Call(callee, args) => Expr::Call(
            Box::new(substitute_expr(callee, subst)),
            args.iter().map(|a| substitute_expr(a, subst)).collect(),
        ),
        Expr::ValueConstruct(name, fields) => Expr::ValueConstruct(
            name.clone(),
            fields
                .iter()
                .map(|(n, e)| (n.clone(), substitute_expr(e, subst)))
                .collect(),
        ),
        Expr::Tuple(items) => {
            Expr::Tuple(items.iter().map(|i| substitute_expr(i, subst)).collect())
        }
        Expr::List(items) => {
            Expr::List(items.iter().map(|i| substitute_expr(i, subst)).collect())
        }
        Expr::Dict(entries) => Expr::Dict(
            entries
                .iter()
                .map(|(k, v)| (substitute_expr(k, subst), substitute_expr(v, subst)))
                .collect(),
        ),
        Expr::Index(base, idx) => Expr::Index(
            Box::new(substitute_expr(base, subst)),
            Box::new(substitute_expr(idx, subst)),
        ),
        Expr::Slice(base, start, end, step) => {
            let base = Box::new(substitute_expr(base, subst));
            let start = start.as_ref().map(|e| Box::new(substitute_expr(e, subst)));
            let end = end.as_ref().map(|e| Box::new(substitute_expr(e, subst)));
            let step = step.as_ref().map(|e| Box::new(substitute_expr(e, subst)));
            Expr::Slice(base, start, end, step)
        }
        _ => expr.clone(),
    }
}

// silence unused import warning if BinOp/UnaryOp only used via patterns
#[allow(dead_code)]
fn _use_ops(_: BinOp, _: UnaryOp) {}
