//! Bolide Compiler
//!
//! 使用 Cranelift 进行代码生成

mod aot;
mod builtins;
mod closure_capture;
mod deps;
mod ffi_spec;
mod inline;
mod jit;
mod monomorph;

pub use aot::AotCompileResult;
pub use aot::AotCompiler;
pub use aot::RUNTIME_SYMBOLS;
pub use deps::DependencyManifest;
pub use jit::JitCompiler;

pub(crate) use builtins::inject_builtin_classes;
pub(crate) use inline::inline_expand;
pub(crate) use monomorph::monomorphize;

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
