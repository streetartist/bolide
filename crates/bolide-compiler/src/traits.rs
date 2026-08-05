//! Trait 与 class mixin 脱糖
//!
//! ## Trait
//! ```bolide
//! trait Drawable {
//!     fn draw();
//!     fn label() -> str { return "item"; }
//! }
//! impl Drawable for Circle {
//!     fn draw() { print(self.r); }
//! }
//! ```
//! 展开为：把方法注入目标 class（缺省用 trait 默认实现）。
//!
//! ## dyn Trait（运行时多态）
//! ```bolide
//! fn paint(d: dyn Drawable) { d.draw(); }
//! paint(Circle(3));  // class tag 分派
//! ```
//! 为每个 trait 生成合成类 `__Dyn_Trait`（仅承载方法签名 / 默认体），
//! `Type::Dyn("T")` 改写为 `Type::Custom("__Dyn_T")`。运行时仍是普通对象指针，
//! 方法调用走现有 class-tag 分派到真实实现类。
//!
//! ## Supertrait
//! ```bolide
//! trait Countable: Drawable { fn count() -> int; }
//! ```
//! `impl Countable for C` 要求 C 已实现（或同时满足）Drawable。
//!
//! ## 多继承（安全子集）
//! ```bolide
//! class Child: Primary, Mixin1, Mixin2 { }
//! ```
//! - **Primary**：唯一可贡献字段的父类（布局 + `super`）
//! - **Mixin***：必须无字段；方法复制进子类（以子类 `self` 布局编译）
//! - 两 mixin 同名方法且子类未覆盖 → 编译错误（强制显式消歧）
//!
//! ## 运算符 / Iterator 协议
//! 类上若存在对应 dunder / `next`，自动记入 `impl_traits`，供 `T: Add` 等约束使用。

use bolide_parser::{
    dyn_trait_class_name, ClassDef, Expr, FuncDef, Param, Program, Statement, TraitDef, TraitImpl,
    Type,
};
use std::collections::{HashMap, HashSet};

/// 运算符 / 协议 trait 名 → 满足条件的方法名
const PROTOCOL_TRAITS: &[(&str, &[&str])] = &[
    ("Add", &["__add__"]),
    ("Sub", &["__sub__"]),
    ("Mul", &["__mul__"]),
    ("Div", &["__div__"]),
    ("Mod", &["__mod__"]),
    ("Eq", &["__eq__"]),
    ("Ord", &["__lt__"]),
    ("BitAnd", &["__and__"]),
    ("BitOr", &["__or__"]),
    ("BitXor", &["__xor__"]),
    ("Shl", &["__lshift__"]),
    ("Shr", &["__rshift__"]),
    ("Neg", &["__neg__"]),
    ("Not", &["__not__"]),
    // 任意提供 next() 的类型视为 Iterator（for-in 协议）
    ("Iterator", &["next"]),
];

