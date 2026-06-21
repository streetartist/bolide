//! 内联展开 pass
//!
//! 在单态化之后、后端编译之前，将对 `inline fn` 的调用在 AST 层展开为函数体。
//!
//! 方式：
//!   - 形参直接替换为实参表达式
//!   - 函数体内的 let 变量逐层展开，最终合并进 return 表达式
//!   - inline fn 定义不进入最终程序运行时代码
//!
//! 限制：
//!   - 仅支持 `let 绑定 + return 表达式` 模式（不含 if / while / 递归）

use bolide_parser::{Assign, Expr, FuncDef, Program, Statement, VarDecl};
use std::collections::HashMap;

pub fn inline_expand(program: Program) -> Result<Program, String> {
    let mut inline_defs = HashMap::new();
    for stmt in &program.statements {
        if let Statement::FuncDef(func) = stmt {
            if func.is_inline {
                inline_defs.insert(func.name.clone(), func.clone());
            }
        }
    }
    let inliner = Inliner { inline_defs };
    inliner.run(program)
}

struct Inliner {
    inline_defs: HashMap<String, FuncDef>,
}

impl Inliner {
    fn run(&self, program: Program) -> Result<Program, String> {
        let mut new_stmts = Vec::new();
        for stmt in program.statements {
            match stmt {
                Statement::FuncDef(func) if func.is_inline => {
                    // 内联函数已展开到所有调用点，不保留
                }
                Statement::FuncDef(func) => {
                    new_stmts.push(Statement::FuncDef(self.expand_function(func)?));
                }
                other => {
                    new_stmts.push(self.expand_stmt_in_place(other)?);
                }
            }
        }
        Ok(Program {
            statements: new_stmts,
        })
    }

    fn expand_function(&self, func: FuncDef) -> Result<FuncDef, String> {
        let body = self.expand_stmts(func.body)?;
        Ok(FuncDef { body, ..func })
    }

    fn expand_stmts(&self, stmts: Vec<Statement>) -> Result<Vec<Statement>, String> {
        stmts.into_iter().map(|s| self.expand_statement(s)).collect()
    }

