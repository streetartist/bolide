//! Bolide Compiler
//!
//! 使用 Cranelift 进行代码生成

mod aot;
mod builtins;
mod closure_capture;
mod deps;
mod ffi_spec;
mod generators;
mod inline;
mod jit;
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
pub use traits::desugar_traits;
pub use macro_expand::{expand_macros, expand_macros_with_ctx, pretty_print, ExpandContext};

pub(crate) use builtins::inject_builtin_classes;
pub(crate) use inline::inline_expand;
pub(crate) use monomorph::{from_converter_name, monomorphize};

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
