//! Bolide Compiler
//!
//! 默认使用 Cranelift 进行 JIT/AOT 代码生成。
//! 另提供独立的 **LLVM 后端**（`llvm_backend`），与 Cranelift 路径并存、互不影响。

mod aot;
mod builtins;
mod closure_capture;
mod deps;
mod ffi_spec;
mod generators;
mod inline;
mod jit;
mod llvm_backend;
mod macro_expand;
mod monomorph;
mod operator_overload;
mod traits;

pub use aot::AotCompileResult;
pub use aot::AotCompiler;
pub use aot::RUNTIME_SYMBOLS;
pub use deps::DependencyManifest;
pub use generators::desugar_generators;
pub use jit::JitCompiler;
pub use llvm_backend::{LlvmAotCompileResult, LlvmAotCompiler, LlvmJitCompiler};
pub use traits::desugar_traits;
pub use macro_expand::{expand_macros, expand_macros_with_ctx, pretty_print, ExpandContext};

pub(crate) use builtins::inject_builtin_classes;
pub(crate) use inline::inline_expand;
pub(crate) use monomorph::{from_converter_name, monomorphize};

use bolide_parser::{Expr, Program, Statement};

/// 程序是否可能跨函数传播异常（throw / try / ? / ! unwrap / try 表达式）。
///
/// 若整份程序都不会设置 pending exception，调用点可跳过
/// `bolide_exception_pending` 检查——这对 fib 这类纯递归数值代码是巨大收益
///（每次调用省一次 TLS 读 + 分支 + 额外 basic block）。
pub(crate) fn program_needs_exception_checks(program: &Program) -> bool {
    program.statements.iter().any(stmt_needs_exception_checks)
}

fn stmt_needs_exception_checks(stmt: &Statement) -> bool {
    match stmt {
        Statement::Throw(_) | Statement::Try(_) => true,
        Statement::Expr(e) | Statement::Return(Some(e)) => expr_needs_exception_checks(e),
        Statement::VarDecl(d) => d
            .value
            .as_ref()
            .map(expr_needs_exception_checks)
            .unwrap_or(false),
        Statement::Assign(a) => expr_needs_exception_checks(&a.value),
        Statement::If(i) => {
            expr_needs_exception_checks(&i.condition)
                || i.then_body.iter().any(stmt_needs_exception_checks)
                || i.elif_branches.iter().any(|(c, b)| {
                    expr_needs_exception_checks(c) || b.iter().any(stmt_needs_exception_checks)
                })
                || i.else_body
                    .as_ref()
                    .map(|b| b.iter().any(stmt_needs_exception_checks))
                    .unwrap_or(false)
        }
        Statement::While(w) => {
            expr_needs_exception_checks(&w.condition)
                || w.body.iter().any(stmt_needs_exception_checks)
        }
        Statement::For(f) => {
            expr_needs_exception_checks(&f.iter) || f.body.iter().any(stmt_needs_exception_checks)
        }
        Statement::Match(m) => {
            expr_needs_exception_checks(&m.expr)
                || m.arms
                    .iter()
                    .any(|arm| arm.body.iter().any(stmt_needs_exception_checks))
        }
        Statement::FuncDef(f) => f.body.iter().any(stmt_needs_exception_checks),
        Statement::ClassDef(c) => c
            .methods
            .iter()
            .any(|m| m.body.iter().any(stmt_needs_exception_checks)),
        Statement::With(w) => {
            w.items
                .iter()
                .any(|it| expr_needs_exception_checks(&it.expr))
                || w.body.iter().any(stmt_needs_exception_checks)
        }
        Statement::SpawnSelect(s) => s.branches.iter().any(|br| match br {
            bolide_parser::SpawnSelectBranch::Bind { expr, body, .. } => {
                expr_needs_exception_checks(expr) || body.iter().any(stmt_needs_exception_checks)
            }
            bolide_parser::SpawnSelectBranch::Expr { expr, body } => {
                expr_needs_exception_checks(expr) || body.iter().any(stmt_needs_exception_checks)
            }
        }),
        Statement::Pool(p) => {
            expr_needs_exception_checks(&p.size) || p.body.iter().any(stmt_needs_exception_checks)
        }
        Statement::Yield(e) => expr_needs_exception_checks(e),
        Statement::TraitImpl(i) => i
            .methods
            .iter()
            .any(|m| m.body.iter().any(stmt_needs_exception_checks)),
        Statement::MacroRep { body, .. } => body.iter().any(stmt_needs_exception_checks),
        Statement::ComptimeFn(f) => f.body.iter().any(stmt_needs_exception_checks),
        _ => false,
    }
}

