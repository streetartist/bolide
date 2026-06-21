//! pest 解析结果到 AST 的转换

use crate::ast::*;
use crate::{BolideParser, Rule};
use pest::error::{ErrorVariant, InputLocation};
use pest::iterators::Pair;
use pest::iterators::Pairs;
use pest::Parser;
use std::fmt;

/// Structured parse diagnostic with byte-based source location.
#[derive(Debug, Clone)]
pub struct ParseDiagnostic {
    pub message: String,
    pub span: Option<(usize, usize)>,
    pub label: Option<String>,
    pub help: Option<String>,
}

impl ParseDiagnostic {
    fn from_pest(source: &str, error: pest::error::Error<Rule>) -> Self {
        let span = match error.location {
            InputLocation::Pos(offset) => {
                let len = source[offset..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(0);
                Some((offset, len))
            }
            InputLocation::Span((start, end)) => Some((start, end.saturating_sub(start))),
        };

        let (message, help) = match &error.variant {
            ErrorVariant::ParsingError {
                positives,
                negatives,
            } => {
                let expected = format_rules(positives);
                let unexpected = format_rules(negatives);
                let message = match (expected.is_empty(), unexpected.is_empty()) {
                    (false, false) => {
                        format!("invalid syntax: expected {}, not {}", expected, unexpected)
                    }
                    (false, true) => format!("invalid syntax: expected {}", expected),
                    (true, false) => format!("invalid syntax: unexpected {}", unexpected),
                    (true, true) => "invalid syntax".to_string(),
                };
                let help = if expected.contains("';'") {
                    Some("Bolide statements usually end with ';'.".to_string())
                } else if expected.contains("'}'") || expected.contains("block") {
                    Some("Check that every '{' has a matching '}'.".to_string())
                } else if expected.contains("expression") {
                    Some("An expression is required here.".to_string())
                } else {
                    Some("Check the token at the marked position.".to_string())
                };
                (message, help)
            }
            ErrorVariant::CustomError { message } => (message.clone(), None),
        };

        Self {
            message,
            span,
            label: Some("syntax error starts here".to_string()),
            help,
        }
    }

    fn from_conversion(message: String) -> Self {
        Self {
            message,
            span: None,
            label: None,
            help: Some(
                "The parser accepted the syntax but could not build a valid AST.".to_string(),
            ),
        }
    }
}

impl fmt::Display for ParseDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ParseDiagnostic {}

/// 解析源代码为 AST
pub fn parse(source: &str) -> Result<Program, String> {
    parse_with_diagnostics(source).map_err(|e| e.to_string())
}

/// 解析源代码为 AST，并保留语法错误位置。
pub fn parse_with_diagnostics(source: &str) -> Result<Program, ParseDiagnostic> {
    let pairs = BolideParser::parse(Rule::program, source)
        .map_err(|e| ParseDiagnostic::from_pest(source, e))?;
    parse_pairs(pairs).map_err(ParseDiagnostic::from_conversion)
}

fn parse_pairs(pairs: Pairs<Rule>) -> Result<Program, String> {
    let mut statements = Vec::new();
    for pair in pairs {
        if pair.as_rule() == Rule::program {
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::statement => {
                        if let Some(stmt) = parse_statement(inner)? {
                            statements.push(stmt);
                        }
                    }
                    Rule::EOI => {}
                    _ => {}
                }
            }
        }
    }

    Ok(Program { statements })
}

fn format_rules(rules: &[Rule]) -> String {
    let mut parts: Vec<&'static str> = rules.iter().filter_map(friendly_rule_name).collect();
    parts.sort_unstable();
    parts.dedup();
    parts.join(", ")
}

fn friendly_rule_name(rule: &Rule) -> Option<&'static str> {
    match rule {
        Rule::EOI => Some("end of file"),
        Rule::statement => Some("statement"),
        Rule::expr | Rule::or_expr | Rule::and_expr => Some("expression"),
        Rule::primary => Some("literal, identifier, or parenthesized expression"),
        Rule::ident => Some("identifier"),
        Rule::block => Some("block"),
        Rule::type_expr => Some("type"),
        Rule::param => Some("parameter"),
        Rule::param_list => Some("parameter list"),
        Rule::call_arg | Rule::call_args => Some("call argument"),
        Rule::var_decl => Some("'let'/'var' declaration"),
        Rule::func_def => Some("'fn' definition"),
        Rule::class_def => Some("'class' definition"),
        Rule::value_def => Some("'value' definition"),
        Rule::if_stmt => Some("'if' statement"),
        Rule::while_stmt => Some("'while' statement"),
        Rule::for_stmt => Some("'for' statement"),
        Rule::return_stmt => Some("'return' statement"),
        Rule::expr_stmt => Some("expression statement"),
        Rule::assign_stmt => Some("assignment"),
        Rule::string_lit => Some("string literal"),
        Rule::int_lit | Rule::float_lit | Rule::sci_lit | Rule::bigint_lit | Rule::decimal_lit => {
            Some("number literal")
        }
        _ => None,
    }
}

fn parse_statement(pair: Pair<Rule>) -> Result<Option<Statement>, String> {
    match pair.as_rule() {
        Rule::statement => {
            // statement 规则包含具体的语句类型
            let inner = pair.into_inner().next().unwrap();
            parse_statement(inner)
        }
        Rule::func_def => Ok(Some(Statement::FuncDef(parse_func_def(pair)?))),
        Rule::enum_def => Ok(Some(Statement::EnumDef(parse_enum_def(pair)?))),
        Rule::var_decl => Ok(Some(Statement::VarDecl(parse_var_decl(pair)?))),
        Rule::assign_stmt => Ok(Some(Statement::Assign(parse_assign(pair)?))),
        Rule::if_stmt => Ok(Some(Statement::If(parse_if_stmt(pair)?))),
        Rule::while_stmt => Ok(Some(Statement::While(parse_while_stmt(pair)?))),
        Rule::for_stmt => Ok(Some(Statement::For(parse_for_stmt(pair)?))),
        Rule::pool_stmt => Ok(Some(Statement::Pool(parse_pool_stmt(pair)?))),
        Rule::select_stmt => Ok(Some(Statement::Select(parse_select_stmt(pair)?))),
        Rule::await_scope_stmt => Ok(Some(Statement::AwaitScope(parse_await_scope_stmt(pair)?))),
        Rule::spawn_select_stmt => Ok(Some(Statement::SpawnSelect(parse_spawn_select_stmt(pair)?))),
        Rule::break_stmt => Ok(Some(Statement::Break)),
        Rule::continue_stmt => Ok(Some(Statement::Continue)),
        Rule::throw_stmt => Ok(Some(Statement::Throw(parse_expr(
            pair.into_inner().next().unwrap(),
        )?))),
        Rule::try_stmt => Ok(Some(Statement::Try(parse_try_stmt(pair)?))),
        Rule::match_stmt => Ok(Some(Statement::Match(parse_match_stmt(pair)?))),
        Rule::return_stmt => Ok(Some(parse_return_stmt(pair)?)),
        Rule::expr_stmt => Ok(Some(Statement::Expr(parse_expr_stmt(pair)?))),
        Rule::import_stmt => Ok(Some(Statement::Import(parse_import(pair)?))),
        Rule::class_def => Ok(Some(Statement::ClassDef(parse_class_def(pair)?))),
        Rule::value_def => Ok(Some(Statement::ValueDef(parse_value_def(pair)?))),
        Rule::extern_block => Ok(Some(Statement::ExternBlock(parse_extern_block(pair)?))),
        Rule::EOI => Ok(None),
        _ => Ok(None),
    }
}

