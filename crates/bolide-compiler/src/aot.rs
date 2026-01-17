//! AOT 编译器
//!
//! 使用 Cranelift 实现的提前编译器，生成目标文件

use cranelift::prelude::*;
use cranelift::prelude::isa::{TargetIsa, CallConv};
use cranelift_object::{ObjectBuilder, ObjectModule};
use cranelift_module::{DataDescription, Linkage, Module, FuncId, DataId};
use cranelift_codegen::ir::{FuncRef, StackSlotData, StackSlotKind};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use bolide_parser::{Program, Statement, Expr, Type as BolideType, FuncDef, Param, ParamMode, ExternBlock, ExternDecl, CType, BinOp, UnaryOp};

/// AOT 编译结果
#[derive(Debug)]
pub struct AotCompileResult {
    /// 目标文件字节码
    pub object_code: Vec<u8>,
    /// 外部库列表 (库路径)
    pub extern_libs: Vec<String>,
}

/// Trampoline 信息
struct TrampolineInfo {
    func_id: FuncId,
    param_types: Vec<BolideType>,
    env_size: i64,
}

/// 类字段信息
#[derive(Clone)]
struct FieldInfo {
    name: String,
    ty: BolideType,
    offset: usize,
}

/// 类信息
#[derive(Clone)]
struct ClassInfo {
    name: String,
    parent: Option<String>,
    fields: Vec<FieldInfo>,
    methods: Vec<String>,
    size: usize,
}

/// AOT 编译器
pub struct AotCompiler {
    module: ObjectModule,
    ctx: codegen::Context,
    data_desc: DataDescription,
    /// 函数名 -> 函数ID 映射
    functions: HashMap<String, FuncId>,
    /// 函数名 -> 返回类型 映射
    func_return_types: HashMap<String, Option<BolideType>>,
    /// 函数名 -> 参数列表 映射
    func_params: HashMap<String, Vec<Param>>,
    /// 被 spawn 的函数名 -> trampoline 信息
    trampolines: HashMap<String, TrampolineInfo>,
    /// trampoline 计数器
    trampoline_counter: usize,
    /// 指针类型
    ptr_type: types::Type,
    /// 类名 -> 类信息 映射
    classes: HashMap<String, ClassInfo>,
    /// async 函数集合
    async_funcs: HashSet<String>,
    /// extern 函数信息: 函数名 -> (库路径, 函数声明)
    extern_funcs: HashMap<String, (String, bolide_parser::ExternFunc)>,
    /// 模块名映射: 模块名 -> 文件路径
    modules: HashMap<String, String>,
    /// 使用生命周期模式的函数集合
    lifetime_funcs: HashSet<String>,
    /// 字符串常量数据
    string_data: HashMap<String, DataId>,
}

/// 运行时符号列表
pub const RUNTIME_SYMBOLS: &[&str] = &[
    // 基本类型打印
    "print_int", "print_float", "print_bool", "print_bigint",
    "print_decimal", "print_string", "print_dynamic",
    // 用户输入
    "input", "input_prompt",
    // BigInt
    "bigint_from_i64", "bigint_from_str", "bigint_add", "bigint_sub",
    "bigint_mul", "bigint_div", "bigint_rem", "bigint_neg",
    "bigint_eq", "bigint_lt", "bigint_le", "bigint_gt", "bigint_ge",
    "bigint_to_i64", "bigint_clone", "bigint_debug_stats",
    // Decimal
    "decimal_from_i64", "decimal_from_f64", "decimal_from_str",
    "decimal_add", "decimal_sub", "decimal_mul", "decimal_div",
    "decimal_neg", "decimal_eq", "decimal_lt", "decimal_to_i64",
    "decimal_to_f64", "decimal_clone",
    // Dynamic
    "dynamic_from_int", "dynamic_from_float", "dynamic_from_bool",
    "dynamic_from_string", "dynamic_from_list", "dynamic_from_bigint",
    "dynamic_from_decimal", "dynamic_add", "dynamic_sub", "dynamic_mul",
    "dynamic_div", "dynamic_neg", "dynamic_eq", "dynamic_lt", "dynamic_clone",
    // String
    "string_from_slice", "string_literal", "string_as_cstr", "string_concat",
    "string_eq", "string_from_int", "string_from_float", "string_from_bool",
    "string_from_bigint", "string_from_decimal", "string_to_int", "string_to_float",
    // Memory
    "bolide_alloc", "bolide_free",
    // Object
    "object_alloc", "object_retain", "object_release", "object_clone",
    // Thread
    "thread_spawn_int", "thread_spawn_float", "thread_spawn_ptr",
    "thread_spawn_int_with_env", "thread_spawn_float_with_env", "thread_spawn_ptr_with_env",
    "thread_join_int", "thread_join_float", "thread_join_ptr",
    "thread_handle_free", "thread_cancel", "thread_is_cancelled",
    // Pool
    "pool_create", "pool_enter", "pool_exit", "pool_is_active",
    "pool_spawn_int", "pool_spawn_float", "pool_spawn_ptr",
    "pool_spawn_int_with_env", "pool_spawn_float_with_env", "pool_spawn_ptr_with_env",
    "pool_join_int", "pool_join_float", "pool_join_ptr",
    "pool_handle_free", "pool_destroy",
    // Channel
    "channel_create", "channel_create_buffered", "channel_send",
    "channel_recv", "channel_close", "channel_free", "channel_select",
    // Coroutine
    "coroutine_spawn_int", "coroutine_spawn_float", "coroutine_spawn_ptr",
    "coroutine_await_int", "coroutine_await_float", "coroutine_await_ptr",
    "coroutine_cancel", "coroutine_free",
    "coroutine_spawn_int_with_env", "coroutine_spawn_float_with_env", "coroutine_spawn_ptr_with_env",
    "scope_enter", "scope_register", "scope_exit",
    // Select
    "select_wait_first",
    // Tuple
    "tuple_new", "tuple_free", "tuple_set", "tuple_get", "tuple_len", "print_tuple",
    // FFI
    "ffi_load_library", "ffi_get_symbol", "ffi_cleanup", "test_callback", "map_int",
    // RC
    "string_retain", "string_release", "string_clone",
    "bigint_retain", "bigint_release",
    "decimal_retain", "decimal_release",
    "list_retain", "list_release", "list_clone",
    "list_new", "list_push", "list_pop", "list_len", "list_get", "list_set",
    "list_insert", "list_remove", "list_clear", "list_reverse", "list_extend",
    "list_contains", "list_index_of", "list_count", "list_sort", "list_slice",
    "list_is_empty", "list_first", "list_last", "print_list",
    // Dict
    "dict_new", "dict_retain", "dict_release", "dict_clone",
    "dict_set", "dict_get", "dict_contains", "dict_remove",
    "dict_len", "dict_is_empty", "dict_clear", "dict_keys", "dict_values",
    "dict_iter", "print_dict",
    "dynamic_retain", "dynamic_release",
];

impl AotCompiler {
    /// 创建新的 AOT 编译器
    pub fn new() -> Result<Self, String> {
        let isa_builder = cranelift_native::builder()
            .map_err(|e| format!("Failed to create ISA builder: {}", e))?;

        let flag_builder = settings::builder();
        let flags = settings::Flags::new(flag_builder);
        let isa = isa_builder.finish(flags)
            .map_err(|e| format!("Failed to create ISA: {}", e))?;

        let builder = ObjectBuilder::new(
            isa,
            "bolide_program",
            cranelift_module::default_libcall_names(),
        ).map_err(|e| format!("Failed to create object builder: {}", e))?;

        let module = ObjectModule::new(builder);
        let ptr_type = module.target_config().pointer_type();
        let ctx = module.make_context();
        let data_desc = DataDescription::new();

        Ok(Self {
            module,
            ctx,
            data_desc,
            functions: HashMap::new(),
            func_return_types: HashMap::new(),
            func_params: HashMap::new(),
            trampolines: HashMap::new(),
            trampoline_counter: 0,
            ptr_type,
            classes: HashMap::new(),
            async_funcs: HashSet::new(),
            extern_funcs: HashMap::new(),
            modules: HashMap::new(),
            lifetime_funcs: HashSet::new(),
            string_data: HashMap::new(),
        })
    }

    /// Get or create a data object for a string literal
    fn get_or_create_string_data(&mut self, s: &str) -> Result<DataId, String> {
        if let Some(&data_id) = self.string_data.get(s) {
            return Ok(data_id);
        }

        // Create a unique name for this string data
        let name = format!("str_{}", self.string_data.len());

        // Declare the data object
        let data_id = self.module.declare_data(&name, Linkage::Local, false, false)
            .map_err(|e| format!("Failed to declare string data: {}", e))?;

        // Define the data with the string bytes
        self.data_desc.clear();
        self.data_desc.define(s.as_bytes().to_vec().into_boxed_slice());

        self.module.define_data(data_id, &self.data_desc)
            .map_err(|e| format!("Failed to define string data: {}", e))?;

        self.string_data.insert(s.to_string(), data_id);
        Ok(data_id)
    }

    /// Collect all string literals from statements
    fn collect_strings_from_stmts(&self, stmts: &[Statement]) -> HashSet<String> {
        let mut strings = HashSet::new();
        for stmt in stmts {
            self.collect_strings_from_stmt(stmt, &mut strings);
        }
        strings
    }

    fn collect_strings_from_stmt(&self, stmt: &Statement, strings: &mut HashSet<String>) {
        match stmt {
            Statement::Expr(e) => self.collect_strings_from_expr(e, strings),
            Statement::VarDecl(v) => {
                if let Some(ref e) = v.value {
                    self.collect_strings_from_expr(e, strings);
                }
            }
            Statement::Assign(a) => {
                self.collect_strings_from_expr(&a.target, strings);
                self.collect_strings_from_expr(&a.value, strings);
            }
            Statement::If(if_stmt) => {
                self.collect_strings_from_expr(&if_stmt.condition, strings);
                for s in &if_stmt.then_body { self.collect_strings_from_stmt(s, strings); }
                for (cond, body) in &if_stmt.elif_branches {
                    self.collect_strings_from_expr(cond, strings);
                    for s in body { self.collect_strings_from_stmt(s, strings); }
                }
                if let Some(ref eb) = if_stmt.else_body {
                    for s in eb { self.collect_strings_from_stmt(s, strings); }
                }
            }
            Statement::While(while_stmt) => {
                self.collect_strings_from_expr(&while_stmt.condition, strings);
                for s in &while_stmt.body { self.collect_strings_from_stmt(s, strings); }
            }
            Statement::For(for_stmt) => {
                self.collect_strings_from_expr(&for_stmt.iter, strings);
                for s in &for_stmt.body { self.collect_strings_from_stmt(s, strings); }
            }
            Statement::Return(Some(e)) => self.collect_strings_from_expr(e, strings),
            _ => {}
        }
    }

    fn collect_strings_from_expr(&self, expr: &Expr, strings: &mut HashSet<String>) {
        match expr {
            Expr::String(s) => { strings.insert(s.clone()); }
            Expr::Call(callee, args) => {
                self.collect_strings_from_expr(callee, strings);
                for a in args { self.collect_strings_from_expr(a, strings); }
            }
            Expr::BinOp(l, _, r) => {
                self.collect_strings_from_expr(l, strings);
                self.collect_strings_from_expr(r, strings);
            }
            Expr::UnaryOp(_, e) => self.collect_strings_from_expr(e, strings),
            Expr::Index(b, i) => {
                self.collect_strings_from_expr(b, strings);
                self.collect_strings_from_expr(i, strings);
            }
            Expr::Member(b, _) => self.collect_strings_from_expr(b, strings),
            Expr::List(items) => {
                for i in items { self.collect_strings_from_expr(i, strings); }
            }
            Expr::Tuple(items) => {
                for i in items { self.collect_strings_from_expr(i, strings); }
            }
            Expr::Dict(entries) => {
                for (k, v) in entries {
                    self.collect_strings_from_expr(k, strings);
                    self.collect_strings_from_expr(v, strings);
                }
            }
            _ => {}
        }
    }

