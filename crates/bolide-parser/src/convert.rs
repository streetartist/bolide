//! pest 解析结果到 AST 的转换

use pest::Parser;
use pest::iterators::Pair;
use crate::{BolideParser, Rule};
use crate::ast::*;

/// 解析源代码为 AST
pub fn parse(source: &str) -> Result<Program, String> {
    let pairs = BolideParser::parse(Rule::program, source)
        .map_err(|e| format!("Parse error: {}", e))?;

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

fn parse_statement(pair: Pair<Rule>) -> Result<Option<Statement>, String> {
    match pair.as_rule() {
        Rule::statement => {
            // statement 规则包含具体的语句类型
            let inner = pair.into_inner().next().unwrap();
            parse_statement(inner)
        }
        Rule::func_def => Ok(Some(Statement::FuncDef(parse_func_def(pair)?))),
        Rule::var_decl => Ok(Some(Statement::VarDecl(parse_var_decl(pair)?))),
        Rule::assign_stmt => Ok(Some(Statement::Assign(parse_assign(pair)?))),
        Rule::if_stmt => Ok(Some(Statement::If(parse_if_stmt(pair)?))),
        Rule::while_stmt => Ok(Some(Statement::While(parse_while_stmt(pair)?))),
        Rule::for_stmt => Ok(Some(Statement::For(parse_for_stmt(pair)?))),
        Rule::pool_stmt => Ok(Some(Statement::Pool(parse_pool_stmt(pair)?))),
        Rule::select_stmt => Ok(Some(Statement::Select(parse_select_stmt(pair)?))),
        Rule::await_scope_stmt => Ok(Some(Statement::AwaitScope(parse_await_scope_stmt(pair)?))),
        Rule::async_select_stmt => Ok(Some(Statement::AsyncSelect(parse_async_select_stmt(pair)?))),
        Rule::send_stmt => Ok(Some(Statement::Send(parse_send_stmt(pair)?))),
        Rule::return_stmt => Ok(Some(parse_return_stmt(pair)?)),
        Rule::expr_stmt => Ok(Some(Statement::Expr(parse_expr_stmt(pair)?))),
        Rule::import_stmt => Ok(Some(Statement::Import(parse_import(pair)?))),
        Rule::class_def => Ok(Some(Statement::ClassDef(parse_class_def(pair)?))),
        Rule::extern_block => Ok(Some(Statement::ExternBlock(parse_extern_block(pair)?))),
        Rule::EOI => Ok(None),
        _ => Ok(None),
    }
}

fn parse_assign(pair: Pair<Rule>) -> Result<Assign, String> {
    let mut inner = pair.into_inner();
    let target_pair = inner.next().unwrap();
    let target = parse_assign_target(target_pair)?;
    let value = parse_expr(inner.next().unwrap())?;
    Ok(Assign { target, value })
}

fn parse_assign_target(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let first = inner.next().unwrap();

    // 处理 ident 或 self_lit
    let ident = match first.as_rule() {
        Rule::self_lit => "self".to_string(),
        Rule::ident => first.as_str().to_string(),
        _ => return Err(format!("Unexpected rule in assign_target: {:?}", first.as_rule())),
    };
    let mut expr = Expr::Ident(ident);

    // 处理成员访问链 (obj.field1.field2)
    for member_pair in inner {
        let member_name = member_pair.into_inner().next().unwrap().as_str().to_string();
        expr = Expr::Member(Box::new(expr), member_name);
    }

    Ok(expr)
}

fn parse_func_def(pair: Pair<Rule>) -> Result<FuncDef, String> {
    let mut inner = pair.into_inner();
    let mut is_async = false;

    // 检查第一个 token 是否是 async_keyword
    let first = inner.next().unwrap();
    let name = if first.as_rule() == Rule::async_keyword {
        is_async = true;
        inner.next().unwrap().as_str().to_string()
    } else {
        first.as_str().to_string()
    };

    let mut params = Vec::new();
    let mut return_type = None;
    let mut lifetime_deps = None;
    let mut body = Vec::new();

    for item in inner {
        match item.as_rule() {
            Rule::param_list => {
                for param_pair in item.into_inner() {
                    params.push(parse_param(param_pair)?);
                }
            }
            Rule::type_expr => {
                return_type = Some(parse_type(item)?);
            }
            Rule::lifetime_clause => {
                // 解析生命周期依赖: from x, y
                let deps: Vec<String> = item.into_inner()
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

    Ok(FuncDef { name, is_async, params, return_type, lifetime_deps, body })
}

fn parse_param(pair: Pair<Rule>) -> Result<Param, String> {
    let mut inner = pair.into_inner();
    let mut mode = ParamMode::Borrow;  // 默认借用

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
    Ok(Param { name, ty, mode })
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
            let types: Result<Vec<_>, _> = type_pair.into_inner()
                .map(parse_type).collect();
            Type::Tuple(types?)
        }
        Rule::list_type => {
            let elem_type = parse_type(type_pair.into_inner().next().unwrap())?;
            Type::List(Box::new(elem_type))
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
        Rule::basic_type => {
            let s = type_pair.as_str();
            match s {
                "int" => Type::Int,
                "float" => Type::Float,
                "bool" => Type::Bool,
                "str" => Type::Str,
                "bigint" => Type::BigInt,
                "decimal" => Type::Decimal,
                "dynamic" => Type::Dynamic,
                "ptr" => Type::Ptr,
                "future" => Type::Future,
                "func" => Type::Func,
                _ => Type::Custom(s.to_string()),
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

    Ok(VarDecl { name, ty, value })
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

    Ok(IfStmt { condition, then_body, elif_branches, else_body })
}

fn parse_while_stmt(pair: Pair<Rule>) -> Result<WhileStmt, String> {
    let mut inner = pair.into_inner();
    let condition = parse_expr(inner.next().unwrap())?;
    let body = parse_block(inner.next().unwrap())?;
    Ok(WhileStmt { condition, body })
}

fn parse_for_stmt(pair: Pair<Rule>) -> Result<ForStmt, String> {
    let mut inner = pair.into_inner();
    let var = inner.next().unwrap().as_str().to_string();
    let iter = parse_expr(inner.next().unwrap())?;
    let body = parse_block(inner.next().unwrap())?;
    Ok(ForStmt { var, iter, body })
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

fn parse_send_stmt(pair: Pair<Rule>) -> Result<SendStmt, String> {
    let mut inner = pair.into_inner();
    let channel = inner.next().unwrap().as_str().to_string();
    let value = parse_expr(inner.next().unwrap())?;
    Ok(SendStmt { channel, value })
}

fn parse_await_scope_stmt(pair: Pair<Rule>) -> Result<AwaitScopeStmt, String> {
    let body = parse_block(pair.into_inner().next().unwrap())?;
    Ok(AwaitScopeStmt { body })
}

fn parse_async_select_stmt(pair: Pair<Rule>) -> Result<AsyncSelectStmt, String> {
    let mut branches = Vec::new();
    for branch_pair in pair.into_inner() {
        branches.push(parse_async_select_branch(branch_pair)?);
    }
    Ok(AsyncSelectStmt { branches })
}

fn parse_async_select_branch(pair: Pair<Rule>) -> Result<AsyncSelectBranch, String> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::async_select_bind => {
            let mut bind_inner = inner.into_inner();
            let var = bind_inner.next().unwrap().as_str().to_string();
            let expr = parse_expr(bind_inner.next().unwrap())?;
            let body = parse_block(bind_inner.next().unwrap())?;
            Ok(AsyncSelectBranch::Bind { var, expr, body })
        }
        Rule::async_select_expr => {
            let mut expr_inner = inner.into_inner();
            let expr = parse_expr(expr_inner.next().unwrap())?;
            let body = parse_block(expr_inner.next().unwrap())?;
            Ok(AsyncSelectBranch::Expr { expr, body })
        }
        _ => Err(format!("Unknown async select branch: {:?}", inner.as_rule())),
    }
}

fn parse_return_stmt(pair: Pair<Rule>) -> Result<Statement, String> {
    let expr = pair.into_inner().next().map(|p| parse_expr(p)).transpose()?;
    Ok(Statement::Return(expr))
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
            let fp = s[1..s.len()-1].to_string();
            (Vec::new(), Some(fp))
        }
        Rule::module_path => {
            // 模块路径导入: import math.utils;
            let p: Vec<String> = first.into_inner()
                .map(|p| p.as_str().to_string())
                .collect();
            (p, None)
        }
        _ => return Err(format!("Unexpected import path: {:?}", first.as_rule())),
    };

    let alias = inner.next().map(|p| p.as_str().to_string());
    Ok(Import { path, file_path, alias })
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
                            fields.push(ClassField { name: fname, ty: fty, default_value });
                        }
                        Rule::method_def => {
                            methods.push(parse_func_def(member_inner.into_inner().next().unwrap())?);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(ClassDef { name, parent, fields, methods })
}

// 表达式解析
fn parse_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    parse_or_expr(pair.into_inner().next().unwrap())
}

fn parse_or_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_and_expr(inner.next().unwrap())?;
    while let Some(right_pair) = inner.next() {
        let right = parse_and_expr(right_pair)?;
        left = Expr::BinOp(Box::new(left), BinOp::Or, Box::new(right));
    }
    Ok(left)
}

fn parse_and_expr(pair: Pair<Rule>) -> Result<Expr, String> {
    let mut inner = pair.into_inner();
    let mut left = parse_cmp_expr(inner.next().unwrap())?;
    while let Some(right_pair) = inner.next() {
        let right = parse_cmp_expr(right_pair)?;
        left = Expr::BinOp(Box::new(left), BinOp::And, Box::new(right));
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
                let args: Result<Vec<_>, _> = item.into_inner()
                    .map(parse_expr).collect();
                expr = Expr::Call(Box::new(expr), args?);
            }
            Rule::index => {
                let idx = parse_expr(item.into_inner().next().unwrap())?;
                expr = Expr::Index(Box::new(expr), Box::new(idx));
            }
            Rule::member => {
                let name = item.into_inner().next().unwrap().as_str().to_string();
                expr = Expr::Member(Box::new(expr), name);
            }
            _ => {}
        }
    }
    Ok(expr)
}