fn parse_assign(pair: Pair<Rule>) -> Result<Assign, String> {
    let mut inner = pair.into_inner();
    let target_pair = inner.next().unwrap();
    let target = parse_assign_target(target_pair)?;
    let op_pair = inner.next().unwrap();
    let op_str = op_pair.as_str().to_string();
    let rhs = parse_expr(inner.next().unwrap())?;

    // 复合赋值脱糖: a += b → a = a + b
    let value = match op_str.as_str() {
        "=" => rhs,
        "+=" => Expr::BinOp(Box::new(target.clone()), BinOp::Add, Box::new(rhs)),
        "-=" => Expr::BinOp(Box::new(target.clone()), BinOp::Sub, Box::new(rhs)),
        "*=" => Expr::BinOp(Box::new(target.clone()), BinOp::Mul, Box::new(rhs)),
        "/=" => Expr::BinOp(Box::new(target.clone()), BinOp::Div, Box::new(rhs)),
        "%=" => Expr::BinOp(Box::new(target.clone()), BinOp::Mod, Box::new(rhs)),
        other => return Err(format!("Unknown assignment operator: {}", other)),
    };
    Ok(Assign { target, value })
}

fn parse_assign_target(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let first = inner.next().unwrap();

    // 处理 ident 或 self_lit
    let ident = match first.as_rule() {
        Rule::self_lit => "self".to_string(),
        Rule::ident => first.as_str().to_string(),
        _ => {
            return Err(format!(
                "Unexpected rule in assign_target: {:?}",
                first.as_rule()
            ))
        }
    };
    let mut expr = Expr::Ident(ident);

    // 处理成员访问链和索引访问 (obj.field1.field2 或 list[0])
    for item in inner {
        match item.as_rule() {
            Rule::member => {
                let member_name = item.into_inner().next().unwrap().as_str().to_string();
                expr = Expr::Member(Box::new(expr), member_name);
            }
            Rule::index => {
                let idx = parse_expr(item.into_inner().next().unwrap())?;
                expr = Expr::Index(Box::new(expr), Box::new(idx));
            }
            _ => {}
        }
    }

    Ok(expr)
}

fn parse_func_def(pair: Pair<Rule>) -> Result<FuncDef, String> {
    let span_start = pair.as_span().start();
    let mut inner = pair.into_inner();
    let mut is_async = false;
    let mut is_export = false;
    let mut is_inline = false;

    // 前缀修饰符可按任意顺序出现：export? async? inline? fn
    let mut first = inner.next().unwrap();
    if first.as_rule() == Rule::export_keyword {
        is_export = true;
        first = inner.next().unwrap();
    }
    if first.as_rule() == Rule::async_keyword {
        is_async = true;
        first = inner.next().unwrap();
    }
    if first.as_rule() == Rule::inline {
        is_inline = true;
        first = inner.next().unwrap();
    }
    let name = first.as_str().to_string();

    let mut type_params = Vec::new();
    let mut params = Vec::new();
    let mut throws = Vec::new();
    let mut return_type = None;
    let mut lifetime_deps = None;
    let mut body = Vec::new();

    for item in inner {
        match item.as_rule() {
            Rule::generic_param_list => {
                type_params = parse_generic_param_list(item);
            }
            Rule::param_list => {
                for param_pair in item.into_inner() {
                    params.push(parse_param(param_pair)?);
                }
                validate_params(&params)?;
            }
            Rule::throws_clause => {
                throws = item
                    .into_inner()
                    .filter(|p| p.as_rule() == Rule::type_expr)
                    .map(parse_type)
                    .collect::<Result<Vec<_>, _>>()?;
            }
            Rule::type_expr => {
                return_type = Some(parse_type(item)?);
            }
            Rule::lifetime_clause => {
                // 解析生命周期依赖: from x, y（跳过 kw_from 关键字对）
                let deps: Vec<String> = item
                    .into_inner()
                    .filter(|p| p.as_rule() == Rule::ident)
                    .map(|p| p.as_str().to_string())
                    .collect();
                lifetime_deps = Some(deps);
            }
            Rule::block => {
                body = parse_block(item)?;
            }
            _ => {}
        }
    }

    for param in &mut params {
        rewrite_type_generics(&mut param.ty, &type_params);
    }
    if let Some(ref mut ret_ty) = return_type {
        rewrite_type_generics(ret_ty, &type_params);
    }
    for throw_ty in &mut throws {
        rewrite_type_generics(throw_ty, &type_params);
    }
    Ok(FuncDef {
        name,
        is_async,
        is_export,
        is_inline,
        type_params,
        params,
        throws,
        return_type,
        lifetime_deps,
        body,
        def_span_start: Some(span_start),
    })
}

fn parse_param(pair: Pair<Rule>) -> Result<Param, String> {
    let mut inner_pair = pair
        .into_inner()
        .next()
        .ok_or("Parameter is missing body")?;
    if inner_pair.as_rule() == Rule::normal_param {
        inner_pair = inner_pair
            .into_inner()
            .next()
            .ok_or("Normal parameter is missing body")?;
    }
    let inner_rule = inner_pair.as_rule();
    let mut inner = inner_pair.into_inner();
    let mut mode = ParamMode::Borrow; // 默认借用

    if inner_rule == Rule::variadic_param {
        let name = inner
            .next()
            .ok_or("Variadic parameter is missing name")?
            .as_str()
            .to_string();
        let elem_ty = parse_type(inner.next().ok_or("Variadic parameter is missing type")?)?;
        return Ok(Param {
            name,
            ty: Type::List(Box::new(elem_ty)),
            mode,
            default_value: None,
            is_variadic: true,
            is_kw_variadic: false,
        });
    }

    if inner_rule == Rule::kw_variadic_param {
        let name = inner
            .next()
            .ok_or("Keyword variadic parameter is missing name")?
            .as_str()
            .to_string();
        let value_ty = parse_type(
            inner
                .next()
                .ok_or("Keyword variadic parameter is missing type")?,
        )?;
        return Ok(Param {
            name,
            ty: Type::Dict(Box::new(Type::Str), Box::new(value_ty)),
            mode,
            default_value: None,
            is_variadic: false,
            is_kw_variadic: true,
        });
    }

    // 检查是否有参数模式
    let first = inner.next().unwrap();
    let name = if first.as_rule() == Rule::param_mode {
        mode = match first.as_str() {
            "owned" => ParamMode::Owned,
            "ref" => ParamMode::Ref,
            _ => ParamMode::Borrow,
        };
        inner.next().unwrap().as_str().to_string()
    } else {
        first.as_str().to_string()
    };

    let ty = parse_type(inner.next().unwrap())?;
    let default_value = inner.next().map(parse_expr).transpose()?;
    if mode == ParamMode::Ref && default_value.is_some() {
        return Err(format!(
            "ref parameter '{}' cannot have a default value",
            name
        ));
    }
    Ok(Param {
        name,
        ty,
        mode,
        default_value,
        is_variadic: false,
        is_kw_variadic: false,
    })
}