fn expr_needs_exception_checks(expr: &Expr) -> bool {
    match expr {
        Expr::Propagate(_) | Expr::Raise(_) | Expr::TryExpr(_) => true,
        Expr::Call(c, args) => {
            expr_needs_exception_checks(c) || args.iter().any(expr_needs_exception_checks)
        }
        Expr::Member(b, _) | Expr::UnaryOp(_, b) | Expr::Await(b) => {
            expr_needs_exception_checks(b)
        }
        Expr::Index(b, i) => {
            expr_needs_exception_checks(b) || expr_needs_exception_checks(i)
        }
        Expr::BinOp(l, _, r) => {
            expr_needs_exception_checks(l) || expr_needs_exception_checks(r)
        }
        Expr::List(items) | Expr::Tuple(items) | Expr::SpawnAll(items) => {
            items.iter().any(expr_needs_exception_checks)
        }
        Expr::Dict(pairs) => pairs.iter().any(|(k, v)| {
            expr_needs_exception_checks(k) || expr_needs_exception_checks(v)
        }),
        Expr::Spawn(_, args) | Expr::SpawnThread(_, args) => {
            args.iter().any(expr_needs_exception_checks)
        }
        Expr::ValueConstruct(_, fields) => fields
            .iter()
            .any(|(_, e)| expr_needs_exception_checks(e)),
        Expr::Closure { body, .. } => body.iter().any(stmt_needs_exception_checks),
        Expr::ListComprehension {
            expr,
            iter,
            filter,
            ..
        } => {
            expr_needs_exception_checks(expr)
                || expr_needs_exception_checks(iter)
                || filter
                    .as_ref()
                    .map(|e| expr_needs_exception_checks(e))
                    .unwrap_or(false)
        }
        Expr::NamedArg(_, e) | Expr::SpreadArg(e) => expr_needs_exception_checks(e),
        _ => false,
    }
}

/// 标准库 import 候选路径。
///
/// 支持短写：
/// - `std/fs` / `std/fs.bl` → 同时尝试 `std/fs/fs.bl`
/// - `std/fs/fs.bl` 保持原样
pub(crate) fn std_import_candidates(file_path: &str) -> Vec<String> {
    let mut out = Vec::new();
    out.push(file_path.to_string());
    if let Some(rest) = file_path.strip_prefix("std/") {
        let bare = rest.strip_suffix(".bl").unwrap_or(rest);
        // 单段模块名：std/fs 或 std/fs.bl
        if !bare.is_empty() && !bare.contains('/') && !bare.contains('\\') {
            let nested = format!("std/{}/{}.bl", bare, bare);
            if nested != file_path {
                out.push(nested);
            }
            let flat = format!("std/{}.bl", bare);
            if flat != file_path && !out.iter().any(|p| p == &flat) {
                out.push(flat);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
	use super::AotCompiler;

	#[test]
	fn aot_compiles_value_type_raytracer_example() {
		let source = std::fs::read_to_string("../../examples/raytracer_vt.bl")
			.expect("failed to read raytracer_vt example");
		let program = bolide_parser::parse_source(&source)
			.expect("failed to parse raytracer_vt example");

		let result = AotCompiler::new()
			.expect("failed to create aot compiler")
			.compile(&program);

		assert!(
			result.is_ok(),
			"expected AOT compile to succeed, got: {:?}",
			result.err()
		);
	}
}