    fn expand_statement(&self, stmt: Statement) -> Result<Statement, String> {
        match stmt {
            Statement::Expr(e) => Ok(Statement::Expr(self.expand_expr(e)?)),
            Statement::Return(Some(e)) => Ok(Statement::Return(Some(self.expand_expr(e)?))),
            Statement::Return(None) => Ok(Statement::Return(None)),
            Statement::VarDecl(decl) => {
                let value = match decl.value {
                    Some(v) => Some(self.expand_expr(v)?),
                    None => None,
                };
                Ok(Statement::VarDecl(VarDecl { value, ..decl }))
            }
            Statement::Assign(assign) => {
                Ok(Statement::Assign(Assign {
                    target: assign.target,
                    value: self.expand_expr(assign.value)?,
                }))
            }
            Statement::If(if_stmt) => {
                let cond = self.expand_expr(if_stmt.condition)?;
                let then_body = self.expand_stmts(if_stmt.then_body)?;
                let elifs = if_stmt
                    .elif_branches
                    .into_iter()
                    .map(|(c, b)| {
                        Ok((self.expand_expr(c)?, self.expand_stmts(b)?))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let else_body = if_stmt.else_body.map(|b| self.expand_stmts(b)).transpose()?;
                Ok(Statement::If(bolide_parser::IfStmt {
                    condition: cond,
                    then_body,
                    elif_branches: elifs,
                    else_body,
                }))
            }
            Statement::While(ws) => {
                let cond = self.expand_expr(ws.condition)?;
                let body = self.expand_stmts(ws.body)?;
                Ok(Statement::While(bolide_parser::WhileStmt {
                    condition: cond,
                    body,
                }))
            }
            other => Ok(other),
        }
    }

    fn expand_expr(&self, expr: Expr) -> Result<Expr, String> {
        match expr {
            Expr::Call(callee, args) => {
                // 先递归展开子表达式
                let callee = self.expand_expr(*callee)?;
                let args: Result<Vec<_>, _> = args
                    .into_iter()
                    .map(|a| self.expand_expr(a))
                    .collect();
                let args = args?;

                if let Expr::Ident(name) = &callee {
                    if let Some(inline_func) = self.inline_defs.get(name) {
                        return self.inline_call(&args, inline_func);
                    }
                }

                Ok(Expr::Call(Box::new(callee), args))
            }
            Expr::Member(base, member) => {
                Ok(Expr::Member(
                    Box::new(self.expand_expr(*base)?),
                    member,
                ))
            }
            Expr::BinOp(l, op, r) => Ok(Expr::BinOp(
                Box::new(self.expand_expr(*l)?),
                op,
                Box::new(self.expand_expr(*r)?),
            )),
            Expr::UnaryOp(op, operand) => {
                Ok(Expr::UnaryOp(op, Box::new(self.expand_expr(*operand)?)))
            }
            Expr::Index(base, idx) => Ok(Expr::Index(
                Box::new(self.expand_expr(*base)?),
                Box::new(self.expand_expr(*idx)?),
            )),
            Expr::List(items) => {
                let mut new_items = Vec::new();
                for i in items {
                    new_items.push(self.expand_expr(i)?);
                }
                Ok(Expr::List(new_items))
            }
            Expr::Tuple(items) => {
                let mut new_items = Vec::new();
                for i in items {
                    new_items.push(self.expand_expr(i)?);
                }
                Ok(Expr::Tuple(new_items))
            }
            Expr::Dict(entries) => {
                let mut new_entries = Vec::new();
                for (k, v) in entries {
                    new_entries.push((self.expand_expr(k)?, self.expand_expr(v)?));
                }
                Ok(Expr::Dict(new_entries))
            }
            Expr::ValueConstruct(name, fields) => {
                let mut new_fields = Vec::new();
                for (n, e) in fields {
                    new_fields.push((n, self.expand_expr(e)?));
                }
                Ok(Expr::ValueConstruct(name, new_fields))
            }
            _ => Ok(expr),
        }
    }

    /// 展开 inline 调用。直接替换形参 → 实参，let 变量展开到 return 表达式中
    fn inline_call(&self, args: &[Expr], func: &FuncDef) -> Result<Expr, String> {
        let last_idx = func.body.len().saturating_sub(1);
        if let Statement::Return(Some(ret_expr)) = &func.body[last_idx] {
            // 1) 形参 → 实参映射
            let mut param_map = HashMap::new();
            for (i, param) in func.params.iter().enumerate() {
                if i < args.len() {
                    param_map.insert(param.name.clone(), args[i].clone());
                }
            }
            // 2) 先对 inline 函数体内的局部 let 做 alpha rename，避免捕获调用点中的同名标识符
            let mut local_rename = HashMap::new();
            if last_idx > 0 {
                for (index, stmt) in func.body[..last_idx].iter().enumerate() {
                    if let Statement::VarDecl(decl) = stmt {
                        local_rename.insert(
                            decl.name.clone(),
                            Expr::Ident(format!("__inline_{}_{}_{}", func.name, index, decl.name)),
                        );
                    }
                }
            }

            // 3) 展开 return 中的局部名和形参
            let renamed_ret = substitute_expr(ret_expr, &local_rename);
            let mut result = substitute_expr(&renamed_ret, &param_map);
            // 4) 收集 return 前的 let 绑定，逐层内联进 return
            if last_idx > 0 {
                let mut let_vals: Vec<(String, Expr)> = Vec::new();
                for s in &func.body[..last_idx] {
                    if let Statement::VarDecl(decl) = s {
                        if let Some(ref val) = decl.value {
                            let renamed_val = substitute_expr(val, &local_rename);
                            let local_name = match local_rename.get(&decl.name) {
                                Some(Expr::Ident(name)) => name.clone(),
                                _ => decl.name.clone(),
                            };
                            let_vals.push((local_name, substitute_expr(&renamed_val, &param_map)));
                        }
                    }
                }
                // 反向替换（后定义的先展开）
                for (name, val) in let_vals.into_iter().rev() {
                    let mut local = HashMap::new();
                    local.insert(name, val);
                    result = substitute_expr(&result, &local);
                }
            }
            // 5) 递归展开嵌套的 inline 调用
            self.expand_expr(result)
        } else {
            Err(format!(
                "inline function '{}' must end with a single return expression",
                func.name
            ))
        }
    }

    fn expand_stmt_in_place(&self, stmt: Statement) -> Result<Statement, String> {
        match stmt {
            Statement::Expr(e) => Ok(Statement::Expr(self.expand_expr(e)?)),
            Statement::VarDecl(decl) => {
                let value = match decl.value {
                    Some(v) => Some(self.expand_expr(v)?),
                    None => None,
                };
                Ok(Statement::VarDecl(VarDecl { value, ..decl }))
            }
            other => Ok(other),
        }
    }
}

/// AST 表达式中的变量替换（纯函数，不依赖 self）
fn substitute_expr(expr: &Expr, subst: &HashMap<String, Expr>) -> Expr {
    match expr {
        Expr::Ident(name) => subst.get(name).cloned().unwrap_or_else(|| expr.clone()),
        Expr::Member(base, member) => Expr::Member(
            Box::new(substitute_expr(base, subst)),
            member.clone(),
        ),
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
        Expr::Tuple(items) => Expr::Tuple(
            items.iter().map(|i| substitute_expr(i, subst)).collect(),
        ),
        Expr::List(items) => Expr::List(
            items.iter().map(|i| substitute_expr(i, subst)).collect(),
        ),
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
            let start: Option<Box<Expr>> = match start {
                Some(e) => Some(Box::new(substitute_expr(e, subst))),
                None => None,
            };
            let end: Option<Box<Expr>> = match end {
                Some(e) => Some(Box::new(substitute_expr(e, subst))),
                None => None,
            };
            let step: Option<Box<Expr>> = match step {
                Some(e) => Some(Box::new(substitute_expr(e, subst))),
                None => None,
            };
            Expr::Slice(base, start, end, step)
        }
        _ => expr.clone(),
    }
}