fn parse_generic_param_list(pair: Pair<Rule>) -> Vec<String> {
    pair.into_inner()
        .filter(|p| p.as_rule() == Rule::ident)
        .map(|p| p.as_str().to_string())
        .collect()
}

fn rewrite_type_generics(ty: &mut Type, type_params: &[String]) {
    match ty {
        Type::Custom(name) if type_params.iter().any(|p| p == name) => {
            *ty = Type::Generic(name.clone());
        }
        Type::Channel(inner) | Type::List(inner) | Type::Weak(inner) | Type::Unowned(inner) => {
            rewrite_type_generics(inner, type_params)
        }
        Type::Dict(k, v) => {
            rewrite_type_generics(k, type_params);
            rewrite_type_generics(v, type_params);
        }
        Type::Tuple(items) => {
            for item in items {
                rewrite_type_generics(item, type_params);
            }
        }
        Type::FuncSig(params, ret) => {
            for param in params {
                rewrite_type_generics(param, type_params);
            }
            if let Some(ret) = ret {
                rewrite_type_generics(ret, type_params);
            }
        }
        Type::Adt(_, args) => {
            for arg in args {
                rewrite_type_generics(arg, type_params);
            }
        }
        _ => {}
    }
}

fn validate_params(params: &[Param]) -> Result<(), String> {
    let mut seen_variadic = false;
    let mut seen_kw_variadic = false;
    for (i, param) in params.iter().enumerate() {
        if params[..i].iter().any(|p| p.name == param.name) {
            return Err(format!("Duplicate parameter '{}'", param.name));
        }
        if param.is_variadic {
            if seen_variadic {
                return Err("Only one *args parameter is allowed".to_string());
            }
            if seen_kw_variadic {
                return Err("*args parameter must appear before **kwargs".to_string());
            }
            seen_variadic = true;
        }
        if param.is_kw_variadic {
            if seen_kw_variadic {
                return Err("Only one **kwargs parameter is allowed".to_string());
            }
            if i + 1 != params.len() {
                return Err(format!(
                    "Keyword variadic parameter '{}' must be the last parameter",
                    param.name
                ));
            }
            seen_kw_variadic = true;
        }
        if param.is_variadic && params[i + 1..].iter().any(|p| !p.is_kw_variadic) {
            return Err(format!(
                "Variadic parameter '{}' must appear after normal parameters",
                param.name
            ));
        }
    }
    Ok(())
}

fn parse_enum_def(pair: Pair<Rule>) -> Result<EnumDef, String> {
    let is_union = pair.as_str().trim_start().starts_with("union");
    let mut inner = pair.into_inner();
    let name = inner
        .next()
        .ok_or("enum/union is missing name")?
        .as_str()
        .to_string();
    let mut type_params = Vec::new();
    let mut variants = Vec::new();

    for item in inner {
        match item.as_rule() {
            Rule::generic_param_list => {
                type_params = parse_generic_param_list(item);
            }
            Rule::enum_variant => {
                variants.push(parse_enum_variant(item, &type_params)?);
            }
            _ => {}
        }
    }

    if variants.is_empty() {
        return Err(format!(
            "enum/union '{}' must declare at least one variant",
            name
        ));
    }

    Ok(EnumDef {
        name,
        type_params,
        variants,
        is_union,
    })
}

