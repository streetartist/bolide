//! 闭包自由变量分析
//!
//! 纯 AST 分析：找出闭包体中引用、但未在闭包内部绑定的标识符。
//! 这些「候选自由变量」中，真正属于外层局部变量的会被捕获进 env；
//! 全局变量与函数名由后端直接访问，不参与捕获（在创建点按作用域过滤）。

use std::collections::HashSet;

use bolide_parser::{
    Expr, ForStmt, IfStmt, MatchArm, Param, Pattern, SelectBranch, Statement, WhileStmt,
};

/// 计算闭包体的候选自由变量（保持首次出现顺序，便于稳定的 env 布局）。
pub fn free_variables(params: &[Param], body: &[Statement]) -> Vec<String> {
    let mut bound: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
    let mut free: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for stmt in body {
        collect_stmt(stmt, &mut bound, &mut free, &mut seen);
    }
    free
}

fn add_free(
    name: &str,
    bound: &HashSet<String>,
    free: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    if bound.contains(name) || seen.contains(name) {
        return;
    }
    // self / super 不是普通捕获变量
    if name == "self" || name == "super" {
        return;
    }
    seen.insert(name.to_string());
    free.push(name.to_string());
}

fn collect_stmt(
    stmt: &Statement,
    bound: &mut HashSet<String>,
    free: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    match stmt {
        Statement::VarDecl(decl) => {
            if let Some(ref v) = decl.value {
                collect_expr(v, bound, free, seen);
            }
            // let 引入新绑定（后续语句中遮蔽外层）
            bound.insert(decl.name.clone());
        }
        Statement::Assign(assign) => {
            collect_expr(&assign.target, bound, free, seen);
            collect_expr(&assign.value, bound, free, seen);
        }
        Statement::Expr(e) => collect_expr(e, bound, free, seen),
        Statement::Return(Some(e)) => collect_expr(e, bound, free, seen),
        Statement::Return(None) => {}
        Statement::Throw(e) => collect_expr(e, bound, free, seen),
        Statement::If(IfStmt {
            condition,
            then_body,
            elif_branches,
            else_body,
        }) => {
            collect_expr(condition, bound, free, seen);
            collect_block(then_body, bound, free, seen);
            for (cond, body) in elif_branches {
                collect_expr(cond, bound, free, seen);
                collect_block(body, bound, free, seen);
            }
            if let Some(body) = else_body {
                collect_block(body, bound, free, seen);
            }
        }
        Statement::While(WhileStmt { condition, body }) => {
            collect_expr(condition, bound, free, seen);
            collect_block(body, bound, free, seen);
        }
        Statement::For(ForStmt { vars, iter, body }) => {
            collect_expr(iter, bound, free, seen);
            // for 变量是循环内的新绑定
            let mut inner = bound.clone();
            for v in vars {
                inner.insert(v.clone());
            }
            collect_block_with(body, &mut inner, free, seen);
        }
        Statement::Match(m) => {
            collect_expr(&m.expr, bound, free, seen);
            for arm in &m.arms {
                collect_match_arm(arm, bound, free, seen);
            }
        }
        Statement::Try(t) => {
            collect_block(&t.try_body, bound, free, seen);
            for clause in &t.catch_clauses {
                let mut inner = bound.clone();
                inner.insert(clause.var.clone());
                collect_block_with(&clause.body, &mut inner, free, seen);
            }
            if let Some(fin) = &t.finally {
                collect_block(fin, bound, free, seen);
            }
        }
        Statement::Pool(p) => {
            collect_expr(&p.size, bound, free, seen);
            collect_block(&p.body, bound, free, seen);
        }
        Statement::AwaitScope(a) => collect_block(&a.body, bound, free, seen),
        Statement::Select(s) => {
            for branch in &s.branches {
                match branch {
                    SelectBranch::Recv { var, channel, body } => {
                        add_free(channel, bound, free, seen);
                        let mut inner = bound.clone();
                        inner.insert(var.clone());
                        collect_block_with(body, &mut inner, free, seen);
                    }
                    SelectBranch::Timeout { duration, body } => {
                        collect_expr(duration, bound, free, seen);
                        collect_block(body, bound, free, seen);
                    }
                    SelectBranch::Default { body } => collect_block(body, bound, free, seen),
                }
            }
        }
        Statement::Send(s) => {
            add_free(&s.channel, bound, free, seen);
            collect_expr(&s.value, bound, free, seen);
        }
        // 其余语句不涉及变量引用或暂不分析
        _ => {}
    }
}

