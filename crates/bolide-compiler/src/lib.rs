//! Bolide Compiler
//!
//! 使用 Cranelift 进行代码生成

mod jit;
mod aot;

pub use jit::JitCompiler;
pub use aot::AotCompiler;
pub use aot::AotCompileResult;
pub use aot::RUNTIME_SYMBOLS;