    /// 编译程序并返回目标文件字节
    pub fn compile(mut self, program: &Program) -> Result<AotCompileResult, String> {
        // 预处理 import 语句
        let program = self.process_imports(program)?;

        // 注册内置函数
        self.register_builtins()?;

        // 处理 extern 块
        for stmt in &program.statements {
            if let Statement::ExternBlock(eb) = stmt {
                self.register_extern_block(eb)?;
            }
        }

        // 收集类定义
        self.collect_classes(&program)?;

        // 第一遍：收集函数声明
        for stmt in &program.statements {
            if let Statement::FuncDef(func) = stmt {
                self.declare_function(func)?;
                if func.is_async {
                    self.async_funcs.insert(func.name.clone());
                }
            }
        }

        // 声明类构造函数和方法
        for class_name in self.classes.keys().cloned().collect::<Vec<_>>() {
            self.declare_class_constructor(&class_name)?;
        }
        self.declare_class_methods(&program)?;

        // 生成 trampolines
        let spawn_targets = self.collect_spawn_targets(&program);
        self.generate_trampolines(&spawn_targets)?;

        // 编译类
        for class_name in self.classes.keys().cloned().collect::<Vec<_>>() {
            self.compile_class_constructor(&class_name)?;
        }
        self.compile_class_methods(&program)?;

        // 第二遍：编译函数
        let mut toplevel_stmts = Vec::new();
        for stmt in &program.statements {
            match stmt {
                Statement::FuncDef(func) => {
                    self.compile_function(func)?;
                }
                Statement::ClassDef(_) => {}
                _ => {
                    toplevel_stmts.push(stmt.clone());
                }
            }
        }

        // 包装顶层代码为 main 函数
        let main_func = FuncDef {
            name: "main".to_string(),
            is_async: false,
            params: vec![],
            return_type: Some(BolideType::Int),
            lifetime_deps: None,
            body: toplevel_stmts,
        };
        self.declare_function(&main_func)?;
        self.compile_function(&main_func)?;

        // 收集外部库列表 (去重)
        let extern_libs: Vec<String> = self.extern_funcs.values()
            .map(|(lib_path, _)| lib_path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // 生成目标文件
        let product = self.module.finish();
        let object_code = product.emit().map_err(|e| format!("Emit error: {}", e))?;

        Ok(AotCompileResult {
            object_code,
            extern_libs,
        })
    }

    /// Bolide 类型转换为 Cranelift 类型
    fn bolide_type_to_cranelift(&self, ty: &BolideType) -> types::Type {
        match ty {
            BolideType::Int => types::I64,
            BolideType::Float => types::F64,
            BolideType::Bool => types::I64,
            BolideType::Str => self.ptr_type,
            BolideType::BigInt => self.ptr_type,
            BolideType::Decimal => self.ptr_type,
            BolideType::Dynamic => self.ptr_type,
            BolideType::Ptr => self.ptr_type,
            BolideType::Channel(_) => self.ptr_type,
            BolideType::Future => self.ptr_type,
            BolideType::Func => self.ptr_type,
            BolideType::FuncSig(_, _) => self.ptr_type,
            BolideType::List(_) => self.ptr_type,
            BolideType::Dict(_, _) => self.ptr_type,
            BolideType::Tuple(_) => self.ptr_type,
            BolideType::Custom(_) => self.ptr_type,
            BolideType::Weak(_) => self.ptr_type,
            BolideType::Unowned(_) => self.ptr_type,
        }
    }

    /// 处理 import 语句
    fn process_imports(&mut self, program: &Program) -> Result<Program, String> {
        let mut merged_statements = Vec::new();
        let mut imported_files: HashSet<String> = HashSet::new();

        for stmt in &program.statements {
            if let Statement::Import(import) = stmt {
                if let Some(ref file_path) = import.file_path {
                    if imported_files.contains(file_path) {
                        continue;
                    }
                    imported_files.insert(file_path.clone());

                    let module_name = Self::extract_module_name(file_path);
                    self.modules.insert(module_name.clone(), file_path.clone());

                    let imported = self.load_module(file_path)?;

                    for imp_stmt in imported.statements {
                        match imp_stmt {
                            Statement::FuncDef(mut func) => {
                                func.name = format!("@{}_{}", module_name, func.name);
                                merged_statements.push(Statement::FuncDef(func));
                            }
                            Statement::ClassDef(mut class) => {
                                class.name = format!("@{}_{}", module_name, class.name);
                                merged_statements.push(Statement::ClassDef(class));
                            }
                            Statement::ExternBlock(ext) => {
                                merged_statements.push(Statement::ExternBlock(ext));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        for stmt in &program.statements {
            merged_statements.push(stmt.clone());
        }

        Ok(Program { statements: merged_statements })
    }

    fn extract_module_name(file_path: &str) -> String {
        Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module")
            .to_string()
    }

    fn load_module(&self, file_path: &str) -> Result<Program, String> {
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to load module '{}': {}", file_path, e))?;
        bolide_parser::parse_source(&content)
            .map_err(|e| format!("Failed to parse module '{}': {}", file_path, e))
    }

    /// 注册内置函数
    fn register_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_print_int(int) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_print_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("print_int".to_string(), id);

        // bolide_print_float(float) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        let id = self.module.declare_function("bolide_print_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("print_float".to_string(), id);

        // bolide_print_bool(int) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_print_bool", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("print_bool".to_string(), id);

        // bolide_print_bigint(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_print_bigint", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("print_bigint".to_string(), id);

        // bolide_print_decimal(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_print_decimal", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("print_decimal".to_string(), id);

        // bolide_print_string(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_print_string", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("print_string".to_string(), id);

        self.register_more_builtins()
    }

    fn register_more_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_input() -> ptr
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_input", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("input".to_string(), id);

        // bolide_input_prompt(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_input_prompt", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("input_prompt".to_string(), id);

        // bolide_string_from_int(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_from_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_int".to_string(), id);

        // bolide_string_from_float(f64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_from_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_float".to_string(), id);

        // bolide_string_from_bool(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_from_bool", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_bool".to_string(), id);

        // bolide_string_from_bigint(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_from_bigint", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_bigint".to_string(), id);

        // bolide_string_from_decimal(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_from_decimal", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_decimal".to_string(), id);

        self.register_string_builtins()
    }

    fn register_string_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_string_literal(ptr, len) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_literal", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_literal".to_string(), id);

        // bolide_string_concat(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_concat", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_concat".to_string(), id);

        // bolide_string_eq(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_string_eq", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_eq".to_string(), id);

        // bolide_string_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_release".to_string(), id);

        // bolide_string_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_string_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_clone".to_string(), id);

        // bolide_string_to_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_string_to_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_to_int".to_string(), id);

        // bolide_string_to_float(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self.module.declare_function("bolide_string_to_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("string_to_float".to_string(), id);

        self.register_bigint_builtins()
    }

    fn register_bigint_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_bigint_from_i64(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_bigint_from_i64", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_from_i64".to_string(), id);

        // bolide_bigint_from_str(ptr, len) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_bigint_from_str", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_from_str".to_string(), id);

        // bigint 二元运算: add, sub, mul, div, rem
        for op in &["add", "sub", "mul", "div", "rem"] {
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(ptr));
            sig.params.push(AbiParam::new(ptr));
            sig.returns.push(AbiParam::new(ptr));
            let linker_name = format!("bolide_bigint_{}", op);
            let internal_name = format!("bigint_{}", op);
            let id = self.module.declare_function(&linker_name, Linkage::Import, &sig)
                .map_err(|e| format!("{}", e))?;
            self.functions.insert(internal_name, id);
        }

        // bigint 比较运算: eq, lt, le, gt, ge
        for op in &["eq", "lt", "le", "gt", "ge"] {
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(ptr));
            sig.params.push(AbiParam::new(ptr));
            sig.returns.push(AbiParam::new(types::I64));
            let linker_name = format!("bolide_bigint_{}", op);
            let internal_name = format!("bigint_{}", op);
            let id = self.module.declare_function(&linker_name, Linkage::Import, &sig)
                .map_err(|e| format!("{}", e))?;
            self.functions.insert(internal_name, id);
        }

        // bolide_bigint_release, bolide_bigint_clone
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_bigint_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_release".to_string(), id);

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_bigint_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_clone".to_string(), id);

        // bolide_bigint_debug_stats() -> void
        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("bolide_bigint_debug_stats", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_debug_stats".to_string(), id);

        self.register_list_builtins()
    }

    fn register_list_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_list_new(elem_type: i8) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I8));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_list_new", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("list_new".to_string(), id);

        // bolide_list_push(ptr, i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_list_push", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("list_push".to_string(), id);

        // bolide_list_len(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_list_len", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("list_len".to_string(), id);

        // bolide_list_get(ptr, i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_list_get", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("list_get".to_string(), id);

        // bolide_list_set(ptr, i64, i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_list_set", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("list_set".to_string(), id);

        // bolide_list_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_list_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("list_release".to_string(), id);

        self.register_memory_builtins()
    }

    fn register_memory_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_alloc(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_alloc", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("bolide_alloc".to_string(), id);

        // bolide_free(ptr, i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_free", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("bolide_free".to_string(), id);

        // object_alloc(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("object_alloc", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("object_alloc".to_string(), id);

        // object_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("object_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("object_release".to_string(), id);

        self.register_tuple_builtins()
    }

    fn register_tuple_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_tuple_new(len: i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_tuple_new", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_new".to_string(), id);

        // bolide_tuple_set(ptr, i64, i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_tuple_set", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_set".to_string(), id);

        // bolide_tuple_get(ptr, i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_tuple_get", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_get".to_string(), id);

        // bolide_tuple_free(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_tuple_free", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_free".to_string(), id);

        // bolide_tuple_debug_stats() -> void
        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("bolide_tuple_debug_stats", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_debug_stats".to_string(), id);

        self.register_dict_builtins()
    }

    fn register_dict_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_dict_new(key_type: i8, val_type: i8) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I8));
        sig.params.push(AbiParam::new(types::I8));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_dict_new", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_new".to_string(), id);

        // bolide_dict_set(ptr, i64, i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_dict_set", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_set".to_string(), id);

        // bolide_dict_get(ptr, key) -> value
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_dict_get", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_get".to_string(), id);

        self.register_decimal_builtins()
    }

    fn register_decimal_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_decimal_from_f64(f64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_decimal_from_f64", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_from_f64".to_string(), id);

        // decimal 二元运算: add, sub, mul, div, rem
        for op in &["add", "sub", "mul", "div", "rem"] {
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(ptr));
            sig.params.push(AbiParam::new(ptr));
            sig.returns.push(AbiParam::new(ptr));
            let linker_name = format!("bolide_decimal_{}", op);
            let internal_name = format!("decimal_{}", op);
            let id = self.module.declare_function(&linker_name, Linkage::Import, &sig)
                .map_err(|e| format!("{}", e))?;
            self.functions.insert(internal_name, id);
        }

        // decimal 比较运算: eq, lt, le, gt, ge
        for op in &["eq", "lt", "le", "gt", "ge"] {
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(ptr));
            sig.params.push(AbiParam::new(ptr));
            sig.returns.push(AbiParam::new(types::I64));
            let linker_name = format!("bolide_decimal_{}", op);
            let internal_name = format!("decimal_{}", op);
            let id = self.module.declare_function(&linker_name, Linkage::Import, &sig)
                .map_err(|e| format!("{}", e))?;
            self.functions.insert(internal_name, id);
        }

        self.register_async_builtins()
    }

    fn register_async_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_coroutine_spawn_int(fn_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_coroutine_spawn_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_spawn_int".to_string(), id);

        // bolide_coroutine_await_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_coroutine_await_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_await_int".to_string(), id);

        self.register_channel_builtins()
    }

    fn register_channel_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_channel_create() -> ptr
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_channel_create", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_create".to_string(), id);

        // bolide_channel_recv(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_channel_recv", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_recv".to_string(), id);

        // bolide_channel_send(ptr, i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_channel_send", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_send".to_string(), id);

        // bolide_channel_select(ptr, i64, i64, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));   // channels array
        sig.params.push(AbiParam::new(types::I64)); // count
        sig.params.push(AbiParam::new(types::I64)); // timeout
        sig.params.push(AbiParam::new(ptr));   // value out ptr
        sig.returns.push(AbiParam::new(types::I64)); // selected index
        let id = self.module.declare_function("bolide_channel_select", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_select".to_string(), id);

        self.register_pool_builtins()
    }

    fn register_pool_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // bolide_pool_create(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_pool_create", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_create".to_string(), id);

        // bolide_pool_enter(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_pool_enter", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_enter".to_string(), id);

        // bolide_pool_exit() -> void
        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("bolide_pool_exit", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_exit".to_string(), id);

        // bolide_pool_destroy(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_pool_destroy", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_destroy".to_string(), id);

        self.register_scope_builtins()
    }

    fn register_scope_builtins(&mut self) -> Result<(), String> {
        // bolide_scope_enter() -> void
        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("bolide_scope_enter", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("scope_enter".to_string(), id);

        // bolide_scope_exit() -> void
        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("bolide_scope_exit", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("scope_exit".to_string(), id);

        Ok(())
    }

    /// 注册 extern 块中的函数
    fn register_extern_block(&mut self, eb: &ExternBlock) -> Result<(), String> {
        for decl in &eb.declarations {
            if let ExternDecl::Function(func) = decl {
                let mut sig = self.module.make_signature();
                for param in &func.params {
                    sig.params.push(AbiParam::new(self.ctype_to_cranelift(&param.ty)));
                }
                if let Some(ref ret_ty) = func.return_type {
                    sig.returns.push(AbiParam::new(self.ctype_to_cranelift(ret_ty)));
                }
                let id = self.module.declare_function(&func.name, Linkage::Import, &sig)
                    .map_err(|e| format!("{}", e))?;
                self.functions.insert(func.name.clone(), id);
                self.extern_funcs.insert(func.name.clone(), (eb.lib_path.clone(), func.clone()));
            }
        }
        Ok(())
    }

    /// CType 转换为 Cranelift 类型
    fn ctype_to_cranelift(&self, ty: &CType) -> types::Type {
        match ty {
            CType::Void => types::I64,
            CType::Char | CType::UChar | CType::I8 | CType::U8 => types::I8,
            CType::Short | CType::UShort | CType::I16 | CType::U16 => types::I16,
            CType::Int | CType::UInt | CType::I32 | CType::U32 => types::I32,
            CType::Long | CType::ULong | CType::LongLong | CType::ULongLong
            | CType::I64 | CType::U64 | CType::SizeT | CType::PtrDiffT => types::I64,
            CType::Float => types::F32,
            CType::Double => types::F64,
            CType::Bool => types::I8,
            CType::Ptr(_) | CType::Array(_, _) | CType::FuncPtr { .. } => self.ptr_type,
            CType::Struct(_) => self.ptr_type,
        }
    }

    /// 收集类定义
    fn collect_classes(&mut self, program: &Program) -> Result<(), String> {
        for stmt in &program.statements {
            if let Statement::ClassDef(class) = stmt {
                let mut fields = Vec::new();
                let mut offset = 0usize;

                // 如果有父类，先继承父类字段
                if let Some(ref parent) = class.parent {
                    if let Some(parent_info) = self.classes.get(parent) {
                        fields = parent_info.fields.clone();
                        offset = parent_info.size;
                    }
                }

                // 添加本类字段
                for field in &class.fields {
                    let size = 8; // 所有类型都是 8 字节
                    fields.push(FieldInfo {
                        name: field.name.clone(),
                        ty: field.ty.clone(),
                        offset,
                    });
                    offset += size;
                }

                let methods: Vec<String> = class.methods.iter()
                    .map(|m| m.name.clone())
                    .collect();

                self.classes.insert(class.name.clone(), ClassInfo {
                    name: class.name.clone(),
                    parent: class.parent.clone(),
                    fields,
                    methods,
                    size: offset,
                });
            }
        }
        Ok(())
    }

    /// 声明函数
    fn declare_function(&mut self, func: &FuncDef) -> Result<(), String> {
        let mut sig = self.module.make_signature();

        for param in &func.params {
            let ty = self.bolide_type_to_cranelift(&param.ty);
            sig.params.push(AbiParam::new(ty));
        }

        if let Some(ref ret_ty) = func.return_type {
            sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        let func_id = self.module
            .declare_function(&func.name, Linkage::Export, &sig)
            .map_err(|e| format!("Declare function error: {}", e))?;

        self.functions.insert(func.name.clone(), func_id);
        self.func_return_types.insert(func.name.clone(), func.return_type.clone());
        self.func_params.insert(func.name.clone(), func.params.clone());

        if func.lifetime_deps.is_some() {
            self.lifetime_funcs.insert(func.name.clone());
        }
        Ok(())
    }

    /// 声明类构造函数
    fn declare_class_constructor(&mut self, class_name: &str) -> Result<(), String> {
        let class_info = self.classes.get(class_name)
            .ok_or_else(|| format!("Class {} not found", class_name))?
            .clone();

        let mut sig = self.module.make_signature();
        // 构造函数参数：每个字段一个参数
        for field in &class_info.fields {
            sig.params.push(AbiParam::new(self.bolide_type_to_cranelift(&field.ty)));
        }
        // 返回对象指针
        sig.returns.push(AbiParam::new(self.ptr_type));

        let func_id = self.module
            .declare_function(class_name, Linkage::Export, &sig)
            .map_err(|e| format!("Declare constructor error: {}", e))?;

        self.functions.insert(class_name.to_string(), func_id);
        self.func_return_types.insert(class_name.to_string(), Some(BolideType::Custom(class_name.to_string())));
        Ok(())
    }

    /// 声明类方法
    fn declare_class_methods(&mut self, program: &Program) -> Result<(), String> {
        for stmt in &program.statements {
            if let Statement::ClassDef(class) = stmt {
                for method in &class.methods {
                    let method_name = format!("{}_{}", class.name, method.name);
                    let mut sig = self.module.make_signature();
                    // self 参数
                    sig.params.push(AbiParam::new(self.ptr_type));
                    for param in &method.params {
                        sig.params.push(AbiParam::new(self.bolide_type_to_cranelift(&param.ty)));
                    }
                    if let Some(ref ret_ty) = method.return_type {
                        sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
                    }

                    let func_id = self.module
                        .declare_function(&method_name, Linkage::Export, &sig)
                        .map_err(|e| format!("Declare method error: {}", e))?;

                    self.functions.insert(method_name.clone(), func_id);
                    self.func_return_types.insert(method_name.clone(), method.return_type.clone());
                }
            }
        }
        Ok(())
    }

    /// 收集 spawn 目标函数
    fn collect_spawn_targets(&self, program: &Program) -> HashSet<String> {
        let mut targets = HashSet::new();
        self.collect_spawn_in_stmts(&program.statements, &mut targets);
        targets
    }

    fn collect_spawn_in_stmts(&self, stmts: &[Statement], targets: &mut HashSet<String>) {
        for stmt in stmts {
            self.collect_spawn_in_stmt(stmt, targets);
        }
    }

    fn collect_spawn_in_stmt(&self, stmt: &Statement, targets: &mut HashSet<String>) {
        match stmt {
            Statement::Expr(expr) => self.collect_spawn_in_expr(expr, targets),
            Statement::VarDecl(v) => {
                if let Some(ref val) = v.value {
                    self.collect_spawn_in_expr(val, targets);
                }
            }
            Statement::Assign(a) => self.collect_spawn_in_expr(&a.value, targets),
            Statement::If(i) => {
                self.collect_spawn_in_expr(&i.condition, targets);
                self.collect_spawn_in_stmts(&i.then_body, targets);
                if let Some(ref else_body) = i.else_body {
                    self.collect_spawn_in_stmts(else_body, targets);
                }
            }
            Statement::While(w) => {
                self.collect_spawn_in_expr(&w.condition, targets);
                self.collect_spawn_in_stmts(&w.body, targets);
            }
            Statement::For(f) => {
                self.collect_spawn_in_stmts(&f.body, targets);
            }
            Statement::FuncDef(f) => {
                self.collect_spawn_in_stmts(&f.body, targets);
            }
            Statement::Return(Some(e)) => self.collect_spawn_in_expr(e, targets),
            _ => {}
        }
    }

    fn collect_spawn_in_expr(&self, expr: &Expr, targets: &mut HashSet<String>) {
        match expr {
            Expr::Spawn(name, args) if !args.is_empty() => {
                targets.insert(name.clone());
            }
            Expr::BinOp(l, _, r) => {
                self.collect_spawn_in_expr(l, targets);
                self.collect_spawn_in_expr(r, targets);
            }
            Expr::Call(callee, args) => {
                self.collect_spawn_in_expr(callee, targets);
                for arg in args {
                    self.collect_spawn_in_expr(arg, targets);
                }
            }
            _ => {}
        }
    }

    /// 生成 trampolines
    fn generate_trampolines(&mut self, targets: &HashSet<String>) -> Result<(), String> {
        for func_name in targets {
            if let Some(params) = self.func_params.get(func_name).cloned() {
                if params.is_empty() {
                    continue;
                }
                self.create_trampoline(func_name, &params)?;
            }
        }
        Ok(())
    }

    /// 创建单个 trampoline 函数
    fn create_trampoline(&mut self, func_name: &str, params: &[Param]) -> Result<(), String> {
        let trampoline_name = format!("__trampoline_{}_{}", func_name, self.trampoline_counter);
        self.trampoline_counter += 1;

        let env_size = (params.len() * 8) as i64;
        let param_types: Vec<BolideType> = params.iter().map(|p| p.ty.clone()).collect();

        // 声明 trampoline 函数
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(self.ptr_type));
        if let Some(Some(ret_ty)) = self.func_return_types.get(func_name) {
            sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        let trampoline_id = self.module
            .declare_function(&trampoline_name, Linkage::Export, &sig)
            .map_err(|e| format!("{}", e))?;

        // 获取目标函数 ID
        let target_func_id = *self.functions.get(func_name)
            .ok_or_else(|| format!("Target function {} not declared", func_name))?;

        // 预计算参数类型
        let cranelift_types: Vec<types::Type> = params.iter()
            .map(|p| self.bolide_type_to_cranelift(&p.ty))
            .collect();

        // 构建函数体
        self.ctx.func.signature = sig;
        let mut fbc = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut fbc);

        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        let env_ptr = builder.block_params(entry)[0];
        let target_ref = self.module.declare_func_in_func(target_func_id, builder.func);

        // 从 env 加载参数
        let mut call_args = Vec::new();
        for (i, ty) in cranelift_types.iter().enumerate() {
            let offset = (i * 8) as i32;
            let val = builder.ins().load(*ty, MemFlags::trusted(), env_ptr, offset);
            call_args.push(val);
        }

        // 调用目标函数
        let call = builder.ins().call(target_ref, &call_args);
        let result_val = {
            let results = builder.inst_results(call);
            if results.is_empty() { None } else { Some(results[0]) }
        };

        if let Some(val) = result_val {
            builder.ins().return_(&[val]);
        } else {
            builder.ins().return_(&[]);
        }

        builder.finalize();

        self.module.define_function(trampoline_id, &mut self.ctx)
            .map_err(|e| format!("Define trampoline error: {}", e))?;
        self.module.clear_context(&mut self.ctx);

        self.trampolines.insert(func_name.to_string(), TrampolineInfo {
            func_id: trampoline_id,
            param_types,
            env_size,
        });
        self.functions.insert(trampoline_name, trampoline_id);

        Ok(())
    }

    /// 编译类构造函数
    fn compile_class_constructor(&mut self, class_name: &str) -> Result<(), String> {
        let class_info = self.classes.get(class_name)
            .ok_or_else(|| format!("Class {} not found", class_name))?
            .clone();

        let func_id = *self.functions.get(class_name)
            .ok_or_else(|| format!("Constructor {} not declared", class_name))?;

        let mut sig = self.module.make_signature();
        for field in &class_info.fields {
            sig.params.push(AbiParam::new(self.bolide_type_to_cranelift(&field.ty)));
        }
        sig.returns.push(AbiParam::new(self.ptr_type));

        self.ctx.func.signature = sig;
        let mut fbc = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut fbc);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        // 分配对象内存
        let alloc_id = *self.functions.get("object_alloc")
            .ok_or("object_alloc not found")?;
        let alloc_ref = self.module.declare_func_in_func(alloc_id, builder.func);
        let size = builder.ins().iconst(types::I64, class_info.size as i64);
        let call = builder.ins().call(alloc_ref, &[size]);
        let obj_ptr = builder.inst_results(call)[0];

        // 设置字段值
        for (i, field) in class_info.fields.iter().enumerate() {
            let param = builder.block_params(entry)[i];
            let offset = field.offset as i32;
            builder.ins().store(MemFlags::new(), param, obj_ptr, offset);
        }

        builder.ins().return_(&[obj_ptr]);
        builder.finalize();

        self.module.define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Define constructor error: {}", e))?;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }

    /// 编译类方法
    fn compile_class_methods(&mut self, program: &Program) -> Result<(), String> {
        for stmt in &program.statements {
            if let Statement::ClassDef(class) = stmt {
                for method in &class.methods {
                    self.compile_class_method(&class.name, method)?;
                }
            }
        }
        Ok(())
    }

    fn compile_class_method(&mut self, class_name: &str, method: &FuncDef) -> Result<(), String> {
        let method_name = format!("{}_{}", class_name, method.name);
        let func_id = *self.functions.get(&method_name)
            .ok_or_else(|| format!("Method {} not declared", method_name))?;

        // Collect string literals and create data objects
        let strings = self.collect_strings_from_stmts(&method.body);
        let mut string_data_ids: HashMap<String, DataId> = HashMap::new();
        for s in &strings {
            let data_id = self.get_or_create_string_data(s)?;
            string_data_ids.insert(s.clone(), data_id);
        }

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(self.ptr_type)); // self
        for param in &method.params {
            sig.params.push(AbiParam::new(self.bolide_type_to_cranelift(&param.ty)));
        }
        if let Some(ref ret_ty) = method.return_type {
            sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        self.ctx.func.signature = sig;
        let mut fbc = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut fbc);

        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        // 导入函数引用
        let mut func_refs = HashMap::new();
        for (name, &fid) in &self.functions {
            let fref = self.module.declare_func_in_func(fid, builder.func);
            func_refs.insert(name.clone(), fref);
        }

        // Declare string data in function and create GlobalValues
        let mut string_globals = HashMap::new();
        for (s, data_id) in &string_data_ids {
            let gv = self.module.declare_data_in_func(*data_id, builder.func);
            string_globals.insert(s.clone(), (gv, s.len()));
        }

        // 使用作用域来确保 ctx 在 finalize 之前被释放
        {
            let mut ctx = AotCompileContext::new(
                &mut builder,
                func_refs,
                self.ptr_type,
                self.classes.clone(),
                self.async_funcs.clone(),
                self.func_return_types.clone(),
                string_globals,
                self.modules.clone(),
            );

            // 设置 self 参数
            let params: Vec<_> = ctx.builder.block_params(entry).to_vec();
            let self_var = ctx.declare_variable("self", self.ptr_type);
            ctx.builder.def_var(self_var, params[0]);
            ctx.var_types.insert("self".to_string(), BolideType::Custom(class_name.to_string()));

            // 设置其他参数变量
            for (i, param) in method.params.iter().enumerate() {
                let ty = ctx.bolide_type_to_cranelift(&param.ty);
                let var = ctx.declare_variable(&param.name, ty);
                ctx.builder.def_var(var, params[i + 1]); // +1 因为 self 是第一个参数
                ctx.var_types.insert(param.name.clone(), param.ty.clone());
            }

            // 编译方法体
            let mut returned = false;
            for stmt in &method.body {
                if ctx.compile_stmt(stmt)? {
                    returned = true;
                    break;
                }
            }

            // 如果没有显式返回，添加默认返回
            if !returned {
                if method.return_type.is_some() {
                    let zero = ctx.builder.ins().iconst(types::I64, 0);
                    ctx.builder.ins().return_(&[zero]);
                } else {
                    ctx.builder.ins().return_(&[]);
                }
            }
        }

        builder.finalize();
        self.module.define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Define method error: {}", e))?;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }

    /// 编译函数
    fn compile_function(&mut self, func: &FuncDef) -> Result<(), String> {
        let func_id = *self.functions.get(&func.name)
            .ok_or_else(|| format!("Function {} not declared", func.name))?;

        // Collect string literals and create data objects
        let strings = self.collect_strings_from_stmts(&func.body);
        let mut string_data_ids: HashMap<String, DataId> = HashMap::new();
        for s in &strings {
            let data_id = self.get_or_create_string_data(s)?;
            string_data_ids.insert(s.clone(), data_id);
        }

        let mut sig = self.module.make_signature();
        for param in &func.params {
            sig.params.push(AbiParam::new(self.bolide_type_to_cranelift(&param.ty)));
        }
        if let Some(ref ret_ty) = func.return_type {
            sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        self.ctx.func.signature = sig;
        let mut fbc = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut fbc);

        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        // 导入函数引用
        let mut func_refs = HashMap::new();
        for (name, &fid) in &self.functions {
            let fref = self.module.declare_func_in_func(fid, builder.func);
            func_refs.insert(name.clone(), fref);
        }

        // Declare string data in function and create GlobalValues
        let mut string_globals = HashMap::new();
        for (s, data_id) in &string_data_ids {
            let gv = self.module.declare_data_in_func(*data_id, builder.func);
            string_globals.insert(s.clone(), (gv, s.len()));
        }

        // 使用作用域来确保 ctx 在 finalize 之前被释放
        {
            let mut ctx = AotCompileContext::new(
                &mut builder,
                func_refs,
                self.ptr_type,
                self.classes.clone(),
                self.async_funcs.clone(),
                self.func_return_types.clone(),
                string_globals,
                self.modules.clone(),
            );

            // 设置参数变量
            let params: Vec<_> = ctx.builder.block_params(entry).to_vec();
            for (i, param) in func.params.iter().enumerate() {
                let ty = ctx.bolide_type_to_cranelift(&param.ty);
                let var = ctx.declare_variable(&param.name, ty);
                ctx.builder.def_var(var, params[i]);
                ctx.var_types.insert(param.name.clone(), param.ty.clone());
            }

            // 编译函数体
            let mut returned = false;
            for stmt in &func.body {
                if ctx.compile_stmt(stmt)? {
                    returned = true;
                    break;
                }
            }

            // 如果没有显式返回，添加默认返回
            if !returned {
                if func.return_type.is_some() {
                    let zero = ctx.builder.ins().iconst(types::I64, 0);
                    ctx.builder.ins().return_(&[zero]);
                } else {
                    ctx.builder.ins().return_(&[]);
                }
            }
        } // ctx 在这里被释放

        builder.finalize();
        // println!("Compiling Aot function: {}", func.name);
        if let Err(e) = self.ctx.verify_if(&*self.module.isa()) {
            println!("Verify Error for {}: {:?}", func.name, e);
            println!("{}", self.ctx.func.display());
        }

        self.module.define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Define function error in {}: {}", func.name, e))?;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }
}

/// AOT 编译上下文
struct AotCompileContext<'a, 'b> {
    builder: &'a mut FunctionBuilder<'b>,
    func_refs: HashMap<String, FuncRef>,
    variables: HashMap<String, Variable>,
    var_types: HashMap<String, BolideType>,
    var_counter: usize,
    ptr_type: types::Type,
    classes: HashMap<String, ClassInfo>,
    async_funcs: HashSet<String>,
    func_return_types: HashMap<String, Option<BolideType>>,
    /// String data global values (string content -> GlobalValue)
    string_globals: HashMap<String, (cranelift_codegen::ir::GlobalValue, usize)>,
    /// 模块名映射
    modules: HashMap<String, String>,
    /// RC variables to be released at scope exit/return
    rc_variables: Vec<(Variable, BolideType)>,
    /// Temporary RC values from expressions (to be released at statement end)
    temp_rc_values: Vec<(Value, BolideType)>,
}

impl<'a, 'b> AotCompileContext<'a, 'b> {
    fn new(
        builder: &'a mut FunctionBuilder<'b>,
        func_refs: HashMap<String, FuncRef>,
        ptr_type: types::Type,
        classes: HashMap<String, ClassInfo>,
        async_funcs: HashSet<String>,
        func_return_types: HashMap<String, Option<BolideType>>,
        string_globals: HashMap<String, (cranelift_codegen::ir::GlobalValue, usize)>,
        modules: HashMap<String, String>,
    ) -> Self {
        Self {
            builder,
            func_refs,
            variables: HashMap::new(),
            var_types: HashMap::new(),
            var_counter: 0,
            ptr_type,
            classes,
            async_funcs,
            func_return_types,
            string_globals,
            modules,
            rc_variables: Vec::new(),
            temp_rc_values: Vec::new(),
        }
    }

    fn enter_scope(&self) -> usize {
        self.rc_variables.len()
    }

    fn leave_scope(&mut self, start_index: usize) {
        // Release vars declared in this scope (stack-like)
        for i in (start_index..self.rc_variables.len()).rev() {
             let (var, ty) = self.rc_variables[i].clone();
             let val = self.builder.use_var(var);
             self.emit_release(val, &ty);
        }
        // Truncate
        self.rc_variables.truncate(start_index);
    }

    fn declare_variable(&mut self, name: &str, ty: types::Type) -> Variable {
        let var = Variable::new(self.var_counter);
        self.var_counter += 1;
        self.builder.declare_var(var, ty);
        self.variables.insert(name.to_string(), var);
        var
    }

    /// Bolide 类型转换为 Cranelift 类型
    fn bolide_type_to_cranelift(&self, ty: &BolideType) -> types::Type {
        match ty {
            BolideType::Int => types::I64,
            BolideType::Float => types::F64,
            BolideType::Bool => types::I64,
            BolideType::Str => self.ptr_type,
            BolideType::BigInt => self.ptr_type,
            BolideType::Decimal => self.ptr_type,
            BolideType::Dynamic => self.ptr_type,
            BolideType::Ptr => self.ptr_type,
            BolideType::Channel(_) => self.ptr_type,
            BolideType::Future => self.ptr_type,
            BolideType::Func => self.ptr_type,
            BolideType::FuncSig(_, _) => self.ptr_type,
            BolideType::List(_) => self.ptr_type,
            BolideType::Dict(_, _) => self.ptr_type,
            BolideType::Tuple(_) => self.ptr_type,
            BolideType::Custom(_) => self.ptr_type,
            BolideType::Weak(_) => self.ptr_type,
            BolideType::Unowned(_) => self.ptr_type,
        }
    }

    /// 检查类型是否需要 RC 管理
    fn is_rc_type(ty: &BolideType) -> bool {
        match ty {
            BolideType::Weak(_) | BolideType::Unowned(_) => false,
            _ => matches!(ty,
                BolideType::Str |
                BolideType::BigInt |
                BolideType::Decimal |
                BolideType::List(_) |
                BolideType::Dict(_, _) |
                BolideType::Dynamic |
                BolideType::Custom(_) |
                BolideType::Tuple(_)
            )
        }
    }

    /// 获取类型对应的 release 函数名
    fn get_release_func_name(ty: &BolideType) -> Option<&'static str> {
        match ty {
            BolideType::Str => Some("string_release"),
            BolideType::BigInt => Some("bigint_release"),
            BolideType::Decimal => Some("decimal_release"),
            BolideType::List(_) => Some("list_release"),
            BolideType::Dict(_, _) => Some("dict_release"),
            BolideType::Dynamic => Some("dynamic_release"),
            BolideType::Custom(_) => Some("object_release"),
            BolideType::Tuple(_) => Some("tuple_free"),
            _ => None,
        }
    }

    /// 记录 RC 变量
    fn track_rc_variable(&mut self, name: &str, ty: &BolideType) {
        if Self::is_rc_type(ty) {
            if let Some(&var) = self.variables.get(name) {
                self.rc_variables.push((var, ty.clone()));
            }
        }
    }

    /// 为所有 RC 变量生成 release 调用
    fn emit_rc_cleanup(&mut self) {
        // Collect variables to release
        let vars_to_release = self.rc_variables.clone();

        for (var, ty) in vars_to_release {
            let val = self.builder.use_var(var);
            self.emit_release(val, &ty);
        }
    }

    /// 统一的 release 辅助函数
    fn emit_release(&mut self, val: Value, ty: &BolideType) {
        if let BolideType::Tuple(inner_types) = ty {
            // 元组需要先释放元素
            if let Some(&get_func) = self.func_refs.get("tuple_get") {
                for (i, elem_ty) in inner_types.iter().enumerate() {
                    if Self::is_rc_type(elem_ty) {
                        let idx_val = self.builder.ins().iconst(types::I64, i as i64);
                        let call = self.builder.ins().call(get_func, &[val, idx_val]);
                        // Check if result is available
                        let results = self.builder.inst_results(call);
                        if !results.is_empty() {
                            let elem_val = results[0];
                            self.emit_release(elem_val, elem_ty);
                        }
                    }
                }
            }
            // 最后释放元组本身
            if let Some(&free_func) = self.func_refs.get("tuple_free") {
                self.builder.ins().call(free_func, &[val]);
            }
        } else if let BolideType::Custom(ref class_name) = ty {
            self.emit_object_fields_cleanup(val, class_name);
            if let Some(&release_func) = self.func_refs.get("object_release") {
                self.builder.ins().call(release_func, &[val]);
            }
        } else {
            if let Some(func_name) = Self::get_release_func_name(ty) {
                if let Some(&func_ref) = self.func_refs.get(func_name) {
                    self.builder.ins().call(func_ref, &[val]);
                }
            }
        }
    }

    /// 释放对象内部的 RC 字段
    fn emit_object_fields_cleanup(&mut self, obj_ptr: Value, class_name: &str) {
        if let Some(class_info) = self.classes.get(class_name).cloned() {
            for field in &class_info.fields {
                if Self::is_rc_type(&field.ty) {
                    if let Some(func_name) = Self::get_release_func_name(&field.ty) {
                        if let Some(&func_ref) = self.func_refs.get(func_name) {
                            let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field.offset as i64);
                            let field_val = self.builder.ins().load(types::I64, MemFlags::new(), field_ptr, 0);
                            self.builder.ins().call(func_ref, &[field_val]);
                        }
                    }
                }
            }
        }
    }

    /// 编译表达式
    fn compile_expr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Int(n) => Ok(self.builder.ins().iconst(types::I64, *n)),
            Expr::Float(f) => Ok(self.builder.ins().f64const(*f)),
            Expr::Bool(b) => Ok(self.builder.ins().iconst(types::I64, if *b { 1 } else { 0 })),
            Expr::String(s) => self.compile_string_literal(s),
            Expr::BigInt(s) => self.compile_bigint_literal(s),
            Expr::Decimal(s) => self.compile_decimal_literal(s),
            Expr::Ident(name) => self.compile_ident(name),
            Expr::BinOp(left, op, right) => self.compile_binop(left, op, right),
            Expr::UnaryOp(op, operand) => self.compile_unary(op, operand),
            Expr::Call(callee, args) => self.compile_call(callee, args),
            Expr::None => Ok(self.builder.ins().iconst(types::I64, 0)),
            Expr::Index(base, index) => self.compile_index(base, index),
            Expr::Member(base, member) => self.compile_member(base, member),
            Expr::List(items) => self.compile_list(items),
            Expr::Tuple(items) => self.compile_tuple(items),
            Expr::Dict(entries) => self.compile_dict(entries),
            Expr::Spawn(name, args) => self.compile_spawn(name, args),
            Expr::Await(inner) => self.compile_await(inner),
            Expr::Recv(channel) => self.compile_recv_channel(channel),
            Expr::AwaitAll(exprs) => self.compile_await_all(exprs),
        }
    }

    /// 编译字符串字面量
    fn compile_string_literal(&mut self, s: &str) -> Result<Value, String> {
        let func_ref = *self.func_refs.get("string_literal")
            .ok_or("string_literal not found")?;

        // Get the GlobalValue for this string from string_globals
        let (gv, len) = *self.string_globals.get(s)
            .ok_or_else(|| format!("String data not found for: {}", s))?;

        // Get the address of the data at runtime
        let ptr_val = self.builder.ins().global_value(self.ptr_type, gv);
        let len_val = self.builder.ins().iconst(types::I64, len as i64);

        let call = self.builder.ins().call(func_ref, &[ptr_val, len_val]);
        let result = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(result, &BolideType::Str);
        Ok(result)
    }

    /// 编译 BigInt 字面量
    fn compile_bigint_literal(&mut self, s: &str) -> Result<Value, String> {
        let val;
        if let Ok(n) = s.parse::<i64>() {
            let func_ref = *self.func_refs.get("bigint_from_i64")
                .ok_or("bigint_from_i64 not found")?;
            let arg = self.builder.ins().iconst(types::I64, n);
            let call = self.builder.ins().call(func_ref, &[arg]);
            val = self.builder.inst_results(call)[0];
        } else {
            let func_ref = *self.func_refs.get("bigint_from_str")
                .ok_or("bigint_from_str not found")?;
            let bytes: Box<[u8]> = s.as_bytes().into();
            let ptr = Box::leak(bytes).as_ptr();
            let len = s.len();
            let ptr_val = self.builder.ins().iconst(self.ptr_type, ptr as i64);
            let len_val = self.builder.ins().iconst(types::I64, len as i64);
            let call = self.builder.ins().call(func_ref, &[ptr_val, len_val]);
            val = self.builder.inst_results(call)[0];
        }
        self.track_temp_rc_value(val, &BolideType::BigInt);
        Ok(val)
    }

    /// 编译 Decimal 字面量
    fn compile_decimal_literal(&mut self, s: &str) -> Result<Value, String> {
        let val;
        if let Ok(f) = s.parse::<f64>() {
            let func_ref = *self.func_refs.get("decimal_from_f64")
                .ok_or("decimal_from_f64 not found")?;
            let arg = self.builder.ins().f64const(f);
            let call = self.builder.ins().call(func_ref, &[arg]);
            val = self.builder.inst_results(call)[0];
        } else {
            // Fallback to parsing from string
            let func_ref = *self.func_refs.get("decimal_from_str")
                 .ok_or("decimal_from_str not found")?;
             let bytes: Box<[u8]> = s.as_bytes().into();
             let ptr = Box::leak(bytes).as_ptr();
             let len = s.len();
             let ptr_val = self.builder.ins().iconst(self.ptr_type, ptr as i64);
             let len_val = self.builder.ins().iconst(types::I64, len as i64);
             let call = self.builder.ins().call(func_ref, &[ptr_val, len_val]);
             val = self.builder.inst_results(call)[0];
        }
        self.track_temp_rc_value(val, &BolideType::Decimal);
        Ok(val)
    }


    /// 记录临时 RC 值（表达式中间结果）
    fn track_temp_rc_value(&mut self, val: Value, ty: &BolideType) {
        if Self::is_rc_type(ty) {
            self.temp_rc_values.push((val, ty.clone()));
        }
    }

    /// 移除临时 RC 值（所有权转移）
    fn remove_temp_rc_value(&mut self, val: Value) {
        if let Some(pos) = self.temp_rc_values.iter().position(|(v, _)| *v == val) {
            self.temp_rc_values.remove(pos);
        }
    }

    /// 释放所有临时 RC 值
    fn release_temp_rc_values(&mut self) {
        let temps = std::mem::take(&mut self.temp_rc_values);
        for (val, ty) in temps {
            self.emit_release(val, &ty);
        }
    }

    /// 获取类型对应的 clone 函数名
    fn get_clone_func_name(ty: &BolideType) -> Option<&'static str> {
        match ty {
            BolideType::Str => Some("string_clone"),
            BolideType::BigInt => Some("bigint_clone"),
            BolideType::Decimal => Some("decimal_clone"),
            BolideType::List(_) => Some("list_clone"),
            BolideType::Dict(_, _) => Some("dict_clone"),
            BolideType::Dynamic => Some("dynamic_clone"),
            BolideType::Custom(_) => Some("object_clone"),
             _ => None,
        }
    }

    /// 统一的 retain (clone) 辅助函数
    fn emit_retain(&mut self, val: Value, ty: &BolideType) -> Value {
        if let BolideType::Tuple(inner_types) = ty {
             // Tuple Deep Copy: create new tuple and clone elements
             if let Some(&new_func) = self.func_refs.get("tuple_new") {
                 let len = self.builder.ins().iconst(types::I64, inner_types.len() as i64);
                 let call = self.builder.ins().call(new_func, &[len]);
                 let new_tuple = self.builder.inst_results(call)[0];

                 if let Some(&get_func) = self.func_refs.get("tuple_get") {
                     if let Some(&set_func) = self.func_refs.get("tuple_set") {
                         for (i, elem_ty) in inner_types.iter().enumerate() {
                             let idx_val = self.builder.ins().iconst(types::I64, i as i64);
                             // Get from old tuple
                             let call_get = self.builder.ins().call(get_func, &[val, idx_val]);
                             let elem_val = self.builder.inst_results(call_get)[0];
                             
                             // Retain element
                             let new_elem_val = if Self::is_rc_type(elem_ty) {
                                 self.emit_retain(elem_val, elem_ty)
                             } else {
                                 elem_val
                             };

                             // Set to new tuple
                             self.builder.ins().call(set_func, &[new_tuple, idx_val, new_elem_val]);
                         }
                     }
                 }
                 return new_tuple;
             }
             // Fallback if functions missing (should not happen)
             return val; 
        } else {
            if let Some(func_name) = Self::get_clone_func_name(ty) {
                if let Some(&func_ref) = self.func_refs.get(func_name) {
                    let call = self.builder.ins().call(func_ref, &[val]);
                    return self.builder.inst_results(call)[0];
                }
            }
            // If no clone function (e.g. Bool, Int), just return value
            return val;
        }
    }

    /// 编译标识符
    fn compile_ident(&mut self, name: &str) -> Result<Value, String> {
        if let Some(&var) = self.variables.get(name) {
            let val = self.builder.use_var(var);
            // Retain if RC type
            if let Some(ty) = self.var_types.get(name).cloned() {
                if Self::is_rc_type(&ty) {
                     let new_val = self.emit_retain(val, &ty);
                     self.track_temp_rc_value(new_val, &ty);
                     return Ok(new_val);
                }
            }
            return Ok(val);
        }
        if let Some(&func_ref) = self.func_refs.get(name) {
            return Ok(self.builder.ins().func_addr(self.ptr_type, func_ref));
        }
        Err(format!("Undefined variable: {}", name))
    }

    /// 编译二元运算
    fn compile_binop(&mut self, left: &Expr, op: &BinOp, right: &Expr) -> Result<Value, String> {
        // 检查操作数类型以决定使用整数还是浮点运算
        let left_type = self.infer_expr_type(left);
        let right_type = self.infer_expr_type(right);
        let is_float = matches!(left_type, Some(BolideType::Float))
            || matches!(right_type, Some(BolideType::Float));
        let is_string = matches!(left_type, Some(BolideType::Str))
            || matches!(right_type, Some(BolideType::Str));
        let is_bigint = matches!(left_type, Some(BolideType::BigInt))
            || matches!(right_type, Some(BolideType::BigInt));
        let is_decimal = matches!(left_type, Some(BolideType::Decimal))
            || matches!(right_type, Some(BolideType::Decimal));

        // 字符串操作
        if is_string {
            return self.compile_string_binop(left, op, right);
        }

        // BigInt 操作
        if is_bigint {
            let lhs = self.compile_expr(left)?;
            let rhs = self.compile_expr(right)?;
            return self.compile_bigint_binop(lhs, op, rhs);
        }

        // Decimal 操作
        if is_decimal {
            let lhs = self.compile_expr(left)?;
            let rhs = self.compile_expr(right)?;
            return self.compile_decimal_binop(lhs, op, rhs);
        }

        let lhs = self.compile_expr(left)?;
        let rhs = self.compile_expr(right)?;

        if is_float {
            // 浮点运算
            match op {
                BinOp::Add => Ok(self.builder.ins().fadd(lhs, rhs)),
                BinOp::Sub => Ok(self.builder.ins().fsub(lhs, rhs)),
                BinOp::Mul => Ok(self.builder.ins().fmul(lhs, rhs)),
                BinOp::Div => Ok(self.builder.ins().fdiv(lhs, rhs)),
                BinOp::Mod => {
                    // 浮点取模：a % b = a - floor(a/b) * b
                    let div = self.builder.ins().fdiv(lhs, rhs);
                    let floored = self.builder.ins().floor(div);
                    let mul = self.builder.ins().fmul(floored, rhs);
                    Ok(self.builder.ins().fsub(lhs, mul))
                }
                BinOp::Eq => {
                    let cmp = self.builder.ins().fcmp(FloatCC::Equal, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Ne => {
                    let cmp = self.builder.ins().fcmp(FloatCC::NotEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Lt => {
                    let cmp = self.builder.ins().fcmp(FloatCC::LessThan, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Le => {
                    let cmp = self.builder.ins().fcmp(FloatCC::LessThanOrEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Gt => {
                    let cmp = self.builder.ins().fcmp(FloatCC::GreaterThan, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Ge => {
                    let cmp = self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::And | BinOp::Or => {
                    Err("Logical operations not supported for floats".to_string())
                }
            }
        } else {
            // 整数运算
            match op {
                BinOp::Add => Ok(self.builder.ins().iadd(lhs, rhs)),
                BinOp::Sub => Ok(self.builder.ins().isub(lhs, rhs)),
                BinOp::Mul => Ok(self.builder.ins().imul(lhs, rhs)),
                BinOp::Div => Ok(self.builder.ins().sdiv(lhs, rhs)),
                BinOp::Mod => Ok(self.builder.ins().srem(lhs, rhs)),
                BinOp::Eq => {
                    let cmp = self.builder.ins().icmp(IntCC::Equal, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Ne => {
                    let cmp = self.builder.ins().icmp(IntCC::NotEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Lt => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedLessThan, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Le => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Gt => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedGreaterThan, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Ge => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::And => Ok(self.builder.ins().band(lhs, rhs)),
                BinOp::Or => Ok(self.builder.ins().bor(lhs, rhs)),
            }
        }
    }

    /// 编译字符串二元运算
    fn compile_string_binop(&mut self, left: &Expr, op: &BinOp, right: &Expr) -> Result<Value, String> {
        let lhs = self.compile_expr(left)?;
        let rhs = self.compile_expr(right)?;

        match op {
            BinOp::Add => {
                // 字符串连接
                let func_ref = *self.func_refs.get("string_concat")
                    .ok_or("string_concat not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            BinOp::Eq => {
                // 字符串相等比较
                let func_ref = *self.func_refs.get("string_eq")
                    .ok_or("string_eq not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                Ok(self.builder.inst_results(call)[0])
            }
            BinOp::Ne => {
                // 字符串不等比较
                let func_ref = *self.func_refs.get("string_eq")
                    .ok_or("string_eq not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                let eq_result = self.builder.inst_results(call)[0];
                // 取反
                let zero = self.builder.ins().iconst(types::I64, 0);
                let cmp = self.builder.ins().icmp(IntCC::Equal, eq_result, zero);
                Ok(self.builder.ins().uextend(types::I64, cmp))
            }
            _ => Err(format!("Unsupported string operation: {:?}", op)),
        }
    }

    /// 编译 BigInt 二元运算
    fn compile_bigint_binop(&mut self, lhs: Value, op: &BinOp, rhs: Value) -> Result<Value, String> {
        let func_name = match op {
            BinOp::Add => "bigint_add",
            BinOp::Sub => "bigint_sub",
            BinOp::Mul => "bigint_mul",
            BinOp::Div => "bigint_div",
            BinOp::Mod => "bigint_rem",
            BinOp::Eq => "bigint_eq",
            BinOp::Ne => {
                // ne = !eq
                let eq_ref = *self.func_refs.get("bigint_eq")
                    .ok_or("bigint_eq not found")?;
                let call = self.builder.ins().call(eq_ref, &[lhs, rhs]);
                let eq_result = self.builder.inst_results(call)[0];
                let one = self.builder.ins().iconst(types::I64, 1);
                return Ok(self.builder.ins().isub(one, eq_result));
            }
            BinOp::Lt => "bigint_lt",
            BinOp::Le => "bigint_le",
            BinOp::Gt => "bigint_gt",
            BinOp::Ge => "bigint_ge",
            BinOp::And | BinOp::Or => {
                return Err("Logical operations not supported for BigInt".to_string());
            }
        };

        let func_ref = *self.func_refs.get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
        let result = self.builder.inst_results(call)[0];
        
        // Track arithmetic results as temps
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod) {
             self.track_temp_rc_value(result, &BolideType::BigInt);
        }
        
        Ok(result)
    }

    /// 编译 Decimal 二元运算
    fn compile_decimal_binop(&mut self, lhs: Value, op: &BinOp, rhs: Value) -> Result<Value, String> {
        let func_name = match op {
            BinOp::Add => "decimal_add",
            BinOp::Sub => "decimal_sub",
            BinOp::Mul => "decimal_mul",
            BinOp::Div => "decimal_div",
            BinOp::Mod => "decimal_rem",
            BinOp::Eq => "decimal_eq",
            BinOp::Ne => {
                // ne = !eq
                let eq_ref = *self.func_refs.get("decimal_eq")
                    .ok_or("decimal_eq not found")?;
                let call = self.builder.ins().call(eq_ref, &[lhs, rhs]);
                let eq_result = self.builder.inst_results(call)[0];
                let one = self.builder.ins().iconst(types::I64, 1);
                return Ok(self.builder.ins().isub(one, eq_result));
            }
            BinOp::Lt => "decimal_lt",
            BinOp::Le => "decimal_le",
            BinOp::Gt => "decimal_gt",
            BinOp::Ge => "decimal_ge",
            BinOp::And | BinOp::Or => {
                return Err("Logical operations not supported for Decimal".to_string());
            }
        };

        let func_ref = *self.func_refs.get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
        let result = self.builder.inst_results(call)[0];

        // Track arithmetic results as temps
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod) {
             self.track_temp_rc_value(result, &BolideType::Decimal);
        }

        Ok(result)
    }

    /// 编译一元运算
    fn compile_unary(&mut self, op: &UnaryOp, operand: &Expr) -> Result<Value, String> {
        let operand_type = self.infer_expr_type(operand);
        let val = self.compile_expr(operand)?;

        match op {
            UnaryOp::Neg => {
                match operand_type {
                    Some(BolideType::Float) => Ok(self.builder.ins().fneg(val)),
                    Some(BolideType::BigInt) => {
                        let func_ref = *self.func_refs.get("bigint_neg")
                            .ok_or("bigint_neg not found")?;
                        let call = self.builder.ins().call(func_ref, &[val]);
                        let result = self.builder.inst_results(call)[0];
                        self.track_temp_rc_value(result, &BolideType::BigInt);
                        Ok(result)
                    },
                    Some(BolideType::Decimal) => {
                        let func_ref = *self.func_refs.get("decimal_neg")
                            .ok_or("decimal_neg not found")?;
                        let call = self.builder.ins().call(func_ref, &[val]);
                        let result = self.builder.inst_results(call)[0];
                        self.track_temp_rc_value(result, &BolideType::Decimal);
                        Ok(result)
                    },
                    _ => Ok(self.builder.ins().ineg(val)),
                }
            }
            UnaryOp::Not => {
                let zero = self.builder.ins().iconst(types::I64, 0);
                let cmp = self.builder.ins().icmp(IntCC::Equal, val, zero);
                Ok(self.builder.ins().uextend(types::I64, cmp))
            }
        }
    }

    /// 编译函数调用
    fn compile_call(&mut self, callee: &Expr, args: &[Expr]) -> Result<Value, String> {
        match callee {
            Expr::Ident(name) => self.compile_named_call(name, args),
            Expr::Member(base, method_name) => {
                // 先检查是否是模块调用
                if let Expr::Ident(module_name) = base.as_ref() {
                    if self.modules.contains_key(module_name) {
                        // 模块调用: module.func() -> @module_func()
                        let func_name = format!("@{}_{}", module_name, method_name);
                        return self.compile_named_call(&func_name, args);
                    }
                }
                // 不是模块调用，是方法调用
                self.compile_method_call(base, method_name, args)
            }
            _ => Err("Unsupported callee type".to_string()),
        }
    }

    /// 编译方法调用
    fn compile_method_call(&mut self, base: &Expr, method_name: &str, args: &[Expr]) -> Result<Value, String> {
        let base_type = self.infer_expr_type(base);

        // 处理列表方法
        if let Some(BolideType::List(_)) = &base_type {
            return self.compile_list_method(base, method_name, args);
        }

        // 处理字符串方法
        if let Some(BolideType::Str) = &base_type {
            return self.compile_string_method(base, method_name, args);
        }

        // 处理类方法
        if let Some(BolideType::Custom(class_name)) = base_type {
            let base_val = self.compile_expr(base)?;
            let method_full_name = format!("{}_{}", class_name, method_name);

            if let Some(&func_ref) = self.func_refs.get(&method_full_name) {
                // Self is passed as first argument and ownership is transferred
                self.remove_temp_rc_value(base_val);
                
                let mut arg_vals = vec![base_val]; // self 作为第一个参数
                for arg in args {
                    let val = self.compile_expr(arg)?;
                    self.remove_temp_rc_value(val);
                    arg_vals.push(val);
                }
                let call = self.builder.ins().call(func_ref, &arg_vals);
                let results = self.builder.inst_results(call);
                if results.is_empty() {
                    return Ok(self.builder.ins().iconst(types::I64, 0));
                }
                let result = results[0];
                let ret_ty_opt = self.func_return_types.get(&method_full_name).cloned().flatten();
                if let Some(ret_ty) = ret_ty_opt {
                    if Self::is_rc_type(&ret_ty) {
                        self.track_temp_rc_value(result, &ret_ty);
                    }
                }
                return Ok(result);
            }
        }

        Err(format!("Unknown method: {}", method_name))
    }

    /// 编译列表方法
    fn compile_list_method(&mut self, base: &Expr, method_name: &str, args: &[Expr]) -> Result<Value, String> {
        let list_val = self.compile_expr(base)?;

        match method_name {
            "len" => {
                let func_ref = *self.func_refs.get("list_len").ok_or("list_len not found")?;
                let call = self.builder.ins().call(func_ref, &[list_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "push" => {
                let func_ref = *self.func_refs.get("list_push").ok_or("list_push not found")?;
                let val = self.compile_expr(&args[0])?;
                // Consume value ownership
                self.remove_temp_rc_value(val);
                self.builder.ins().call(func_ref, &[list_val, val]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "get" => {
                let func_ref = *self.func_refs.get("list_get").ok_or("list_get not found")?;
                let idx = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(func_ref, &[list_val, idx]);
                Ok(self.builder.inst_results(call)[0])
            }
            "set" => {
                let func_ref = *self.func_refs.get("list_set").ok_or("list_set not found")?;
                let idx = self.compile_expr(&args[0])?;
                let val = self.compile_expr(&args[1])?;
                // Consume value ownership
                self.remove_temp_rc_value(val);
                self.builder.ins().call(func_ref, &[list_val, idx, val]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            _ => Err(format!("Unknown list method: {}", method_name)),
        }
    }

    /// 编译字符串方法
    fn compile_string_method(&mut self, base: &Expr, method_name: &str, _args: &[Expr]) -> Result<Value, String> {
        let _str_val = self.compile_expr(base)?;

        match method_name {
            // 可以添加更多字符串方法
            _ => Err(format!("Unknown string method: {}", method_name)),
        }
    }

    /// 编译命名函数调用
    fn compile_named_call(&mut self, name: &str, args: &[Expr]) -> Result<Value, String> {
        // 处理 print 函数
        if name == "print" && args.len() == 1 {
            return self.compile_print(&args[0]);
        }

        // 处理类型转换和内置函数
        match name {
            "int" => return self.compile_to_int(args),
            "float" => return self.compile_to_float(args),
            "str" => return self.compile_to_str(args),
            "bigint" => return self.compile_to_bigint(args),
            "decimal" => return self.compile_to_decimal(args),
            "input" => return self.compile_input(args),
            "join" => return self.compile_join(args),
            "channel" => return self.compile_channel_create(args),
            _ => {}
        }

        // 检查是否是 async 函数调用
        if self.async_funcs.contains(name) {
            return self.compile_async_call(name, args);
        }

        // 查找函数引用
        let func_ref = *self.func_refs.get(name)
            .ok_or_else(|| format!("Function not found: {}", name))?;

        // 编译参数
        let mut arg_vals = Vec::new();
        for arg in args {
            let val = self.compile_expr(arg)?;
            // Consume temp RC value (pass ownership to callee)
            self.remove_temp_rc_value(val);
            arg_vals.push(val);
        }

        // 调用函数
        let call = self.builder.ins().call(func_ref, &arg_vals);
        let results = self.builder.inst_results(call);
        if results.is_empty() {
            Ok(self.builder.ins().iconst(types::I64, 0))
        } else {
            let result = results[0];
            let ret_ty_opt = self.func_return_types.get(name).cloned().flatten();
            if let Some(ret_ty) = ret_ty_opt {
                if Self::is_rc_type(&ret_ty) {
                    self.track_temp_rc_value(result, &ret_ty);
                }
            }
            Ok(result)
        }
    }

    /// 编译 async 函数调用 - 启动协程并返回 Future
    fn compile_async_call(&mut self, func_name: &str, args: &[Expr]) -> Result<Value, String> {
        // 获取函数地址
        let target_func_ref = *self.func_refs.get(func_name)
            .ok_or_else(|| format!("Undefined async function: {}", func_name))?;
        let func_addr = self.builder.ins().func_addr(self.ptr_type, target_func_ref);

        // 调用 coroutine_spawn_int 启动协程
        let spawn_ref = *self.func_refs.get("coroutine_spawn_int")
            .ok_or("coroutine_spawn_int not found")?;
        let call = self.builder.ins().call(spawn_ref, &[func_addr]);
        Ok(self.builder.inst_results(call)[0])
    }

    /// 编译 print 函数
    fn compile_print(&mut self, arg: &Expr) -> Result<Value, String> {
        let val = self.compile_expr(arg)?;

        // 使用类型推断来选择正确的打印函数
        let inferred_type = self.infer_expr_type(arg);
        let func_name = self.get_print_func_name(&inferred_type);

        let func_ref = *self.func_refs.get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        self.builder.ins().call(func_ref, &[val]);
        Ok(self.builder.ins().iconst(types::I64, 0))
    }

    /// 根据类型获取打印函数名
    fn get_print_func_name(&self, ty: &Option<BolideType>) -> &'static str {
        match ty {
            Some(BolideType::Int) => "print_int",
            Some(BolideType::Float) => "print_float",
            Some(BolideType::Bool) => "print_bool",
            Some(BolideType::Str) => "print_string",
            Some(BolideType::BigInt) => "print_bigint",
            Some(BolideType::Decimal) => "print_decimal",
            Some(BolideType::Dynamic) => "print_dynamic",
            Some(BolideType::List(_)) => "print_list",
            Some(BolideType::Dict(_, _)) => "print_dict",
            Some(BolideType::Tuple(_)) => "print_tuple",
            _ => "print_int",
        }
    }

    /// 编译 int() 类型转换
    fn compile_to_int(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("int() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        match arg_type {
            Some(BolideType::Int) => Ok(val),
            Some(BolideType::Float) => {
                Ok(self.builder.ins().fcvt_to_sint(types::I64, val))
            }
            Some(BolideType::Str) => {
                let func_ref = *self.func_refs.get("string_to_int")
                    .ok_or("string_to_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::BigInt) => {
                let func_ref = *self.func_refs.get("bigint_to_i64")
                    .ok_or("bigint_to_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Decimal) => {
                let func_ref = *self.func_refs.get("decimal_to_i64")
                    .ok_or("decimal_to_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Ok(val),
        }
    }

    /// 编译 float() 类型转换
    fn compile_to_float(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("float() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        match arg_type {
            Some(BolideType::Float) => Ok(val),
            Some(BolideType::Int) => {
                Ok(self.builder.ins().fcvt_from_sint(types::F64, val))
            }
            Some(BolideType::Str) => {
                let func_ref = *self.func_refs.get("string_to_float")
                    .ok_or("string_to_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Decimal) => {
                let func_ref = *self.func_refs.get("decimal_to_f64")
                    .ok_or("decimal_to_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Ok(self.builder.ins().fcvt_from_sint(types::F64, val)),
        }
    }

    /// 编译 str() 类型转换
    fn compile_to_str(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("str() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        let val = match arg_type {
            Some(BolideType::Str) => Ok::<Value, String>(val),
            Some(BolideType::Int) => {
                let func_ref = *self.func_refs.get("string_from_int")
                    .ok_or("string_from_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Float) => {
                let func_ref = *self.func_refs.get("string_from_float")
                    .ok_or("string_from_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Bool) => {
                let func_ref = *self.func_refs.get("string_from_bool")
                    .ok_or("string_from_bool not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::BigInt) => {
                let func_ref = *self.func_refs.get("string_from_bigint")
                    .ok_or("string_from_bigint not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Decimal) => {
                let func_ref = *self.func_refs.get("string_from_decimal")
                    .ok_or("string_from_decimal not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => {
                let func_ref = *self.func_refs.get("string_from_int")
                    .ok_or("string_from_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
        }?;
        
        // Track the new string if it's not the original string (which is borrowed/moved but not created new here, wait)
        // If arg was Str, we returned val. val is borrowed/owned. 
        // str("abc") -> "abc" (no new string).
        // str(1) -> new string.
        
        if !matches!(arg_type, Some(BolideType::Str)) {
            self.track_temp_rc_value(val, &BolideType::Str);
        }
        Ok(val)
    }

    /// 编译 bigint() 类型转换
    fn compile_to_bigint(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("bigint() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        let val = match arg_type {
            Some(BolideType::BigInt) => Ok::<Value, String>(val),
            Some(BolideType::Int) => {
                let func_ref = *self.func_refs.get("bigint_from_i64")
                    .ok_or("bigint_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Str) => {
                let func_ref = *self.func_refs.get("bigint_from_str")
                    .ok_or("bigint_from_str not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => {
                let func_ref = *self.func_refs.get("bigint_from_i64")
                    .ok_or("bigint_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
        }?;
        
        if !matches!(arg_type, Some(BolideType::BigInt)) {
            self.track_temp_rc_value(val, &BolideType::BigInt);
        }
        Ok(val)
    }

    /// 编译 decimal() 类型转换
    fn compile_to_decimal(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("decimal() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        let val = match arg_type {
            Some(BolideType::Decimal) => Ok::<Value, String>(val),
            Some(BolideType::Int) => {
                let func_ref = *self.func_refs.get("decimal_from_i64")
                    .ok_or("decimal_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Float) => {
                let func_ref = *self.func_refs.get("decimal_from_f64")
                    .ok_or("decimal_from_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Str) => {
                let func_ref = *self.func_refs.get("decimal_from_str")
                    .ok_or("decimal_from_str not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => {
                let func_ref = *self.func_refs.get("decimal_from_f64")
                    .ok_or("decimal_from_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
        }?;
        
        if !matches!(arg_type, Some(BolideType::Decimal)) {
            self.track_temp_rc_value(val, &BolideType::Decimal);
        }
        Ok(val)
    }

    /// 编译 input() 函数
    fn compile_input(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.is_empty() {
            let func_ref = *self.func_refs.get("input")
                .ok_or("input not found")?;
            let call = self.builder.ins().call(func_ref, &[]);
            let result = self.builder.inst_results(call)[0];
            self.track_temp_rc_value(result, &BolideType::Str);
            Ok(result)
        } else if args.len() == 1 {
            let prompt = self.compile_expr(&args[0])?;
            let func_ref = *self.func_refs.get("input_prompt")
                .ok_or("input_prompt not found")?;
            let call = self.builder.ins().call(func_ref, &[prompt]);
            let result = self.builder.inst_results(call)[0];
            self.track_temp_rc_value(result, &BolideType::Str);
            Ok(result)
        } else {
            Err("input() expects 0 or 1 argument".to_string())
        }
    }

    /// 编译 join() 函数
    fn compile_join(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("join() expects 1 argument".to_string());
        }
        let handle = self.compile_expr(&args[0])?;
        let func_ref = *self.func_refs.get("thread_join_int")
            .ok_or("thread_join_int not found")?;
        let call = self.builder.ins().call(func_ref, &[handle]);
        Ok(self.builder.inst_results(call)[0])
    }

    /// 编译 channel() 函数
    fn compile_channel_create(&mut self, args: &[Expr]) -> Result<Value, String> {
        let func_ref = *self.func_refs.get("channel_create")
            .ok_or("channel_create not found")?;
        if args.is_empty() {
            let call = self.builder.ins().call(func_ref, &[]);
            Ok(self.builder.inst_results(call)[0])
        } else if args.len() == 1 {
            let size = self.compile_expr(&args[0])?;
            let buffered_ref = *self.func_refs.get("channel_create_buffered")
                .ok_or("channel_create_buffered not found")?;
            let call = self.builder.ins().call(buffered_ref, &[size]);
            Ok(self.builder.inst_results(call)[0])
        } else {
            Err("channel() expects 0 or 1 argument".to_string())
        }
    }

    /// 编译索引访问
    fn compile_index(&mut self, base: &Expr, index: &Expr) -> Result<Value, String> {
        let base_type = self.infer_expr_type(base);
        let base_val = self.compile_expr(base)?;
        let index_val = self.compile_expr(index)?;

        // 根据类型选择不同的索引函数
        // 根据类型选择不同的索引函数
        match base_type {
            Some(BolideType::List(elem_ty)) => {
                let func_ref = *self.func_refs.get("list_get")
                    .ok_or("list_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                let val = self.builder.inst_results(call)[0];
                if Self::is_rc_type(&elem_ty) {
                    let retained = self.emit_retain(val, &elem_ty);
                    self.track_temp_rc_value(retained, &elem_ty);
                    Ok(retained)
                } else {
                    Ok(val)
                }
            }
            Some(BolideType::Dict(_, val_ty)) => {
                let func_ref = *self.func_refs.get("dict_get")
                    .ok_or("dict_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                let val = self.builder.inst_results(call)[0];
                if Self::is_rc_type(&val_ty) {
                    let retained = self.emit_retain(val, &val_ty);
                    self.track_temp_rc_value(retained, &val_ty);
                    Ok(retained)
                } else {
                    Ok(val)
                }
            }
            Some(BolideType::Tuple(inner_types)) => {
                let func_ref = *self.func_refs.get("tuple_get")
                    .ok_or("tuple_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                let val = self.builder.inst_results(call)[0];
                
                // Try to determine element type from constant index
                if let Expr::Int(i) = index {
                    if let Some(elem_ty) = inner_types.get(*i as usize) {
                        if Self::is_rc_type(elem_ty) {
                            let retained = self.emit_retain(val, elem_ty);
                            self.track_temp_rc_value(retained, elem_ty);
                            return Ok(retained);
                        }
                    }
                }
                // If we can't determine specific type (e.g. dynamic index on heterog. tuple),
                // we assume it might be convertible or just return as is (unsafe/incomplete).
                // Ideally tuple access should be type-safe.
                // For now, if we don't know type, we can't retain properly because we need type for retain/release.
                // But generally tuple indices ARE constant.
                Ok(val)
            }
            _ => {
                // If type unknown, assume tuple or dynamic
                let func_ref = *self.func_refs.get("tuple_get")
                    .ok_or("tuple_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                let val = self.builder.inst_results(call)[0];
                
                // Without type info, we can't safely retain.
                // This might be a limitation for untyped/dynamic code.
                Ok(val)
            }
        }
    }

    /// 编译成员访问
    fn compile_member(&mut self, base: &Expr, member: &str) -> Result<Value, String> {
        let base_val = self.compile_expr(base)?;

        // 尝试获取基础表达式的类型
        let base_type = self.infer_expr_type(base);

        if let Some(BolideType::Custom(class_name)) = base_type {
            // 类成员访问
            if let Some(class_info) = self.classes.get(&class_name).cloned() {
                for field in &class_info.fields {
                    if field.name == member {
                        let offset = field.offset as i32;
                        let field_ty = self.bolide_type_to_cranelift(&field.ty);
                        let val = self.builder.ins().load(
                            field_ty,
                            MemFlags::new(),
                            base_val,
                            offset,
                        );
                        if Self::is_rc_type(&field.ty) {
                             let retained = self.emit_retain(val, &field.ty);
                             self.track_temp_rc_value(retained, &field.ty);
                             return Ok(retained);
                        }
                        return Ok(val);
                    }
                }
                return Err(format!("Field '{}' not found in class '{}'", member, class_name));
            }
        }

        // 默认返回 0（用于未知类型）
        Ok(self.builder.ins().iconst(types::I64, 0))
    }

    /// 推断表达式类型
    fn infer_expr_type(&self, expr: &Expr) -> Option<BolideType> {
        match expr {
            Expr::Ident(name) => self.var_types.get(name).cloned(),
            Expr::Int(_) => Some(BolideType::Int),
            Expr::Float(_) => Some(BolideType::Float),
            Expr::Bool(_) => Some(BolideType::Bool),
            Expr::String(_) => Some(BolideType::Str),
            Expr::BigInt(_) => Some(BolideType::BigInt),
            Expr::Decimal(_) => Some(BolideType::Decimal),
            Expr::List(items) => {
                if let Some(first) = items.first() {
                    let elem_ty = self.infer_expr_type(first).unwrap_or(BolideType::Dynamic);
                    Some(BolideType::List(Box::new(elem_ty)))
                } else {
                    Some(BolideType::List(Box::new(BolideType::Dynamic)))
                }
            }
            Expr::Dict(_) => Some(BolideType::Dict(Box::new(BolideType::Dynamic), Box::new(BolideType::Dynamic))),
            Expr::Tuple(exprs) => {
                let elem_types: Vec<BolideType> = exprs.iter()
                    .map(|e| self.infer_expr_type(e).unwrap_or(BolideType::Dynamic))
                    .collect();
                Some(BolideType::Tuple(elem_types))
            }
            Expr::Index(base, idx) => {
                let base_ty = self.infer_expr_type(base)?;
                match base_ty {
                    BolideType::Tuple(elem_types) => {
                        if let Expr::Int(i) = idx.as_ref() {
                            let index = *i as usize;
                            elem_types.get(index).cloned()
                        } else {
                            elem_types.first().cloned()
                        }
                    }
                    BolideType::List(elem_ty) => Some(*elem_ty),
                    BolideType::Dict(_, val_ty) => Some(*val_ty),
                    _ => Some(BolideType::Dynamic),
                }
            }
            Expr::Call(callee, _args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    match name.as_str() {
                        "bigint" => Some(BolideType::BigInt),
                        "decimal" => Some(BolideType::Decimal),
                        "int" => Some(BolideType::Int),
                        "float" => Some(BolideType::Float),
                        "str" => Some(BolideType::Str),
                        "input" => Some(BolideType::Str),
                        _ => {
                            // Check user-defined function return types
                            self.func_return_types.get(name.as_str()).cloned().flatten()
                        }
                    }
                } else {
                    None
                }
            }
            Expr::BinOp(left, op, right) => {
                let left_ty = self.infer_expr_type(left);
                let right_ty = self.infer_expr_type(right);
                match (&left_ty, &right_ty) {
                    (Some(BolideType::Str), Some(BolideType::Str)) => {
                        match op {
                            BinOp::Add => Some(BolideType::Str),
                            BinOp::Eq | BinOp::Ne => Some(BolideType::Bool),
                            _ => Some(BolideType::Int),
                        }
                    }
                    (Some(BolideType::BigInt), _) | (_, Some(BolideType::BigInt)) => {
                        match op {
                            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Some(BolideType::Bool),
                            _ => Some(BolideType::BigInt),
                        }
                    }
                    (Some(BolideType::Decimal), _) | (_, Some(BolideType::Decimal)) => {
                        match op {
                            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Some(BolideType::Bool),
                            _ => Some(BolideType::Decimal),
                        }
                    }
                    (Some(BolideType::Float), _) | (_, Some(BolideType::Float)) => Some(BolideType::Float),
                    _ => match op {
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                        | BinOp::And | BinOp::Or => Some(BolideType::Bool),
                        _ => Some(BolideType::Int),
                    }
                }
            }
            Expr::None => None,
            _ => None,
        }
    }

    /// 编译列表字面量
    fn compile_list(&mut self, items: &[Expr]) -> Result<Value, String> {
        let func_ref = *self.func_refs.get("list_new")
            .ok_or("list_new not found")?;
        let elem_type = self.builder.ins().iconst(types::I8, 0);
        let call = self.builder.ins().call(func_ref, &[elem_type]);
        let list_ptr = self.builder.inst_results(call)[0];

        let push_ref = *self.func_refs.get("list_push")
            .ok_or("list_push not found")?;
        for item in items {
            let val = self.compile_expr(item)?;
            self.remove_temp_rc_value(val); // Consume value
            self.builder.ins().call(push_ref, &[list_ptr, val]);
        }

        Ok(list_ptr)
    }

    /// 编译 Tuple 字面量
    fn compile_tuple(&mut self, items: &[Expr]) -> Result<Value, String> {
        let func_ref = *self.func_refs.get("tuple_new")
            .ok_or("tuple_new not found")?;
        let len = self.builder.ins().iconst(types::I64, items.len() as i64);
        let call = self.builder.ins().call(func_ref, &[len]);
        let tuple_ptr = self.builder.inst_results(call)[0];

        let set_ref = *self.func_refs.get("tuple_set")
            .ok_or("tuple_set not found")?;
        for (i, item) in items.iter().enumerate() {
            let val = self.compile_expr(item)?;
            self.remove_temp_rc_value(val); // Consume value
            let idx = self.builder.ins().iconst(types::I64, i as i64);
            self.builder.ins().call(set_ref, &[tuple_ptr, idx, val]);
        }
        Ok(tuple_ptr)
    }

    /// 编译 Dict 字面量
    fn compile_dict(&mut self, entries: &[(Expr, Expr)]) -> Result<Value, String> {
        let func_ref = *self.func_refs.get("dict_new")
            .ok_or("dict_new not found")?;
        let key_type = self.builder.ins().iconst(types::I8, 0);
        let val_type = self.builder.ins().iconst(types::I8, 0);
        let call = self.builder.ins().call(func_ref, &[key_type, val_type]);
        let dict_ptr = self.builder.inst_results(call)[0];

        let set_ref = *self.func_refs.get("dict_set")
            .ok_or("dict_set not found")?;
        for (key, value) in entries {
            let k = self.compile_expr(key)?;
            let v = self.compile_expr(value)?;
            self.remove_temp_rc_value(k); // Consume key
            self.remove_temp_rc_value(v); // Consume value
            self.builder.ins().call(set_ref, &[dict_ptr, k, v]);
        }

        Ok(dict_ptr)
    }

    /// 编译 Spawn 表达式
    fn compile_spawn(&mut self, name: &str, args: &[Expr]) -> Result<Value, String> {
        if args.is_empty() {
            // 无参数：直接 spawn
            let func_ref = *self.func_refs.get("coroutine_spawn_int")
                .ok_or("coroutine_spawn_int not found")?;
            if let Some(&target_ref) = self.func_refs.get(name) {
                let fn_ptr = self.builder.ins().func_addr(self.ptr_type, target_ref);
                let null_env = self.builder.ins().iconst(self.ptr_type, 0);
                let call = self.builder.ins().call(func_ref, &[fn_ptr, null_env]);
                return Ok(self.builder.inst_results(call)[0]);
            }
        } else {
            // 有参数：使用 trampoline
            return self.compile_spawn_with_args(name, args);
        }
        Ok(self.builder.ins().iconst(types::I64, 0))
    }

    /// 编译带参数的 Spawn
    fn compile_spawn_with_args(&mut self, name: &str, args: &[Expr]) -> Result<Value, String> {
        // 分配 env 内存
        let env_size = (args.len() * 8) as i64;
        let alloc_ref = *self.func_refs.get("bolide_alloc")
            .ok_or("bolide_alloc not found")?;
        let size_val = self.builder.ins().iconst(types::I64, env_size);
        let call = self.builder.ins().call(alloc_ref, &[size_val]);
        let env_ptr = self.builder.inst_results(call)[0];

        // 将参数存入 env
        for (i, arg) in args.iter().enumerate() {
            let val = self.compile_expr(arg)?;
            let offset = (i * 8) as i32;
            self.builder.ins().store(MemFlags::new(), val, env_ptr, offset);
        }

        // 获取 trampoline 函数地址
        let trampoline_name = self.get_trampoline_name(name);
        let trampoline_ref = *self.func_refs.get(&trampoline_name)
            .ok_or_else(|| format!("Trampoline not found: {}", trampoline_name))?;
        let fn_ptr = self.builder.ins().func_addr(self.ptr_type, trampoline_ref);

        // 调用 spawn
        let spawn_ref = *self.func_refs.get("coroutine_spawn_int")
            .ok_or("coroutine_spawn_int not found")?;
        let call = self.builder.ins().call(spawn_ref, &[fn_ptr, env_ptr]);
        Ok(self.builder.inst_results(call)[0])
    }

    /// 获取 trampoline 函数名
    fn get_trampoline_name(&self, func_name: &str) -> String {
        for name in self.func_refs.keys() {
            if name.starts_with(&format!("__trampoline_{}_", func_name)) {
                return name.clone();
            }
        }
        format!("__trampoline_{}_0", func_name)
    }

    /// 编译 Await 表达式
    fn compile_await(&mut self, inner: &Expr) -> Result<Value, String> {
        let future = self.compile_expr(inner)?;
        let func_ref = *self.func_refs.get("coroutine_await_int")
            .ok_or("coroutine_await_int not found")?;
        let call = self.builder.ins().call(func_ref, &[future]);
        Ok(self.builder.inst_results(call)[0])
    }

    /// 编译 Recv 表达式 (从通道接收)
    fn compile_recv_channel(&mut self, channel_name: &str) -> Result<Value, String> {
        // 获取通道变量
        let ch = if let Some(&var) = self.variables.get(channel_name) {
            self.builder.use_var(var)
        } else {
            return Err(format!("Channel not found: {}", channel_name));
        };
        let func_ref = *self.func_refs.get("channel_recv")
            .ok_or("channel_recv not found")?;
        let call = self.builder.ins().call(func_ref, &[ch]);
        Ok(self.builder.inst_results(call)[0])
    }

    /// 编译 AwaitAll 表达式
    fn compile_await_all(&mut self, exprs: &[Expr]) -> Result<Value, String> {
        // 先启动所有协程，收集 Future 指针
        let mut futures = Vec::new();
        for expr in exprs {
            let future_ptr = self.compile_expr(expr)?;
            futures.push(future_ptr);
        }

        // 依次等待所有 Future
        let mut results = Vec::new();
        for (i, future_ptr) in futures.iter().enumerate() {
            let expr_type = self.infer_expr_type(&exprs[i]);
            let await_func_name = match &expr_type {
                Some(BolideType::Float) => "coroutine_await_float",
                Some(BolideType::Str) | Some(BolideType::BigInt) | Some(BolideType::Decimal)
                | Some(BolideType::List(_)) | Some(BolideType::Custom(_)) => "coroutine_await_ptr",
                _ => "coroutine_await_int",
            };

            let await_ref = *self.func_refs.get(await_func_name)
                .ok_or_else(|| format!("{} not found", await_func_name))?;

            let call = self.builder.ins().call(await_ref, &[*future_ptr]);
            let result = self.builder.inst_results(call)[0];
            results.push(result);
        }

        // 将结果存储到元组中
        if results.is_empty() {
            Ok(self.builder.ins().iconst(types::I64, 0))
        } else if results.len() == 1 {
            Ok(results[0])
        } else {
            // 使用运行时元组存储所有结果
            let tuple_new = *self.func_refs.get("tuple_new")
                .ok_or("tuple_new not found")?;
            let len = self.builder.ins().iconst(types::I64, results.len() as i64);
            let call = self.builder.ins().call(tuple_new, &[len]);
            let tuple_ptr = self.builder.inst_results(call)[0];

            let tuple_set = *self.func_refs.get("tuple_set")
                .ok_or("tuple_set not found")?;
            for (i, result) in results.iter().enumerate() {
                let idx = self.builder.ins().iconst(types::I64, i as i64);
                self.builder.ins().call(tuple_set, &[tuple_ptr, idx, *result]);
            }

            Ok(tuple_ptr)
        }
    }

    /// 编译语句
    fn compile_stmt(&mut self, stmt: &Statement) -> Result<bool, String> {
        let is_terminator = match stmt {
            Statement::VarDecl(decl) => {
                self.compile_var_decl(decl)?;
                false
            }
            Statement::Assign(assign) => {
                self.compile_assign(assign)?;
                false
            }
            Statement::Return(expr) => {
                self.compile_return(expr.as_ref())?;
                true
            }
            Statement::Expr(e) => {
                self.compile_expr(e)?;
                false
            }
            Statement::If(if_stmt) => self.compile_if(if_stmt)?,
            Statement::While(while_stmt) => {
                self.compile_while(while_stmt)?;
                false
            }
            Statement::For(for_stmt) => {
                self.compile_for(for_stmt)?;
                false
            }
            Statement::Send(send_stmt) => {
                self.compile_send(send_stmt)?;
                false
            }
            Statement::Import(_) | Statement::ExternBlock(_) | Statement::FuncDef(_) | Statement::ClassDef(_) => {
                // 这些语句在顶层处理，函数体内忽略
                false
            }
            Statement::Pool(pool_stmt) => {
                self.compile_pool(pool_stmt)?;
                false
            }
            Statement::Select(select_stmt) => {
                self.compile_select(select_stmt)?;
                false
            }
            Statement::AwaitScope(scope_stmt) => {
                self.compile_await_scope(scope_stmt)?;
                false
            }
            Statement::AsyncSelect(async_select) => {
                self.compile_async_select(async_select)?;
                false
            }
        };

        if !is_terminator {
            // Release temporary values created by this statement if it didn't terminate
            self.release_temp_rc_values();
        }
        
        Ok(is_terminator)
    }

    /// 编译 Send 语句
    fn compile_send(&mut self, send_stmt: &bolide_parser::SendStmt) -> Result<(), String> {
        let ch = if let Some(&var) = self.variables.get(&send_stmt.channel) {
            self.builder.use_var(var)
        } else {
            return Err(format!("Channel not found: {}", send_stmt.channel));
        };
        let val = self.compile_expr(&send_stmt.value)?;
        let func_ref = *self.func_refs.get("channel_send")
            .ok_or("channel_send not found")?;
        self.builder.ins().call(func_ref, &[ch, val]);
        Ok(())
    }

    /// 编译 Pool 语句
    fn compile_pool(&mut self, pool_stmt: &bolide_parser::PoolStmt) -> Result<(), String> {
        let size = self.compile_expr(&pool_stmt.size)?;

        // 创建线程池
        let pool_create_ref = *self.func_refs.get("pool_create")
            .ok_or("pool_create not found")?;
        let call = self.builder.ins().call(pool_create_ref, &[size]);
        let pool_ptr = self.builder.inst_results(call)[0];

        // 进入线程池上下文
        let pool_enter_ref = *self.func_refs.get("pool_enter")
            .ok_or("pool_enter not found")?;
        self.builder.ins().call(pool_enter_ref, &[pool_ptr]);

        // 编译 pool 块内的语句
        for stmt in &pool_stmt.body {
            self.compile_stmt(stmt)?;
        }

        // 退出线程池上下文
        let pool_exit_ref = *self.func_refs.get("pool_exit")
            .ok_or("pool_exit not found")?;
        self.builder.ins().call(pool_exit_ref, &[]);

        // 销毁线程池
        let pool_destroy_ref = *self.func_refs.get("pool_destroy")
            .ok_or("pool_destroy not found")?;
        self.builder.ins().call(pool_destroy_ref, &[pool_ptr]);

        Ok(())
    }

    /// 编译 Select 语句
    fn compile_select(&mut self, select_stmt: &bolide_parser::SelectStmt) -> Result<(), String> {
        use bolide_parser::SelectBranch;

        let mut recv_branches: Vec<(&str, &str, &Vec<Statement>)> = Vec::new();
        let mut timeout_branch: Option<(&Expr, &Vec<Statement>)> = None;
        let mut default_branch: Option<&Vec<Statement>> = None;

        for branch in &select_stmt.branches {
            match branch {
                SelectBranch::Recv { var, channel, body } => {
                    recv_branches.push((var.as_str(), channel.as_str(), body));
                }
                SelectBranch::Timeout { duration, body } => {
                    timeout_branch = Some((duration, body));
                }
                SelectBranch::Default { body } => {
                    default_branch = Some(body);
                }
            }
        }

        if recv_branches.is_empty() {
            if let Some(body) = default_branch {
                for stmt in body {
                    self.compile_stmt(stmt)?;
                }
            }
            return Ok(());
        }

        // 使用 channel_select 实现多分支
        let channel_count = recv_branches.len();

        // 分配 channel 数组
        let array_size = (channel_count * 8) as i64;
        let alloc_ref = *self.func_refs.get("bolide_alloc")
            .ok_or("bolide_alloc not found")?;
        let size_val = self.builder.ins().iconst(types::I64, array_size);
        let call = self.builder.ins().call(alloc_ref, &[size_val]);
        let array_ptr = self.builder.inst_results(call)[0];

        // 填充 channel 数组
        for (i, (_, channel_name, _)) in recv_branches.iter().enumerate() {
            let ch_var = *self.variables.get(*channel_name)
                .ok_or_else(|| format!("Undefined channel: {}", channel_name))?;
            let ch_ptr = self.builder.use_var(ch_var);
            let offset = (i * 8) as i32;
            self.builder.ins().store(MemFlags::new(), ch_ptr, array_ptr, offset);
        }

        // 分配接收值空间
        let value_size = self.builder.ins().iconst(types::I64, 8);
        let call = self.builder.ins().call(alloc_ref, &[value_size]);
        let value_ptr = self.builder.inst_results(call)[0];

        // 确定 timeout 值
        let timeout_val = if default_branch.is_some() {
            self.builder.ins().iconst(types::I64, -2) // has default
        } else if let Some((duration_expr, _)) = &timeout_branch {
            self.compile_expr(duration_expr)?
        } else {
            self.builder.ins().iconst(types::I64, -1) // no timeout
        };

        // 调用 channel_select
        let select_ref = *self.func_refs.get("channel_select")
            .ok_or("channel_select not found")?;
        let count_val = self.builder.ins().iconst(types::I64, channel_count as i64);
        let call = self.builder.ins().call(select_ref, &[array_ptr, count_val, timeout_val, value_ptr]);
        let selected_idx = self.builder.inst_results(call)[0];

        // 创建各分支的基本块
        let exit_block = self.builder.create_block();
        let mut branch_blocks = Vec::new();
        for _ in 0..channel_count {
            branch_blocks.push(self.builder.create_block());
        }
        let timeout_block = if timeout_branch.is_some() {
            Some(self.builder.create_block())
        } else {
            None
        };
        let default_block_opt = if default_branch.is_some() {
            Some(self.builder.create_block())
        } else {
            None
        };

        // 生成分支跳转逻辑
        for (i, &block) in branch_blocks.iter().enumerate() {
            let idx_val = self.builder.ins().iconst(types::I64, i as i64);
            let is_match = self.builder.ins().icmp(IntCC::Equal, selected_idx, idx_val);
            let next_block = self.builder.create_block();
            self.builder.ins().brif(is_match, block, &[], next_block, &[]);
            self.builder.switch_to_block(next_block);
            self.builder.seal_block(next_block);
        }

        // 检查 timeout (-1)
        if let Some(block) = timeout_block {
            let timeout_val = self.builder.ins().iconst(types::I64, -1);
            let is_timeout = self.builder.ins().icmp(IntCC::Equal, selected_idx, timeout_val);
            let next_block = self.builder.create_block();
            self.builder.ins().brif(is_timeout, block, &[], next_block, &[]);
            self.builder.switch_to_block(next_block);
            self.builder.seal_block(next_block);
        }

        // 检查 default (-2)
        if let Some(block) = default_block_opt {
            self.builder.ins().jump(block, &[]);
        } else {
            self.builder.ins().jump(exit_block, &[]);
        }

        // 编译各 recv 分支
        for (i, (var_name, _, body)) in recv_branches.iter().enumerate() {
            self.builder.switch_to_block(branch_blocks[i]);
            self.builder.seal_block(branch_blocks[i]);

            let recv_val = self.builder.ins().load(types::I64, MemFlags::new(), value_ptr, 0);
            let var = self.declare_variable(var_name, types::I64);
            self.builder.def_var(var, recv_val);

            for stmt in *body {
                self.compile_stmt(stmt)?;
            }
            self.builder.ins().jump(exit_block, &[]);
        }

        // 编译 timeout 分支
        if let (Some(block), Some((_, body))) = (timeout_block, &timeout_branch) {
            self.builder.switch_to_block(block);
            self.builder.seal_block(block);
            for stmt in *body {
                self.compile_stmt(stmt)?;
            }
            self.builder.ins().jump(exit_block, &[]);
        }

        // 编译 default 分支
        if let (Some(block), Some(body)) = (default_block_opt, default_branch) {
            self.builder.switch_to_block(block);
            self.builder.seal_block(block);
            for stmt in body {
                self.compile_stmt(stmt)?;
            }
            self.builder.ins().jump(exit_block, &[]);
        }

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }

    /// 编译 AwaitScope 语句
    fn compile_await_scope(&mut self, scope_stmt: &bolide_parser::AwaitScopeStmt) -> Result<(), String> {
        // 进入作用域
        let scope_enter_ref = *self.func_refs.get("scope_enter")
            .ok_or("scope_enter not found")?;
        self.builder.ins().call(scope_enter_ref, &[]);

        // 编译作用域内的语句
        for stmt in &scope_stmt.body {
            self.compile_stmt(stmt)?;
        }

        // 退出作用域
        let scope_exit_ref = *self.func_refs.get("scope_exit")
            .ok_or("scope_exit not found")?;
        self.builder.ins().call(scope_exit_ref, &[]);

        Ok(())
    }

    /// 编译 AsyncSelect 语句
    fn compile_async_select(&mut self, async_select: &bolide_parser::AsyncSelectStmt) -> Result<(), String> {
        use bolide_parser::AsyncSelectBranch;
        use cranelift_codegen::ir::StackSlotData;
        use cranelift_codegen::ir::StackSlotKind;

        if async_select.branches.is_empty() {
            return Ok(());
        }

        let branch_count = async_select.branches.len();

        // 1. 启动所有异步任务，收集 futures
        let mut futures: Vec<Value> = Vec::new();
        for branch in &async_select.branches {
            let expr = match branch {
                AsyncSelectBranch::Bind { expr, .. } => expr,
                AsyncSelectBranch::Expr { expr, .. } => expr,
            };
            let future = self.compile_expr(expr)?;
            futures.push(future);
        }

        // 2. 在栈上分配数组存储 futures
        let array_size = (branch_count * 8) as u32;
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            array_size,
            0,
        ));
        let array_ptr = self.builder.ins().stack_addr(types::I64, slot, 0);

        // 3. 将 futures 存入数组
        for (i, future) in futures.iter().enumerate() {
            let offset = (i * 8) as i32;
            self.builder.ins().store(MemFlags::new(), *future, array_ptr, offset);
        }

        // 4. 调用 select_wait_first 获取第一个完成的索引
        let select_wait_first = *self.func_refs.get("select_wait_first")
            .ok_or("select_wait_first not found")?;
        let count = self.builder.ins().iconst(types::I64, branch_count as i64);
        let call = self.builder.ins().call(select_wait_first, &[array_ptr, count]);
        let winner_idx = self.builder.inst_results(call)[0];

        // 5. 根据获胜索引执行对应分支
        self.compile_async_select_branches(async_select, &futures, winner_idx)?;

        Ok(())
    }

    /// 编译 async select 分支选择逻辑
    fn compile_async_select_branches(
        &mut self,
        select_stmt: &bolide_parser::AsyncSelectStmt,
        futures: &[Value],
        winner_idx: Value,
    ) -> Result<(), String> {
        use bolide_parser::AsyncSelectBranch;

        let merge_block = self.builder.create_block();

        for (i, branch) in select_stmt.branches.iter().enumerate() {
            let branch_block = self.builder.create_block();
            let next_block = self.builder.create_block();

            // 比较 winner_idx == i
            let idx_const = self.builder.ins().iconst(types::I64, i as i64);
            let cmp = self.builder.ins().icmp(IntCC::Equal, winner_idx, idx_const);
            self.builder.ins().brif(cmp, branch_block, &[], next_block, &[]);

            // 分支块
            self.builder.switch_to_block(branch_block);
            self.builder.seal_block(branch_block);

            match branch {
                AsyncSelectBranch::Bind { var, body, .. } => {
                    // await 获取结果并绑定变量
                    let await_int = *self.func_refs.get("coroutine_await_int")
                        .ok_or("coroutine_await_int not found")?;
                    let call = self.builder.ins().call(await_int, &[futures[i]]);
                    let result = self.builder.inst_results(call)[0];

                    let var_decl = self.declare_variable(var, types::I64);
                    self.builder.def_var(var_decl, result);

                    for stmt in body {
                        self.compile_stmt(stmt)?;
                    }
                }
                AsyncSelectBranch::Expr { body, .. } => {
                    for stmt in body {
                        self.compile_stmt(stmt)?;
                    }
                }
            }

            self.builder.ins().jump(merge_block, &[]);
            self.builder.switch_to_block(next_block);
            self.builder.seal_block(next_block);
        }

        // 最后一个 next_block 直接跳转到 merge
        self.builder.ins().jump(merge_block, &[]);
        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);

        Ok(())
    }

    /// 编译变量声明
    fn compile_var_decl(&mut self, decl: &bolide_parser::VarDecl) -> Result<(), String> {
        let ty = if let Some(ref t) = decl.ty {
            self.bolide_type_to_cranelift(t)
        } else {
            types::I64
        };
        let var = self.declare_variable(&decl.name, ty);

        // Store the type in var_types
        if let Some(ref t) = decl.ty {
            self.var_types.insert(decl.name.clone(), t.clone());
        } else if let Some(ref value) = decl.value {
            // Infer type from value expression
            if let Some(inferred_ty) = self.infer_expr_type(value) {
                println!("VarDecl: {} inferred type: {:?}", decl.name, inferred_ty);
                self.var_types.insert(decl.name.clone(), inferred_ty);
            } else {
                println!("VarDecl: {} inferred type: None", decl.name);
            }
        }

        if let Some(ref value) = decl.value {
            let val = self.compile_expr(value)?;
            
            // Take ownership if it's a temp RC value
            self.remove_temp_rc_value(val);
            
            self.builder.def_var(var, val);
        } else {
            let zero = self.builder.ins().iconst(types::I64, 0);
            self.builder.def_var(var, zero);
        }

        // Register for cleanup
        if let Some(ty) = self.var_types.get(&decl.name).cloned() {
            self.track_rc_variable(&decl.name, &ty);
        }

        Ok(())
    }

    /// 编译赋值语句
    fn compile_assign(&mut self, assign: &bolide_parser::Assign) -> Result<(), String> {
        match &assign.target {
            Expr::Ident(var_name) => {
                let var = *self.variables.get(var_name)
                    .ok_or_else(|| format!("Undefined variable: {}", var_name))?;
                let val = self.compile_expr(&assign.value)?;
                
                // Release old value if RC type
                if let Some(ty) = self.var_types.get(var_name).cloned() {
                    if Self::is_rc_type(&ty) {
                        let old_val = self.builder.use_var(var);
                        self.emit_release(old_val, &ty);
                        
                        // Take ownership of new value if it's a temp
                        self.remove_temp_rc_value(val);
                    }
                }
                
                self.builder.def_var(var, val);
            }
            Expr::Member(base, member) => {
                self.compile_member_assign(base, member, &assign.value)?;
            }
            Expr::Index(base, index) => {
                self.compile_index_assign(base, index, &assign.value)?;
            }
            _ => return Err("Unsupported assignment target".to_string()),
        }
        Ok(())
    }

    /// 编译成员赋值
    fn compile_member_assign(&mut self, base: &Expr, member: &str, value: &Expr) -> Result<(), String> {
        let base_val = self.compile_expr(base)?;
        let val = self.compile_expr(value)?;

        let base_type = self.infer_expr_type(base);
        if let Some(BolideType::Custom(class_name)) = base_type {
            if let Some(class_info) = self.classes.get(&class_name).cloned() {
                for field in &class_info.fields {
                    if field.name == member {
                        let offset = field.offset as i32;
                        
                        // Release old value if RC type
                        if Self::is_rc_type(&field.ty) {
                            let field_ptr = self.builder.ins().iadd_imm(base_val, offset as i64);
                            let old_val = self.builder.ins().load(types::I64, MemFlags::new(), field_ptr, 0);
                            self.emit_release(old_val, &field.ty);
                            
                            // Take ownership of new value if it's a temp
                            self.remove_temp_rc_value(val);
                        }
                        
                        self.builder.ins().store(MemFlags::new(), val, base_val, offset);
                        return Ok(());
                    }
                }
                return Err(format!("Field '{}' not found in class '{}'", member, class_name));
            }
        }
        Err("Cannot assign to member of non-class type".to_string())
    }

    /// 编译索引赋值
    fn compile_index_assign(&mut self, base: &Expr, index: &Expr, value: &Expr) -> Result<(), String> {
        let base_val = self.compile_expr(base)?;
        let index_val = self.compile_expr(index)?;
        let val = self.compile_expr(value)?;

        // Consume value ownership
        self.remove_temp_rc_value(val);

        let func_ref = *self.func_refs.get("list_set")
            .ok_or("list_set not found")?;
        self.builder.ins().call(func_ref, &[base_val, index_val, val]);
        Ok(())
    }

    /// 编译返回语句
    fn compile_return(&mut self, expr: Option<&Expr>) -> Result<(), String> {
        if let Some(e) = expr {
            let val = self.compile_expr(e)?;
            
            // If val is in temp_rc_values, remove it so it's not released here
            // (Function return transfers ownership of +1 ref count)
            self.remove_temp_rc_value(val);
            
            // Release other temporary values
            self.release_temp_rc_values();
            
            // Cleanup variables before returning
            self.emit_rc_cleanup();
            self.builder.ins().return_(&[val]);
        } else {
            // Release temporary values
            self.release_temp_rc_values();
            
            self.emit_rc_cleanup();
            self.builder.ins().return_(&[]);
        }
        Ok(())
    }

    /// 编译 if 语句
    fn compile_if(&mut self, if_stmt: &bolide_parser::IfStmt) -> Result<bool, String> {
        let cond = self.compile_expr(&if_stmt.condition)?;

        let then_block = self.builder.create_block();
        let else_block = self.builder.create_block();
        let merge_block = self.builder.create_block();

        let zero = self.builder.ins().iconst(types::I64, 0);
        let cond_bool = self.builder.ins().icmp(IntCC::NotEqual, cond, zero);
        
        // Release condition temps before branching
        self.release_temp_rc_values();
        
        self.builder.ins().brif(cond_bool, then_block, &[], else_block, &[]);

        // then 分支
        self.builder.switch_to_block(then_block);
        self.builder.seal_block(then_block);
        
        let scope_idx = self.enter_scope();
        let mut then_returned = false;
        for stmt in &if_stmt.then_body {
            if self.compile_stmt(stmt)? {
                then_returned = true;
                break;
            }
        }
        if !then_returned {
             self.leave_scope(scope_idx);
             self.builder.ins().jump(merge_block, &[]);
        }
        // Scope variables released before jump

        // else 分支
        self.builder.switch_to_block(else_block);
        self.builder.seal_block(else_block);
        
        let scope_idx_else = self.enter_scope();
        let mut else_returned = false;
        if let Some(ref else_body) = if_stmt.else_body {
            for stmt in else_body {
                if self.compile_stmt(stmt)? {
                    else_returned = true;
                    break;
                }
            }
        }
        if !else_returned {
            self.leave_scope(scope_idx_else);
            self.builder.ins().jump(merge_block, &[]);
        }

        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);

        Ok(then_returned && else_returned)
    }

    /// 编译 while 语句
    fn compile_while(&mut self, while_stmt: &bolide_parser::WhileStmt) -> Result<(), String> {
        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        self.builder.ins().jump(header_block, &[]);

        // 条件检查
        self.builder.switch_to_block(header_block);
        let cond = self.compile_expr(&while_stmt.condition)?;
        let zero = self.builder.ins().iconst(types::I64, 0);
        let cond_bool = self.builder.ins().icmp(IntCC::NotEqual, cond, zero);
        
        // Release condition temps before branching
        self.release_temp_rc_values();
        
        self.builder.ins().brif(cond_bool, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);
        
        let scope_idx = self.enter_scope();
        let mut body_returned = false;
        for stmt in &while_stmt.body {
            if self.compile_stmt(stmt)? {
                body_returned = true;
                break;
            }
        }
        
        if !body_returned {
             self.leave_scope(scope_idx);
             self.builder.ins().jump(header_block, &[]);
        }

        // 现在所有 header_block 的前驱都已添加，可以 seal 了
        self.builder.seal_block(header_block);

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }

    /// 编译 for 语句
    fn compile_for(&mut self, for_stmt: &bolide_parser::ForStmt) -> Result<(), String> {
        // 检查是否是 range() 调用
        if let Expr::Call(callee, args) = &for_stmt.iter {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "range" {
                    return self.compile_range_for(for_stmt, args);
                }
            }
        }

        // 列表迭代
        self.compile_list_for(for_stmt)
    }

    /// 编译 range for 循环
    fn compile_range_for(&mut self, for_stmt: &bolide_parser::ForStmt, args: &[Expr]) -> Result<(), String> {
        // 解析 range 参数: range(end) 或 range(start, end) 或 range(start, end, step)
        let (start, end, step) = match args.len() {
            1 => {
                let end = self.compile_expr(&args[0])?;
                let start = self.builder.ins().iconst(types::I64, 0);
                let step = self.builder.ins().iconst(types::I64, 1);
                (start, end, step)
            }
            2 => {
                let start = self.compile_expr(&args[0])?;
                let end = self.compile_expr(&args[1])?;
                let step = self.builder.ins().iconst(types::I64, 1);
                (start, end, step)
            }
            3 => {
                let start = self.compile_expr(&args[0])?;
                let end = self.compile_expr(&args[1])?;
                let step = self.compile_expr(&args[2])?;
                (start, end, step)
            }
            _ => return Err("range() requires 1-3 arguments".to_string()),
        };

        // 创建循环变量
        let var_name = for_stmt.vars.first()
            .ok_or("For loop requires at least one variable")?;
        let loop_var = self.declare_variable(var_name, types::I64);
        self.builder.def_var(loop_var, start);

        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        self.builder.ins().jump(header_block, &[]);

        // 条件检查
        self.builder.switch_to_block(header_block);
        let idx = self.builder.use_var(loop_var);
        let cond = self.builder.ins().icmp(IntCC::SignedLessThan, idx, end);
        self.builder.ins().brif(cond, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);

        let scope_idx = self.enter_scope();
        let mut body_returned = false;
        for stmt in &for_stmt.body {
            if self.compile_stmt(stmt)? {
                body_returned = true;
                break;
            }
        }
        
        if !body_returned {
             self.leave_scope(scope_idx);

             // 递增索引
             let idx = self.builder.use_var(loop_var);
             let new_idx = self.builder.ins().iadd(idx, step);
             self.builder.def_var(loop_var, new_idx);

             self.builder.ins().jump(header_block, &[]);
        }

        self.builder.seal_block(header_block);

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }

    /// 编译列表 for 循环
    fn compile_list_for(&mut self, for_stmt: &bolide_parser::ForStmt) -> Result<(), String> {
        // 编译迭代器
        let iter_val = self.compile_expr(&for_stmt.iter)?;
        
        // Infer element type
        let elem_type = match self.infer_expr_type(&for_stmt.iter) {
            Some(BolideType::List(inner)) => *inner,
            _ => BolideType::Int, // Fallback
        };

        // 获取列表长度
        let len_ref = *self.func_refs.get("list_len")
            .ok_or("list_len not found")?;
        let call = self.builder.ins().call(len_ref, &[iter_val]);
        let len = self.builder.inst_results(call)[0];

        // 创建索引变量
        let idx_var = self.declare_variable("__for_idx", types::I64);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.def_var(idx_var, zero);

        // 创建循环变量
        let var_name = for_stmt.vars.first()
            .ok_or("For loop requires at least one variable")?;
        let loop_var = self.declare_variable(var_name, types::I64);
        self.builder.def_var(loop_var, zero);
        
        self.var_types.insert(var_name.clone(), elem_type.clone());

        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        self.builder.ins().jump(header_block, &[]);

        // 条件检查
        self.builder.switch_to_block(header_block);
        let idx = self.builder.use_var(idx_var);
        let cond = self.builder.ins().icmp(IntCC::SignedLessThan, idx, len);
        self.builder.ins().brif(cond, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);
        
        let scope_idx = self.enter_scope();
        if Self::is_rc_type(&elem_type) {
            self.track_rc_variable(var_name, &elem_type);
        }

        let get_ref = *self.func_refs.get("list_get")
            .ok_or("list_get not found")?;
        let idx = self.builder.use_var(idx_var);
        let call = self.builder.ins().call(get_ref, &[iter_val, idx]);
        let elem = self.builder.inst_results(call)[0];
        
        let elem = if Self::is_rc_type(&elem_type) {
             self.emit_retain(elem, &elem_type)
        } else {
             elem
        };
        self.builder.def_var(loop_var, elem);

        let mut body_returned = false;
        for stmt in &for_stmt.body {
            if self.compile_stmt(stmt)? {
                body_returned = true;
                break;
            }
        }
        
        if !body_returned {
            self.leave_scope(scope_idx);

            // 递增索引
            let idx = self.builder.use_var(idx_var);
            let one = self.builder.ins().iconst(types::I64, 1);
            let new_idx = self.builder.ins().iadd(idx, one);
            self.builder.def_var(idx_var, new_idx);

            self.builder.ins().jump(header_block, &[]);
        }

        self.builder.seal_block(header_block);

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }
}
