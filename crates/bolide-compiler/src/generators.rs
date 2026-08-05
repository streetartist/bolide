//! 懒生成器（yield）
//!
//! 含 `yield` 的函数/方法脱糖为迭代器类：
//! - class `__Gen_<name>`：`__st` + 参数/局部 +（方法时）`__owner`
//! - `next() -> Option<T>` 状态机
//! - 原函数变为构造器
//!
//! 支持：while / if-elif-else / for(range|list) / break / continue / 类方法生成器

use bolide_parser::{
    BinOp, ClassDef, ClassField, Expr, FuncDef, Program, Statement, Type,
};
use std::collections::{HashMap, HashSet};

const ST: &str = "__st";
const OWNER: &str = "__owner";
const DONE: i64 = -1;

/// 将程序中所有生成器函数脱糖为懒迭代器类 + 构造函数。
pub fn desugar_generators(program: Program) -> Result<Program, String> {
    let mut statements = Vec::new();
    for stmt in program.statements {
        statements.extend(desugar_stmt(stmt)?);
    }
    Ok(Program { statements })
}

fn desugar_stmt(stmt: Statement) -> Result<Vec<Statement>, String> {
    match stmt {
        Statement::FuncDef(f) => desugar_func_item(f, None),
        Statement::ClassDef(mut c) => {
            let class_name = c.name.clone();
            let mut methods = Vec::new();
            let mut hoisted = Vec::new();
            for m in std::mem::take(&mut c.methods) {
                let parts = desugar_func_item(m, Some(&class_name))?;
                for p in parts {
                    match p {
                        Statement::FuncDef(fd) => methods.push(fd),
                        Statement::ClassDef(gen_class) => {
                            hoisted.push(Statement::ClassDef(gen_class));
                        }
                        other => {
                            return Err(format!(
                                "unexpected statement from generator method desugar: {:?}",
                                std::mem::discriminant(&other)
                            ));
                        }
                    }
                }
            }
            c.methods = methods;
            let mut out = hoisted;
            out.push(Statement::ClassDef(c));
            Ok(out)
        }
        Statement::Yield(_) => Err(
            "`yield` is only allowed inside a function body (generator function)".to_string(),
        ),
        other => Ok(vec![other]),
    }
}

/// `owner_class`：若为 class 方法生成器，传入所属类名
fn desugar_func_item(
    mut f: FuncDef,
    owner_class: Option<&str>,
) -> Result<Vec<Statement>, String> {
    f.body = map_nested_non_gen(f.body)?;

    if !body_contains_yield(&f.body) {
        return Ok(vec![Statement::FuncDef(f)]);
    }

    let elem_ty = infer_yield_elem_type(&f)?;
    let gen_class_name = match owner_class {
        Some(cls) => format!("__Gen_{}_{}", cls, f.name),
        None => format!("__Gen_{}", f.name),
    };

    // 局部变量（不含参数；方法的 self 不是显式参数）
    let mut locals = collect_locals(&f.body);
    for p in &f.params {
        locals.remove(&p.name);
    }
    // for 循环变量 + 列表 for 的临时字段
    collect_for_vars(&f.body, &mut locals);
    collect_for_temps(&f.body, &mut locals);

    let mut fields: Vec<ClassField> = vec![ClassField {
        name: ST.to_string(),
        ty: Type::Int,
        default_value: Some(Expr::Int(0)),
    }];

    if let Some(cls) = owner_class {
        fields.push(ClassField {
            name: OWNER.to_string(),
            ty: Type::Custom(cls.to_string()),
            default_value: None,
        });
    }

    for p in &f.params {
        fields.push(ClassField {
            name: p.name.clone(),
            ty: p.ty.clone(),
            default_value: None,
        });
    }

    let mut local_names: Vec<(String, Type)> = locals.into_iter().collect();
    local_names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, ty) in &local_names {
        fields.push(ClassField {
            name: name.clone(),
            ty: ty.clone(),
            default_value: Some(zero_expr(ty)),
        });
    }

    let field_set: HashSet<String> = fields.iter().map(|f| f.name.clone()).collect();
    let mut builder = StateBuilder::new(field_set, owner_class.is_some());
    transform_block(&f.body, &mut builder)?;
    // 若末状态已 return/break 出去，勿再写入死代码（会导致 Cranelift verifier 失败）
    if !builder.is_terminated() {
        builder.emit_assign_st(DONE);
        builder.emit(Statement::Return(Some(option_none())));
    }

    let next_body = builder.finish_next_method()?;

    let next_method = FuncDef {
        name: "next".to_string(),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params: vec![],
        throws: vec![],
        return_type: Some(Type::Adt("Option".to_string(), vec![elem_ty.clone()])),
        lifetime_deps: None,
        body: next_body,
        def_span_start: None,
        attrs: vec![],
    };

    let gen_class = ClassDef {
        name: gen_class_name.clone(),
        parent: None,
        mixins: vec![],
        fields: fields.clone(),
        methods: vec![next_method],
        attrs: vec![],
        impl_traits: vec![],
    };

    // 构造参数：__st=0, [__owner=self], params..., local zeros
    let mut ctor_args = vec![Expr::Int(0)];
    if owner_class.is_some() {
        ctor_args.push(Expr::Ident("self".to_string()));
    }
    for p in &f.params {
        ctor_args.push(Expr::Ident(p.name.clone()));
    }
    for (_, ty) in &local_names {
        ctor_args.push(zero_expr(ty));
    }

    let ctor = FuncDef {
        name: f.name,
        is_async: false,
        is_export: f.is_export,
        is_inline: false,
        type_params: f.type_params,
        trait_bounds: f.trait_bounds,
        params: f.params,
        throws: f.throws,
        return_type: Some(Type::Custom(gen_class_name.clone())),
        lifetime_deps: None,
        body: vec![Statement::Return(Some(Expr::Call(
            Box::new(Expr::Ident(gen_class_name)),
            ctor_args,
        )))],
        def_span_start: f.def_span_start,
        attrs: vec![],
    };

    Ok(vec![
        Statement::ClassDef(gen_class),
        Statement::FuncDef(ctor),
    ])
}

