//! Bolide Parser
//!
//! 使用 pest 进行语法分析

mod ast;
mod convert;

use pest_derive::Parser;

pub use ast::*;
pub use convert::parse;

#[derive(Parser)]
#[grammar = "bolide.pest"]
pub struct BolideParser;

/// 解析源代码为 AST
pub fn parse_source(source: &str) -> Result<Program, String> {
    let ast = parse(source)?;
    Ok(ast)
}