/// 展开 trait/impl 与 mixin 方法合并；剥离 TraitDef/TraitImpl 语句；
/// 注入 `__Dyn_*` 合成类并改写 `dyn T` 类型。
pub fn desugar_traits(program: Program) -> Result<Program, String> {
    let mut traits: HashMap<String, TraitDef> = HashMap::new();
    let mut impls: Vec<TraitImpl> = Vec::new();
    let mut classes: HashMap<String, ClassDef> = HashMap::new();
    let mut class_order: Vec<String> = Vec::new();
    let mut other: Vec<Statement> = Vec::new();

    for stmt in program.statements {
        match stmt {
            Statement::TraitDef(t) => {
                if traits.contains_key(&t.name) {
                    return Err(format!("duplicate trait '{}'", t.name));
                }
                traits.insert(t.name.clone(), t);
            }
            Statement::TraitImpl(i) => impls.push(i),
            Statement::ClassDef(c) => {
                if classes.contains_key(&c.name) {
                    return Err(format!("duplicate class '{}'", c.name));
                }
                class_order.push(c.name.clone());
                classes.insert(c.name.clone(), c);
            }
            other_stmt => other.push(other_stmt),
        }
    }

    // 校验 supertrait 引用
    for t in traits.values() {
        for s in &t.supers {
            if !traits.contains_key(s) && !is_builtin_protocol_trait(s) {
                return Err(format!(
                    "trait '{}': unknown supertrait '{}'",
                    t.name, s
                ));
            }
        }
    }

    // 1) trait impl → 注入 class 方法
    for imp in &impls {
        apply_trait_impl(&traits, &mut classes, imp)?;
    }

    // 1b) supertrait：impl Child 时要求已有 Parent（含传递闭包）
    for imp in &impls {
        if let Some(tr) = traits.get(&imp.trait_name) {
            check_supertraits(tr, &imp.type_name, &traits, &classes)?;
        }
    }

    // 1c) 运算符 / Iterator 协议：按方法自动登记 impl_traits
    for class in classes.values_mut() {
        auto_register_protocol_traits(class);
    }

    // 2) mixin 方法并入（在 trait 注入之后，子类已有方法优先）
    let names: Vec<String> = class_order.clone();
    for name in &names {
        merge_mixins_into_class(&mut classes, name)?;
    }

    // 3) 为每个用户 trait 注入 `__Dyn_Trait` 合成类（方法签名 + 默认/桩实现）
    let mut dyn_classes: Vec<ClassDef> = Vec::new();
    for t in traits.values() {
        let dyn_name = dyn_trait_class_name(&t.name);
        if classes.contains_key(&dyn_name) {
            return Err(format!(
                "class name '{}' conflicts with dyn trait object for '{}'",
                dyn_name, t.name
            ));
        }
        dyn_classes.push(build_dyn_class(t, &traits)?);
    }
    // 内置协议 trait 若用户未定义，也生成空的 dyn 类（便于 `dyn Iterator` 注解）
    for (name, _) in PROTOCOL_TRAITS {
        if traits.contains_key(*name) {
            continue;
        }
        let dyn_name = dyn_trait_class_name(name);
        if classes.contains_key(&dyn_name) {
            continue;
        }
        // 若用户代码用了 dyn Iterator 但未定义 trait，合成最小桩
        dyn_classes.push(ClassDef {
            name: dyn_name,
            parent: None,
            mixins: vec![],
            fields: vec![],
            methods: vec![stub_method(
                "next",
                vec![],
                Some(Type::Adt("Option".into(), vec![Type::Dynamic])),
            )],
            attrs: vec![],
            impl_traits: vec![(*name).to_string()],
        });
    }

    // 4) 重建程序
    let mut out = Vec::new();
    for name in &class_order {
        if let Some(c) = classes.remove(name) {
            out.push(Statement::ClassDef(c));
        }
    }
    for c in classes.into_values() {
        out.push(Statement::ClassDef(c));
    }
    for c in dyn_classes {
        out.push(Statement::ClassDef(c));
    }
    out.extend(other);

    // 5) 全程序改写 Type::Dyn(t) → Type::Custom(__Dyn_t)
    let mut program = Program { statements: out };
    rewrite_dyn_types(&mut program);

    Ok(program)
}

fn is_builtin_protocol_trait(name: &str) -> bool {
    PROTOCOL_TRAITS.iter().any(|(n, _)| *n == name)
}

fn check_supertraits(
    tr: &TraitDef,
    type_name: &str,
    traits: &HashMap<String, TraitDef>,
    classes: &HashMap<String, ClassDef>,
) -> Result<(), String> {
    let class = classes.get(type_name).ok_or_else(|| {
        format!(
            "impl {} for {}: type '{}' not found",
            tr.name, type_name, type_name
        )
    })?;
    for s in &tr.supers {
        if class_implements(class, s, classes) {
            continue;
        }
        // 内置协议：按方法存在判断
        if protocol_satisfied(class, s) {
            continue;
        }
        // super 也可能有自己的 super
        if let Some(st) = traits.get(s) {
            // 若父 trait 全部方法已在 class 上（含默认注入），也算满足
            if supertrait_methods_present(st, class, traits) {
                continue;
            }
        }
        return Err(format!(
            "impl {} for {}: missing supertrait '{}' (add `impl {} for {}` or implement its methods)",
            tr.name, type_name, s, s, type_name
        ));
    }
    Ok(())
}