/// 嵌套非生成器函数原样保留；嵌套生成器暂拆到同层（少见）
fn map_nested_non_gen(stmts: Vec<Statement>) -> Result<Vec<Statement>, String> {
    let mut out = Vec::new();
    for s in stmts {
        match s {
            Statement::FuncDef(f) if body_contains_yield(&f.body) => {
                return Err(
                    "nested generator functions are not supported; define them at top level"
                        .to_string(),
                );
            }
            Statement::If(mut i) => {
                i.then_body = map_nested_non_gen(i.then_body)?;
                i.elif_branches = i
                    .elif_branches
                    .into_iter()
                    .map(|(c, b)| Ok((c, map_nested_non_gen(b)?)))
                    .collect::<Result<_, String>>()?;
                if let Some(eb) = i.else_body {
                    i.else_body = Some(map_nested_non_gen(eb)?);
                }
                out.push(Statement::If(i));
            }
            Statement::While(mut w) => {
                w.body = map_nested_non_gen(w.body)?;
                out.push(Statement::While(w));
            }
            Statement::For(mut f) => {
                f.body = map_nested_non_gen(f.body)?;
                out.push(Statement::For(f));
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

// ---------- 状态机构建 ----------

struct StateBuilder {
    states: Vec<Vec<Statement>>,
    current: usize,
    field_set: HashSet<String>,
    /// (continue_target / loop head, break_target / after)
    loop_stack: Vec<(usize, usize)>,
    has_owner: bool,
    /// 当前状态已 return / 跳转离开，禁止再写 fallthrough
    terminated: HashSet<usize>,
}

impl StateBuilder {
    fn new(field_set: HashSet<String>, has_owner: bool) -> Self {
        Self {
            states: vec![Vec::new()],
            current: 0,
            field_set,
            loop_stack: Vec::new(),
            has_owner,
            terminated: HashSet::new(),
        }
    }

    fn is_terminated(&self) -> bool {
        self.terminated.contains(&self.current)
    }

    fn mark_terminated(&mut self) {
        self.terminated.insert(self.current);
    }

    fn emit(&mut self, stmt: Statement) {
        if self.is_terminated() {
            return;
        }
        self.states[self.current].push(stmt);
    }

    fn emit_goto(&mut self, st: usize) {
        self.emit_assign_st(st as i64);
    }

    /// 仅在当前状态仍可 fallthrough 时跳转
    fn emit_goto_if_live(&mut self, st: usize) {
        if !self.is_terminated() {
            self.emit_goto(st);
        }
    }

    fn emit_assign_st(&mut self, st: i64) {
        self.emit(Statement::Assign(bolide_parser::Assign {
            target: Expr::Member(Box::new(Expr::Ident("self".into())), ST.to_string()),
            value: Expr::Int(st),
        }));
    }

    fn new_state(&mut self) -> usize {
        let id = self.states.len();
        self.states.push(Vec::new());
        id
    }

    fn switch_to(&mut self, id: usize) {
        self.current = id;
    }

    fn rewrite_expr(&self, expr: Expr) -> Expr {
        rewrite_expr_fields(expr, &self.field_set, self.has_owner)
    }

    fn finish_next_method(self) -> Result<Vec<Statement>, String> {
        let mut arms = Vec::new();
        for (i, stmts) in self.states.into_iter().enumerate() {
            let cond = Expr::BinOp(
                Box::new(Expr::Member(
                    Box::new(Expr::Ident("self".into())),
                    ST.to_string(),
                )),
                BinOp::Eq,
                Box::new(Expr::Int(i as i64)),
            );
            arms.push((cond, stmts));
        }
        if arms.is_empty() {
            return Ok(vec![Statement::Return(Some(option_none()))]);
        }
        let (first_cond, first_body) = arms.remove(0);
        let if_stmt = Statement::If(bolide_parser::IfStmt {
            condition: first_cond,
            then_body: first_body,
            elif_branches: arms,
            else_body: Some(vec![Statement::Return(Some(option_none()))]),
        });
        Ok(vec![Statement::While(bolide_parser::WhileStmt {
            condition: Expr::Bool(true),
            body: vec![if_stmt],
        })])
    }
}

fn transform_block(stmts: &[Statement], b: &mut StateBuilder) -> Result<(), String> {
    for s in stmts {
        if b.is_terminated() {
            break;
        }
        transform_stmt(s, b)?;
    }
    Ok(())
}

fn transform_stmt(stmt: &Statement, b: &mut StateBuilder) -> Result<(), String> {
    // 纯顺序语句（无 yield / break / continue / bare return）整块改写后发射
    if !stmt_needs_state_machine(stmt) {
        if let Some(s) = rewrite_stmt_fields(stmt.clone(), &b.field_set, b.has_owner) {
            b.emit(s);
            return Ok(());
        }
    }

    match stmt {
        Statement::Yield(e) => {
            let resume = b.new_state();
            b.emit_goto(resume);
            let val = b.rewrite_expr(e.clone());
            b.emit(Statement::Return(Some(option_some(val))));
            b.mark_terminated();
            b.switch_to(resume);
            Ok(())
        }
        Statement::VarDecl(d) => {
            if let Some(v) = &d.value {
                let v = b.rewrite_expr(v.clone());
                b.emit(Statement::Assign(bolide_parser::Assign {
                    target: Expr::Member(Box::new(Expr::Ident("self".into())), d.name.clone()),
                    value: v,
                }));
            }
            Ok(())
        }
        Statement::Assign(a) => {
            b.emit(Statement::Assign(bolide_parser::Assign {
                target: b.rewrite_expr(a.target.clone()),
                value: b.rewrite_expr(a.value.clone()),
            }));
            Ok(())
        }
        Statement::Expr(e) => {
            b.emit(Statement::Expr(b.rewrite_expr(e.clone())));
            Ok(())
        }
        Statement::Return(None) => {
            b.emit_assign_st(DONE);
            b.emit(Statement::Return(Some(option_none())));
            b.mark_terminated();
            Ok(())
        }
        Statement::Return(Some(_)) => Err(
            "generator functions cannot `return` a value; use `yield` and bare `return` to finish"
                .to_string(),
        ),
        Statement::Break => {
            let (_, after) = b
                .loop_stack
                .last()
                .copied()
                .ok_or("`break` outside of a loop in generator")?;
            b.emit_goto(after);
            // 跳出循环后由外层 while 按 __st 再分发；本状态不再 fallthrough
            b.mark_terminated();
            Ok(())
        }
        Statement::Continue => {
            let (head, _) = b
                .loop_stack
                .last()
                .copied()
                .ok_or("`continue` outside of a loop in generator")?;
            b.emit_goto(head);
            b.mark_terminated();
            Ok(())
        }
        Statement::While(w) => transform_while(&w.condition, &w.body, b),
        Statement::If(i) => transform_if(i, b),
        Statement::For(f) => transform_for(f, b),
        Statement::Throw(e) => {
            b.emit(Statement::Throw(b.rewrite_expr(e.clone())));
            b.mark_terminated();
            Ok(())
        }
        Statement::FuncDef(_) => {
            Err("nested functions inside generators are not supported".into())
        }
        other => Err(format!(
            "statement not yet supported in generators: {:?}",
            std::mem::discriminant(other)
        )),
    }
}

fn transform_while(
    cond: &Expr,
    body: &[Statement],
    b: &mut StateBuilder,
) -> Result<(), String> {
    let head = b.new_state();
    let body_s = b.new_state();
    let after = b.new_state();
    b.emit_goto(head);
    b.switch_to(head);
    let cond = b.rewrite_expr(cond.clone());
    b.emit(Statement::If(bolide_parser::IfStmt {
        condition: cond,
        then_body: vec![goto_stmt(body_s)],
        elif_branches: vec![],
        else_body: Some(vec![goto_stmt(after)]),
    }));
    b.switch_to(body_s);
    b.loop_stack.push((head, after));
    transform_block(body, b)?;
    b.loop_stack.pop();
    // body 若 break/continue/return，不再回跳 head
    b.emit_goto_if_live(head);
    b.switch_to(after);
    Ok(())
}

fn transform_if(i: &bolide_parser::IfStmt, b: &mut StateBuilder) -> Result<(), String> {
    // 将 if/elif/else 归一为嵌套 if-else，再状态化
    let normalized = normalize_if_elif(i);
    transform_if_simple(&normalized.0, &normalized.1, normalized.2.as_deref(), b)
}

/// (cond, then, else_opt)
fn normalize_if_elif(i: &bolide_parser::IfStmt) -> (Expr, Vec<Statement>, Option<Vec<Statement>>) {
    if i.elif_branches.is_empty() {
        return (
            i.condition.clone(),
            i.then_body.clone(),
            i.else_body.clone(),
        );
    }
    // if c0 { t0 } elif c1 { t1 } elif c2 { t2 } else { e }
    // → if c0 { t0 } else { if c1 { t1 } else { if c2 { t2 } else { e } } }
    let mut else_chain = i.else_body.clone();
    for (cond, body) in i.elif_branches.iter().rev() {
        else_chain = Some(vec![Statement::If(bolide_parser::IfStmt {
            condition: cond.clone(),
            then_body: body.clone(),
            elif_branches: vec![],
            else_body: else_chain,
        })]);
    }
    (
        i.condition.clone(),
        i.then_body.clone(),
        else_chain,
    )
}

fn transform_if_simple(
    cond: &Expr,
    then_body: &[Statement],
    else_body: Option<&[Statement]>,
    b: &mut StateBuilder,
) -> Result<(), String> {
    let then_s = b.new_state();
    let else_s = b.new_state();
    let join = b.new_state();
    let cond = b.rewrite_expr(cond.clone());
    b.emit(Statement::If(bolide_parser::IfStmt {
        condition: cond,
        then_body: vec![goto_stmt(then_s)],
        elif_branches: vec![],
        else_body: Some(vec![goto_stmt(else_s)]),
    }));
    b.switch_to(then_s);
    transform_block(then_body, b)?;
    b.emit_goto_if_live(join);
    b.switch_to(else_s);
    if let Some(eb) = else_body {
        transform_block(eb, b)?;
    }
    b.emit_goto_if_live(join);
    b.switch_to(join);
    Ok(())
}

fn transform_for(f: &bolide_parser::ForStmt, b: &mut StateBuilder) -> Result<(), String> {
    // for 的 continue 必须落到「递增」状态，不能直接回条件（否则跳过步进死循环）
    if let Expr::Call(callee, args) = &f.iter {
        if let Expr::Ident(name) = callee.as_ref() {
            if name == "range" {
                return transform_for_range(&f.vars, args, &f.body, b);
            }
        }
    }
    if f.vars.len() != 1 {
        return Err(
            "generator for-loops over lists support a single variable".into(),
        );
    }
    transform_for_list(&f.vars[0], &f.iter, &f.body, b)
}

/// for i in range(...)：init → head? → body → incr → head；continue → incr
fn transform_for_range(
    vars: &[String],
    args: &[Expr],
    body: &[Statement],
    b: &mut StateBuilder,
) -> Result<(), String> {
    if vars.len() != 1 {
        return Err("range for-loop needs a single variable".into());
    }
    let var = &vars[0];
    let (start, end, step) = match args.len() {
        1 => (Expr::Int(0), b.rewrite_expr(args[0].clone()), Expr::Int(1)),
        2 => (
            b.rewrite_expr(args[0].clone()),
            b.rewrite_expr(args[1].clone()),
            Expr::Int(1),
        ),
        3 => (
            b.rewrite_expr(args[0].clone()),
            b.rewrite_expr(args[1].clone()),
            b.rewrite_expr(args[2].clone()),
        ),
        _ => return Err("range() expects 1, 2, or 3 arguments".into()),
    };

    // init
    b.emit(Statement::Assign(bolide_parser::Assign {
        target: Expr::Member(Box::new(Expr::Ident("self".into())), var.clone()),
        value: start,
    }));

    let head = b.new_state();
    let body_s = b.new_state();
    let incr = b.new_state();
    let after = b.new_state();

    b.emit_goto(head);
    b.switch_to(head);
    let cond = Expr::BinOp(
        Box::new(Expr::Member(
            Box::new(Expr::Ident("self".into())),
            var.clone(),
        )),
        BinOp::Lt,
        Box::new(end),
    );
    b.emit(Statement::If(bolide_parser::IfStmt {
        condition: cond,
        then_body: vec![goto_stmt(body_s)],
        elif_branches: vec![],
        else_body: Some(vec![goto_stmt(after)]),
    }));

    b.switch_to(body_s);
    // continue → incr（先步进再判条件）
    b.loop_stack.push((incr, after));
    transform_block(body, b)?;
    b.loop_stack.pop();
    b.emit_goto_if_live(incr);

    b.switch_to(incr);
    b.emit(Statement::Assign(bolide_parser::Assign {
        target: Expr::Member(Box::new(Expr::Ident("self".into())), var.clone()),
        value: Expr::BinOp(
            Box::new(Expr::Member(
                Box::new(Expr::Ident("self".into())),
                var.clone(),
            )),
            BinOp::Add,
            Box::new(step),
        ),
    }));
    b.emit_goto(head);

    b.switch_to(after);
    Ok(())
}

/// for x in list：存列表 + 下标，continue → 下标++
fn transform_for_list(
    var: &str,
    iter: &Expr,
    body: &[Statement],
    b: &mut StateBuilder,
) -> Result<(), String> {
    let idx = format!("__fi_{}", var);
    let iter_name = format!("__fl_{}", var);
    b.field_set.insert(idx.clone());
    b.field_set.insert(iter_name.clone());

    let iter_e = b.rewrite_expr(iter.clone());
    b.emit(Statement::Assign(bolide_parser::Assign {
        target: Expr::Member(Box::new(Expr::Ident("self".into())), iter_name.clone()),
        value: iter_e,
    }));
    b.emit(Statement::Assign(bolide_parser::Assign {
        target: Expr::Member(Box::new(Expr::Ident("self".into())), idx.clone()),
        value: Expr::Int(0),
    }));

    let head = b.new_state();
    let body_s = b.new_state();
    let incr = b.new_state();
    let after = b.new_state();

    b.emit_goto(head);
    b.switch_to(head);
    let len_call = Expr::Call(
        Box::new(Expr::Member(
            Box::new(Expr::Member(
                Box::new(Expr::Ident("self".into())),
                iter_name.clone(),
            )),
            "len".to_string(),
        )),
        vec![],
    );
    let cond = Expr::BinOp(
        Box::new(Expr::Member(
            Box::new(Expr::Ident("self".into())),
            idx.clone(),
        )),
        BinOp::Lt,
        Box::new(len_call),
    );
    b.emit(Statement::If(bolide_parser::IfStmt {
        condition: cond,
        then_body: vec![goto_stmt(body_s)],
        elif_branches: vec![],
        else_body: Some(vec![goto_stmt(after)]),
    }));

    b.switch_to(body_s);
    // x = list[idx]
    let get_elem = Expr::Index(
        Box::new(Expr::Member(
            Box::new(Expr::Ident("self".into())),
            iter_name,
        )),
        Box::new(Expr::Member(
            Box::new(Expr::Ident("self".into())),
            idx.clone(),
        )),
    );
    b.emit(Statement::Assign(bolide_parser::Assign {
        target: Expr::Member(Box::new(Expr::Ident("self".into())), var.to_string()),
        value: get_elem,
    }));
    b.loop_stack.push((incr, after));
    transform_block(body, b)?;
    b.loop_stack.pop();
    b.emit_goto_if_live(incr);

    b.switch_to(incr);
    b.emit(Statement::Assign(bolide_parser::Assign {
        target: Expr::Member(Box::new(Expr::Ident("self".into())), idx.clone()),
        value: Expr::BinOp(
            Box::new(Expr::Member(
                Box::new(Expr::Ident("self".into())),
                idx,
            )),
            BinOp::Add,
            Box::new(Expr::Int(1)),
        ),
    }));
    b.emit_goto(head);

    b.switch_to(after);
    Ok(())
}

fn goto_stmt(st: usize) -> Statement {
    Statement::Assign(bolide_parser::Assign {
        target: Expr::Member(Box::new(Expr::Ident("self".into())), ST.to_string()),
        value: Expr::Int(st as i64),
    })
}

// ---------- 表达式/语句改写 ----------

fn rewrite_expr_fields(expr: Expr, fields: &HashSet<String>, has_owner: bool) -> Expr {
    match expr {
        Expr::Ident(name) if name == "self" && has_owner => {
            // 原 class 的 self → 生成器的 __owner
            Expr::Member(Box::new(Expr::Ident("self".into())), OWNER.to_string())
        }
        Expr::Ident(name) if fields.contains(&name) && name != "self" => {
            Expr::Member(Box::new(Expr::Ident("self".into())), name)
        }
        Expr::Member(base, m) => {
            let base = rewrite_expr_fields(*base, fields, has_owner);
            Expr::Member(Box::new(base), m)
        }
        Expr::BinOp(l, op, r) => Expr::BinOp(
            Box::new(rewrite_expr_fields(*l, fields, has_owner)),
            op,
            Box::new(rewrite_expr_fields(*r, fields, has_owner)),
        ),
        Expr::UnaryOp(op, e) => {
            Expr::UnaryOp(op, Box::new(rewrite_expr_fields(*e, fields, has_owner)))
        }
        Expr::Call(c, args) => Expr::Call(
            Box::new(rewrite_expr_fields(*c, fields, has_owner)),
            args.into_iter()
                .map(|a| rewrite_expr_fields(a, fields, has_owner))
                .collect(),
        ),
        Expr::Index(b, i) => Expr::Index(
            Box::new(rewrite_expr_fields(*b, fields, has_owner)),
            Box::new(rewrite_expr_fields(*i, fields, has_owner)),
        ),
        Expr::List(xs) => Expr::List(
            xs.into_iter()
                .map(|x| rewrite_expr_fields(x, fields, has_owner))
                .collect(),
        ),
        other => other,
    }
}

fn rewrite_stmt_fields(
    stmt: Statement,
    fields: &HashSet<String>,
    has_owner: bool,
) -> Option<Statement> {
    Some(match stmt {
        Statement::VarDecl(d) => Statement::Assign(bolide_parser::Assign {
            target: Expr::Member(Box::new(Expr::Ident("self".into())), d.name),
            value: d
                .value
                .map(|v| rewrite_expr_fields(v, fields, has_owner))
                .unwrap_or(Expr::Int(0)),
        }),
        Statement::Assign(a) => Statement::Assign(bolide_parser::Assign {
            target: rewrite_expr_fields(a.target, fields, has_owner),
            value: rewrite_expr_fields(a.value, fields, has_owner),
        }),
        Statement::Expr(e) => Statement::Expr(rewrite_expr_fields(e, fields, has_owner)),
        Statement::Throw(e) => Statement::Throw(rewrite_expr_fields(e, fields, has_owner)),
        Statement::Return(e) => {
            Statement::Return(e.map(|x| rewrite_expr_fields(x, fields, has_owner)))
        }
        Statement::If(mut i) => {
            i.condition = rewrite_expr_fields(i.condition, fields, has_owner);
            i.then_body = i
                .then_body
                .into_iter()
                .filter_map(|s| rewrite_stmt_fields(s, fields, has_owner))
                .collect();
            i.elif_branches = i
                .elif_branches
                .into_iter()
                .map(|(c, b)| {
                    (
                        rewrite_expr_fields(c, fields, has_owner),
                        b.into_iter()
                            .filter_map(|s| rewrite_stmt_fields(s, fields, has_owner))
                            .collect(),
                    )
                })
                .collect();
            if let Some(eb) = i.else_body {
                i.else_body = Some(
                    eb.into_iter()
                        .filter_map(|s| rewrite_stmt_fields(s, fields, has_owner))
                        .collect(),
                );
            }
            Statement::If(i)
        }
        Statement::While(mut w) => {
            w.condition = rewrite_expr_fields(w.condition, fields, has_owner);
            w.body = w
                .body
                .into_iter()
                .filter_map(|s| rewrite_stmt_fields(s, fields, has_owner))
                .collect();
            Statement::While(w)
        }
        Statement::For(mut f) => {
            f.iter = rewrite_expr_fields(f.iter, fields, has_owner);
            f.body = f
                .body
                .into_iter()
                .filter_map(|s| rewrite_stmt_fields(s, fields, has_owner))
                .collect();
            Statement::For(f)
        }
        // break/continue/bare return 必须走状态机路径
        Statement::Break | Statement::Continue | Statement::Return(None) => return None,
        Statement::Return(Some(_)) => return None,
        other => other,
    })
}

/// 需要拆成状态机跳转的语句（不能整块原样塞进 next）
fn stmt_needs_state_machine(stmt: &Statement) -> bool {
    stmt_contains_yield(stmt)
        || stmt_contains_break_continue(stmt)
        || stmt_contains_bare_return(stmt)
}

fn stmt_contains_yield(stmt: &Statement) -> bool {
    match stmt {
        Statement::Yield(_) => true,
        Statement::If(i) => {
            body_contains_yield(&i.then_body)
                || i.elif_branches
                    .iter()
                    .any(|(_, b)| body_contains_yield(b))
                || i.else_body
                    .as_ref()
                    .map(|b| body_contains_yield(b))
                    .unwrap_or(false)
        }
        Statement::While(w) => body_contains_yield(&w.body),
        Statement::For(f) => body_contains_yield(&f.body),
        _ => false,
    }
}

fn stmt_contains_break_continue(stmt: &Statement) -> bool {
    match stmt {
        Statement::Break | Statement::Continue => true,
        Statement::If(i) => {
            body_contains_break_continue(&i.then_body)
                || i.elif_branches
                    .iter()
                    .any(|(_, b)| body_contains_break_continue(b))
                || i.else_body
                    .as_ref()
                    .map(|b| body_contains_break_continue(b))
                    .unwrap_or(false)
        }
        Statement::While(w) => body_contains_break_continue(&w.body),
        Statement::For(f) => body_contains_break_continue(&f.body),
        _ => false,
    }
}

fn body_contains_break_continue(stmts: &[Statement]) -> bool {
    stmts.iter().any(stmt_contains_break_continue)
}

/// bare `return` 必须变成 `return Option.None`，不能原样进 next()
fn stmt_contains_bare_return(stmt: &Statement) -> bool {
    match stmt {
        Statement::Return(None) => true,
        Statement::Return(Some(_)) => true,
        Statement::If(i) => {
            body_contains_bare_return(&i.then_body)
                || i.elif_branches
                    .iter()
                    .any(|(_, b)| body_contains_bare_return(b))
                || i.else_body
                    .as_ref()
                    .map(|b| body_contains_bare_return(b))
                    .unwrap_or(false)
        }
        Statement::While(w) => body_contains_bare_return(&w.body),
        Statement::For(f) => body_contains_bare_return(&f.body),
        _ => false,
    }
}

fn body_contains_bare_return(stmts: &[Statement]) -> bool {
    stmts.iter().any(stmt_contains_bare_return)
}

// ---------- helpers ----------

fn option_some(val: Expr) -> Expr {
    Expr::Call(
        Box::new(Expr::Member(
            Box::new(Expr::Ident("Option".into())),
            "Some".into(),
        )),
        vec![val],
    )
}

fn option_none() -> Expr {
    Expr::Call(
        Box::new(Expr::Member(
            Box::new(Expr::Ident("Option".into())),
            "None".into(),
        )),
        vec![],
    )
}

fn zero_expr(ty: &Type) -> Expr {
    match ty {
        Type::Int | Type::BigInt => Expr::Int(0),
        Type::Float | Type::Decimal => Expr::Float(0.0),
        Type::Bool => Expr::Bool(false),
        Type::Str => Expr::String(String::new()),
        Type::List(_) => Expr::List(vec![]),
        _ => Expr::Int(0),
    }
}

fn collect_locals(stmts: &[Statement]) -> HashMap<String, Type> {
    let mut map = HashMap::new();
    collect_locals_in(stmts, &mut map);
    map
}

fn collect_locals_in(stmts: &[Statement], map: &mut HashMap<String, Type>) {
    for s in stmts {
        match s {
            Statement::VarDecl(d) => {
                let ty = d
                    .ty
                    .clone()
                    .unwrap_or_else(|| d.value.as_ref().map(guess_expr_type).unwrap_or(Type::Int));
                map.insert(d.name.clone(), ty);
            }
            Statement::If(i) => {
                collect_locals_in(&i.then_body, map);
                for (_, b) in &i.elif_branches {
                    collect_locals_in(b, map);
                }
                if let Some(eb) = &i.else_body {
                    collect_locals_in(eb, map);
                }
            }
            Statement::While(w) => collect_locals_in(&w.body, map),
            Statement::For(f) => {
                for v in &f.vars {
                    map.entry(v.clone()).or_insert(Type::Int);
                }
                collect_locals_in(&f.body, map);
            }
            _ => {}
        }
    }
}

fn collect_for_vars(stmts: &[Statement], map: &mut HashMap<String, Type>) {
    collect_locals_in(stmts, map);
}

/// 列表 for 的临时字段：`__fi_<var>` 下标、`__fl_<var>` 被迭代列表
fn collect_for_temps(stmts: &[Statement], map: &mut HashMap<String, Type>) {
    for s in stmts {
        match s {
            Statement::For(f) => {
                if !is_range_call(&f.iter) && f.vars.len() == 1 {
                    let var = &f.vars[0];
                    let idx = format!("__fi_{}", var);
                    let iter_name = format!("__fl_{}", var);
                    map.entry(idx).or_insert(Type::Int);
                    let elem = map.get(var).cloned().unwrap_or_else(|| guess_iter_elem_type(&f.iter));
                    map.entry(iter_name)
                        .or_insert(Type::List(Box::new(elem)));
                    // for 变量本身
                    map.entry(var.clone())
                        .or_insert_with(|| guess_iter_elem_type(&f.iter));
                }
                collect_for_temps(&f.body, map);
            }
            Statement::If(i) => {
                collect_for_temps(&i.then_body, map);
                for (_, b) in &i.elif_branches {
                    collect_for_temps(b, map);
                }
                if let Some(eb) = &i.else_body {
                    collect_for_temps(eb, map);
                }
            }
            Statement::While(w) => collect_for_temps(&w.body, map),
            _ => {}
        }
    }
}

fn is_range_call(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Call(c, _) if matches!(c.as_ref(), Expr::Ident(n) if n == "range")
    )
}

fn guess_iter_elem_type(iter: &Expr) -> Type {
    match iter {
        Expr::List(xs) if !xs.is_empty() => guess_expr_type(&xs[0]),
        Expr::List(_) => Type::Int,
        _ => Type::Int,
    }
}

fn body_contains_yield(stmts: &[Statement]) -> bool {
    stmts.iter().any(stmt_contains_yield)
}

fn infer_yield_elem_type(f: &FuncDef) -> Result<Type, String> {
    if let Some(Type::List(inner)) = &f.return_type {
        return Ok((**inner).clone());
    }
    if let Some(Type::Adt(name, args)) = &f.return_type {
        if name == "Option" && args.len() == 1 {
            return Ok(args[0].clone());
        }
    }
    let mut yields = Vec::new();
    collect_yield_exprs(&f.body, &mut yields);
    if yields.is_empty() {
        return Err(format!("generator '{}' has no yield expressions", f.name));
    }
    Ok(guess_expr_type(&yields[0]))
}

fn collect_yield_exprs(stmts: &[Statement], out: &mut Vec<Expr>) {
    for s in stmts {
        match s {
            Statement::Yield(e) => out.push(e.clone()),
            Statement::If(i) => {
                collect_yield_exprs(&i.then_body, out);
                for (_, b) in &i.elif_branches {
                    collect_yield_exprs(b, out);
                }
                if let Some(eb) = &i.else_body {
                    collect_yield_exprs(eb, out);
                }
            }
            Statement::While(w) => collect_yield_exprs(&w.body, out),
            Statement::For(f) => collect_yield_exprs(&f.body, out),
            _ => {}
        }
    }
}

fn guess_expr_type(expr: &Expr) -> Type {
    match expr {
        Expr::Int(_) => Type::Int,
        Expr::Float(_) => Type::Float,
        Expr::Bool(_) => Type::Bool,
        Expr::String(_) => Type::Str,
        Expr::BinOp(l, _, _) => guess_expr_type(l),
        Expr::UnaryOp(_, e) => guess_expr_type(e),
        _ => Type::Int,
    }
}

pub fn is_generator_class_name(name: &str) -> bool {
    name.starts_with("__Gen_")
}
