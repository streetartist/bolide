use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::fs;

use bolide_parser::parse_source;
use bolide_compiler::JitCompiler;

#[derive(Parser)]
#[command(name = "bolide")]
#[command(about = "Bolide programming language compiler")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a Bolide source file (JIT)
    Run {
        /// Source file path
        file: PathBuf,
    },
    /// Compile a Bolide source file to executable (AOT)
    Compile {
        /// Source file path
        file: PathBuf,
        /// Output file path
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> miette::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { file } => {
            println!("Running: {}", file.display());
            let source = fs::read_to_string(&file)
                .map_err(|e| miette::miette!("Failed to read file: {}", e))?;

            let ast = parse_source(&source)
                .map_err(|e| miette::miette!("Parse error: {}", e))?;

            let mut compiler = JitCompiler::new();
            let main_ptr = compiler.compile(&ast)
                .map_err(|e| miette::miette!("Compile error: {}", e))?;

            // 调用 main 函数
            let main_fn: fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
            let result = main_fn();
            println!("Result: {}", result);
        }
        Commands::Compile { file, output } => {
            let out = output.unwrap_or_else(|| file.with_extension("exe"));
            println!("Compiling: {} -> {}", file.display(), out.display());
            // TODO: 实现 AOT 编译
        }
    }

    Ok(())
}