fn parse_enum_variant(pair: Pair<Rule>, type_params: &[String]) -> Result<EnumVariant, String> {
    let mut inner = pair.into_inner();
    let name = inner
        .next()
        .ok_or("enum variant is missing name")?
        .as_str()
        .to_string();
    let mut fields = Vec::new();

    if let Some(field_group) = inner.next() {
        match field_group.as_rule() {
            Rule::tuple_variant_fields => {
                for field_pair in field_group.into_inner() {
                    if field_pair.as_rule() == Rule::enum_variant_field {
                        let ty_pair = field_pair
                            .into_inner()
                            .next()
                            .ok_or("enum variant field is missing type")?;
                        let mut ty = parse_type(ty_pair)?;
                        rewrite_type_generics(&mut ty, type_params);
                        fields.push(EnumVariantField { name: None, ty });
                    }
                }
            }
            Rule::struct_variant_fields => {
                for field_pair in field_group.into_inner() {
                    if field_pair.as_rule() == Rule::named_enum_variant_field {
                        let mut field_inner = field_pair.into_inner();
                        let field_name = field_inner
                            .next()
                            .ok_or("named enum field is missing name")?
                            .as_str()
                            .to_string();
                        let mut ty = parse_type(
                            field_inner
                                .next()
                                .ok_or("named enum field is missing type")?,
                        )?;
                        rewrite_type_generics(&mut ty, type_params);
                        fields.push(EnumVariantField {
                            name: Some(field_name),
                            ty,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(EnumVariant { name, fields })
}

fn parse_type(pair: Pair<Rule>) -> Result<Type, String> {
    let mut inner_iter = pair.into_inner();
    let first = inner_iter.next().unwrap();

    // 检查是否有 ref_mode (weak/unowned)
    let (ref_mode, type_pair) = if first.as_rule() == Rule::ref_mode {
        let mode = first.as_str();
        let type_pair = inner_iter.next().unwrap();
        (Some(mode), type_pair)
    } else {
        (None, first)
    };

    // 解析基础类型
    let base_type = match type_pair.as_rule() {
        Rule::tuple_type => {
            let types: Result<Vec<_>, _> = type_pair.into_inner().map(parse_type).collect();
            Type::Tuple(types?)
        }
        Rule::list_type => {
            let elem_type = parse_type(type_pair.into_inner().next().unwrap())?;
            Type::List(Box::new(elem_type))
        }
        Rule::dict_type => {
            let mut inner = type_pair.into_inner();
            let key_type = parse_type(inner.next().unwrap())?;
            let value_type = parse_type(inner.next().unwrap())?;
            Type::Dict(Box::new(key_type), Box::new(value_type))
        }
        Rule::channel_type => {
            let elem_type = parse_type(type_pair.into_inner().next().unwrap())?;
            Type::Channel(Box::new(elem_type))
        }

        Rule::func_type => {
            let mut func_inner = type_pair.into_inner();
            let mut param_types = Vec::new();
            let mut return_type = None;

            for item in func_inner {
                match item.as_rule() {
                    Rule::func_type_params => {
                        for param in item.into_inner() {
                            param_types.push(parse_type(param)?);
                        }
                    }
                    Rule::type_expr => {
                        return_type = Some(Box::new(parse_type(item)?));
                    }
                    _ => {}
                }
            }
            Type::FuncSig(param_types, return_type)
        }
        Rule::applied_type => {
            let mut inner = type_pair.into_inner();
            let name_pair = inner.next().ok_or("Applied type is missing type name")?;
            let name = name_pair
                .as_str()
                .split('.')
                .map(|p| p.trim())
                .collect::<Vec<&str>>()
                .join(".");
            let args: Result<Vec<_>, _> = inner.map(parse_type).collect();
            let args = args?;
            match name.as_str() {
                "future" | "task" => {
                    let replacement = if name == "future" {
                        "Future<T>"
                    } else {
                        "Task<T>"
                    };
                    return Err(format!(
                        "legacy type `{}` has been removed; use `{}`",
                        name, replacement
                    ));
                }
                "Future" | "Task" => {
                    if args.len() != 1 {
                        return Err(format!("{} expects exactly one type argument", name));
                    }
                    Type::Future
                }
                _ => Type::Adt(name, args),
            }
        }
        Rule::basic_type => {
            let s = type_pair.as_str().trim();
            // 如果是 qualified_type，去除内部可能的空格
            let clean_s = if s.contains('.') {
                s.split('.')
                    .map(|p| p.trim())
                    .collect::<Vec<&str>>()
                    .join(".")
            } else {
                s.to_string()
            };

            match clean_s.as_str() {
                "int" => Type::Int,
                "float" => Type::Float,
                "bool" => Type::Bool,
                "str" => Type::Str,
                "bytes" => Type::Bytes,
                "bigint" => Type::BigInt,
                "decimal" => Type::Decimal,
                "dynamic" => Type::Dynamic,
                "ptr" => Type::Ptr,
                "func" => Type::Func,
                "future" | "task" => {
                    let replacement = if clean_s == "future" {
                        "Future<T>"
                    } else {
                        "Task<T>"
                    };
                    return Err(format!(
                        "legacy type `{}` has been removed; use `{}`",
                        clean_s, replacement
                    ));
                }
                "Future" | "Task" => {
                    return Err(format!(
                        "`{}` expects exactly one type argument; use `{}<T>`",
                        clean_s, clean_s
                    ));
                }
                _ => Type::Custom(clean_s),
            }
        }
        _ => return Err(format!("Unknown type: {:?}", type_pair.as_rule())),
    };

    // 应用 ref_mode
    Ok(match ref_mode {
        Some("weak") => Type::Weak(Box::new(base_type)),
        Some("unowned") => Type::Unowned(Box::new(base_type)),
        _ => base_type,
    })
}

fn parse_block(pair: Pair<Rule>) -> Result<Vec<Statement>, String> {
    let mut stmts = Vec::new();
    for item in pair.into_inner() {
        if let Some(stmt) = parse_statement(item)? {
            stmts.push(stmt);
        }
    }
    Ok(stmts)
}

fn parse_var_decl(pair: Pair<Rule>) -> Result<VarDecl, String> {
    let mut inner = pair.into_inner();
    let kind = inner.next().unwrap();
    let mutable = kind.as_str() == "var";
    let name = inner.next().unwrap().as_str().to_string();

    let mut ty = None;
    let mut value = None;

    for item in inner {
        match item.as_rule() {
            Rule::type_expr => {
                ty = Some(parse_type(item)?);
            }
            Rule::expr => {
                value = Some(parse_expr(item)?);
            }
            _ => {}
        }
    }

    Ok(VarDecl {
        name,
        mutable,
        ty,
        value,
    })
}

fn parse_if_stmt(pair: Pair<Rule>) -> Result<IfStmt, String> {
    let mut inner = pair.into_inner();
    let condition = parse_expr(inner.next().unwrap())?;
    let then_body = parse_block(inner.next().unwrap())?;

    let mut elif_branches = Vec::new();
    let mut else_body = None;

    for item in inner {
        match item.as_rule() {
            Rule::elif_branch => {
                let mut elif_inner = item.into_inner();
                let cond = parse_expr(elif_inner.next().unwrap())?;
                let body = parse_block(elif_inner.next().unwrap())?;
                elif_branches.push((cond, body));
            }
            Rule::else_branch => {
                else_body = Some(parse_block(item.into_inner().next().unwrap())?);
            }
            _ => {}
        }
    }

    Ok(IfStmt {
        condition,
        then_body,
        elif_branches,
        else_body,
    })
}

fn parse_while_stmt(pair: Pair<Rule>) -> Result<WhileStmt, String> {
    let mut inner = pair.into_inner();
    let condition = parse_expr(inner.next().unwrap())?;
    let body = parse_block(inner.next().unwrap())?;
    Ok(WhileStmt { condition, body })
}

fn parse_for_stmt(pair: Pair<Rule>) -> Result<ForStmt, String> {
    let mut inner = pair.into_inner();
    let mut vars = Vec::new();

    // Collect loop variables
    while let Some(p) = inner.peek() {
        if p.as_rule() == Rule::ident {
            vars.push(inner.next().unwrap().as_str().to_string());
        } else {
            break;
        }
    }

    if vars.is_empty() {
        return Err("For loop must have at least one variable".to_string());
    }

    // Next is iterator expression (skip the `in` keyword pair)
    let mut iter_pair = inner.next().ok_or("Missing iterator expression")?;
    if iter_pair.as_rule() == Rule::kw_in {
        iter_pair = inner.next().ok_or("Missing iterator expression")?;
    }
    let iter = parse_expr(iter_pair)?;

    // Next is block
    let block_pair = inner.next().ok_or("Missing loop body")?;
    let body = parse_block(block_pair)?;

    Ok(ForStmt { vars, iter, body })
}

fn parse_pool_stmt(pair: Pair<Rule>) -> Result<PoolStmt, String> {
    let mut inner = pair.into_inner();
    let size = parse_expr(inner.next().unwrap())?;
    let body = parse_block(inner.next().unwrap())?;
    Ok(PoolStmt { size, body })
}

fn parse_select_stmt(pair: Pair<Rule>) -> Result<SelectStmt, String> {
    let mut branches = Vec::new();
    for branch_pair in pair.into_inner() {
        let branch = parse_select_branch(branch_pair)?;
        branches.push(branch);
    }
    Ok(SelectStmt { branches })
}

fn parse_select_branch(pair: Pair<Rule>) -> Result<SelectBranch, String> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::select_recv => {
            let mut recv_inner = inner.into_inner();
            let var = recv_inner.next().unwrap().as_str().to_string();
            let channel = recv_inner.next().unwrap().as_str().to_string();
            let body = parse_block(recv_inner.next().unwrap())?;
            Ok(SelectBranch::Recv { var, channel, body })
        }
        Rule::select_timeout => {
            let mut timeout_inner = inner.into_inner();
            let duration = parse_expr(timeout_inner.next().unwrap())?;
            let body = parse_block(timeout_inner.next().unwrap())?;
            Ok(SelectBranch::Timeout { duration, body })
        }
        Rule::select_default => {
            let body = parse_block(inner.into_inner().next().unwrap())?;
            Ok(SelectBranch::Default { body })
        }
        _ => Err(format!("Unknown select branch: {:?}", inner.as_rule())),
    }
}

fn parse_await_scope_stmt(pair: Pair<Rule>) -> Result<AwaitScopeStmt, String> {
    let body = parse_block(pair.into_inner().next().unwrap())?;
    Ok(AwaitScopeStmt { body })
}

fn parse_spawn_select_stmt(pair: Pair<Rule>) -> Result<SpawnSelectStmt, String> {
    let mut branches = Vec::new();
    for branch_pair in pair.into_inner() {
        if branch_pair.as_rule() == Rule::kw_spawn {
            continue;
        }
        branches.push(parse_spawn_select_branch(branch_pair)?);
    }
    Ok(SpawnSelectStmt { branches })
}

fn parse_spawn_select_branch(pair: Pair<Rule>) -> Result<SpawnSelectBranch, String> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::spawn_select_bind => {
            let mut bind_inner = inner.into_inner();
            let var = bind_inner.next().unwrap().as_str().to_string();
            let expr = parse_expr(bind_inner.next().unwrap())?;
            let body = parse_block(bind_inner.next().unwrap())?;
            Ok(SpawnSelectBranch::Bind { var, expr, body })
        }
        Rule::spawn_select_expr => {
            let mut expr_inner = inner.into_inner();
            let expr = parse_expr(expr_inner.next().unwrap())?;
            let body = parse_block(expr_inner.next().unwrap())?;
            Ok(SpawnSelectBranch::Expr { expr, body })
        }
        _ => Err(format!(
            "Unknown spawn select branch: {:?}",
            inner.as_rule()
        )),
    }
}

fn parse_return_stmt(pair: Pair<Rule>) -> Result<Statement, String> {
    let expr = pair
        .into_inner()
        .next()
        .map(|p| parse_expr(p))
        .transpose()?;
    Ok(Statement::Return(expr))
}

fn parse_try_stmt(pair: Pair<Rule>) -> Result<TryStmt, String> {
    let mut inner = pair.into_inner();
    let try_body = parse_block(inner.next().unwrap())?;
    let mut catch_clauses = Vec::new();
    let mut finally = None;
    for item in inner {
        match item.as_rule() {
            Rule::catch_clause => {
                let mut c = item.into_inner();
                let var = c.next().unwrap().as_str().to_string();
                let ty = parse_type(c.next().unwrap())?;
                let body = parse_block(c.next().unwrap())?;
                catch_clauses.push(CatchClause { var, ty, body });
            }
            Rule::finally => {
                let body = parse_block(item.into_inner().next().unwrap())?;
                finally = Some(body);
            }
            _ => {}
        }
    }
    Ok(TryStmt {
        try_body,
        catch_clauses,
        finally,
    })
}

fn parse_match_stmt(pair: Pair<Rule>) -> Result<MatchStmt, String> {
    let mut inner = pair.into_inner();
    let expr = parse_expr(inner.next().ok_or("match is missing expression")?)?;
    let mut arms = Vec::new();
    for item in inner {
        if item.as_rule() == Rule::match_arm {
            let mut arm_inner = item.into_inner();
            let pattern = parse_pattern(arm_inner.next().ok_or("match arm is missing pattern")?)?;
            let body = parse_block(arm_inner.next().ok_or("match arm is missing body")?)?;
            arms.push(MatchArm { pattern, body });
        }
    }
    Ok(MatchStmt { expr, arms })
}

fn parse_pattern(pair: Pair<Rule>) -> Result<Pattern, String> {
    let inner = if pair.as_rule() == Rule::pattern {
        pair.into_inner().next().ok_or("pattern is missing body")?
    } else {
        pair
    };

    match inner.as_rule() {
        Rule::wildcard_pattern => Ok(Pattern::Wildcard),
        Rule::bind_pattern => Ok(Pattern::Bind(inner.as_str().to_string())),
        Rule::int_pattern => {
            let s = inner.as_str().replace('_', "");
            let value = if let Some(hex) = s.strip_prefix("0x") {
                i64::from_str_radix(hex, 16)
                    .map_err(|e| format!("Invalid int pattern {}: {}", inner.as_str(), e))?
            } else {
                s.parse::<i64>()
                    .map_err(|e| format!("Invalid int pattern {}: {}", inner.as_str(), e))?
            };
            Ok(Pattern::Int(value))
        }
        Rule::string_pattern => {
            let s = inner.as_str();
            Ok(Pattern::String(unescape_string(&s[1..s.len() - 1])))
        }
        Rule::bool_pattern => Ok(Pattern::Bool(inner.as_str() == "true")),
        Rule::none_pattern => Ok(Pattern::None),
        Rule::variant_pattern => {
            let mut enum_name = None;
            let mut variant = None;
            let mut fields = Vec::new();
            for item in inner.into_inner() {
                match item.as_rule() {
                    Rule::ident => {
                        if variant.is_none() {
                            variant = Some(item.as_str().to_string());
                        } else {
                            enum_name = variant.take();
                            variant = Some(item.as_str().to_string());
                        }
                    }
                    Rule::pattern => fields.push(parse_pattern(item)?),
                    _ => {}
                }
            }
            Ok(Pattern::Variant {
                enum_name,
                variant: variant.ok_or("variant pattern is missing variant name")?,
                fields,
            })
        }
        _ => Err(format!("Unknown pattern: {:?}", inner.as_rule())),
    }
}

fn parse_expr_stmt(pair: Pair<Rule>) -> Result<Expr, String> {
    parse_expr(pair.into_inner().next().unwrap())
}

fn parse_import(pair: Pair<Rule>) -> Result<Import, String> {
    let mut inner = pair.into_inner();
    let first = inner.next().unwrap();

    let (path, file_path) = match first.as_rule() {
        Rule::string_lit => {
            // 文件路径导入: import "file.bl";
            let s = first.as_str();
            let fp = s[1..s.len() - 1].to_string();
            (Vec::new(), Some(fp))
        }
        Rule::module_path => {
            // 模块路径导入: import math.utils;
            let p: Vec<String> = first.into_inner().map(|p| p.as_str().to_string()).collect();
            (p, None)
        }
        _ => return Err(format!("Unexpected import path: {:?}", first.as_rule())),
    };

    // 跳过 kw_as 关键字对，别名是其后的 ident
    let alias = inner
        .find(|p| p.as_rule() == Rule::ident)
        .map(|p| p.as_str().to_string());
    Ok(Import {
        path,
        file_path,
        alias,
    })
}

fn parse_class_def(pair: Pair<Rule>) -> Result<ClassDef, String> {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();

    let mut parent = None;
    let mut fields = Vec::new();
    let mut methods = Vec::new();

    for item in inner {
        match item.as_rule() {
            Rule::ident => {
                // 父类名
                parent = Some(item.as_str().to_string());
            }
            Rule::class_body => {
                for member in item.into_inner() {
                    let member_inner = member.into_inner().next().unwrap();
                    match member_inner.as_rule() {
                        Rule::field_decl => {
                            let mut f = member_inner.into_inner();
                            let fname = f.next().unwrap().as_str().to_string();
                            let fty = parse_type(f.next().unwrap())?;
                            let default_value = f.next().map(|e| parse_expr(e)).transpose()?;
                            fields.push(ClassField {
                                name: fname,
                                ty: fty,
                                default_value,
                            });
                        }
                        Rule::method_def => {
                            methods
                                .push(parse_func_def(member_inner.into_inner().next().unwrap())?);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(ClassDef {
        name,
        parent,
        fields,
        methods,
    })
}

fn parse_value_def(pair: Pair<Rule>) -> Result<ValueDef, String> {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();

    let mut fields = Vec::new();
    if let Some(fields_pair) = inner.next() {
        for field in fields_pair.into_inner() {
            let mut f = field.into_inner();
            let fname = f.next().unwrap().as_str().to_string();
            let fty = parse_type(f.next().unwrap())?;
            fields.push(ValueField { name: fname, ty: fty });
        }
    }

    Ok(ValueDef { name, fields })
}

// 表达式解析
fn parse_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    parse_or_expr(pair.into_inner().next().unwrap())
}

fn parse_or_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_and_expr(inner.next().unwrap())?;
    for right_pair in inner {
        // 跳过 kw_or 关键字对
        if right_pair.as_rule() != Rule::and_expr {
            continue;
        }
        let right = parse_and_expr(right_pair)?;
        left = Expr::BinOp(Box::new(left), BinOp::Or, Box::new(right));
    }
    Ok(left)
}

fn parse_and_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_bitor_expr(inner.next().unwrap())?;
    for right_pair in inner {
        // 跳过 kw_and 关键字对
        if right_pair.as_rule() != Rule::bitor_expr {
            continue;
        }
        let right = parse_bitor_expr(right_pair)?;
        left = Expr::BinOp(Box::new(left), BinOp::And, Box::new(right));
    }
    Ok(left)
}

fn parse_bitor_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_xor_expr(inner.next().unwrap())?;
    while let Some(_op_pair) = inner.next() {
        let right = parse_xor_expr(inner.next().unwrap())?;
        left = Expr::BinOp(Box::new(left), BinOp::BitOr, Box::new(right));
    }
    Ok(left)
}

fn parse_xor_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_bitand_expr(inner.next().unwrap())?;
    while let Some(_op_pair) = inner.next() {
        let right = parse_bitand_expr(inner.next().unwrap())?;
        left = Expr::BinOp(Box::new(left), BinOp::Xor, Box::new(right));
    }
    Ok(left)
}

fn parse_bitand_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_shift_expr(inner.next().unwrap())?;
    while let Some(_op_pair) = inner.next() {
        let right = parse_shift_expr(inner.next().unwrap())?;
        left = Expr::BinOp(Box::new(left), BinOp::BitAnd, Box::new(right));
    }
    Ok(left)
}

fn parse_shift_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_cmp_expr(inner.next().unwrap())?;
    while let Some(op_pair) = inner.next() {
        let op = match op_pair.as_str() {
            "<<" => BinOp::Shl,
            ">>" => BinOp::Shr,
            _ => return Err(format!("Unknown shift op: {}", op_pair.as_str())),
        };
        let right = parse_cmp_expr(inner.next().unwrap())?;
        left = Expr::BinOp(Box::new(left), op, Box::new(right));
    }
    Ok(left)
}

fn parse_add_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_mul_expr(inner.next().unwrap())?;
    while let Some(op_pair) = inner.next() {
        let op = match op_pair.as_str() {
            "+" => BinOp::Add,
            "-" => BinOp::Sub,
            _ => return Err(format!("Unknown add op: {}", op_pair.as_str())),
        };
        let right = parse_mul_expr(inner.next().unwrap())?;
        left = Expr::BinOp(Box::new(left), op, Box::new(right));
    }
    Ok(left)
}

fn parse_mul_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_unary_expr(inner.next().unwrap())?;
    while let Some(op_pair) = inner.next() {
        let op = match op_pair.as_str() {
            "*" => BinOp::Mul,
            "/" => BinOp::Div,
            "%" => BinOp::Mod,
            _ => return Err(format!("Unknown mul op: {}", op_pair.as_str())),
        };
        let right = parse_unary_expr(inner.next().unwrap())?;
        left = Expr::BinOp(Box::new(left), op, Box::new(right));
    }
    Ok(left)
}

