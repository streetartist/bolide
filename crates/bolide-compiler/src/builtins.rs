//! 内置类型注入
//!
//! 在编译开始前向程序注入内置类（如 `Error`），使其复用普通类的
//! 构造/字段访问/方法分派/RC 管理/异常标签机制。两后端（JIT/AOT）共用，
//! 保证语义一致。

use bolide_parser::{
    ClassDef, ClassField, EnumDef, EnumVariant, EnumVariantField, Program, Statement, Type,
};

/// 内置类名集合（用于判断用户是否已自定义同名类）。
fn user_defines_class(program: &Program, name: &str) -> bool {
    program.statements.iter().any(|s| {
        matches!(s, Statement::ClassDef(c) if c.name == name)
    })
}

fn user_defines_enum(program: &Program, name: &str) -> bool {
    program.statements.iter().any(|s| {
        matches!(s, Statement::EnumDef(e) if e.name == name)
    })
}

/// 构造内置 `Error` 类：单字段 `message: str`。
///
/// 用法：
/// ```bolide
/// throw Error("boom");
/// try { ... } catch (e: Error) { print(e.message); }
/// ```
fn error_class() -> ClassDef {
    ClassDef {
        name: "Error".to_string(),
        parent: None,
        fields: vec![ClassField {
            name: "message".to_string(),
            ty: Type::Str,
            default_value: None,
        }],
        methods: vec![],
    }
}

fn option_enum() -> EnumDef {
    EnumDef {
        name: "Option".to_string(),
        type_params: vec!["T".to_string()],
        variants: vec![
            EnumVariant {
                name: "Some".to_string(),
                fields: vec![EnumVariantField {
                    name: None,
                    ty: Type::Generic("T".to_string()),
                }],
            },
            EnumVariant {
                name: "None".to_string(),
                fields: vec![],
            },
        ],
        is_union: false,
    }
}

fn result_enum() -> EnumDef {
    EnumDef {
        name: "Result".to_string(),
        type_params: vec!["T".to_string(), "E".to_string()],
        variants: vec![
            EnumVariant {
                name: "Ok".to_string(),
                fields: vec![EnumVariantField {
                    name: None,
                    ty: Type::Generic("T".to_string()),
                }],
            },
            EnumVariant {
                name: "Err".to_string(),
                fields: vec![EnumVariantField {
                    name: None,
                    ty: Type::Generic("E".to_string()),
                }],
            },
        ],
        is_union: false,
    }
}

/// 把内置类前置注入到程序语句序列。
///
/// 前置（而非追加）确保内置类在用户类之前分配异常标签与处理，
/// 也让用户类可以继承内置类（如 `class MyError: Error`）。
/// 若用户已自定义同名类，则跳过该内置类（用户优先）。
pub fn inject_builtin_classes(mut program: Program) -> Program {
    let mut builtins: Vec<Statement> = Vec::new();

    if !user_defines_class(&program, "Error") {
        builtins.push(Statement::ClassDef(error_class()));
    }
    if !user_defines_enum(&program, "Option") {
        builtins.push(Statement::EnumDef(option_enum()));
    }
    if !user_defines_enum(&program, "Result") {
        builtins.push(Statement::EnumDef(result_enum()));
    }

    if builtins.is_empty() {
        return program;
    }

    builtins.extend(program.statements.drain(..));
    program.statements = builtins;
    program
}
