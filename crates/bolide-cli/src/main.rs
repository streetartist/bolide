use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::fs;
use std::io::{self, Write};

use bolide_parser::parse_source;
use bolide_compiler::JitCompiler;

/// REPL 状态，维护累积的代码
struct ReplState {
    /// 函数定义
    functions: Vec<String>,
    /// 全局变量声明
    globals: Vec<String>,
}

impl ReplState {
    fn new() -> Self {
        Self {
            functions: Vec::new(),
            globals: Vec::new(),
        }
    }

    /// 判断输入类型并添加到状态
    fn add_input(&mut self, input: &str) -> InputType {
        let trimmed = input.trim();

        if trimmed.starts_with("fn ") {
            self.functions.push(input.to_string());
            InputType::FuncDef
        } else if trimmed.starts_with("let ") {
            self.globals.push(input.to_string());
            InputType::VarDecl
        } else if trimmed.starts_with("class ") {
            self.functions.push(input.to_string());
            InputType::ClassDef
        } else {
            InputType::Expr
        }
    }

    /// 生成完整的程序代码
    fn build_program(&self, expr: Option<&str>) -> String {
        let mut code = String::new();

        // 添加函数/类定义
        for func in &self.functions {
            code.push_str(func);
            code.push('\n');
        }

        // 添加全局变量
        for var in &self.globals {
            code.push_str(var);
            code.push('\n');
        }

        // 添加表达式/语句
        if let Some(e) = expr {
            code.push_str(e);
            code.push('\n');
        }

        code
    }
}

#[derive(Debug, PartialEq)]
enum InputType {
    FuncDef,
    VarDecl,
    ClassDef,
    Expr,
}

#[derive(Parser)]
#[command(name = "bolide")]
#[command(about = "Bolide programming language compiler")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
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
        Some(Commands::Run { file }) => {
            run_file(&file)?;
        }
        Some(Commands::Compile { file, output }) => {
            let out = output.unwrap_or_else(|| file.with_extension("exe"));
            println!("Compiling: {} -> {}", file.display(), out.display());
            // TODO: 实现 AOT 编译
        }
        None => {
            run_repl()?;
        }
    }

    Ok(())
}

fn run_file(file: &PathBuf) -> miette::Result<()> {
    println!("Running: {}", file.display());
    let source = fs::read_to_string(file)
        .map_err(|e| miette::miette!("Failed to read file: {}", e))?;

    let ast = parse_source(&source)
        .map_err(|e| miette::miette!("Parse error: {}", e))?;

    let mut compiler = JitCompiler::new();
    let main_ptr = compiler.compile(&ast)
        .map_err(|e| miette::miette!("Compile error: {}", e))?;

    let main_fn: fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
    let result = main_fn();
    println!("Result: {}", result);
    Ok(())
}

fn run_repl() -> miette::Result<()> {
    println!("Bolide {} - Interactive Mode", env!("CARGO_PKG_VERSION"));
    println!("Type 'exit' or 'quit' to exit, 'help' for help.");
    println!();

    let stdin = io::stdin();
    let mut state = ReplState::new();
    let mut input_buffer = String::new();
    let mut in_multiline = false;

    loop {
        if in_multiline {
            print!("... ");
        } else {
            print!(">>> ");
        }
        io::stdout().flush().unwrap();

        let mut line = String::new();
        if stdin.read_line(&mut line).is_err() {
            break;
        }

        let line = line.trim_end_matches('\n').trim_end_matches('\r');

        // 处理多行输入（函数/类定义）
        if in_multiline {
            input_buffer.push_str(line);
            input_buffer.push('\n');

            // 检查是否结束（以 } 结尾）
            if line.trim() == "}" {
                in_multiline = false;
                let input = input_buffer.trim().to_string();
                input_buffer.clear();

                match eval_input(&mut state, &input) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
            continue;
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        match input {
            "exit" | "quit" => break,
            "help" => {
                print_help();
                continue;
            }
            "clear" => {
                state = ReplState::new();
                println!("State cleared.");
                continue;
            }
            _ => {}
        }

        // 检查是否是多行输入的开始（包含 { 但不以 } 结尾）
        if input.contains('{') && !input.trim().ends_with('}') {
            in_multiline = true;
            input_buffer = input.to_string();
            input_buffer.push('\n');
            continue;
        }

        match eval_input(&mut state, input) {
            Ok(msg) => println!("{}", msg),
            Err(e) => eprintln!("Error: {}", e),
        }
    }

    println!("Goodbye!");
    Ok(())
}

fn print_help() {
    println!("Bolide Interactive Mode Commands:");
    println!("  exit, quit  - Exit the REPL");
    println!("  help        - Show this help message");
    println!("  clear       - Clear all defined variables and functions");
    println!();
    println!("Enter Bolide code:");
    println!("  - Variables:   let x: int = 10;");
    println!("  - Functions:   fn add(a: int, b: int) -> int {{ return a + b }}");
}

fn eval_input(state: &mut ReplState, input: &str) -> Result<String, String> {
    let input_type = state.add_input(input);

    match input_type {
        InputType::FuncDef => {
            // 验证函数定义是否有效
            let code = state.build_program(None);
            let ast = parse_source(&code).map_err(|e| {
                state.functions.pop();
                e.to_string()
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.functions.pop();
                e.to_string()
            })?;
            Ok("Function defined.".to_string())
        }
        InputType::ClassDef => {
            let code = state.build_program(None);
            let ast = parse_source(&code).map_err(|e| {
                state.functions.pop();
                e.to_string()
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.functions.pop();
                e.to_string()
            })?;
            Ok("Class defined.".to_string())
        }
        InputType::VarDecl => {
            // 验证变量声明是否有效
            let code = state.build_program(None);
            let ast = parse_source(&code).map_err(|e| {
                state.globals.pop();
                e.to_string()
            })?;
            let mut compiler = JitCompiler::new();
            compiler.compile(&ast).map_err(|e| {
                state.globals.pop();
                e.to_string()
            })?;
            Ok("Variable declared.".to_string())
        }
        InputType::Expr => {
            let code = state.build_program(Some(input));
            let ast = parse_source(&code).map_err(|e| e.to_string())?;
            let mut compiler = JitCompiler::new();
            let main_ptr = compiler.compile(&ast).map_err(|e| e.to_string())?;
            let main_fn: fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
            let result = main_fn();
            // 只有非零结果才显示（print等语句返回0）
            if result != 0 {
                Ok(result.to_string())
            } else {
                Ok(String::new())
            }
        }
    }
}
