//! Bolide Parser
//!
//! 使用 pest 进行语法分析

mod ast;
mod convert;

use pest_derive::Parser;

pub use ast::*;
pub use convert::{from_converter_name, parse, parse_with_diagnostics, ParseDiagnostic};

#[derive(Parser)]
#[grammar = "bolide.pest"]
pub struct BolideParser;

/// 解析源代码为 AST
pub fn parse_source(source: &str) -> Result<Program, String> {
    let ast = parse(source)?;
    Ok(ast)
}

/// 解析源代码为 AST，并返回带源码位置的诊断错误。
pub fn parse_source_with_diagnostics(source: &str) -> Result<Program, ParseDiagnostic> {
    parse_with_diagnostics(source)
}