fn class_implements(
    class: &ClassDef,
    trait_name: &str,
    classes: &HashMap<String, ClassDef>,
) -> bool {
    if class.impl_traits.iter().any(|t| t == trait_name) {
        return true;
    }
    // 沿主父类链查找（继承的 impl）
    if let Some(ref p) = class.parent {
        if let Some(parent) = classes.get(p) {
            return class_implements(parent, trait_name, classes);
        }
    }
    false
}

fn supertrait_methods_present(
    tr: &TraitDef,
    class: &ClassDef,
    traits: &HashMap<String, TraitDef>,
) -> bool {
    for s in &tr.supers {
        if let Some(st) = traits.get(s) {
            if !supertrait_methods_present(st, class, traits) {
                return false;
            }
        }
    }
    for tm in &tr.methods {
        if !class.methods.iter().any(|m| m.name == tm.func.name) {
            return false;
        }
    }
    true
}

fn protocol_satisfied(class: &ClassDef, trait_name: &str) -> bool {
    if let Some((_, methods)) = PROTOCOL_TRAITS.iter().find(|(n, _)| *n == trait_name) {
        return methods
            .iter()
            .all(|m| class.methods.iter().any(|cm| cm.name == *m));
    }
    false
}

fn auto_register_protocol_traits(class: &mut ClassDef) {
    for (trait_name, methods) in PROTOCOL_TRAITS {
        let has_all = methods
            .iter()
            .all(|m| class.methods.iter().any(|cm| cm.name == *m));
        if has_all && !class.impl_traits.iter().any(|t| t == *trait_name) {
            class.impl_traits.push((*trait_name).to_string());
        }
    }
}

fn apply_trait_impl(
    traits: &HashMap<String, TraitDef>,
    classes: &mut HashMap<String, ClassDef>,
    imp: &TraitImpl,
) -> Result<(), String> {
    // 允许对内置协议 trait 写 impl（即使未显式 trait 定义）
    let tr_owned;
    let tr = if let Some(t) = traits.get(&imp.trait_name) {
        t
    } else if is_builtin_protocol_trait(&imp.trait_name) {
        tr_owned = TraitDef {
            name: imp.trait_name.clone(),
            supers: vec![],
            methods: vec![],
            attrs: vec![],
        };
        &tr_owned
    } else {
        return Err(format!(
            "impl {}: trait '{}' not found",
            imp.type_name, imp.trait_name
        ));
    };

    let class = classes.get_mut(&imp.type_name).ok_or_else(|| {
        format!(
            "impl {} for {}: type '{}' is not a class (trait impl currently supports class only)",
            imp.trait_name, imp.type_name, imp.type_name
        )
    })?;

    if !class.impl_traits.iter().any(|t| t == &imp.trait_name) {
        class.impl_traits.push(imp.trait_name.clone());
    }

    let mut provided: HashMap<String, FuncDef> = HashMap::new();
    for m in &imp.methods {
        if provided.insert(m.name.clone(), m.clone()).is_some() {
            return Err(format!(
                "impl {} for {}: duplicate method '{}'",
                imp.trait_name, imp.type_name, m.name
            ));
        }
    }

    // 校验必需方法
    for tm in &tr.methods {
        if !tm.has_default && !provided.contains_key(&tm.func.name) {
            if !class.methods.iter().any(|m| m.name == tm.func.name) {
                return Err(format!(
                    "impl {} for {}: missing required method '{}'",
                    imp.trait_name, imp.type_name, tm.func.name
                ));
            }
        }
    }

    // 注入：impl 提供的优先；否则 trait 默认；class 已有则不覆盖
    let existing: HashSet<String> = class.methods.iter().map(|m| m.name.clone()).collect();

    for tm in &tr.methods {
        let name = &tm.func.name;
        if existing.contains(name) {
            continue;
        }
        if let Some(m) = provided.get(name) {
            class.methods.push(m.clone());
        } else if tm.has_default {
            class.methods.push(tm.func.clone());
        }
    }

    // impl 中多写的方法（非 trait 成员）也注入，方便辅助方法
    for (name, m) in &provided {
        if !class.methods.iter().any(|cm| cm.name == *name) {
            class.methods.push(m.clone());
        }
    }

    Ok(())
}