fn collect_block(
    body: &[Statement],
    bound: &HashSet<String>,
    free: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    // 块内的 let 仅在块内可见；用副本隔离
    let mut inner = bound.clone();
    for stmt in body {
        collect_stmt(stmt, &mut inner, free, seen);
    }
}

fn collect_block_with(
    body: &[Statement],
    bound: &mut HashSet<String>,
    free: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    for stmt in body {
        collect_stmt(stmt, bound, free, seen);
    }
}

fn collect_match_arm(
    arm: &MatchArm,
    bound: &HashSet<String>,
    free: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    let mut inner = bound.clone();
    bind_pattern(&arm.pattern, &mut inner);
    collect_block_with(&arm.body, &mut inner, free, seen);
}

fn bind_pattern(pat: &Pattern, bound: &mut HashSet<String>) {
    match pat {
        Pattern::Bind(name) => {
            bound.insert(name.clone());
        }
        Pattern::Variant { fields, .. } => {
            for f in fields {
                bind_pattern(f, bound);
            }
        }
        _ => {}
    }
}

fn collect_expr(
    expr: &Expr,
    bound: &HashSet<String>,
    free: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    match expr {
        Expr::Ident(name) => add_free(name, bound, free, seen),
        Expr::BinOp(l, _, r) => {
            collect_expr(l, bound, free, seen);
            collect_expr(r, bound, free, seen);
        }
        Expr::UnaryOp(_, e) => collect_expr(e, bound, free, seen),
        Expr::Call(callee, args) => {
            collect_expr(callee, bound, free, seen);
            for a in args {
                collect_expr(a, bound, free, seen);
            }
        }
        Expr::NamedArg(_, e) => collect_expr(e, bound, free, seen),
        Expr::SpreadArg(e) | Expr::KwSpreadArg(e) => collect_expr(e, bound, free, seen),
        Expr::Index(b, i) => {
            collect_expr(b, bound, free, seen);
            collect_expr(i, bound, free, seen);
        }
        Expr::Slice(b, s, e, st) => {
            collect_expr(b, bound, free, seen);
            for part in [s, e, st].into_iter().flatten() {
                collect_expr(part, bound, free, seen);
            }
        }
        Expr::Member(b, _) => collect_expr(b, bound, free, seen),
        Expr::List(items) => {
            for it in items {
                collect_expr(it, bound, free, seen);
            }
        }
        Expr::Dict(entries) => {
            for (k, v) in entries {
                collect_expr(k, bound, free, seen);
                collect_expr(v, bound, free, seen);
            }
        }
        Expr::Tuple(items) => {
            for it in items {
                collect_expr(it, bound, free, seen);
            }
        }
        Expr::Spawn(name, args) => {
            add_free(name, bound, free, seen);
            for a in args {
                collect_expr(a, bound, free, seen);
            }
        }
        Expr::Recv(channel) => add_free(channel, bound, free, seen),
        Expr::Await(e) => collect_expr(e, bound, free, seen),
        Expr::AwaitAll(exprs) => {
            for e in exprs {
                collect_expr(e, bound, free, seen);
            }
        }
        Expr::ListComprehension {
            expr,
            vars,
            iter,
            filter,
        } => {
            collect_expr(iter, bound, free, seen);
            let mut inner = bound.clone();
            for v in vars {
                inner.insert(v.clone());
            }
            collect_expr(expr, &inner, free, seen);
            if let Some(f) = filter {
                collect_expr(f, &inner, free, seen);
            }
        }
        Expr::Closure { params, body, .. } => {
            // 嵌套闭包：其自身参数在其体内绑定；其捕获到的外层变量
            // 对当前闭包来说同样是自由变量，需要透传上去。
            let mut inner = bound.clone();
            for p in params {
                inner.insert(p.name.clone());
            }
            for stmt in body {
                collect_stmt(stmt, &mut inner, free, seen);
            }
        }
        // 字面量等无变量引用
        _ => {}
    }
}
