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
//! 支持：
//! - 顶层泛型函数与 class 泛型方法的直接调用；
//! - 将泛型函数作为一等值使用（赋值、传参等），由期望的 `func(...)` 类型实例化。

use bolide_parser::{
    ClassDef, EnumDef, EnumVariant as AdtVariant, EnumVariantField as AdtVariantField, Expr,
    ForStmt, FuncDef, IfStmt, Param, ParamMode, Pattern, Program, Statement, Type, VarDecl,
    WhileStmt,
};
use std::collections::{HashMap, HashSet};

/// 单态化入口。
pub fn monomorphize(program: Program) -> Result<Program, String> {
    let mut mono = Monomorphizer::new(&program)?;
    mono.run(program)
}

/// Converter name used by `?` for `impl From<Src> for Dst` (parsed into a free function).
pub use bolide_parser::from_converter_name;

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
    /// type_name → 已 impl 的 trait 名集合
    type_traits: HashMap<String, Vec<String>>,
    /// class → 主父类
    class_parents: HashMap<String, String>,
    /// class → 方法名集合（协议 trait 自动满足）
    class_methods: HashMap<String, HashSet<String>>,
    /// 已知 class 名（构造器 `Class(...)` 类型推断）
    class_names: HashSet<String>,
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
        let mut type_traits = HashMap::new();
        let mut class_parents = HashMap::new();
        let mut class_methods = HashMap::new();
        let mut class_names = HashSet::new();

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
                    class_names.insert(cls.name.clone());
                    if !cls.impl_traits.is_empty() {
                        type_traits.insert(cls.name.clone(), cls.impl_traits.clone());
                    }
                    if let Some(ref p) = cls.parent {
                        class_parents.insert(cls.name.clone(), p.clone());
                    }
                    let methods: HashSet<String> =
                        cls.methods.iter().map(|m| m.name.clone()).collect();
                    class_methods.insert(cls.name.clone(), methods);
                    for method in &cls.methods {
                        if method.type_params.is_empty() {
                            // Non-generic method signature for inference (self first).
                            let mut params = vec![Type::Custom(cls.name.clone())];
                            params.extend(method.params.iter().map(|p| p.ty.clone()));
                            let full_name = format!("{}_{}", cls.name, method.name);
                            func_sigs.insert(
                                full_name,
                                (params, method.return_type.clone()),
                            );
                        } else {
                            // Store as free function shape with explicit self parameter.
                            let key = format!("{}::{}", cls.name, method.name);
                            let mut def = method.clone();
                            def.params.insert(
                                0,
                                Param {
                                    name: "self".to_string(),
                                    ty: Type::Custom(cls.name.clone()),
                                    mode: ParamMode::Borrow,
                                    default_value: None,
                                    is_variadic: false,
                                    is_kw_variadic: false,
                                },
                            );
                            generic_defs.insert(key, def);
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
            type_traits,
            class_parents,
            class_methods,
            class_names,
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
                    let rewritten =
                        self.rewrite_stmt(&other, &mut var_types, &HashMap::new(), None)?;
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

        let expected_return = func.return_type.clone();
        let mut new_body = Vec::with_capacity(func.body.len());
        for stmt in func.body.drain(..) {
            new_body.push(self.rewrite_stmt(
                &stmt,
                &mut var_types,
                subst,
                expected_return.as_ref(),
            )?);
        }
        func.body = new_body;
        Ok(())
    }

    fn process_class(&mut self, cls: &mut ClassDef) -> Result<(), String> {
        // Generic methods are monomorphized into top-level functions; drop the templates.
        cls.methods.retain(|m| m.type_params.is_empty());
        for method in &mut cls.methods {
            self.process_function(method, &HashMap::new())?;
        }
        Ok(())
    }

    fn rewrite_stmt(
        &mut self,
        stmt: &Statement,
        var_types: &mut HashMap<String, Type>,
        subst: &HashMap<String, Type>,
        expected_return: Option<&Type>,
    ) -> Result<Statement, String> {
        match stmt {
            Statement::VarDecl(decl) => {
                let mut new_decl = decl.clone();
                if let Some(ref mut ty) = new_decl.ty {
                    *ty = substitute_type(ty, subst);
                }
                if let Some(ref mut val) = new_decl.value {
                    self.rewrite_expr(val, var_types, subst, new_decl.ty.as_ref())?;
                    self.ensure_generic_value_resolved(val, new_decl.ty.as_ref())?;
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
                self.rewrite_expr(&mut new_assign.target, var_types, subst, None)?;
                let expected = match &new_assign.target {
                    Expr::Ident(name) => var_types.get(name).cloned(),
                    _ => None,
                };
                self.rewrite_expr(
                    &mut new_assign.value,
                    var_types,
                    subst,
                    expected.as_ref(),
                )?;
                self.ensure_generic_value_resolved(&new_assign.value, expected.as_ref())?;
                Ok(Statement::Assign(new_assign))
            }
            Statement::If(if_stmt) => {
                let mut new_if = if_stmt.clone();
                self.rewrite_expr(&mut new_if.condition, var_types, subst, None)?;
                new_if.then_body =
                    self.rewrite_block(&new_if.then_body, var_types, subst, expected_return)?;
                new_if.elif_branches = new_if
                    .elif_branches
                    .iter()
                    .map(|(cond, body)| {
                        let mut c = cond.clone();
                        self.rewrite_expr(&mut c, var_types, subst, None)?;
                        Ok((
                            c,
                            self.rewrite_block(body, var_types, subst, expected_return)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                if let Some(ref mut body) = new_if.else_body {
                    *body = self.rewrite_block(body, var_types, subst, expected_return)?;
                }
                Ok(Statement::If(new_if))
            }
            Statement::While(while_stmt) => {
                let mut new_while = while_stmt.clone();
                self.rewrite_expr(&mut new_while.condition, var_types, subst, None)?;
                new_while.body =
                    self.rewrite_block(&new_while.body, var_types, subst, expected_return)?;
                Ok(Statement::While(new_while))
            }
            Statement::For(for_stmt) => {
                let mut new_for = for_stmt.clone();
                self.rewrite_expr(&mut new_for.iter, var_types, subst, None)?;
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
                new_for.body =
                    self.rewrite_block(&new_for.body, var_types, subst, expected_return)?;
                Ok(Statement::For(new_for))
            }
            Statement::Pool(pool) => {
                let mut new_pool = pool.clone();
                self.rewrite_expr(&mut new_pool.size, var_types, subst, None)?;
                new_pool.body =
                    self.rewrite_block(&new_pool.body, var_types, subst, expected_return)?;
                Ok(Statement::Pool(new_pool))
            }
            Statement::Match(match_stmt) => {
                let mut new_match = match_stmt.clone();
                self.rewrite_expr(&mut new_match.expr, var_types, subst, None)?;
                new_match.arms = match_stmt
                    .arms
                    .iter()
                    .map(|arm| {
                        let mut new_arm = arm.clone();
                        self.collect_pattern_bindings(&arm.pattern, var_types);
                        new_arm.body =
                            self.rewrite_block(&arm.body, var_types, subst, expected_return)?;
                        Ok(new_arm)
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(Statement::Match(new_match))
            }
            Statement::Try(try_stmt) => {
                let mut new_try = try_stmt.clone();
                new_try.try_body =
                    self.rewrite_block(&try_stmt.try_body, var_types, subst, expected_return)?;
                new_try.catch_clauses = try_stmt
                    .catch_clauses
                    .iter()
                    .map(|clause| {
                        let mut new_clause = clause.clone();
                        new_clause.ty = substitute_type(&clause.ty, subst);
                        var_types.insert(new_clause.var.clone(), new_clause.ty.clone());
                        new_clause.body =
                            self.rewrite_block(&clause.body, var_types, subst, expected_return)?;
                        Ok(new_clause)
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                if let Some(ref mut body) = new_try.finally {
                    *body = self.rewrite_block(body, var_types, subst, expected_return)?;
                }
                Ok(Statement::Try(new_try))
            }
            Statement::Return(expr) => {
                let mut new_expr = expr.clone();
                if let Some(ref mut e) = new_expr {
                    self.rewrite_expr(e, var_types, subst, expected_return)?;
                    self.ensure_generic_value_resolved(e, expected_return)?;
                }
                Ok(Statement::Return(new_expr))
            }
            Statement::Throw(expr) => {
                let mut new_expr = expr.clone();
                self.rewrite_expr(&mut new_expr, var_types, subst, None)?;
                Ok(Statement::Throw(new_expr))
            }
            Statement::Expr(expr) => {
                let mut new_expr = expr.clone();
                self.rewrite_expr(&mut new_expr, var_types, subst, None)?;
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
        expected_return: Option<&Type>,
    ) -> Result<Vec<Statement>, String> {
        stmts
            .iter()
            .map(|s| self.rewrite_stmt(s, var_types, subst, expected_return))
            .collect()
    }

    /// Error if a bare generic function name remains as a value without a concrete type.
    fn ensure_generic_value_resolved(
        &self,
        expr: &Expr,
        expected: Option<&Type>,
    ) -> Result<(), String> {
        if let Expr::Ident(name) = expr {
            if self.generic_defs.contains_key(name) {
                return Err(format!(
                    "cannot use generic function '{}' as a value without a concrete function type; \
                     annotate the target (e.g. `let f: func(int) -> int = {}`) or pass it where a \
                     `func(...)` parameter type is known",
                    name, name
                ));
            }
            let _ = expected;
        }
        Ok(())
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
        expected: Option<&Type>,
    ) -> Result<(), String> {
        match expr {
            Expr::BinOp(left, _, right) => {
                self.rewrite_expr(left, var_types, subst, None)?;
                self.rewrite_expr(right, var_types, subst, None)?;
            }
            Expr::UnaryOp(_, operand) => {
                self.rewrite_expr(operand, var_types, subst, None)?;
            }
            Expr::Call(callee, args) => {
                // Rewrite callee first (no expected), then args with parameter expectations.
                self.rewrite_expr(callee, var_types, subst, None)?;
                let expected_args = self.expected_arg_types(callee.as_ref(), var_types);
                for (i, arg) in args.iter_mut().enumerate() {
                    let exp = expected_args.get(i);
                    self.rewrite_expr(arg, var_types, subst, exp)?;
                    self.ensure_generic_value_resolved(arg, exp)?;
                }
                if let Expr::Ident(name) = callee.as_ref() {
                    if self.generic_defs.contains_key(name) {
                        let instance_name =
                            self.instantiate_call(name, args, var_types, subst)?;
                        *callee = Box::new(Expr::Ident(instance_name));
                    } else if name.starts_with('@') {
                        if let Some(base) = name.splitn(2, '_').nth(1) {
                            if self.generic_defs.contains_key(base) {
                                let instance_name = self.instantiate_module_call(
                                    name, base, args, var_types, subst,
                                )?;
                                *callee = Box::new(Expr::Ident(instance_name));
                            }
                        }
                    }
                } else if let Expr::Member(obj, method_name) = callee.as_ref() {
                    if let Type::Custom(class_name) = self.infer_expr_type(obj, var_types) {
                        let key = format!("{}::{}", class_name, method_name);
                        if self.generic_defs.contains_key(&key) {
                            let instance_name =
                                self.instantiate_method_call(&key, args, var_types, subst)?;
                            let mut new_args = Vec::with_capacity(args.len() + 1);
                            new_args.push(obj.as_ref().clone());
                            new_args.extend(args.iter().cloned());
                            *expr = Expr::Call(Box::new(Expr::Ident(instance_name)), new_args);
                        }
                    }
                }
            }
            Expr::Index(base, idx) => {
                self.rewrite_expr(base, var_types, subst, None)?;
                self.rewrite_expr(idx, var_types, subst, None)?;
            }
            Expr::Member(base, _) => {
                self.rewrite_expr(base, var_types, subst, None)?;
            }
            Expr::List(items) => {
                let elem_expected = match expected {
                    Some(Type::List(inner)) => Some(inner.as_ref()),
                    _ => None,
                };
                for item in items {
                    self.rewrite_expr(item, var_types, subst, elem_expected)?;
                    self.ensure_generic_value_resolved(item, elem_expected)?;
                }
            }
            Expr::Dict(entries) => {
                let (k_exp, v_exp) = match expected {
                    Some(Type::Dict(k, v)) => (Some(k.as_ref()), Some(v.as_ref())),
                    _ => (None, None),
                };
                for (k, v) in entries {
                    self.rewrite_expr(k, var_types, subst, k_exp)?;
                    self.rewrite_expr(v, var_types, subst, v_exp)?;
                }
            }
            Expr::Tuple(items) => {
                let elem_types: Option<Vec<&Type>> = match expected {
                    Some(Type::Tuple(ts)) => Some(ts.iter().collect()),
                    _ => None,
                };
                for (i, item) in items.iter_mut().enumerate() {
                    let exp = elem_types.as_ref().and_then(|ts| ts.get(i).copied());
                    self.rewrite_expr(item, var_types, subst, exp)?;
                    self.ensure_generic_value_resolved(item, exp)?;
                }
            }
            Expr::Slice(base, start, end, step) => {
                self.rewrite_expr(base, var_types, subst, None)?;
                if let Some(s) = start {
                    self.rewrite_expr(s, var_types, subst, None)?;
                }
                if let Some(e) = end {
                    self.rewrite_expr(e, var_types, subst, None)?;
                }
                if let Some(s) = step {
                    self.rewrite_expr(s, var_types, subst, None)?;
                }
            }
            Expr::Await(inner) => {
                self.rewrite_expr(inner, var_types, subst, None)?;
            }
            Expr::SpawnAll(exprs) => {
                for e in exprs {
                    self.rewrite_expr(e, var_types, subst, None)?;
                }
            }
            Expr::Spawn(_, args) | Expr::SpawnThread(_, args) => {
                for arg in args {
                    self.rewrite_expr(arg, var_types, subst, None)?;
                }
            }
            Expr::Propagate(inner) | Expr::Raise(inner) => {
                self.rewrite_expr(inner, var_types, subst, None)?;
            }
            Expr::TryExpr(body) => {
                let mut inner_types = var_types.clone();
                for stmt in body {
                    let _ = self.rewrite_stmt(stmt, &mut inner_types, subst, None);
                }
            }
            Expr::Ident(name) => {
                // Generic function used as a first-class value: instantiate from expected func type.
                if self.generic_defs.contains_key(name) {
                    if let Some(expected) = expected {
                        if let Some(instance_name) =
                            self.instantiate_as_value(name, expected, subst)
                        {
                            *expr = Expr::Ident(instance_name);
                        }
                    }
                } else if name.starts_with('@') {
                    // Module-qualified: @mod_func — base name after first '_'
                    if let Some(base) = name.splitn(2, '_').nth(1) {
                        if self.generic_defs.contains_key(base) {
                            if let Some(expected) = expected {
                                if let Some(instance_base) =
                                    self.instantiate_as_value(base, expected, subst)
                                {
                                    // @mod_id + @I... suffix from id@I... → @mod_id@I...
                                    if let Some(suffix) = instance_base.strip_prefix(base) {
                                        *expr = Expr::Ident(format!("{}{}", name, suffix));
                                    } else {
                                        *expr = Expr::Ident(instance_base);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // 其余表达式没有子表达式。
            _ => {}
        }
        Ok(())
    }

    /// Expected types for call arguments from the callee's known signature.
    fn expected_arg_types(
        &self,
        callee: &Expr,
        var_types: &HashMap<String, Type>,
    ) -> Vec<Type> {
        match callee {
            Expr::Ident(name) => {
                if let Some((params, _)) = self.func_sigs.get(name) {
                    return params.clone();
                }
                if let Some(gen_def) = self.generic_defs.get(name) {
                    return gen_def.params.iter().map(|p| p.ty.clone()).collect();
                }
                if let Some(Type::FuncSig(params, _)) = var_types.get(name) {
                    return params.clone();
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Instantiate a generic function as a value given an expected `func(...)` type.
    fn instantiate_as_value(
        &mut self,
        name: &str,
        expected: &Type,
        outer_subst: &HashMap<String, Type>,
    ) -> Option<String> {
        let (exp_params, exp_ret) = match expected {
            Type::FuncSig(params, ret) => (params.clone(), ret.clone()),
            Type::Func => return None,
            _ => return None,
        };
        let gen_def = self.generic_defs.get(name)?.clone();
        if gen_def.params.len() != exp_params.len() {
            return None;
        }
        let mut bindings: HashMap<String, Type> = HashMap::new();
        for (param, exp_ty) in gen_def.params.iter().zip(exp_params.iter()) {
            let param_ty = substitute_type(&param.ty, outer_subst);
            unify_types(&param_ty, exp_ty, &mut bindings);
        }
        if let (Some(gen_ret), Some(exp_r)) = (&gen_def.return_type, exp_ret.as_ref()) {
            let gen_ret = substitute_type(gen_ret, outer_subst);
            unify_types(&gen_ret, exp_r.as_ref(), &mut bindings);
        }
        let type_args: Vec<Type> = gen_def
            .type_params
            .iter()
            .map(|tp| bindings.get(tp).cloned().unwrap_or(Type::Dynamic))
            .collect();
        if type_args.iter().any(|t| matches!(t, Type::Dynamic)) {
            return None;
        }
        self.create_instance(name, &gen_def, &type_args, outer_subst)
            .ok()
    }

    fn instantiate_call(
        &mut self,
        name: &str,
        args: &[Expr],
        var_types: &HashMap<String, Type>,
        outer_subst: &HashMap<String, Type>,
    ) -> Result<String, String> {
        let gen_def = self.generic_defs.get(name).cloned().unwrap();
        let type_args = self.infer_generic_type_args(&gen_def, args, 0, var_types, outer_subst);
        self.create_instance(name, &gen_def, &type_args, outer_subst)
    }

    fn instantiate_method_call(
        &mut self,
        key: &str,
        args: &[Expr],
        var_types: &HashMap<String, Type>,
        outer_subst: &HashMap<String, Type>,
    ) -> Result<String, String> {
        let gen_def = self.generic_defs.get(key).cloned().unwrap();
        // Skip synthetic self when unifying call-site args.
        let type_args = self.infer_generic_type_args(&gen_def, args, 1, var_types, outer_subst);
        // Mangle as Class_method@... so it looks like a normal free function.
        let base = key.replace("::", "_");
        self.create_instance(&base, &gen_def, &type_args, outer_subst)
    }

    fn instantiate_module_call(
        &mut self,
        full_name: &str,
        base_name: &str,
        args: &[Expr],
        var_types: &HashMap<String, Type>,
        outer_subst: &HashMap<String, Type>,
    ) -> Result<String, String> {
        let gen_def = self.generic_defs.get(base_name).cloned().unwrap();
        let type_args = self.infer_generic_type_args(&gen_def, args, 0, var_types, outer_subst);
        let instance_base = self.create_instance(base_name, &gen_def, &type_args, outer_subst)?;
        // 保留模块前缀：@module_func@... -> @module_func@...
        Ok(format!(
            "{}@{}",
            full_name.split('@').next().unwrap_or(full_name),
            instance_base
                .split('@')
                .skip(1)
                .collect::<Vec<_>>()
                .join("@")
        ))
    }

    fn infer_generic_type_args(
        &self,
        gen_def: &FuncDef,
        args: &[Expr],
        param_skip: usize,
        var_types: &HashMap<String, Type>,
        outer_subst: &HashMap<String, Type>,
    ) -> Vec<Type> {
        let mut bindings: HashMap<String, Type> = HashMap::new();
        for (param, arg) in gen_def.params.iter().skip(param_skip).zip(args.iter()) {
            let param_ty = substitute_type(&param.ty, outer_subst);
            let arg_ty = self.infer_expr_type(arg, var_types);
            unify_types(&param_ty, &arg_ty, &mut bindings);
        }
        // Also unify return type generics when possible via func_sigs of named args — already
        // handled through FuncSig param unification when arg types carry signatures.
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
    ) -> Result<String, String> {
        self.check_trait_bounds(base_name, gen_def, type_args)?;
        let instance_name = mangle_name(base_name, type_args);
        if !self.instances.contains_key(&instance_name) {
            let mut instance = gen_def.clone();
            instance.name = instance_name.clone();
            instance.type_params.clear();
            instance.trait_bounds.clear();

            let mut full_subst = outer_subst.clone();
            for (tp, ta) in gen_def.type_params.iter().zip(type_args.iter()) {
                full_subst.insert(tp.clone(), ta.clone());
            }

            substitute_func_def(&mut instance, &full_subst);
            self.func_sigs.insert(
                instance_name.clone(),
                (
                    instance.params.iter().map(|p| p.ty.clone()).collect(),
                    instance.return_type.clone(),
                ),
            );
            self.instances.insert(instance_name.clone(), instance);
            self.pending.push(instance_name.clone());
        }
        Ok(instance_name)
    }

    fn check_trait_bounds(
        &self,
        base_name: &str,
        gen_def: &FuncDef,
        type_args: &[Type],
    ) -> Result<(), String> {
        if gen_def.trait_bounds.is_empty() {
            return Ok(());
        }
        for (tp, ta) in gen_def.type_params.iter().zip(type_args.iter()) {
            let Some((_, bounds)) = gen_def.trait_bounds.iter().find(|(n, _)| n == tp) else {
                continue;
            };
            for trait_name in bounds {
                self.ensure_implements(ta, trait_name).map_err(|e| {
                    format!(
                        "when monomorphizing '{}': type parameter '{}' = {}: {}",
                        base_name,
                        tp,
                        type_display(ta),
                        e
                    )
                })?;
            }
        }
        Ok(())
    }

    fn ensure_implements(&self, ty: &Type, trait_name: &str) -> Result<(), String> {
        let type_name = match ty {
            Type::Custom(n) => n.as_str(),
            Type::Dyn(n) => {
                // dyn Trait 本身即该 trait 对象
                if n == trait_name {
                    return Ok(());
                }
                // dyn Child 可能需要父 trait 约束 —— 简化：对象类型名匹配或拒绝
                return Err(format!(
                    "dyn '{}' does not satisfy trait bound '{}'",
                    n, trait_name
                ));
            }
            Type::Adt(n, _) => n.as_str(),
            Type::Dynamic => return Ok(()),
            other => {
                return Err(format!(
                    "type '{}' does not implement trait '{}' (only class types can)",
                    type_display(other),
                    trait_name
                ));
            }
        };
        // `__Dyn_Trait` 合成类视为实现该 trait
        if let Some(t) = bolide_parser::dyn_trait_from_class_name(type_name) {
            if t == trait_name {
                return Ok(());
            }
        }
        if self.class_has_trait(type_name, trait_name) {
            Ok(())
        } else {
            Err(format!(
                "type '{}' does not implement trait '{}'; add `impl {} for {} {{ ... }}`",
                type_name, trait_name, trait_name, type_name
            ))
        }
    }

    fn class_has_trait(&self, type_name: &str, trait_name: &str) -> bool {
        let mut current = Some(type_name.to_string());
        while let Some(name) = current {
            if let Some(impls) = self.type_traits.get(&name) {
                if impls.iter().any(|t| t == trait_name) {
                    return true;
                }
            }
            // 协议方法：本类有对应方法也算（继承链上查方法）
            if protocol_methods(trait_name).map_or(false, |ms| {
                ms.iter().all(|m| self.class_has_method(&name, m))
            }) {
                return true;
            }
            current = self.class_parents.get(&name).cloned();
        }
        false
    }

    fn class_has_method(&self, type_name: &str, method: &str) -> bool {
        let mut current = Some(type_name.to_string());
        while let Some(name) = current {
            if self
                .class_methods
                .get(&name)
                .map_or(false, |ms| ms.contains(method))
            {
                return true;
            }
            current = self.class_parents.get(&name).cloned();
        }
        false
    }

    fn infer_expr_type(&self, expr: &Expr, var_types: &HashMap<String, Type>) -> Type {
        match expr {
            Expr::Int(_) => Type::Int,
            Expr::Float(_) => Type::Float,
            Expr::Bool(_) => Type::Bool,
            Expr::String(_) => Type::Str,
            Expr::BigInt(_) => Type::BigInt,
            Expr::Decimal(_) => Type::Decimal,
            Expr::NullPtr => Type::Ptr,
            Expr::Ident(name) => {
                if let Some(ty) = var_types.get(name) {
                    return ty.clone();
                }
                // Function values used as generic args: recover signature when known.
                if let Some((params, ret)) = self.func_sigs.get(name) {
                    return Type::FuncSig(params.clone(), ret.clone().map(Box::new));
                }
                Type::Int
            }
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

            // class 构造器 Class(...)
            if self.class_names.contains(name) {
                return Type::Custom(name.clone());
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

/// 内置协议 trait 所需方法
fn protocol_methods(trait_name: &str) -> Option<&'static [&'static str]> {
    match trait_name {
        "Add" => Some(&["__add__"]),
        "Sub" => Some(&["__sub__"]),
        "Mul" => Some(&["__mul__"]),
        "Div" => Some(&["__div__"]),
        "Mod" => Some(&["__mod__"]),
        "Eq" => Some(&["__eq__"]),
        "Ord" => Some(&["__lt__"]),
        "BitAnd" => Some(&["__and__"]),
        "BitOr" => Some(&["__or__"]),
        "BitXor" => Some(&["__xor__"]),
        "Shl" => Some(&["__lshift__"]),
        "Shr" => Some(&["__rshift__"]),
        "Neg" => Some(&["__neg__"]),
        "Not" => Some(&["__not__"]),
        "Iterator" => Some(&["next"]),
        _ => None,
    }
}

fn type_display(ty: &Type) -> String {
    match ty {
        Type::Int => "int".into(),
        Type::Float => "float".into(),
        Type::Bool => "bool".into(),
        Type::Str => "str".into(),
        Type::Custom(n) => n.clone(),
        Type::Dyn(n) => format!("dyn {}", n),
        Type::Adt(n, args) if args.is_empty() => n.clone(),
        Type::Adt(n, args) => format!(
            "{}<{}>",
            n,
            args.iter().map(type_display).collect::<Vec<_>>().join(", ")
        ),
        Type::Generic(n) => n.clone(),
        Type::List(e) => format!("list<{}>", type_display(e)),
        Type::Dynamic => "dynamic".into(),
        other => format!("{:?}", other),
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
        Type::Dyn(name) => format!("dyn_{}", name),
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
        Type::Dyn(name) => Type::Dyn(name.clone()),
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
