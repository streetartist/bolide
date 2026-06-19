//! 泛型函数单态化（Monomorphization）pass
//!
//! 在 JIT/AOT 后端编译之前，把带泛型参数的普通函数展开成多个具体实例。
//! 例如：
//!
//! ```bolide
//! fn id<T>(x: T) -> T { return x; }
//! print(id(42));
//! print(id("hi"));
//! ```
//!
//! 会被单态化为：
//!
//! ```bolide
//! fn id@I(x: int) -> int { return x; }
//! fn id@S(x: str) -> str { return x; }
//! print(id@I(42));
//! print(id@S("hi"));
//! ```
//!
//! 单态化在 import 合并与内置类注入之后、后端收集函数声明之前运行，
//! 因此对 JIT/AOT 完全透明。
//!
//! 当前限制：
//! - 仅支持顶层泛型函数的直接调用；
//! - 泛型方法、将泛型函数作为一等值传递尚未支持（会给出明确错误）。

use bolide_parser::{
    ClassDef, EnumDef, EnumVariant as AdtVariant, EnumVariantField as AdtVariantField, Expr,
    ForStmt, FuncDef, IfStmt, Pattern, Program, Statement, Type, VarDecl, WhileStmt,
};
use std::collections::HashMap;

/// 单态化入口。
pub fn monomorphize(program: Program) -> Result<Program, String> {
    let mut mono = Monomorphizer::new(&program)?;
    mono.run(program)
}

/// 单态化器状态。
struct Monomorphizer {
    /// 原始泛型函数定义，key 为函数名。
    generic_defs: HashMap<String, FuncDef>,
    /// 已生成的实例，key 为 mangled 名称。
    instances: HashMap<String, FuncDef>,
    /// 非泛型函数签名表，用于类型推断。
    func_sigs: HashMap<String, (Vec<Type>, Option<Type>)>,
    /// 全局变量类型表，用于顶层表达式推断。
    global_var_types: HashMap<String, Type>,
    /// ADT 定义表，用于 ADT 构造器推断。
    adts: HashMap<String, AdtInfo>,
    /// 待处理的实例名称队列。
    pending: Vec<String>,
}

#[derive(Clone)]
struct AdtInfo {
    type_params: Vec<String>,
    variants: Vec<AdtVariant>,
}