fn parse_unary_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let first = inner.next().unwrap();

    if first.as_rule() == Rule::unary_op {
        let op = match first.as_str() {
            "-" => UnaryOp::Neg,
            "not" => UnaryOp::Not,
            _ => return Err(format!("Unknown unary op: {}", first.as_str())),
        };
        let expr = parse_postfix_expr(inner.next().unwrap())?;
        Ok(Expr::UnaryOp(op, Box::new(expr)))
    } else {
        parse_postfix_expr(first)
    }
}

fn parse_cmp_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_add_expr(inner.next().unwrap())?;
    while let Some(op_pair) = inner.next() {
        let op = match op_pair.as_str() {
            "==" => BinOp::Eq,
            "!=" => BinOp::Ne,
            "<" => BinOp::Lt,
            "<=" => BinOp::Le,
            ">" => BinOp::Gt,
            ">=" => BinOp::Ge,
            _ => return Err(format!("Unknown cmp op: {}", op_pair.as_str())),
        };
        let right = parse_add_expr(inner.next().unwrap())?;
        left = Expr::BinOp(Box::new(left), op, Box::new(right));
    }
    Ok(left)
}

fn parse_postfix_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut expr = parse_primary(inner.next().unwrap())?;

    for item in inner {
        match item.as_rule() {
            Rule::call_args => {
                let args: Result<Vec<_>, _> = item.into_inner().map(parse_call_arg).collect();
                expr = Expr::Call(Box::new(expr), args?);
            }
            Rule::index => {
                let inner = item.into_inner().next().unwrap();
                match inner.as_rule() {
                    Rule::slice => {
                        let (start, end, step) = parse_slice(inner)?;
                        expr = Expr::Slice(Box::new(expr), start, end, step);
                    }
                    _ => {
                        let idx = parse_expr(inner)?;
                        expr = Expr::Index(Box::new(expr), Box::new(idx));
                    }
                }
            }
            Rule::member => {
                let name = item.into_inner().next().unwrap().as_str().to_string();
                expr = Expr::Member(Box::new(expr), name);
            }
            Rule::propagate_op => {
                expr = Expr::Propagate(Box::new(expr));
            }
            Rule::raise_op => {
                expr = Expr::Raise(Box::new(expr));
            }
            _ => {}
        }
    }
    Ok(expr)
}