/// 构建 `__Dyn_Trait`：包含 trait（及 supertrait）全部方法
fn build_dyn_class(
    tr: &TraitDef,
    traits: &HashMap<String, TraitDef>,
) -> Result<ClassDef, String> {
    let mut methods: HashMap<String, FuncDef> = HashMap::new();
    collect_trait_methods(tr, traits, &mut methods, &mut HashSet::new())?;

    Ok(ClassDef {
        name: dyn_trait_class_name(&tr.name),
        parent: None,
        mixins: vec![],
        fields: vec![],
        methods: methods.into_values().collect(),
        attrs: vec![],
        impl_traits: vec![tr.name.clone()],
    })
}

fn collect_trait_methods(
    tr: &TraitDef,
    traits: &HashMap<String, TraitDef>,
    out: &mut HashMap<String, FuncDef>,
    visiting: &mut HashSet<String>,
) -> Result<(), String> {
    if !visiting.insert(tr.name.clone()) {
        return Err(format!("cycle in trait hierarchy involving '{}'", tr.name));
    }
    for s in &tr.supers {
        if let Some(st) = traits.get(s) {
            collect_trait_methods(st, traits, out, visiting)?;
        }
    }
    for tm in &tr.methods {
        if out.contains_key(&tm.func.name) {
            continue; // 子 trait 覆盖父默认时，先收集父再被子覆盖
        }
        if tm.has_default {
            out.insert(tm.func.name.clone(), tm.func.clone());
        } else {
            out.insert(
                tm.func.name.clone(),
                make_stub_from_sig(&tm.func),
            );
        }
    }
    // 子 trait 方法覆盖
    for tm in &tr.methods {
        if tm.has_default {
            out.insert(tm.func.name.clone(), tm.func.clone());
        }
    }
    visiting.remove(&tr.name);
    Ok(())
}

fn make_stub_from_sig(f: &FuncDef) -> FuncDef {
    stub_method(
        &f.name,
        f.params.clone(),
        f.return_type.clone(),
    )
}

fn stub_method(name: &str, params: Vec<Param>, return_type: Option<Type>) -> FuncDef {
    let body = stub_body(&return_type);
    FuncDef {
        name: name.to_string(),
        is_async: false,
        is_export: false,
        is_inline: false,
        type_params: vec![],
        trait_bounds: vec![],
        params,
        throws: vec![],
        return_type,
        lifetime_deps: None,
        body,
        def_span_start: None,
        attrs: vec![],
    }
}

fn stub_body(return_type: &Option<Type>) -> Vec<Statement> {
    match return_type {
        None => vec![],
        Some(Type::Int) | Some(Type::Bool) => {
            vec![Statement::Return(Some(Expr::Int(0)))]
        }
        Some(Type::Float) => vec![Statement::Return(Some(Expr::Float(0.0)))],
        Some(Type::Str) => {
            vec![Statement::Return(Some(Expr::String(String::new())))]
        }
        Some(Type::Dynamic) => {
            // 用 none 不合适；返回 0 装箱? 简化返回 int 0 不够。
            // 动态：返回空字符串作为弱默认（用户不应调用桩）
            vec![Statement::Return(Some(Expr::Int(0)))]
        }
        Some(Type::Adt(name, _)) if name == "Option" => {
            // Option.None
            vec![Statement::Return(Some(Expr::Call(
                Box::new(Expr::Member(
                    Box::new(Expr::Ident("Option".into())),
                    "None".into(),
                )),
                vec![],
            )))]
        }
        Some(Type::Adt(name, _)) if name == "Result" => {
            vec![Statement::Return(Some(Expr::Call(
                Box::new(Expr::Member(
                    Box::new(Expr::Ident("Result".into())),
                    "Err".into(),
                )),
                vec![Expr::String("dyn trait stub".into())],
            )))]
        }
        Some(Type::List(_)) => {
            vec![Statement::Return(Some(Expr::List(vec![])))]
        }
        // 其它引用类型：返回 0（空指针语义，仅桩，不应被执行）
        _ => vec![Statement::Return(Some(Expr::Int(0)))],
    }
}