fn parse_primary(pair: Pair<Rule>) -> Result<Expr, String> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::int_lit => {
            let n: i64 = inner.as_str().parse().unwrap();
            Ok(Expr::Int(n))
        }
        Rule::float_lit => {
            let f: f64 = inner.as_str().parse().unwrap();
            Ok(Expr::Float(f))
        }
        Rule::bigint_lit => {
            // 去掉后缀 B/b
            let s = inner.as_str();
            let num_str = &s[..s.len()-1];
            Ok(Expr::BigInt(num_str.to_string()))
        }
        Rule::decimal_lit => {
            // 去掉后缀 D/d
            let s = inner.as_str();
            let num_str = &s[..s.len()-1];
            Ok(Expr::Decimal(num_str.to_string()))
        }
        Rule::string_lit => {
            let s = inner.as_str();
            Ok(Expr::String(s[1..s.len()-1].to_string()))
        }
        Rule::bool_lit => {
            Ok(Expr::Bool(inner.as_str() == "true"))
        }
        Rule::none_lit => Ok(Expr::None),
        Rule::ident => Ok(Expr::Ident(inner.as_str().to_string())),
        Rule::list_literal => {
            let items: Result<Vec<_>, _> = inner.into_inner()
                .map(parse_expr).collect();
            Ok(Expr::List(items?))
        }
        Rule::spawn_expr => {
            let mut spawn_inner = inner.into_inner();
            let func_name = spawn_inner.next().unwrap().as_str().to_string();
            let args: Result<Vec<_>, _> = spawn_inner.next().unwrap()
                .into_inner()
                .map(parse_expr)
                .collect();
            Ok(Expr::Spawn(func_name, args?))
        }
        Rule::recv_expr => {
            let channel = inner.into_inner().next().unwrap().as_str().to_string();
            Ok(Expr::Recv(channel))
        }
        Rule::await_expr => {
            let expr = parse_expr(inner.into_inner().next().unwrap())?;
            Ok(Expr::Await(Box::new(expr)))
        }
        Rule::await_all_expr => {
            let exprs: Result<Vec<_>, _> = inner.into_inner()
                .map(parse_expr).collect();
            Ok(Expr::AwaitAll(exprs?))
        }
        Rule::tuple_literal => {
            let exprs: Result<Vec<_>, _> = inner.into_inner()
                .map(parse_expr).collect();
            Ok(Expr::Tuple(exprs?))
        }
        Rule::self_lit => Ok(Expr::Ident("self".to_string())),
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
        s[1..s.len()-1].to_string()  // 去掉引号
    };

    // 解析声明列表
    let mut declarations = Vec::new();
    for decl_pair in inner {
        if decl_pair.as_rule() == Rule::extern_decl {
            let decl = parse_extern_decl(decl_pair)?;
            declarations.push(decl);
        }
    }

    Ok(ExternBlock { lib_path, declarations })
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

    Ok(ExternFunc { name, params, return_type, variadic })
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
            fields.push(CField { name: fname, ty: fty });
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
            Ok(CType::FuncPtr { params, return_type })
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