fn parse_call_arg(pair: Pair<Rule>) -> Result<Expr, String> {
    let inner = pair
        .into_inner()
        .next()
        .ok_or("Call argument is missing body")?;
    match inner.as_rule() {
        Rule::spread_arg => {
            let value = parse_expr(
                inner
                    .into_inner()
                    .next()
                    .ok_or("Spread argument is missing value")?,
            )?;
            Ok(Expr::SpreadArg(Box::new(value)))
        }
        Rule::kw_spread_arg => {
            let value = parse_expr(
                inner
                    .into_inner()
                    .next()
                    .ok_or("Keyword spread argument is missing value")?,
            )?;
            Ok(Expr::KwSpreadArg(Box::new(value)))
        }
        Rule::named_arg => {
            let mut named = inner.into_inner();
            let name = named
                .next()
                .ok_or("Named argument is missing name")?
                .as_str()
                .to_string();
            let value = parse_expr(named.next().ok_or("Named argument is missing value")?)?;
            Ok(Expr::NamedArg(name, Box::new(value)))
        }
        Rule::expr => parse_expr(inner),
        _ => Err(format!("Unknown call argument: {:?}", inner.as_rule())),
    }
}

/// 解析切片 start? : end? (: step?)? 为三个 Option<Box<Expr>>。
/// 各段为独立规则（slice_start/slice_end/slice_step），内部 expr 可缺省。
fn parse_slice(
    pair: Pair<Rule>,
) -> Result<(Option<Box<Expr>>, Option<Box<Expr>>, Option<Box<Expr>>), String> {
    let mut start = None;
    let mut end = None;
    let mut step = None;
    for seg in pair.into_inner() {
        let rule = seg.as_rule();
        // 段内若有 expr 则解析，否则保持 None
        let inner_expr = seg.into_inner().next();
        let parsed = match inner_expr {
            Some(e) => Some(Box::new(parse_expr(e)?)),
            None => None,
        };
        match rule {
            Rule::slice_start => start = parsed,
            Rule::slice_end => end = parsed,
            Rule::slice_step => step = parsed,
            _ => {}
        }
    }
    Ok((start, end, step))
}