fn merge_mixins_into_class(
    classes: &mut HashMap<String, ClassDef>,
    class_name: &str,
) -> Result<(), String> {
    let mixins = classes
        .get(class_name)
        .map(|c| c.mixins.clone())
        .unwrap_or_default();
    if mixins.is_empty() {
        return Ok(());
    }

    let mut merged: Vec<FuncDef> = Vec::new();
    let mut source: HashMap<String, String> = HashMap::new();

    for mixin_name in &mixins {
        validate_fieldless_mixin(classes, mixin_name)?;
        let methods = collect_fieldless_methods(classes, mixin_name, &mut HashSet::new())?;
        for m in methods {
            if let Some(prev) = source.get(&m.name) {
                if prev != mixin_name {
                    let child_has = classes
                        .get(class_name)
                        .map(|c| c.methods.iter().any(|cm| cm.name == m.name))
                        .unwrap_or(false);
                    if !child_has {
                        return Err(format!(
                            "class '{}': method '{}' inherited from both mixin '{}' and '{}'; override it in '{}' to disambiguate",
                            class_name, m.name, prev, mixin_name, class_name
                        ));
                    }
                }
            } else {
                source.insert(m.name.clone(), mixin_name.clone());
                merged.push(m);
            }
        }
    }

    let class = classes
        .get_mut(class_name)
        .ok_or_else(|| format!("class '{}' not found", class_name))?;
    let existing: HashSet<String> = class.methods.iter().map(|m| m.name.clone()).collect();
    for m in merged {
        if !existing.contains(&m.name) {
            class.methods.push(m);
        }
    }
    Ok(())
}

fn validate_fieldless_mixin(
    classes: &HashMap<String, ClassDef>,
    name: &str,
) -> Result<(), String> {
    let c = classes
        .get(name)
        .ok_or_else(|| format!("mixin/parent class '{}' not found", name))?;
    if !c.fields.is_empty() {
        return Err(format!(
            "class used as mixin must have no fields (got '{}' with {} field(s)); only the first parent may define layout fields",
            name,
            c.fields.len()
        ));
    }
    if let Some(ref p) = c.parent {
        validate_fieldless_chain(classes, p)?;
    }
    for m in &c.mixins {
        validate_fieldless_mixin(classes, m)?;
    }
    Ok(())
}

fn validate_fieldless_chain(
    classes: &HashMap<String, ClassDef>,
    name: &str,
) -> Result<(), String> {
    let c = classes
        .get(name)
        .ok_or_else(|| format!("class '{}' not found", name))?;
    if !c.fields.is_empty() {
        return Err(format!(
            "mixin parent '{}' has fields; mixin hierarchy must be fieldless",
            name
        ));
    }
    if let Some(ref p) = c.parent {
        validate_fieldless_chain(classes, p)?;
    }
    Ok(())
}

fn collect_fieldless_methods(
    classes: &HashMap<String, ClassDef>,
    name: &str,
    visiting: &mut HashSet<String>,
) -> Result<Vec<FuncDef>, String> {
    if !visiting.insert(name.to_string()) {
        return Err(format!("cycle in mixin hierarchy involving '{}'", name));
    }
    let c = classes
        .get(name)
        .ok_or_else(|| format!("class '{}' not found", name))?;

    let mut out = Vec::new();
    if let Some(ref p) = c.parent {
        out.extend(collect_fieldless_methods(classes, p, visiting)?);
    }
    for m in &c.mixins {
        out.extend(collect_fieldless_methods(classes, m, visiting)?);
    }
    let mut by_name: HashMap<String, FuncDef> = HashMap::new();
    for m in out {
        by_name.insert(m.name.clone(), m);
    }
    for m in &c.methods {
        by_name.insert(m.name.clone(), m.clone());
    }
    visiting.remove(name);
    Ok(by_name.into_values().collect())
}

// ---------------------------------------------------------------------------
// Type::Dyn → Type::Custom("__Dyn_…")
// ---------------------------------------------------------------------------

fn rewrite_dyn_types(program: &mut Program) {
    for stmt in &mut program.statements {
        rewrite_stmt_types(stmt);
    }
}