impl Monomorphizer {
    fn new(program: &Program) -> Result<Self, String> {
        let mut generic_defs = HashMap::new();
        let mut func_sigs = HashMap::new();
        let mut global_var_types = HashMap::new();
        let mut adts = HashMap::new();

        for stmt in &program.statements {
            match stmt {
                Statement::FuncDef(func) => {
                    if func.type_params.is_empty() {
                        func_sigs.insert(
                            func.name.clone(),
                            (
                                func.params.iter().map(|p| p.ty.clone()).collect(),
                                func.return_type.clone(),
                            ),
                        );
                    } else {
                        generic_defs.insert(func.name.clone(), func.clone());
                    }
                }
                Statement::VarDecl(decl) => {
                    let ty = decl
                        .ty
                        .clone()
                        .or_else(|| decl.value.as_ref().map(|v| Self::infer_literal_type(v)))
                        .unwrap_or(Type::Int);
                    global_var_types.insert(decl.name.clone(), ty);
                }
                Statement::EnumDef(enm) => {
                    adts.insert(
                        enm.name.clone(),
                        AdtInfo {
                            type_params: enm.type_params.clone(),
                            variants: enm.variants.clone(),
                        },
                    );
                }
                Statement::ClassDef(cls) => {
                    for method in &cls.methods {
                        if !method.type_params.is_empty() {
                            return Err(format!(
                                "generic methods are not yet supported (class '{}' method '{}')",
                                cls.name, method.name
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(Monomorphizer {
            generic_defs,
            instances: HashMap::new(),
            func_sigs,
            global_var_types,
            adts,
            pending: Vec::new(),
        })
    }

    fn run(&mut self, program: Program) -> Result<Program, String> {
        // 第一步：重写整个程序中的调用点，并收集需要生成的实例。
        let mut new_program = Program {
            statements: Vec::with_capacity(program.statements.len()),
        };

        for stmt in program.statements {
            match stmt {
                Statement::FuncDef(func) if func.type_params.is_empty() => {
                    let mut func = func.clone();
                    self.process_function(&mut func, &HashMap::new())?;
                    new_program.statements.push(Statement::FuncDef(func));
                }
                Statement::FuncDef(_) => {
                    // 原始泛型函数定义不再输出。
                }
                Statement::ClassDef(mut cls) => {
                    self.process_class(&mut cls)?;
                    new_program.statements.push(Statement::ClassDef(cls));
                }
                other => {
                    let mut var_types = self.global_var_types.clone();
                    let rewritten = self.rewrite_stmt(&other, &mut var_types, &HashMap::new())?;
                    new_program.statements.push(rewritten);
                }
            }
        }

        // 第二步：处理 pending 实例，直到没有新实例产生。
        while let Some(name) = self.pending.pop() {
            if let Some(mut instance) = self.instances.remove(&name) {
                self.process_function(&mut instance, &HashMap::new())?;
                self.instances.insert(name, instance);
            }
        }

        // 第三步：将生成的实例插入到程序中（放在最前面，避免前向引用问题）。
        let instances: Vec<Statement> = self
            .instances
            .drain()
            .map(|(_, func)| Statement::FuncDef(func))
            .collect();
        new_program.statements.splice(0..0, instances);

        Ok(new_program)
    }

    /// 处理一个具体函数：替换其体内的泛型调用，并建立局部变量类型上下文。
    fn process_function(
        &mut self,
        func: &mut FuncDef,
        subst: &HashMap<String, Type>,
    ) -> Result<(), String> {
        // 先替换类型注解中的泛型参数。
        func.params
            .iter_mut()
            .for_each(|p| p.ty = substitute_type(&p.ty, subst));
        if let Some(ref mut ret) = func.return_type {
            *ret = substitute_type(ret, subst);
        }

        let mut var_types = self.global_var_types.clone();
        for param in &func.params {
            var_types.insert(param.name.clone(), param.ty.clone());
        }

        let mut new_body = Vec::with_capacity(func.body.len());
        for stmt in func.body.drain(..) {
            new_body.push(self.rewrite_stmt(&stmt, &mut var_types, subst)?);
        }
        func.body = new_body;
        Ok(())
    }

    fn process_class(&mut self, cls: &mut ClassDef) -> Result<(), String> {
        for method in &mut cls.methods {
            if !method.type_params.is_empty() {
                return Err(format!(
                    "generic methods are not yet supported (class '{}' method '{}')",
                    cls.name, method.name
                ));
            }
            self.process_function(method, &HashMap::new())?;
        }
        Ok(())
    }

    fn rewrite_stmt(
        &mut self,
        stmt: &Statement,
        var_types: &mut HashMap<String, Type>,
        subst: &HashMap<String, Type>,
    ) -> Result<Statement, String> {
        match stmt {
            Statement::VarDecl(decl) => {
                let mut new_decl = decl.clone();
                if let Some(ref mut ty) = new_decl.ty {
                    *ty = substitute_type(ty, subst);
                }
                if let Some(ref mut val) = new_decl.value {
                    self.rewrite_expr(val, var_types, subst);
                }
                let inferred_ty = new_decl
                    .ty
                    .clone()
                    .or_else(|| {
                        new_decl
                            .value
                            .as_ref()
                            .map(|v| self.infer_expr_type(v, var_types))
                    })
                    .unwrap_or(Type::Int);
                var_types.insert(new_decl.name.clone(), inferred_ty);
                Ok(Statement::VarDecl(new_decl))
            }
            Statement::Assign(assign) => {
                let mut new_assign = assign.clone();
                self.rewrite_expr(&mut new_assign.target, var_types, subst);
                self.rewrite_expr(&mut new_assign.value, var_types, subst);
                Ok(Statement::Assign(new_assign))
            }
            Statement::If(if_stmt) => {
                let mut new_if = if_stmt.clone();
                self.rewrite_expr(&mut new_if.condition, var_types, subst);
                new_if.then_body = self.rewrite_block(&new_if.then_body, var_types, subst)?;
                new_if.elif_branches = new_if
                    .elif_branches
                    .iter()
                    .map(|(cond, body)| {
                        let mut c = cond.clone();
                        self.rewrite_expr(&mut c, var_types, subst);
                        Ok((c, self.rewrite_block(body, var_types, subst)?))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                if let Some(ref mut body) = new_if.else_body {
                    *body = self.rewrite_block(body, var_types, subst)?;
                }
                Ok(Statement::If(new_if))
            }
            Statement::While(while_stmt) => {
                let mut new_while = while_stmt.clone();
                self.rewrite_expr(&mut new_while.condition, var_types, subst);
                new_while.body = self.rewrite_block(&new_while.body, var_types, subst)?;
                Ok(Statement::While(new_while))
            }
            Statement::For(for_stmt) => {
                let mut new_for = for_stmt.clone();
                self.rewrite_expr(&mut new_for.iter, var_types, subst);
                let iter_ty = self.infer_expr_type(&new_for.iter, var_types);
                for (i, var) in new_for.vars.iter().enumerate() {
                    let elem_ty = match &iter_ty {
                        Type::List(inner) => *inner.clone(),
                        Type::Dict(k, v) => {
                            if i == 0 {
                                *k.clone()
                            } else {
                                *v.clone()
                            }
                        }
                        Type::Adt(name, _) if name == "Option" => Type::Int,
                        Type::Adt(name, _) if name == "Result" => Type::Int,
                        _ => Type::Int,
                    };
                    var_types.insert(var.clone(), elem_ty);
                }
                new_for.body = self.rewrite_block(&new_for.body, var_types, subst)?;
                Ok(Statement::For(new_for))
            }
            Statement::Pool(pool) => {
                let mut new_pool = pool.clone();
                self.rewrite_expr(&mut new_pool.size, var_types, subst);
                new_pool.body = self.rewrite_block(&new_pool.body, var_types, subst)?;
                Ok(Statement::Pool(new_pool))
            }
            Statement::Match(match_stmt) => {
                let mut new_match = match_stmt.clone();
                self.rewrite_expr(&mut new_match.expr, var_types, subst);
                new_match.arms = match_stmt
                    .arms
                    .iter()
                    .map(|arm| {
                        let mut new_arm = arm.clone();
                        self.collect_pattern_bindings(&arm.pattern, var_types);
                        new_arm.body = self.rewrite_block(&arm.body, var_types, subst)?;
                        Ok(new_arm)
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(Statement::Match(new_match))
            }
            Statement::Try(try_stmt) => {
                let mut new_try = try_stmt.clone();
                new_try.try_body = self.rewrite_block(&try_stmt.try_body, var_types, subst)?;
                new_try.catch_clauses = try_stmt
                    .catch_clauses
                    .iter()
                    .map(|clause| {
                        let mut new_clause = clause.clone();
                        new_clause.ty = substitute_type(&clause.ty, subst);
                        var_types.insert(new_clause.var.clone(), new_clause.ty.clone());
                        new_clause.body = self.rewrite_block(&clause.body, var_types, subst)?;
                        Ok(new_clause)
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                if let Some(ref mut body) = new_try.finally {
                    *body = self.rewrite_block(body, var_types, subst)?;
                }
                Ok(Statement::Try(new_try))
            }
            Statement::Return(expr) => {
                let mut new_expr = expr.clone();
                if let Some(ref mut e) = new_expr {
                    self.rewrite_expr(e, var_types, subst);
                }
                Ok(Statement::Return(new_expr))
            }
            Statement::Throw(expr) => {
                let mut new_expr = expr.clone();
                self.rewrite_expr(&mut new_expr, var_types, subst);
                Ok(Statement::Throw(new_expr))
            }
            Statement::Expr(expr) => {
                let mut new_expr = expr.clone();
                self.rewrite_expr(&mut new_expr, var_types, subst);
                Ok(Statement::Expr(new_expr))
            }
            Statement::FuncDef(func) if func.type_params.is_empty() => {
                let mut func = func.clone();
                self.process_function(&mut func, subst)?;
                Ok(Statement::FuncDef(func))
            }
            Statement::FuncDef(func) => Err(format!(
                "nested generic functions are not supported: {}",
                func.name
            )),
            Statement::ClassDef(cls) => {
                let mut cls = cls.clone();
                self.process_class(&mut cls)?;
                Ok(Statement::ClassDef(cls))
            }
            // 其余语句不含表达式或类型注解，直接返回。
            other => Ok(other.clone()),
        }
    }

    fn rewrite_block(
        &mut self,
        stmts: &[Statement],
        var_types: &mut HashMap<String, Type>,
        subst: &HashMap<String, Type>,
    ) -> Result<Vec<Statement>, String> {
        stmts
            .iter()
            .map(|s| self.rewrite_stmt(s, var_types, subst))
            .collect()
    }

    fn collect_pattern_bindings(&self, pattern: &Pattern, var_types: &mut HashMap<String, Type>) {
        match pattern {
            Pattern::Bind(name) => {
                var_types.insert(name.clone(), Type::Dynamic);
            }
            Pattern::Variant { fields, .. } => {
                for f in fields {
                    self.collect_pattern_bindings(f, var_types);
                }
            }
            _ => {}
        }
    }

    fn rewrite_expr(
        &mut self,
        expr: &mut Expr,
        var_types: &HashMap<String, Type>,
        subst: &HashMap<String, Type>,
    ) {
        match expr {
            Expr::BinOp(left, _, right) => {
                self.rewrite_expr(left, var_types, subst);
                self.rewrite_expr(right, var_types, subst);
            }
            Expr::UnaryOp(_, operand) => {
                self.rewrite_expr(operand, var_types, subst);
            }
            Expr::Call(callee, args) => {
                self.rewrite_expr(callee, var_types, subst);
                for arg in &mut *args {
                    self.rewrite_expr(arg, var_types, subst);
                }
                if let Expr::Ident(name) = callee.as_ref() {
                    if self.generic_defs.contains_key(name) {
                        let instance_name = self.instantiate_call(name, args, var_types, subst);
                        *callee = Box::new(Expr::Ident(instance_name));
                    } else if name.starts_with('@') {
                        // 模块化的泛型函数：名称为 @module_func，需要识别。
                        if let Some(base) = name.splitn(2, '_').nth(1) {
                            if self.generic_defs.contains_key(base) {
                                let instance_name = self
                                    .instantiate_module_call(name, base, args, var_types, subst);
                                *callee = Box::new(Expr::Ident(instance_name));
                            }
                        }
                    }
                }
            }
            Expr::Index(base, idx) => {
                self.rewrite_expr(base, var_types, subst);
                self.rewrite_expr(idx, var_types, subst);
            }
            Expr::Member(base, _) => {
                self.rewrite_expr(base, var_types, subst);
            }
            Expr::List(items) => {
                for item in items {
                    self.rewrite_expr(item, var_types, subst);
                }
            }
            Expr::Dict(entries) => {
                for (k, v) in entries {
                    self.rewrite_expr(k, var_types, subst);
                    self.rewrite_expr(v, var_types, subst);
                }
            }
            Expr::Tuple(items) => {
                for item in items {
                    self.rewrite_expr(item, var_types, subst);
                }
            }
            Expr::Slice(base, start, end, step) => {
                self.rewrite_expr(base, var_types, subst);
                if let Some(s) = start {
                    self.rewrite_expr(s, var_types, subst);
                }
                if let Some(e) = end {
                    self.rewrite_expr(e, var_types, subst);
                }
                if let Some(s) = step {
                    self.rewrite_expr(s, var_types, subst);
                }
            }
            Expr::Await(inner) => {
                self.rewrite_expr(inner, var_types, subst);
            }
            Expr::SpawnAll(exprs) => {
                for e in exprs {
                    self.rewrite_expr(e, var_types, subst);
                }
            }
            Expr::Spawn(_, args) | Expr::SpawnThread(_, args) => {
                for arg in args {
                    self.rewrite_expr(arg, var_types, subst);
                }
            }
            Expr::Propagate(inner) | Expr::Raise(inner) => {
                self.rewrite_expr(inner, var_types, subst);
            }
            Expr::TryExpr(body) => {
                let mut inner_types = var_types.clone();
                for stmt in body {
                    let _ = self.rewrite_stmt(stmt, &mut inner_types, subst);
                }
            }
            // 其余表达式没有子表达式。
            _ => {}
        }
    }

    fn instantiate_call(
        &mut self,
        name: &str,
        args: &[Expr],
        var_types: &HashMap<String, Type>,
        outer_subst: &HashMap<String, Type>,
    ) -> String {
        let gen_def = self.generic_defs.get(name).cloned().unwrap();
        let type_args = self.infer_generic_type_args(&gen_def, args, var_types, outer_subst);
        self.create_instance(name, &gen_def, &type_args, outer_subst)
    }

    fn instantiate_module_call(
        &mut self,
        full_name: &str,
        base_name: &str,
        args: &[Expr],
        var_types: &HashMap<String, Type>,
        outer_subst: &HashMap<String, Type>,
    ) -> String {
        let gen_def = self.generic_defs.get(base_name).cloned().unwrap();
        let type_args = self.infer_generic_type_args(&gen_def, args, var_types, outer_subst);
        let instance_base = self.create_instance(base_name, &gen_def, &type_args, outer_subst);
        // 保留模块前缀：@module_func@... -> @module_func@...
        format!(
            "{}@{}",
            full_name.split('@').next().unwrap_or(full_name),
            instance_base
                .split('@')
                .skip(1)
                .collect::<Vec<_>>()
                .join("@")
        )
    }

    fn infer_generic_type_args(
        &self,
        gen_def: &FuncDef,
        args: &[Expr],
        var_types: &HashMap<String, Type>,
        outer_subst: &HashMap<String, Type>,
    ) -> Vec<Type> {
        let mut bindings: HashMap<String, Type> = HashMap::new();
        for (param, arg) in gen_def.params.iter().zip(args.iter()) {
            let param_ty = substitute_type(&param.ty, outer_subst);
            let arg_ty = self.infer_expr_type(arg, var_types);
            unify_types(&param_ty, &arg_ty, &mut bindings);
        }
        gen_def
            .type_params
            .iter()
            .map(|tp| bindings.get(tp).cloned().unwrap_or(Type::Dynamic))
            .collect()
    }

    fn create_instance(
        &mut self,
        base_name: &str,
        gen_def: &FuncDef,
        type_args: &[Type],
        outer_subst: &HashMap<String, Type>,
    ) -> String {
        let instance_name = mangle_name(base_name, type_args);
        if !self.instances.contains_key(&instance_name) {
            let mut instance = gen_def.clone();
            instance.name = instance_name.clone();
            instance.type_params.clear();

            let mut full_subst = outer_subst.clone();
            for (tp, ta) in gen_def.type_params.iter().zip(type_args.iter()) {
                full_subst.insert(tp.clone(), ta.clone());
            }

            substitute_func_def(&mut instance, &full_subst);
            self.instances.insert(instance_name.clone(), instance);
            self.pending.push(instance_name.clone());
        }
        instance_name
    }

    fn infer_expr_type(&self, expr: &Expr, var_types: &HashMap<String, Type>) -> Type {
        match expr {
            Expr::Int(_) => Type::Int,
            Expr::Float(_) => Type::Float,
            Expr::Bool(_) => Type::Bool,
            Expr::String(_) => Type::Str,
            Expr::BigInt(_) => Type::BigInt,
            Expr::Decimal(_) => Type::Decimal,
            Expr::None => Type::Int,
            Expr::Ident(name) => var_types.get(name).cloned().unwrap_or(Type::Int),
            Expr::List(items) => {
                let elem_ty = if items.is_empty() {
                    Type::Int
                } else {
                    let first = self.infer_expr_type(&items[0], var_types);
                    let mut ty = first;
                    for item in items.iter().skip(1) {
                        let t = self.infer_expr_type(item, var_types);
                        if t != ty {
                            ty = Type::Dynamic;
                        }
                    }
                    ty
                };
                Type::List(Box::new(elem_ty))
            }
            Expr::Dict(entries) => {
                if entries.is_empty() {
                    Type::Dict(Box::new(Type::Int), Box::new(Type::Int))
                } else {
                    let (mut k_ty, mut v_ty) = (
                        self.infer_expr_type(&entries[0].0, var_types),
                        self.infer_expr_type(&entries[0].1, var_types),
                    );
                    for (k, v) in entries.iter().skip(1) {
                        let kt = self.infer_expr_type(k, var_types);
                        let vt = self.infer_expr_type(v, var_types);
                        if kt != k_ty {
                            k_ty = Type::Dynamic;
                        }
                        if vt != v_ty {
                            v_ty = Type::Dynamic;
                        }
                    }
                    Type::Dict(Box::new(k_ty), Box::new(v_ty))
                }
            }
            Expr::Tuple(exprs) => Type::Tuple(
                exprs
                    .iter()
                    .map(|e| self.infer_expr_type(e, var_types))
                    .collect(),
            ),
            Expr::BinOp(left, op, right) => {
                let left_ty = self.infer_expr_type(left, var_types);
                let right_ty = self.infer_expr_type(right, var_types);
                match op {
                    bolide_parser::BinOp::Add
                    | bolide_parser::BinOp::Sub
                    | bolide_parser::BinOp::Mul
                    | bolide_parser::BinOp::Div
                    | bolide_parser::BinOp::Mod => {
                        if left_ty == Type::Float || right_ty == Type::Float {
                            Type::Float
                        } else {
                            left_ty
                        }
                    }
                    _ => Type::Bool,
                }
            }
            Expr::UnaryOp(_, operand) => self.infer_expr_type(operand, var_types),
            Expr::Call(callee, args) => self.infer_call_type(callee, args, var_types),
            Expr::Index(base, _) => {
                let base_ty = self.infer_expr_type(base, var_types);
                match base_ty {
                    Type::List(inner) => *inner,
                    Type::Str => Type::Str,
                    Type::Tuple(ts) => ts.first().cloned().unwrap_or(Type::Int),
                    _ => Type::Int,
                }
            }
            Expr::Member(base, member) => {
                if let Expr::Ident(module_name) = base.as_ref() {
                    if module_name.starts_with('@') {
                        // 模块常量/函数：无法静态推断，返回 Int 兜底。
                        return Type::Int;
                    }
                }
                // 对象字段/方法：保守返回 Int。
                Type::Int
            }
            Expr::Slice(base, _, _, _) => self.infer_expr_type(base, var_types),
            Expr::Await(_) => Type::Int,
            Expr::SpawnAll(_) => Type::Tuple(vec![]),
            Expr::Spawn(_, _) | Expr::SpawnThread(_, _) => Type::Future,
            Expr::Propagate(inner) | Expr::Raise(inner) => match self
                .infer_expr_type(inner, var_types)
            {
                Type::Adt(name, args) if name == "Result" && !args.is_empty() => args[0].clone(),
                Type::Adt(name, args) if name == "Option" && !args.is_empty() => args[0].clone(),
                _ => Type::Int,
            },
            Expr::TryExpr(body) => {
                let ok_ty = body
                    .last()
                    .and_then(|stmt| match stmt {
                        Statement::Expr(expr) => Some(self.infer_expr_type(expr, var_types)),
                        _ => None,
                    })
                    .unwrap_or(Type::Int);
                Type::Adt(
                    "Result".to_string(),
                    vec![ok_ty, Type::Custom("Error".to_string())],
                )
            }
            _ => Type::Int,
        }
    }

    fn infer_call_type(
        &self,
        callee: &Expr,
        args: &[Expr],
        var_types: &HashMap<String, Type>,
    ) -> Type {
        // ADT 构造器
        if let Expr::Member(base, variant_name) = callee {
            if let Expr::Ident(adt_name) = base.as_ref() {
                if let Some(adt_info) = self.adts.get(adt_name) {
                    if let Some(variant) =
                        adt_info.variants.iter().find(|v| v.name == *variant_name)
                    {
                        let type_args =
                            self.infer_adt_type_args(adt_info, variant, args, var_types);
                        return Type::Adt(adt_name.clone(), type_args);
                    }
                }
            }
        }

        if let Expr::Ident(name) = callee {
            // 泛型函数直接调用
            if let Some(gen_def) = self.generic_defs.get(name) {
                let mut bindings = HashMap::new();
                for (param, arg) in gen_def.params.iter().zip(args.iter()) {
                    let actual = self.infer_expr_type(arg, var_types);
                    unify_types(&param.ty, &actual, &mut bindings);
                }
                let subst: HashMap<String, Type> = gen_def
                    .type_params
                    .iter()
                    .cloned()
                    .zip(
                        gen_def
                            .type_params
                            .iter()
                            .map(|tp| bindings.get(tp).cloned().unwrap_or(Type::Dynamic)),
                    )
                    .collect();
                return gen_def
                    .return_type
                    .as_ref()
                    .map(|r| substitute_type(r, &subst))
                    .unwrap_or(Type::Int);
            }

            // 特殊内置函数
            if let Some(ty) = Self::infer_special_call(name, args) {
                return ty;
            }

            // 普通函数
            if let Some((_, ret)) = self.func_sigs.get(name) {
                return ret.clone().unwrap_or(Type::Int);
            }

            // 模块函数 @module_func
            if name.starts_with('@') {
                let base = name.splitn(2, '_').nth(1).unwrap_or(name);
                if let Some((_, ret)) = self.func_sigs.get(base) {
                    return ret.clone().unwrap_or(Type::Int);
                }
            }
        }

        // 方法调用或间接调用：保守返回 Int。
        Type::Int
    }

    fn infer_adt_type_args(
        &self,
        adt_info: &AdtInfo,
        variant: &AdtVariant,
        args: &[Expr],
        var_types: &HashMap<String, Type>,
    ) -> Vec<Type> {
        let mut bindings = HashMap::new();
        for (field, arg) in variant.fields.iter().zip(args.iter()) {
            let actual = self.infer_expr_type(arg, var_types);
            unify_types(&field.ty, &actual, &mut bindings);
        }
        adt_info
            .type_params
            .iter()
            .map(|tp| bindings.get(tp).cloned().unwrap_or(Type::Dynamic))
            .collect()
    }

    fn infer_special_call(name: &str, args: &[Expr]) -> Option<Type> {
        match name {
            "int" => Some(Type::Int),
            "float" => Some(Type::Float),
            "str" => Some(Type::Str),
            "bigint" => Some(Type::BigInt),
            "decimal" => Some(Type::Decimal),
            "bool" => Some(Type::Bool),
            "print" | "println" | "bigint_debug_stats" | "tuple_debug_stats" => Some(Type::Int),
            "input" => Some(Type::Str),
            "channel" => {
                let elem = args.first().map(|_| Type::Int).unwrap_or(Type::Int);
                Some(Type::Channel(Box::new(elem)))
            }
            _ => None,
        }
    }

    fn infer_literal_type(expr: &Expr) -> Type {
        match expr {
            Expr::Int(_) => Type::Int,
            Expr::Float(_) => Type::Float,
            Expr::Bool(_) => Type::Bool,
            Expr::String(_) => Type::Str,
            Expr::BigInt(_) => Type::BigInt,
            Expr::Decimal(_) => Type::Decimal,
            Expr::List(items) => {
                let elem = items
                    .first()
                    .map(|e| Self::infer_literal_type(e))
                    .unwrap_or(Type::Int);
                Type::List(Box::new(elem))
            }
            Expr::Tuple(items) => {
                Type::Tuple(items.iter().map(|e| Self::infer_literal_type(e)).collect())
            }
            _ => Type::Int,
        }
    }
}

fn mangle_name(base: &str, type_args: &[Type]) -> String {
    if type_args.is_empty() {
        base.to_string()
    } else {
        let serialized: Vec<String> = type_args.iter().map(serialize_type).collect();
        format!("{}@{}", base, serialized.join("@"))
    }
}

fn serialize_type(ty: &Type) -> String {
    match ty {
        Type::Int => "I".to_string(),
        Type::Float => "F".to_string(),
        Type::Bool => "B".to_string(),
        Type::Str => "S".to_string(),
        Type::Bytes => "Bytes".to_string(),
        Type::BigInt => "BI".to_string(),
        Type::Decimal => "D".to_string(),
        Type::Dynamic => "Dyn".to_string(),
        Type::Ptr => "Ptr".to_string(),
        Type::Future => "Fut".to_string(),
        Type::Func => "Fn".to_string(),
        Type::FuncSig(params, ret) => {
            let mut parts = vec!["Fn".to_string(), params.len().to_string()];
            for p in params {
                parts.push(serialize_type(p));
            }
            if let Some(r) = ret {
                parts.push(format!("R{}", serialize_type(r)));
            } else {
                parts.push("Rvoid".to_string());
            }
            parts.join("@")
        }
        Type::List(inner) => format!("L@{}", serialize_type(inner)),
        Type::Dict(k, v) => format!("Dict@{}@{}", serialize_type(k), serialize_type(v)),
        Type::Tuple(ts) => {
            let mut parts = vec!["T".to_string(), ts.len().to_string()];
            parts.extend(ts.iter().map(serialize_type));
            parts.join("@")
        }
        Type::Generic(name) => format!("G@{}", name),
        Type::Adt(name, args) => {
            let mut parts = vec![name.clone(), args.len().to_string()];
            parts.extend(args.iter().map(serialize_type));
            parts.join("@")
        }
        Type::Custom(name) => name.clone(),
        Type::Weak(inner) => format!("W@{}", serialize_type(inner)),
        Type::Unowned(inner) => format!("U@{}", serialize_type(inner)),
        Type::Channel(inner) => format!("C@{}", serialize_type(inner)),
    }
}

fn substitute_type(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Generic(name) => subst
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::Generic(name.clone())),
        Type::List(inner) => Type::List(Box::new(substitute_type(inner, subst))),
        Type::Dict(k, v) => Type::Dict(
            Box::new(substitute_type(k, subst)),
            Box::new(substitute_type(v, subst)),
        ),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| substitute_type(t, subst)).collect()),
        Type::FuncSig(params, ret) => Type::FuncSig(
            params.iter().map(|p| substitute_type(p, subst)).collect(),
            ret.as_ref().map(|r| Box::new(substitute_type(r, subst))),
        ),
        Type::Adt(name, args) => Type::Adt(
            name.clone(),
            args.iter().map(|a| substitute_type(a, subst)).collect(),
        ),
        Type::Weak(inner) => Type::Weak(Box::new(substitute_type(inner, subst))),
        Type::Unowned(inner) => Type::Unowned(Box::new(substitute_type(inner, subst))),
        Type::Channel(inner) => Type::Channel(Box::new(substitute_type(inner, subst))),
        other => other.clone(),
    }
}

fn substitute_func_def(func: &mut FuncDef, subst: &HashMap<String, Type>) {
    for param in &mut func.params {
        param.ty = substitute_type(&param.ty, subst);
    }
    if let Some(ref mut ret) = func.return_type {
        *ret = substitute_type(ret, subst);
    }
    func.body = func
        .body
        .iter()
        .map(|s| substitute_stmt_types(s, subst))
        .collect();
    func.type_params.clear();
}

fn substitute_stmt_types(stmt: &Statement, subst: &HashMap<String, Type>) -> Statement {
    match stmt {
        Statement::VarDecl(decl) => {
            let mut new = decl.clone();
            if let Some(ref mut ty) = new.ty {
                *ty = substitute_type(ty, subst);
            }
            Statement::VarDecl(new)
        }
        Statement::FuncDef(func) => {
            let mut new = func.clone();
            substitute_func_def(&mut new, subst);
            Statement::FuncDef(new)
        }
        Statement::ClassDef(cls) => {
            let mut new = cls.clone();
            for field in &mut new.fields {
                field.ty = substitute_type(&field.ty, subst);
            }
            for method in &mut new.methods {
                substitute_func_def(method, subst);
            }
            Statement::ClassDef(new)
        }
        Statement::EnumDef(enm) => {
            let mut new = enm.clone();
            for variant in &mut new.variants {
                for field in &mut variant.fields {
                    field.ty = substitute_type(&field.ty, subst);
                }
            }
            Statement::EnumDef(new)
        }
        Statement::If(if_stmt) => {
            let mut new = if_stmt.clone();
            new.then_body = substitute_block_types(&new.then_body, subst);
            new.elif_branches = new
                .elif_branches
                .iter()
                .map(|(c, b)| (c.clone(), substitute_block_types(b, subst)))
                .collect();
            if let Some(ref mut b) = new.else_body {
                *b = substitute_block_types(b, subst);
            }
            Statement::If(new)
        }
        Statement::While(while_stmt) => {
            let mut new = while_stmt.clone();
            new.body = substitute_block_types(&new.body, subst);
            Statement::While(new)
        }
        Statement::For(for_stmt) => {
            let mut new = for_stmt.clone();
            new.body = substitute_block_types(&new.body, subst);
            Statement::For(new)
        }
        Statement::Pool(pool) => {
            let mut new = pool.clone();
            new.body = substitute_block_types(&new.body, subst);
            Statement::Pool(new)
        }
        Statement::Match(match_stmt) => {
            let mut new = match_stmt.clone();
            new.arms = new
                .arms
                .iter()
                .map(|arm| {
                    let mut a = arm.clone();
                    a.body = substitute_block_types(&a.body, subst);
                    a
                })
                .collect();
            Statement::Match(new)
        }
        Statement::Try(try_stmt) => {
            let mut new = try_stmt.clone();
            new.try_body = substitute_block_types(&new.try_body, subst);
            new.catch_clauses = new
                .catch_clauses
                .iter()
                .map(|c| {
                    let mut cc = c.clone();
                    cc.ty = substitute_type(&cc.ty, subst);
                    cc.body = substitute_block_types(&cc.body, subst);
                    cc
                })
                .collect();
            if let Some(ref mut b) = new.finally {
                *b = substitute_block_types(b, subst);
            }
            Statement::Try(new)
        }
        other => other.clone(),
    }
}

fn substitute_block_types(stmts: &[Statement], subst: &HashMap<String, Type>) -> Vec<Statement> {
    stmts
        .iter()
        .map(|s| substitute_stmt_types(s, subst))
        .collect()
}

fn unify_types(pattern: &Type, actual: &Type, bindings: &mut HashMap<String, Type>) {
    match pattern {
        Type::Generic(name) => {
            bindings
                .entry(name.clone())
                .or_insert_with(|| actual.clone());
        }
        Type::List(p) => {
            if let Type::List(a) = actual {
                unify_types(p, a, bindings);
            }
        }
        Type::Dict(pk, pv) => {
            if let Type::Dict(ak, av) = actual {
                unify_types(pk, ak, bindings);
                unify_types(pv, av, bindings);
            }
        }
        Type::Tuple(ps) => {
            if let Type::Tuple(as_) = actual {
                for (p, a) in ps.iter().zip(as_.iter()) {
                    unify_types(p, a, bindings);
                }
            }
        }
        Type::Adt(pn, ps) => {
            if let Type::Adt(an, as_) = actual {
                if pn == an {
                    for (p, a) in ps.iter().zip(as_.iter()) {
                        unify_types(p, a, bindings);
                    }
                }
            }
        }
        Type::FuncSig(params, ret) => {
            if let Type::FuncSig(aparams, aret) = actual {
                for (p, a) in params.iter().zip(aparams.iter()) {
                    unify_types(p, a, bindings);
                }
                if let (Some(rp), Some(ra)) = (ret, aret) {
                    unify_types(rp, ra, bindings);
                }
            }
        }
        Type::Weak(p) => {
            if let Type::Weak(a) = actual {
                unify_types(p, a, bindings);
            }
        }
        Type::Unowned(p) => {
            if let Type::Unowned(a) = actual {
                unify_types(p, a, bindings);
            }
        }
        Type::Channel(p) => {
            if let Type::Channel(a) = actual {
                unify_types(p, a, bindings);
            }
        }
        _ => {}
    }
}