fn parse_list_comprehension(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let expr = parse_expr(
        inner
            .next()
            .ok_or("list comprehension missing expression")?,
    )?;

    let mut vars = Vec::new();
    let mut iter = None;
    let mut filter = None;

    for item in inner {
        match item.as_rule() {
            Rule::ident => {
                vars.push(item.as_str().to_string());
            }
            Rule::expr => {
                if iter.is_none() {
                    iter = Some(parse_expr(item)?);
                } else {
                    filter = Some(Box::new(parse_expr(item)?));
                }
            }
            _ => {}
        }
    }

    if vars.is_empty() {
        return Err("list comprehension must have at least one loop variable".to_string());
    }
    let iter = iter.ok_or("list comprehension missing iterator")?;

    Ok(Expr::ListComprehension {
        expr: Box::new(expr),
        vars,
        iter: Box::new(iter),
        filter,
    })
}

fn parse_closure_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut params = Vec::new();
    let mut return_type = None;
    let mut body = Vec::new();

    for item in inner {
        match item.as_rule() {
            Rule::param_list => {
                for param_pair in item.into_inner() {
                    params.push(parse_param(param_pair)?);
                }
                validate_params(&params)?;
            }
            Rule::type_expr => {
                return_type = Some(parse_type(item)?);
            }
            Rule::block => {
                body = parse_block(item)?;
            }
            _ => {}
        }
    }

    Ok(Expr::Closure {
        params,
        return_type,
        body,
    })
}

fn unescape_string(s: &str) -> String {
    let mut res = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => res.push('\n'),
                Some('r') => res.push('\r'),
                Some('t') => res.push('\t'),
                Some('\\') => res.push('\\'),
                Some('"') => res.push('"'),
                Some('\'') => res.push('\''),
                Some('0') => res.push('\0'),
                Some(c) => {
                    res.push('\\');
                    res.push(c);
                }
                None => res.push('\\'),
            }
        } else {
            res.push(c);
        }
    }
    res
}

fn parse_spawn_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    match pair.as_rule() {
        Rule::spawn_thread_expr => parse_spawn_call(pair, true),
        Rule::spawn_func_expr => parse_spawn_call(pair, false),
        Rule::spawn_expr => {
            let inner = pair
                .into_inner()
                .next()
                .ok_or("spawn expression missing body")?;
            parse_spawn_expr(inner)
        }
        _ => Err(format!("Unknown spawn expression: {:?}", pair.as_rule())),
    }
}

fn parse_spawn_call(pair: Pair<Rule>, force_thread: bool) -> Result<Expr, String> {
    let mut func_name = None;
    let mut args = None;

    for item in pair.into_inner() {
        match item.as_rule() {
            Rule::ident => func_name = Some(item.as_str().to_string()),
            Rule::call_args => {
                args = Some(
                    item.into_inner()
                        .map(parse_call_arg)
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
            Rule::kw_spawn | Rule::kw_thread => {}
            _ => {}
        }
    }

    let func_name = func_name.ok_or("spawn expression missing function name")?;
    let args = args.ok_or("spawn expression missing call arguments")?;
    if force_thread {
        Ok(Expr::SpawnThread(func_name, args))
    } else {
        Ok(Expr::Spawn(func_name, args))
    }
}

fn parse_primary(pair: Pair<Rule>) -> Result<Expr, String> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::int_lit => {
            let s = inner.as_str().replace('_', "");
            let n: i64 = if s.starts_with("0x") || s.starts_with("0X") {
                i64::from_str_radix(&s[2..], 16)
                    .map_err(|e| format!("Invalid hex literal '{}': {}", s, e))?
            } else {
                s.parse()
                    .map_err(|e| format!("Invalid int literal '{}': {}", s, e))?
            };
            Ok(Expr::Int(n))
        }
        Rule::float_lit => {
            let s = inner.as_str().replace('_', "");
            let f: f64 = s
                .parse()
                .map_err(|e| format!("Invalid float literal '{}': {}", s, e))?;
            Ok(Expr::Float(f))
        }
        Rule::sci_lit => {
            let s = inner.as_str().replace('_', "");
            let f: f64 = s
                .parse()
                .map_err(|e| format!("Invalid float literal '{}': {}", s, e))?;
            Ok(Expr::Float(f))
        }
        Rule::bigint_lit => {
            // 去掉后缀 B/b 和下划线分隔符
            let s = inner.as_str();
            let num_str = s[..s.len() - 1].replace('_', "");
            Ok(Expr::BigInt(num_str))
        }
        Rule::decimal_lit => {
            // 去掉后缀 D/d 和下划线分隔符
            let s = inner.as_str();
            let num_str = s[..s.len() - 1].replace('_', "");
            Ok(Expr::Decimal(num_str))
        }
        Rule::string_lit => {
            let s = inner.as_str();
            Ok(Expr::String(unescape_string(&s[1..s.len() - 1])))
        }
        Rule::bool_lit => Ok(Expr::Bool(inner.as_str() == "true")),
        Rule::none_lit => Ok(Expr::None),
        Rule::ident => Ok(Expr::Ident(inner.as_str().to_string())),
        Rule::list_literal => {
            let mut inner_iter = inner.into_inner();
            if let Some(first) = inner_iter.next() {
                match first.as_rule() {
                    Rule::list_comprehension => {
                        return parse_list_comprehension(first);
                    }
                    Rule::list_items => {
                        let items: Result<Vec<_>, _> = first.into_inner().map(parse_expr).collect();
                        return Ok(Expr::List(items?));
                    }
                    _ => {}
                }
            }
            Ok(Expr::List(Vec::new()))
        }
        Rule::closure_expr => parse_closure_expr(inner),
        Rule::dict_literal => {
            let mut entries = Vec::new();
            for entry in inner.into_inner() {
                if entry.as_rule() == Rule::dict_entry {
                    let mut entry_inner = entry.into_inner();
                    let key = parse_expr(entry_inner.next().unwrap())?;
                    let value = parse_expr(entry_inner.next().unwrap())?;
                    entries.push((key, value));
                }
            }
            Ok(Expr::Dict(entries))
        }
        Rule::value_construct => {
            let mut inner_iter = inner.into_inner();
            let type_name = inner_iter.next().unwrap().as_str().to_string();
            let mut fields = Vec::new();
            if let Some(args) = inner_iter.next() {
                for arg_pair in args.into_inner() {
                    if arg_pair.as_rule() == Rule::value_arg {
                        let mut e = arg_pair.into_inner();
                        let fname = e.next().unwrap().as_str().to_string();
                        let fexpr = parse_expr(e.next().unwrap())?;
                        fields.push((fname, fexpr));
                    }
                }
            }
            Ok(Expr::ValueConstruct(type_name, fields))
        }
        Rule::spawn_expr => {
            let spawn_inner = inner
                .into_inner()
                .next()
                .ok_or("spawn expression missing body")?;
            parse_spawn_expr(spawn_inner)
        }
        Rule::await_expr => {
            // await 绑定到后缀表达式层级（跳过 kw_await 关键字对）
            let postfix = inner
                .into_inner()
                .find(|p| p.as_rule() == Rule::postfix_expr)
                .ok_or("await expression missing operand")?;
            let expr = parse_postfix_expr(postfix)?;
            Ok(Expr::Await(Box::new(expr)))
        }
        Rule::spawn_all_expr => {
            // 跳过 kw_spawn 关键字对，只收集表达式
            let exprs: Result<Vec<_>, _> = inner
                .into_inner()
                .filter(|p| p.as_rule() == Rule::expr)
                .map(parse_expr)
                .collect();
            Ok(Expr::SpawnAll(exprs?))
        }
        Rule::try_expr => {
            let block = inner
                .into_inner()
                .find(|p| p.as_rule() == Rule::block)
                .ok_or("try expression missing block")?;
            Ok(Expr::TryExpr(parse_block(block)?))
        }
        Rule::tuple_literal => {
            let exprs: Result<Vec<_>, _> = inner.into_inner().map(parse_expr).collect();
            Ok(Expr::Tuple(exprs?))
        }
        Rule::self_lit => Ok(Expr::Ident("self".to_string())),
        Rule::super_lit => Ok(Expr::Ident("super".to_string())),
        Rule::expr => parse_expr(inner),
        _ => Err(format!("Unknown primary: {:?}", inner.as_rule())),
    }
}

