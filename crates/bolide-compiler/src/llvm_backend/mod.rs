//! LLVM backend for Bolide (optional path alongside Cranelift).
//!
//! **Does not modify** `jit.rs` / `aot.rs`. Default CLI still uses Cranelift.
//!
//! Strategy:
//! - Shared frontend pipeline (macros / traits / generators / monomorph / inline)
//! - Emit LLVM IR text, then invoke system `clang` (+ linker) to produce
//!   objects/executables or a temporary shared library for "JIT".
//!
//! Growing toward full language coverage (classes, ADTs/match, dict, generators,
//! channels/select, tuples, operator overload, try/throw, modules). Unsupported
//! constructs return a clear error so users can fall back to `--backend cranelift`.

mod codegen;
mod frontend;
mod link;
mod oop;

use bolide_parser::Program;

/// Result of LLVM AOT compilation (object bytes + metadata).
pub struct LlvmAotCompileResult {
    pub object_code: Vec<u8>,
    pub ir_text: String,
    pub extern_libs: Vec<String>,
}

/// LLVM AOT compiler (independent of Cranelift `AotCompiler`).
pub struct LlvmAotCompiler {
    base_dir: Option<String>,
}

impl LlvmAotCompiler {
    pub fn new() -> Result<Self, String> {
        link::require_clang()?;
        Ok(Self { base_dir: None })
    }

    pub fn set_base_dir(&mut self, dir: &str) {
        self.base_dir = Some(dir.to_string());
    }

    pub fn compile(&mut self, program: &Program) -> Result<LlvmAotCompileResult, String> {
        let prepared = frontend::prepare_program(program, self.base_dir.as_deref())?;
        let ir = codegen::emit_llvm_ir(&prepared)?;
        let object_code = link::compile_ir_to_object(&ir)?;
        Ok(LlvmAotCompileResult {
            object_code,
            ir_text: ir,
            extern_libs: vec![],
        })
    }

    /// Compile + link to an executable path (uses bolide_runtime).
    pub fn compile_and_link(
        &mut self,
        program: &Program,
        output_exe: &std::path::Path,
    ) -> Result<(), String> {
        let prepared = frontend::prepare_program(program, self.base_dir.as_deref())?;
        let ir = codegen::emit_llvm_ir(&prepared)?;
        link::compile_and_link_exe(&ir, output_exe)
    }
}

/// LLVM "JIT": compile to a temp executable and run it, returning process exit code
/// as `i64` (host prints `Result:` like the Cranelift path when used from CLI).
///
/// True in-process ORC JIT can replace this later without touching Cranelift.
pub struct LlvmJitCompiler {
    base_dir: Option<String>,
}

impl LlvmJitCompiler {
    pub fn new() -> Result<Self, String> {
        link::require_clang()?;
        Ok(Self { base_dir: None })
    }

    pub fn set_base_dir(&mut self, dir: &str) {
        self.base_dir = Some(dir.to_string());
    }

    /// Compile and execute; returns the program's `i64` return value (main).
    pub fn compile_and_run(&mut self, program: &Program) -> Result<i64, String> {
        let prepared = frontend::prepare_program(program, self.base_dir.as_deref())?;
        let ir = codegen::emit_llvm_ir(&prepared)?;
        link::compile_run_temp(&ir)
    }
}