fn rewrite_type(ty: &mut Type) {
    match ty {
        Type::Dyn(name) => {
            *ty = Type::Custom(dyn_trait_class_name(name));
        }
        Type::List(inner) | Type::Channel(inner) | Type::Weak(inner) | Type::Unowned(inner) => {
            rewrite_type(inner);
        }
        Type::Dict(k, v) => {
            rewrite_type(k);
            rewrite_type(v);
        }
        Type::Tuple(ts) => {
            for t in ts {
                rewrite_type(t);
            }
        }
        Type::FuncSig(params, ret) => {
            for p in params {
                rewrite_type(p);
            }
            if let Some(r) = ret {
                rewrite_type(r);
            }
        }
        Type::Adt(_, args) => {
            for a in args {
                rewrite_type(a);
            }
        }
        _ => {}
    }
}

fn rewrite_stmt_types(stmt: &mut Statement) {
    match stmt {
        Statement::FuncDef(f) => rewrite_func_types(f),
        Statement::ComptimeFn(cf) => {
            for (_, t) in &mut cf.params {
                rewrite_type(t);
            }
            if let Some(ref mut r) = cf.return_type {
                rewrite_type(r);
            }
            for s in &mut cf.body {
                rewrite_stmt_types(s);
            }
        }
        Statement::ClassDef(c) => {
            for m in &mut c.methods {
                rewrite_func_types(m);
            }
            for field in &mut c.fields {
                rewrite_type(&mut field.ty);
                if let Some(ref mut d) = field.default_value {
                    rewrite_expr_types(d);
                }
            }
        }
        Statement::VarDecl(d) => {
            if let Some(ref mut t) = d.ty {
                rewrite_type(t);
            }
            if let Some(ref mut v) = d.value {
                rewrite_expr_types(v);
            }
        }
        Statement::Assign(a) => {
            rewrite_expr_types(&mut a.target);
            rewrite_expr_types(&mut a.value);
        }
        Statement::Expr(e) | Statement::Throw(e) | Statement::Yield(e) => rewrite_expr_types(e),
        Statement::Return(Some(e)) => rewrite_expr_types(e),
        Statement::If(i) => {
            rewrite_expr_types(&mut i.condition);
            for s in &mut i.then_body {
                rewrite_stmt_types(s);
            }
            for (cond, body) in &mut i.elif_branches {
                rewrite_expr_types(cond);
                for s in body {
                    rewrite_stmt_types(s);
                }
            }
            if let Some(ref mut else_body) = i.else_body {
                for s in else_body {
                    rewrite_stmt_types(s);
                }
            }
        }
        Statement::While(w) => {
            rewrite_expr_types(&mut w.condition);
            for s in &mut w.body {
                rewrite_stmt_types(s);
            }
        }
        Statement::For(f) => {
            rewrite_expr_types(&mut f.iter);
            for s in &mut f.body {
                rewrite_stmt_types(s);
            }
        }
        Statement::Match(m) => {
            rewrite_expr_types(&mut m.expr);
            for arm in &mut m.arms {
                for s in &mut arm.body {
                    rewrite_stmt_types(s);
                }
            }
        }
        Statement::Try(t) => {
            for s in &mut t.try_body {
                rewrite_stmt_types(s);
            }
            for c in &mut t.catch_clauses {
                rewrite_type(&mut c.ty);
                for s in &mut c.body {
                    rewrite_stmt_types(s);
                }
            }
            if let Some(ref mut fin) = t.finally {
                for s in fin {
                    rewrite_stmt_types(s);
                }
            }
        }
        Statement::With(w) => {
            for item in &mut w.items {
                rewrite_expr_types(&mut item.expr);
            }
            for s in &mut w.body {
                rewrite_stmt_types(s);
            }
        }
        Statement::EnumDef(e) => {
            for v in &mut e.variants {
                for f in &mut v.fields {
                    rewrite_type(&mut f.ty);
                }
            }
        }
        Statement::ValueDef(v) => {
            for f in &mut v.fields {
                rewrite_type(&mut f.ty);
            }
        }
        Statement::Pool(p) => {
            rewrite_expr_types(&mut p.size);
            for s in &mut p.body {
                rewrite_stmt_types(s);
            }
        }
        Statement::AwaitScope(a) => {
            for s in &mut a.body {
                rewrite_stmt_types(s);
            }
        }
        Statement::Select(sel) => {
            for b in &mut sel.branches {
                match b {
                    bolide_parser::SelectBranch::Recv { body, .. }
                    | bolide_parser::SelectBranch::Default { body } => {
                        for s in body {
                            rewrite_stmt_types(s);
                        }
                    }
                    bolide_parser::SelectBranch::Timeout { duration, body } => {
                        rewrite_expr_types(duration);
                        for s in body {
                            rewrite_stmt_types(s);
                        }
                    }
                }
            }
        }
        Statement::SpawnSelect(ss) => {
            for b in &mut ss.branches {
                match b {
                    bolide_parser::SpawnSelectBranch::Bind { expr, body, .. }
                    | bolide_parser::SpawnSelectBranch::Expr { expr, body } => {
                        rewrite_expr_types(expr);
                        for s in body {
                            rewrite_stmt_types(s);
                        }
                    }
                }
            }
        }
        Statement::MacroRep { body, .. } => {
            for s in body {
                rewrite_stmt_types(s);
            }
        }
        _ => {}
    }
}