// ============ FFI extern 解析 ============

fn parse_extern_block(pair: Pair<Rule>) -> Result<ExternBlock, String> {
    let mut inner = pair.into_inner();

    // 解析库路径 (string_lit)
    let lib_path_pair = inner.next().unwrap();
    let lib_path = {
        let s = lib_path_pair.as_str();
        s[1..s.len() - 1].to_string() // 去掉引号
    };

    // 解析声明列表
    let mut declarations = Vec::new();
    for decl_pair in inner {
        if decl_pair.as_rule() == Rule::extern_decl {
            let decl = parse_extern_decl(decl_pair)?;
            declarations.push(decl);
        }
    }

    Ok(ExternBlock {
        lib_path,
        declarations,
    })
}

fn parse_extern_decl(pair: Pair<Rule>) -> Result<ExternDecl, String> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::extern_func => Ok(ExternDecl::Function(parse_extern_func(inner)?)),
        Rule::extern_struct => Ok(ExternDecl::Struct(parse_extern_struct(inner)?)),
        Rule::extern_typedef => {
            let mut td = inner.into_inner();
            let name = td.next().unwrap().as_str().to_string();
            let ty = parse_c_type(td.next().unwrap())?;
            Ok(ExternDecl::TypeAlias(name, ty))
        }
        _ => Err(format!("Unknown extern decl: {:?}", inner.as_rule())),
    }
}

fn parse_extern_func(pair: Pair<Rule>) -> Result<ExternFunc, String> {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();

    let mut params = Vec::new();
    let mut variadic = false;
    let mut return_type = None;

    for item in inner {
        match item.as_rule() {
            Rule::extern_param_list => {
                for param_pair in item.into_inner() {
                    params.push(parse_extern_param(param_pair)?);
                }
            }
            Rule::variadic => variadic = true,
            Rule::c_type => return_type = Some(parse_c_type(item)?),
            _ => {}
        }
    }

    Ok(ExternFunc {
        name,
        params,
        return_type,
        variadic,
    })
}

fn parse_extern_param(pair: Pair<Rule>) -> Result<CParam, String> {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();
    let ty = parse_c_type(inner.next().unwrap())?;
    Ok(CParam { name, ty })
}

fn parse_extern_struct(pair: Pair<Rule>) -> Result<ExternStruct, String> {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();

    let mut fields = Vec::new();
    for field_pair in inner {
        if field_pair.as_rule() == Rule::extern_field {
            let mut f = field_pair.into_inner();
            let fname = f.next().unwrap().as_str().to_string();
            let fty = parse_c_type(f.next().unwrap())?;
            fields.push(CField {
                name: fname,
                ty: fty,
            });
        }
    }

    Ok(ExternStruct { name, fields })
}

fn parse_c_type(pair: Pair<Rule>) -> Result<CType, String> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::c_ptr_type => {
            let pointee = parse_c_type(inner.into_inner().next().unwrap())?;
            Ok(CType::Ptr(Box::new(pointee)))
        }
        Rule::c_array_type => {
            let mut arr = inner.into_inner();
            let elem_ty = parse_c_basic_type(arr.next().unwrap())?;
            let size: usize = arr.next().unwrap().as_str().parse().unwrap();
            Ok(CType::Array(Box::new(elem_ty), size))
        }
        Rule::c_func_ptr => {
            let mut fp = inner.into_inner();
            let mut params = Vec::new();
            let mut return_type = Box::new(CType::Void);
            for item in fp {
                match item.as_rule() {
                    Rule::c_type_list => {
                        for t in item.into_inner() {
                            params.push(parse_c_type(t)?);
                        }
                    }
                    Rule::c_type => return_type = Box::new(parse_c_type(item)?),
                    _ => {}
                }
            }
            Ok(CType::FuncPtr {
                params,
                return_type,
            })
        }
        Rule::c_basic_type => parse_c_basic_type(inner),
        _ => Err(format!("Unknown c_type: {:?}", inner.as_rule())),
    }
}

fn parse_c_basic_type(pair: Pair<Rule>) -> Result<CType, String> {
    let s = pair.as_str();
    Ok(match s {
        "void" => CType::Void,
        "char" => CType::Char,
        "uchar" => CType::UChar,
        "short" => CType::Short,
        "ushort" => CType::UShort,
        "c_int" => CType::Int,
        "c_uint" => CType::UInt,
        "long" => CType::Long,
        "ulong" => CType::ULong,
        "longlong" => CType::LongLong,
        "ulonglong" => CType::ULongLong,
        "c_float" => CType::Float,
        "c_double" => CType::Double,
        "c_bool" => CType::Bool,
        "i8" => CType::I8,
        "u8" => CType::U8,
        "i16" => CType::I16,
        "u16" => CType::U16,
        "i32" => CType::I32,
        "u32" => CType::U32,
        "i64" => CType::I64,
        "u64" => CType::U64,
        "size_t" => CType::SizeT,
        "ptrdiff_t" => CType::PtrDiffT,
        _ => CType::Struct(s.to_string()),
    })
}
