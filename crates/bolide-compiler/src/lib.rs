//! Bolide Compiler
//!
//! 使用 Cranelift 进行代码生成

mod aot;
mod builtins;
mod closure_capture;
mod jit;
mod monomorph;

pub use aot::AotCompileResult;
pub use aot::AotCompiler;
pub use aot::RUNTIME_SYMBOLS;
pub use jit::JitCompiler;

pub(crate) use builtins::inject_builtin_classes;
pub(crate) use monomorph::monomorphize;