fn rewrite_func_types(f: &mut FuncDef) {
    for p in &mut f.params {
        rewrite_type(&mut p.ty);
        if let Some(ref mut d) = p.default_value {
            rewrite_expr_types(d);
        }
    }
    if let Some(ref mut r) = f.return_type {
        rewrite_type(r);
    }
    for t in &mut f.throws {
        rewrite_type(t);
    }
    for s in &mut f.body {
        rewrite_stmt_types(s);
    }
}

fn rewrite_expr_types(expr: &mut Expr) {
    match expr {
        Expr::Closure {
            params,
            return_type,
            body,
        } => {
            for p in params {
                rewrite_type(&mut p.ty);
            }
            if let Some(ref mut r) = return_type {
                rewrite_type(r);
            }
            for s in body {
                rewrite_stmt_types(s);
            }
        }
        Expr::Call(c, args) => {
            rewrite_expr_types(c);
            for a in args {
                rewrite_expr_types(a);
            }
        }
        Expr::Spawn(_, args) | Expr::SpawnThread(_, args) => {
            for a in args {
                rewrite_expr_types(a);
            }
        }
        Expr::Member(base, _)
        | Expr::Await(base)
        | Expr::Propagate(base)
        | Expr::UnaryOp(_, base)
        | Expr::NamedArg(_, base)
        | Expr::SpreadArg(base)
        | Expr::KwSpreadArg(base)
        | Expr::Raise(base) => rewrite_expr_types(base),
        Expr::BinOp(l, _, r) | Expr::Index(l, r) => {
            rewrite_expr_types(l);
            rewrite_expr_types(r);
        }
        Expr::Slice(base, start, end, step) => {
            rewrite_expr_types(base);
            if let Some(e) = start {
                rewrite_expr_types(e);
            }
            if let Some(e) = end {
                rewrite_expr_types(e);
            }
            if let Some(e) = step {
                rewrite_expr_types(e);
            }
        }
        Expr::List(items) | Expr::Tuple(items) | Expr::SpawnAll(items) => {
            for e in items {
                rewrite_expr_types(e);
            }
        }
        Expr::Dict(entries) => {
            for (k, v) in entries {
                rewrite_expr_types(k);
                rewrite_expr_types(v);
            }
        }
        Expr::ValueConstruct(_, fields) => {
            for (_, e) in fields {
                rewrite_expr_types(e);
            }
        }
        Expr::TryExpr(body) | Expr::Comptime(body) => {
            for s in body {
                rewrite_stmt_types(s);
            }
        }
        Expr::ListComprehension {
            expr,
            iter,
            filter,
            ..
        } => {
            rewrite_expr_types(expr);
            rewrite_expr_types(iter);
            if let Some(f) = filter {
                rewrite_expr_types(f);
            }
        }
        Expr::MacroInvoke(inv) => match &mut inv.args {
            bolide_parser::MacroArgs::Paren(args) => {
                for a in args {
                    match a {
                        bolide_parser::MacroArg::Expr(e)
                        | bolide_parser::MacroArg::Named { value: e, .. } => {
                            rewrite_expr_types(e);
                        }
                    }
                }
            }
            bolide_parser::MacroArgs::Brace(stmts) => {
                for s in stmts {
                    rewrite_stmt_types(s);
                }
            }
        },
        _ => {}
    }
}
