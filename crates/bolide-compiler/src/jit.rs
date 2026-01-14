//! JIT 编译器
//!
//! 使用 Cranelift 实现的即时编译器

use cranelift::prelude::*;
use cranelift::prelude::isa::{TargetIsa, CallConv};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, Linkage, Module, FuncId};
use cranelift_codegen::ir::{FuncRef, StackSlotData, StackSlotKind};
use std::collections::{HashMap, HashSet};
use bolide_parser::{Program, Statement, Expr, BinOp, UnaryOp, Type as BolideType, FuncDef, VarDecl, Assign, Param, ParamMode, ClassDef, ClassField, ExternBlock};

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
    offset: usize,  // 字段在对象中的偏移（字节）
}

/// 类信息
#[derive(Clone)]
struct ClassInfo {
    name: String,
    parent: Option<String>,
    fields: Vec<FieldInfo>,
    methods: Vec<String>,  // 方法名列表
    size: usize,  // 对象数据大小（字节，不含头部）
}

/// JIT 编译器
pub struct JitCompiler {
    module: JITModule,
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
    /// 已加载的动态库
    loaded_libs: HashMap<String, libloading::Library>,
    /// 模块名映射: 模块名 -> 文件路径
    modules: HashMap<String, String>,
    /// 使用生命周期模式的函数集合（返回借用而非拥有的值）
    lifetime_funcs: HashSet<String>,
}

impl JitCompiler {
    pub fn new() -> Self {
        let mut builder = JITBuilder::new(cranelift_module::default_libcall_names())
            .expect("Failed to create JIT builder");

        // 注册运行时函数 - 基本类型打印 (统一在 print.rs)
        builder.symbol("print_int", bolide_runtime::bolide_print_int as *const u8);
        builder.symbol("print_float", bolide_runtime::bolide_print_float as *const u8);
        builder.symbol("print_bool", bolide_runtime::bolide_print_bool as *const u8);
        builder.symbol("print_bigint", bolide_runtime::bolide_print_bigint as *const u8);
        builder.symbol("print_decimal", bolide_runtime::bolide_print_decimal as *const u8);
        builder.symbol("print_string", bolide_runtime::bolide_print_string as *const u8);
        builder.symbol("print_dynamic", bolide_runtime::bolide_print_dynamic as *const u8);

        // 注册运行时函数 - 用户输入
        builder.symbol("input", bolide_runtime::bolide_input as *const u8);
        builder.symbol("input_prompt", bolide_runtime::bolide_input_prompt as *const u8);

        // 注册运行时函数 - BigInt
        builder.symbol("bigint_from_i64", bolide_runtime::bolide_bigint_from_i64 as *const u8);
        builder.symbol("bigint_from_str", bolide_runtime::bolide_bigint_from_str as *const u8);
        builder.symbol("bigint_add", bolide_runtime::bolide_bigint_add as *const u8);
        builder.symbol("bigint_sub", bolide_runtime::bolide_bigint_sub as *const u8);
        builder.symbol("bigint_mul", bolide_runtime::bolide_bigint_mul as *const u8);
        builder.symbol("bigint_div", bolide_runtime::bolide_bigint_div as *const u8);
        builder.symbol("bigint_rem", bolide_runtime::bolide_bigint_rem as *const u8);
        builder.symbol("bigint_neg", bolide_runtime::bolide_bigint_neg as *const u8);
        builder.symbol("bigint_eq", bolide_runtime::bolide_bigint_eq as *const u8);
        builder.symbol("bigint_lt", bolide_runtime::bolide_bigint_lt as *const u8);
        builder.symbol("bigint_le", bolide_runtime::bolide_bigint_le as *const u8);
        builder.symbol("bigint_gt", bolide_runtime::bolide_bigint_gt as *const u8);
        builder.symbol("bigint_ge", bolide_runtime::bolide_bigint_ge as *const u8);
        builder.symbol("bigint_to_i64", bolide_runtime::bolide_bigint_to_i64 as *const u8);
        builder.symbol("bigint_clone", bolide_runtime::bolide_bigint_clone as *const u8);
        builder.symbol("bigint_debug_stats", bolide_runtime::bolide_bigint_debug_stats as *const u8);

        // 注册运行时函数 - Decimal
        builder.symbol("decimal_from_i64", bolide_runtime::bolide_decimal_from_i64 as *const u8);
        builder.symbol("decimal_from_f64", bolide_runtime::bolide_decimal_from_f64 as *const u8);
        builder.symbol("decimal_from_str", bolide_runtime::bolide_decimal_from_str as *const u8);
        builder.symbol("decimal_add", bolide_runtime::bolide_decimal_add as *const u8);
        builder.symbol("decimal_sub", bolide_runtime::bolide_decimal_sub as *const u8);
        builder.symbol("decimal_mul", bolide_runtime::bolide_decimal_mul as *const u8);
        builder.symbol("decimal_div", bolide_runtime::bolide_decimal_div as *const u8);
        builder.symbol("decimal_neg", bolide_runtime::bolide_decimal_neg as *const u8);
        builder.symbol("decimal_eq", bolide_runtime::bolide_decimal_eq as *const u8);
        builder.symbol("decimal_lt", bolide_runtime::bolide_decimal_lt as *const u8);
        builder.symbol("decimal_to_i64", bolide_runtime::bolide_decimal_to_i64 as *const u8);
        builder.symbol("decimal_to_f64", bolide_runtime::bolide_decimal_to_f64 as *const u8);
        builder.symbol("decimal_clone", bolide_runtime::bolide_decimal_clone as *const u8);

        // 注册运行时函数 - Dynamic
        builder.symbol("dynamic_from_int", bolide_runtime::bolide_dynamic_from_int as *const u8);
        builder.symbol("dynamic_from_float", bolide_runtime::bolide_dynamic_from_float as *const u8);
        builder.symbol("dynamic_from_bool", bolide_runtime::bolide_dynamic_from_bool as *const u8);
        builder.symbol("dynamic_from_string", bolide_runtime::bolide_dynamic_from_string as *const u8);
        builder.symbol("dynamic_from_list", bolide_runtime::bolide_dynamic_from_list as *const u8);
        builder.symbol("dynamic_from_bigint", bolide_runtime::bolide_dynamic_from_bigint as *const u8);
        builder.symbol("dynamic_from_decimal", bolide_runtime::bolide_dynamic_from_decimal as *const u8);
        builder.symbol("dynamic_add", bolide_runtime::bolide_dynamic_add as *const u8);
        builder.symbol("dynamic_sub", bolide_runtime::bolide_dynamic_sub as *const u8);
        builder.symbol("dynamic_mul", bolide_runtime::bolide_dynamic_mul as *const u8);
        builder.symbol("dynamic_div", bolide_runtime::bolide_dynamic_div as *const u8);
        builder.symbol("dynamic_neg", bolide_runtime::bolide_dynamic_neg as *const u8);
        builder.symbol("dynamic_eq", bolide_runtime::bolide_dynamic_eq as *const u8);
        builder.symbol("dynamic_lt", bolide_runtime::bolide_dynamic_lt as *const u8);
        builder.symbol("dynamic_clone", bolide_runtime::bolide_dynamic_clone as *const u8);

        // 注册字符串函数
        builder.symbol("string_from_slice", bolide_runtime::bolide_string_from_slice as *const u8);
        builder.symbol("string_literal", bolide_runtime::bolide_string_literal as *const u8);
        builder.symbol("string_as_cstr", bolide_runtime::bolide_string_as_cstr as *const u8);
        builder.symbol("string_concat", bolide_runtime::bolide_string_concat as *const u8);
        builder.symbol("string_eq", bolide_runtime::bolide_string_eq as *const u8);

        // 注册类型转换函数
        builder.symbol("string_from_int", bolide_runtime::bolide_string_from_int as *const u8);
        builder.symbol("string_from_float", bolide_runtime::bolide_string_from_float as *const u8);
        builder.symbol("string_from_bool", bolide_runtime::bolide_string_from_bool as *const u8);
        builder.symbol("string_from_bigint", bolide_runtime::bolide_string_from_bigint as *const u8);
        builder.symbol("string_from_decimal", bolide_runtime::bolide_string_from_decimal as *const u8);
        builder.symbol("string_to_int", bolide_runtime::bolide_string_to_int as *const u8);
        builder.symbol("string_to_float", bolide_runtime::bolide_string_to_float as *const u8);

        // 注册内存分配函数
        builder.symbol("bolide_alloc", bolide_runtime::bolide_alloc as *const u8);
        builder.symbol("bolide_free", bolide_runtime::bolide_free as *const u8);

        // 注册对象运行时函数
        builder.symbol("object_alloc", bolide_runtime::object_alloc as *const u8);
        builder.symbol("object_retain", bolide_runtime::object_retain as *const u8);
        builder.symbol("object_release", bolide_runtime::object_release as *const u8);
        builder.symbol("object_clone", bolide_runtime::object_clone as *const u8);

        // 注册运行时函数 - 线程（无参版本）
        builder.symbol("thread_spawn_int", bolide_runtime::bolide_thread_spawn_int as *const u8);
        builder.symbol("thread_spawn_float", bolide_runtime::bolide_thread_spawn_float as *const u8);
        builder.symbol("thread_spawn_ptr", bolide_runtime::bolide_thread_spawn_ptr as *const u8);
        // 注册运行时函数 - 线程（带环境版本，用于带参数的 spawn）
        builder.symbol("thread_spawn_int_with_env", bolide_runtime::bolide_thread_spawn_int_with_env as *const u8);
        builder.symbol("thread_spawn_float_with_env", bolide_runtime::bolide_thread_spawn_float_with_env as *const u8);
        builder.symbol("thread_spawn_ptr_with_env", bolide_runtime::bolide_thread_spawn_ptr_with_env as *const u8);
        builder.symbol("thread_join_int", bolide_runtime::bolide_thread_join_int as *const u8);
        builder.symbol("thread_join_float", bolide_runtime::bolide_thread_join_float as *const u8);
        builder.symbol("thread_join_ptr", bolide_runtime::bolide_thread_join_ptr as *const u8);
        builder.symbol("thread_handle_free", bolide_runtime::bolide_thread_handle_free as *const u8);
        builder.symbol("thread_cancel", bolide_runtime::bolide_thread_cancel as *const u8);
        builder.symbol("thread_is_cancelled", bolide_runtime::bolide_thread_is_cancelled as *const u8);

        // 注册运行时函数 - 线程池（无参版本）
        builder.symbol("pool_create", bolide_runtime::bolide_pool_create as *const u8);
        builder.symbol("pool_enter", bolide_runtime::bolide_pool_enter as *const u8);
        builder.symbol("pool_exit", bolide_runtime::bolide_pool_exit as *const u8);
        builder.symbol("pool_is_active", bolide_runtime::bolide_pool_is_active as *const u8);
        builder.symbol("pool_spawn_int", bolide_runtime::bolide_pool_spawn_int as *const u8);
        builder.symbol("pool_spawn_float", bolide_runtime::bolide_pool_spawn_float as *const u8);
        builder.symbol("pool_spawn_ptr", bolide_runtime::bolide_pool_spawn_ptr as *const u8);
        // 注册运行时函数 - 线程池（带环境版本）
        builder.symbol("pool_spawn_int_with_env", bolide_runtime::bolide_pool_spawn_int_with_env as *const u8);
        builder.symbol("pool_spawn_float_with_env", bolide_runtime::bolide_pool_spawn_float_with_env as *const u8);
        builder.symbol("pool_spawn_ptr_with_env", bolide_runtime::bolide_pool_spawn_ptr_with_env as *const u8);
        builder.symbol("pool_join_int", bolide_runtime::bolide_pool_join_int as *const u8);
        builder.symbol("pool_join_float", bolide_runtime::bolide_pool_join_float as *const u8);
        builder.symbol("pool_join_ptr", bolide_runtime::bolide_pool_join_ptr as *const u8);
        builder.symbol("pool_handle_free", bolide_runtime::bolide_pool_handle_free as *const u8);
        builder.symbol("pool_destroy", bolide_runtime::bolide_pool_destroy as *const u8);

        // 注册运行时函数 - 通道
        builder.symbol("channel_create", bolide_runtime::bolide_channel_create as *const u8);
        builder.symbol("channel_create_buffered", bolide_runtime::bolide_channel_create_buffered as *const u8);
        builder.symbol("channel_send", bolide_runtime::bolide_channel_send as *const u8);
        builder.symbol("channel_recv", bolide_runtime::bolide_channel_recv as *const u8);
        builder.symbol("channel_close", bolide_runtime::bolide_channel_close as *const u8);
        builder.symbol("channel_free", bolide_runtime::bolide_channel_free as *const u8);
        builder.symbol("channel_select", bolide_runtime::bolide_channel_select as *const u8);

        // 注册运行时函数 - 协程
        builder.symbol("coroutine_spawn_int", bolide_runtime::bolide_coroutine_spawn_int as *const u8);
        builder.symbol("coroutine_spawn_float", bolide_runtime::bolide_coroutine_spawn_float as *const u8);
        builder.symbol("coroutine_spawn_ptr", bolide_runtime::bolide_coroutine_spawn_ptr as *const u8);
        builder.symbol("coroutine_await_int", bolide_runtime::bolide_coroutine_await_int as *const u8);
        builder.symbol("coroutine_await_float", bolide_runtime::bolide_coroutine_await_float as *const u8);
        builder.symbol("coroutine_await_ptr", bolide_runtime::bolide_coroutine_await_ptr as *const u8);
        builder.symbol("coroutine_cancel", bolide_runtime::bolide_coroutine_cancel as *const u8);
        builder.symbol("coroutine_free", bolide_runtime::bolide_coroutine_free as *const u8);
        builder.symbol("coroutine_spawn_int_with_env", bolide_runtime::bolide_coroutine_spawn_int_with_env as *const u8);
        builder.symbol("coroutine_spawn_float_with_env", bolide_runtime::bolide_coroutine_spawn_float_with_env as *const u8);
        builder.symbol("coroutine_spawn_ptr_with_env", bolide_runtime::bolide_coroutine_spawn_ptr_with_env as *const u8);
        builder.symbol("scope_enter", bolide_runtime::bolide_scope_enter as *const u8);
        builder.symbol("scope_register", bolide_runtime::bolide_scope_register as *const u8);
        builder.symbol("scope_exit", bolide_runtime::bolide_scope_exit as *const u8);

        // 注册运行时函数 - select
        builder.symbol("select_wait_first", bolide_runtime::bolide_select_wait_first as *const u8);

        // 注册运行时函数 - 元组
        builder.symbol("tuple_new", bolide_runtime::bolide_tuple_new as *const u8);
        builder.symbol("tuple_free", bolide_runtime::bolide_tuple_free as *const u8);
        builder.symbol("tuple_set", bolide_runtime::bolide_tuple_set as *const u8);
        builder.symbol("tuple_get", bolide_runtime::bolide_tuple_get as *const u8);
        builder.symbol("tuple_len", bolide_runtime::bolide_tuple_len as *const u8);
        builder.symbol("print_tuple", bolide_runtime::bolide_print_tuple as *const u8);

        // FFI 运行时函数
        builder.symbol("ffi_load_library", bolide_runtime::bolide_ffi_load_library as *const u8);
        builder.symbol("ffi_get_symbol", bolide_runtime::bolide_ffi_get_symbol as *const u8);
        builder.symbol("ffi_cleanup", bolide_runtime::bolide_ffi_cleanup as *const u8);
        builder.symbol("test_callback", bolide_runtime::bolide_test_callback as *const u8);
        builder.symbol("map_int", bolide_runtime::bolide_map_int as *const u8);

        // 注册运行时函数 - RC 引用计数管理
        builder.symbol("string_retain", bolide_runtime::bolide_string_retain as *const u8);
        builder.symbol("string_release", bolide_runtime::bolide_string_release as *const u8);
        builder.symbol("string_clone", bolide_runtime::bolide_string_clone as *const u8);
        builder.symbol("bigint_retain", bolide_runtime::bolide_bigint_retain as *const u8);
        builder.symbol("bigint_release", bolide_runtime::bolide_bigint_release as *const u8);
        builder.symbol("decimal_retain", bolide_runtime::bolide_decimal_retain as *const u8);
        builder.symbol("decimal_release", bolide_runtime::bolide_decimal_release as *const u8);
        builder.symbol("list_retain", bolide_runtime::bolide_list_retain as *const u8);
        builder.symbol("list_release", bolide_runtime::bolide_list_release as *const u8);
        builder.symbol("list_clone", bolide_runtime::bolide_list_clone as *const u8);
        builder.symbol("list_new", bolide_runtime::bolide_list_new as *const u8);
        builder.symbol("list_push", bolide_runtime::bolide_list_push as *const u8);
        builder.symbol("list_pop", bolide_runtime::bolide_list_pop as *const u8);
        builder.symbol("list_len", bolide_runtime::bolide_list_len as *const u8);
        builder.symbol("list_get", bolide_runtime::bolide_list_get as *const u8);
        builder.symbol("list_set", bolide_runtime::bolide_list_set as *const u8);
        builder.symbol("list_insert", bolide_runtime::bolide_list_insert as *const u8);
        builder.symbol("list_remove", bolide_runtime::bolide_list_remove as *const u8);
        builder.symbol("list_clear", bolide_runtime::bolide_list_clear as *const u8);
        builder.symbol("list_reverse", bolide_runtime::bolide_list_reverse as *const u8);
        builder.symbol("list_extend", bolide_runtime::bolide_list_extend as *const u8);
        builder.symbol("list_contains", bolide_runtime::bolide_list_contains as *const u8);
        builder.symbol("list_index_of", bolide_runtime::bolide_list_index_of as *const u8);
        builder.symbol("list_count", bolide_runtime::bolide_list_count as *const u8);
        builder.symbol("list_sort", bolide_runtime::bolide_list_sort as *const u8);
        builder.symbol("list_slice", bolide_runtime::bolide_list_slice as *const u8);
        builder.symbol("list_is_empty", bolide_runtime::bolide_list_is_empty as *const u8);
        builder.symbol("list_first", bolide_runtime::bolide_list_first as *const u8);
        builder.symbol("list_last", bolide_runtime::bolide_list_last as *const u8);
        builder.symbol("print_list", bolide_runtime::bolide_print_list as *const u8);
        // Dict symbols
        builder.symbol("dict_new", bolide_runtime::bolide_dict_new as *const u8);
        builder.symbol("dict_retain", bolide_runtime::bolide_dict_retain as *const u8);
        builder.symbol("dict_release", bolide_runtime::bolide_dict_release as *const u8);
        builder.symbol("dict_clone", bolide_runtime::bolide_dict_clone as *const u8);
        builder.symbol("dict_set", bolide_runtime::bolide_dict_set as *const u8);
        builder.symbol("dict_get", bolide_runtime::bolide_dict_get as *const u8);
        builder.symbol("dict_contains", bolide_runtime::bolide_dict_contains as *const u8);
        builder.symbol("dict_remove", bolide_runtime::bolide_dict_remove as *const u8);
        builder.symbol("dict_len", bolide_runtime::bolide_dict_len as *const u8);
        builder.symbol("dict_is_empty", bolide_runtime::bolide_dict_is_empty as *const u8);
        builder.symbol("dict_clear", bolide_runtime::bolide_dict_clear as *const u8);
        builder.symbol("dict_keys", bolide_runtime::bolide_dict_keys as *const u8);
        builder.symbol("dict_values", bolide_runtime::bolide_dict_values as *const u8);
        builder.symbol("dict_iter", bolide_runtime::bolide_dict_iter as *const u8);
        builder.symbol("print_dict", bolide_runtime::bolide_print_dict as *const u8);
        builder.symbol("dynamic_retain", bolide_runtime::bolide_dynamic_retain as *const u8);
        builder.symbol("dynamic_release", bolide_runtime::bolide_dynamic_release as *const u8);
        builder.symbol("print_dynamic", bolide_runtime::bolide_print_dynamic as *const u8);


        let module = JITModule::new(builder);
        let ptr_type = module.target_config().pointer_type();
        let ctx = module.make_context();
        let data_desc = DataDescription::new();

        Self {
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
            loaded_libs: HashMap::new(),
            modules: HashMap::new(),
            lifetime_funcs: HashSet::new(),
        }
    }

    /// 编译程序并返回入口函数指针
    pub fn compile(&mut self, program: &Program) -> Result<*const u8, String> {
        // 预处理 import 语句，加载并合并导入的模块
        let program = self.process_imports(program)?;

        // 注册内置函数
        self.register_builtins()?;

        // 先处理所有 extern 块（必须在函数声明之前）
        for stmt in &program.statements {
            if let Statement::ExternBlock(eb) = stmt {
                self.register_extern_block(eb)?;
            }
        }

        // 收集所有类定义
        self.collect_classes(&program)?;

        // 第一遍：收集所有函数声明（包括类构造函数）
        for stmt in &program.statements {
            if let Statement::FuncDef(func) = stmt {
                self.declare_function(func)?;
                // 记录 async 函数
                if func.is_async {
                    self.async_funcs.insert(func.name.clone());
                }
            }
        }

        // 声明类构造函数
        for class_name in self.classes.keys().cloned().collect::<Vec<_>>() {
            self.declare_class_constructor(&class_name)?;
        }

        // 声明类方法
        self.declare_class_methods(&program)?;

        // 扫描并生成 trampolines（用于带参数的 spawn）
        let spawn_targets = self.collect_spawn_targets(&program);
        self.generate_trampolines(&spawn_targets)?;

        // 编译类构造函数
        for class_name in self.classes.keys().cloned().collect::<Vec<_>>() {
            self.compile_class_constructor(&class_name)?;
        }

        // 编译类方法
        self.compile_class_methods(&program)?;

        // 第二遍：编译所有函数
        let mut toplevel_stmts = Vec::new();
        for stmt in &program.statements {
            match stmt {
                Statement::FuncDef(func) => {
                    self.compile_function(func)?;
                }
                Statement::ClassDef(_) => {
                    // 类定义已经在 collect_classes 中处理
                }
                _ => {
                    toplevel_stmts.push(stmt.clone());
                }
            }
        }

        // 将顶层代码包装成 __main__ 函数
        let main_func = FuncDef {
            name: "__main__".to_string(),
            is_async: false,
            params: vec![],
            return_type: Some(BolideType::Int),
            lifetime_deps: None,
            body: toplevel_stmts,
        };
        self.declare_function(&main_func)?;
        self.compile_function(&main_func)?;

        self.module.finalize_definitions()
            .map_err(|e| format!("Finalize error: {}", e))?;

        // 获取 __main__ 函数
        let func_id = self.functions.get("__main__")
            .ok_or("No __main__ function found")?;
        let main_ptr = self.module.get_finalized_function(*func_id);
        Ok(main_ptr)
    }

    /// 声明函数（第一遍）
    fn declare_function(&mut self, func: &FuncDef) -> Result<(), String> {
        let mut sig = self.module.make_signature();

        // 添加参数类型
        for param in &func.params {
            let ty = self.bolide_type_to_cranelift(&param.ty);
            sig.params.push(AbiParam::new(ty));
        }

        // 添加返回类型
        if let Some(ref ret_ty) = func.return_type {
            sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        let func_id = self.module
            .declare_function(&func.name, Linkage::Export, &sig)
            .map_err(|e| format!("Declare function error: {}", e))?;

        self.functions.insert(func.name.clone(), func_id);
        // 存储函数返回类型
        self.func_return_types.insert(func.name.clone(), func.return_type.clone());
        // 存储函数参数
        self.func_params.insert(func.name.clone(), func.params.clone());
        // 记录生命周期函数
        if func.lifetime_deps.is_some() {
            self.lifetime_funcs.insert(func.name.clone());
        }
        Ok(())
    }

    /// 处理 import 语句，加载并合并导入的模块
    fn process_imports(&mut self, program: &Program) -> Result<Program, String> {
        let mut merged_statements = Vec::new();
        let mut imported_files: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 先处理所有 import 语句
        for stmt in &program.statements {
            if let Statement::Import(import) = stmt {
                if let Some(ref file_path) = import.file_path {
                    // 避免重复导入
                    if imported_files.contains(file_path) {
                        continue;
                    }
                    imported_files.insert(file_path.clone());

                    // 从文件名提取模块名
                    let module_name = Self::extract_module_name(file_path);
                    self.modules.insert(module_name.clone(), file_path.clone());

                    // 加载并解析文件
                    let imported = self.load_module(file_path)?;

                    // 合并导入的定义，添加模块前缀
                    for imp_stmt in imported.statements {
                        match imp_stmt {
                            Statement::FuncDef(mut func) => {
                                // 重命名函数: func -> @module_func
                                func.name = format!("@{}_{}", module_name, func.name);
                                merged_statements.push(Statement::FuncDef(func));
                            }
                            Statement::ClassDef(mut class) => {
                                // 重命名类: Class -> @module_Class
                                class.name = format!("@{}_{}", module_name, class.name);
                                merged_statements.push(Statement::ClassDef(class));
                            }
                            Statement::ExternBlock(ext) => {
                                // 保留 extern 声明（不添加前缀，C函数名必须保持不变）
                                merged_statements.push(Statement::ExternBlock(ext));
                            }
                            _ => {} // 忽略顶层代码
                        }
                    }
                }
            }
        }

        // 添加原程序的所有语句
        for stmt in &program.statements {
            merged_statements.push(stmt.clone());
        }

        Ok(Program { statements: merged_statements })
    }

    /// 从文件路径提取模块名
    fn extract_module_name(file_path: &str) -> String {
        std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module")
            .to_string()
    }

    /// 加载模块文件
    fn load_module(&self, file_path: &str) -> Result<Program, String> {
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to load module '{}': {}", file_path, e))?;

        bolide_parser::parse_source(&content)
            .map_err(|e| format!("Failed to parse module '{}': {}", file_path, e))
    }

    /// 注册内置函数
    fn register_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // print_int(int) -> void
        let mut print_int_sig = self.module.make_signature();
        print_int_sig.params.push(AbiParam::new(types::I64));
        let print_int_id = self.module
            .declare_function("print_int", Linkage::Import, &print_int_sig)
            .map_err(|e| format!("Declare print_int error: {}", e))?;
        self.functions.insert("print_int".to_string(), print_int_id);

        // print_float(float) -> void
        let mut print_float_sig = self.module.make_signature();
        print_float_sig.params.push(AbiParam::new(types::F64));
        let print_float_id = self.module
            .declare_function("print_float", Linkage::Import, &print_float_sig)
            .map_err(|e| format!("Declare print_float error: {}", e))?;
        self.functions.insert("print_float".to_string(), print_float_id);

        // print_bigint(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("print_bigint", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("print_bigint".to_string(), id);

        // print_decimal(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("print_decimal", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("print_decimal".to_string(), id);

        // print_string(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("print_string", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("print_string".to_string(), id);

        // ===== 用户输入函数 =====
        // input() -> ptr
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("input", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("input".to_string(), id);

        // input_prompt(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("input_prompt", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("input_prompt".to_string(), id);

        // ===== 类型转换函数 =====
        // string_from_int(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_from_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_int".to_string(), id);

        // string_from_float(f64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_from_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_float".to_string(), id);

        // string_from_bool(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_from_bool", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_bool".to_string(), id);

        // string_from_bigint(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_from_bigint", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_bigint".to_string(), id);

        // string_from_decimal(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_from_decimal", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_decimal".to_string(), id);

        // string_to_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("string_to_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_to_int".to_string(), id);

        // string_to_float(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self.module.declare_function("string_to_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_to_float".to_string(), id);

        // ===== RC Release 函数 =====
        // string_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_release", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_release".to_string(), id);

        // bigint_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bigint_release", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_release".to_string(), id);

        // decimal_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_release", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_release".to_string(), id);

        // list_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("list_release", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_release".to_string(), id);

        // dynamic_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_release", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_release".to_string(), id);

        // ===== RC Clone 函数 =====
        // string_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_clone", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_clone".to_string(), id);

        // bigint_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bigint_clone", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_clone".to_string(), id);

        // decimal_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_clone", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_clone".to_string(), id);

        // list_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("list_clone", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_clone".to_string(), id);

        // list_new(elem_type: u8) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I8));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("list_new", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_new".to_string(), id);

        // list_push(list: ptr, value: i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_push", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_push".to_string(), id);

        // list_len(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_len", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_len".to_string(), id);

        // list_get(list: ptr, index: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_get", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_get".to_string(), id);

        // list_set(list: ptr, index: i64, value: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_set", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_set".to_string(), id);

        // list_pop(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_pop", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_pop".to_string(), id);

        // list_insert(list: ptr, index: i64, value: i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_insert", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_insert".to_string(), id);

        // list_remove(list: ptr, index: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_remove", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_remove".to_string(), id);

        // list_clear(list: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("list_clear", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_clear".to_string(), id);

        // list_reverse(list: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("list_reverse", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_reverse".to_string(), id);

        // list_extend(list: ptr, other: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("list_extend", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_extend".to_string(), id);

        // list_contains(list: ptr, value: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_contains", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_contains".to_string(), id);

        // list_index_of(list: ptr, value: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_index_of", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_index_of".to_string(), id);

        // list_count(list: ptr, value: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_count", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_count".to_string(), id);

        // list_sort(list: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("list_sort", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_sort".to_string(), id);

        // list_slice(list: ptr, start: i64, end: i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("list_slice", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_slice".to_string(), id);

        // list_is_empty(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_is_empty", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_is_empty".to_string(), id);

        // list_first(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_first", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_first".to_string(), id);

        // list_last(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("list_last", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("list_last".to_string(), id);

        // print_list(list: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("print_list", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("print_list".to_string(), id);

        // ===== Dict 函数 =====
        // dict_new(key_type: u8, value_type: u8) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I8));
        sig.params.push(AbiParam::new(types::I8));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dict_new", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_new".to_string(), id);

        // dict_retain(dict: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dict_retain", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_retain".to_string(), id);

        // dict_release(dict: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dict_release", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_release".to_string(), id);

        // dict_clone(dict: ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dict_clone", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_clone".to_string(), id);

        // dict_set(dict: ptr, key: i64, value: i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("dict_set", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_set".to_string(), id);

        // dict_get(dict: ptr, key: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("dict_get", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_get".to_string(), id);

        // dict_contains(dict: ptr, key: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("dict_contains", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_contains".to_string(), id);

        // dict_remove(dict: ptr, key: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("dict_remove", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_remove".to_string(), id);

        // dict_len(dict: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("dict_len", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_len".to_string(), id);

        // dict_is_empty(dict: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("dict_is_empty", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_is_empty".to_string(), id);

        // dict_clear(dict: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dict_clear", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_clear".to_string(), id);

        // dict_keys(dict: ptr) -> ptr (list)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dict_keys", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_keys".to_string(), id);

        // dict_values(dict: ptr) -> ptr (list)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dict_values", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_values".to_string(), id);

        // dict_iter(dict: ptr) -> ptr (list of keys for iteration)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dict_iter", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dict_iter".to_string(), id);

        // print_dict(dict: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("print_dict", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("print_dict".to_string(), id);

        // dynamic_clone(ptr) -> ptr


        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_clone", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_clone".to_string(), id);

        // ===== BigInt 函数 =====
        // bigint_from_i64(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bigint_from_i64", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_from_i64".to_string(), id);

        // bigint_from_str(ptr, usize) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bigint_from_str", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_from_str".to_string(), id);

        // bigint_add(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bigint_add", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_add".to_string(), id);

        // bigint_sub(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bigint_sub", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_sub".to_string(), id);

        // bigint_mul(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bigint_mul", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_mul".to_string(), id);

        // bigint_div(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bigint_div", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_div".to_string(), id);

        // bigint_eq(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bigint_eq", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_eq".to_string(), id);

        // bigint_lt(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bigint_lt", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_lt".to_string(), id);

        // bigint_le(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bigint_le", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_le".to_string(), id);

        // bigint_to_i64(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bigint_to_i64", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_to_i64".to_string(), id);

        // bigint_debug_stats() -> void
        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("bigint_debug_stats", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bigint_debug_stats".to_string(), id);

        // ===== Decimal 函数 =====
        // decimal_from_i64(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_from_i64", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_from_i64".to_string(), id);

        // decimal_from_f64(f64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_from_f64", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_from_f64".to_string(), id);

        // decimal_from_str(ptr, usize) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_from_str", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_from_str".to_string(), id);

        // decimal_add(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_add", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_add".to_string(), id);

        // decimal_sub(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_sub", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_sub".to_string(), id);

        // decimal_mul(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_mul", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_mul".to_string(), id);

        // decimal_div(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("decimal_div", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_div".to_string(), id);

        // decimal_eq(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("decimal_eq", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_eq".to_string(), id);

        // decimal_lt(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("decimal_lt", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_lt".to_string(), id);

        // decimal_to_i64(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("decimal_to_i64", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_to_i64".to_string(), id);

        // decimal_to_f64(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self.module.declare_function("decimal_to_f64", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("decimal_to_f64".to_string(), id);

        // ===== Dynamic 函数 =====
        // dynamic_from_int(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_from_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_from_int".to_string(), id);

        // dynamic_from_float(f64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_from_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_from_float".to_string(), id);

        // dynamic_from_string(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_from_string", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_from_string".to_string(), id);

        // dynamic_from_list(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_from_list", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_from_list".to_string(), id);

        // dynamic_add(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_add", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_add".to_string(), id);

        // dynamic_sub(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_sub", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_sub".to_string(), id);

        // dynamic_mul(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_mul", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_mul".to_string(), id);

        // dynamic_div(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("dynamic_div", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_div".to_string(), id);

        // print_dynamic(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("print_dynamic", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("print_dynamic".to_string(), id);

        // dynamic_to_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("dynamic_to_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("dynamic_to_int".to_string(), id);

        // ===== 字符串函数 =====
        // string_from_slice(ptr, i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));      // 字符串数据指针
        sig.params.push(AbiParam::new(types::I64)); // 长度
        sig.returns.push(AbiParam::new(ptr));     // BolideString 指针
        let id = self.module.declare_function("string_from_slice", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_from_slice".to_string(), id);

        // string_literal(ptr, i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_literal", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_literal".to_string(), id);

        // string_as_cstr(ptr) -> ptr  (BolideString* -> char*)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_as_cstr", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_as_cstr".to_string(), id);

        // string_concat(ptr, ptr) -> ptr  (字符串拼接)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("string_concat", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_concat".to_string(), id);

        // string_eq(ptr, ptr) -> i64  (字符串比较)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("string_eq", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("string_eq".to_string(), id);

        // ===== 内存分配函数 =====
        // bolide_alloc(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("bolide_alloc", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bolide_alloc".to_string(), id);

        // bolide_free(ptr, i64)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("bolide_free", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("bolide_free".to_string(), id);

        // ===== 线程函数（trampoline 方案） =====
        // thread_spawn_int(fn() -> i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));  // 函数指针
        sig.returns.push(AbiParam::new(ptr)); // 线程句柄
        let id = self.module.declare_function("thread_spawn_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_spawn_int".to_string(), id);

        // thread_spawn_float(fn() -> f64) -> ptr
        let id = self.module.declare_function("thread_spawn_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_spawn_float".to_string(), id);

        // thread_spawn_ptr(fn() -> ptr) -> ptr
        let id = self.module.declare_function("thread_spawn_ptr", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_spawn_ptr".to_string(), id);

        // ===== 带环境的线程函数 =====
        // thread_spawn_int_with_env(fn(ptr) -> i64, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));  // 函数指针
        sig.params.push(AbiParam::new(ptr));  // 环境指针
        sig.returns.push(AbiParam::new(ptr)); // 线程句柄
        let id = self.module.declare_function("thread_spawn_int_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_spawn_int_with_env".to_string(), id);

        // thread_spawn_float_with_env
        let id = self.module.declare_function("thread_spawn_float_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_spawn_float_with_env".to_string(), id);

        // thread_spawn_ptr_with_env
        let id = self.module.declare_function("thread_spawn_ptr_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_spawn_ptr_with_env".to_string(), id);

        // thread_join_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("thread_join_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_join_int".to_string(), id);

        // thread_join_float(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self.module.declare_function("thread_join_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_join_float".to_string(), id);

        // thread_join_ptr(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("thread_join_ptr", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_join_ptr".to_string(), id);

        // thread_handle_free(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("thread_handle_free", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_handle_free".to_string(), id);

        // thread_cancel(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("thread_cancel", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_cancel".to_string(), id);

        // thread_is_cancelled(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("thread_is_cancelled", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("thread_is_cancelled".to_string(), id);

        // ===== 线程池函数 =====
        // pool_create(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("pool_create", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_create".to_string(), id);

        // pool_enter(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("pool_enter", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_enter".to_string(), id);

        // pool_exit()
        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("pool_exit", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_exit".to_string(), id);

        // pool_is_active() -> i64
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("pool_is_active", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_is_active".to_string(), id);

        // pool_spawn_int(fn() -> i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));  // 函数指针
        sig.returns.push(AbiParam::new(ptr)); // 任务句柄
        let id = self.module.declare_function("pool_spawn_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_spawn_int".to_string(), id);

        // pool_spawn_float
        let id = self.module.declare_function("pool_spawn_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_spawn_float".to_string(), id);

        // pool_spawn_ptr
        let id = self.module.declare_function("pool_spawn_ptr", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_spawn_ptr".to_string(), id);

        // ===== 带环境的线程池函数 =====
        // pool_spawn_int_with_env(fn(ptr) -> i64, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));  // 函数指针
        sig.params.push(AbiParam::new(ptr));  // 环境指针
        sig.returns.push(AbiParam::new(ptr)); // 任务句柄
        let id = self.module.declare_function("pool_spawn_int_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_spawn_int_with_env".to_string(), id);

        // pool_spawn_float_with_env
        let id = self.module.declare_function("pool_spawn_float_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_spawn_float_with_env".to_string(), id);

        // pool_spawn_ptr_with_env
        let id = self.module.declare_function("pool_spawn_ptr_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_spawn_ptr_with_env".to_string(), id);

        // pool_join_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("pool_join_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_join_int".to_string(), id);

        // pool_join_float(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self.module.declare_function("pool_join_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_join_float".to_string(), id);

        // pool_join_ptr(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("pool_join_ptr", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_join_ptr".to_string(), id);

        // pool_handle_free(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("pool_handle_free", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_handle_free".to_string(), id);

        // pool_destroy(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("pool_destroy", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("pool_destroy".to_string(), id);

        // ===== 通道函数 =====
        // channel_create() -> ptr
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("channel_create", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_create".to_string(), id);

        // channel_create_buffered(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("channel_create_buffered", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_create_buffered".to_string(), id);

        // channel_send(ptr, i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("channel_send", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_send".to_string(), id);

        // channel_recv(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("channel_recv", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_recv".to_string(), id);

        // channel_close(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("channel_close", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_close".to_string(), id);

        // channel_free(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("channel_free", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_free".to_string(), id);

        // channel_select(channels_ptr, count, timeout_ms, value_ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));       // channels array pointer
        sig.params.push(AbiParam::new(types::I64)); // count
        sig.params.push(AbiParam::new(types::I64)); // timeout_ms
        sig.params.push(AbiParam::new(ptr));       // value output pointer
        sig.returns.push(AbiParam::new(types::I64)); // selected index
        let id = self.module.declare_function("channel_select", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("channel_select".to_string(), id);

        // ===== 协程函数 =====
        // coroutine_spawn_int(func_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("coroutine_spawn_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_spawn_int".to_string(), id);

        // coroutine_spawn_float(func_ptr) -> ptr
        let id = self.module.declare_function("coroutine_spawn_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_spawn_float".to_string(), id);

        // coroutine_spawn_ptr(func_ptr) -> ptr
        let id = self.module.declare_function("coroutine_spawn_ptr", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_spawn_ptr".to_string(), id);

        // coroutine_spawn_*_with_env(func_ptr, env) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("coroutine_spawn_int_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_spawn_int_with_env".to_string(), id);
        let id = self.module.declare_function("coroutine_spawn_float_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_spawn_float_with_env".to_string(), id);
        let id = self.module.declare_function("coroutine_spawn_ptr_with_env", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_spawn_ptr_with_env".to_string(), id);

        // scope_enter(), scope_register(ptr), scope_exit()
        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("scope_enter", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("scope_enter".to_string(), id);

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("scope_register", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("scope_register".to_string(), id);

        let mut sig = self.module.make_signature();
        let id = self.module.declare_function("scope_exit", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("scope_exit".to_string(), id);

        // select_wait_first(futures_ptr, count) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("select_wait_first", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("select_wait_first".to_string(), id);

        // coroutine_await_int(future_ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("coroutine_await_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_await_int".to_string(), id);

        // coroutine_await_float(future_ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self.module.declare_function("coroutine_await_float", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_await_float".to_string(), id);

        // coroutine_await_ptr(future_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("coroutine_await_ptr", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_await_ptr".to_string(), id);

        // coroutine_cancel(future_ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("coroutine_cancel", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_cancel".to_string(), id);

        // coroutine_free(future_ptr)
        let id = self.module.declare_function("coroutine_free", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("coroutine_free".to_string(), id);

        // ===== Tuple 函数 =====
        // tuple_new(len) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("tuple_new", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_new".to_string(), id);

        // tuple_free(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("tuple_free", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_free".to_string(), id);

        // tuple_set(ptr, index, value)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("tuple_set", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_set".to_string(), id);

        // tuple_get(ptr, index) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("tuple_get", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_get".to_string(), id);

        // tuple_len(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("tuple_len", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("tuple_len".to_string(), id);

        // print_tuple(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("print_tuple", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("print_tuple".to_string(), id);

        // ===== FFI 函数 =====
        // ffi_load_library(path_ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("ffi_load_library", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("ffi_load_library".to_string(), id);

        // ffi_get_symbol(lib_path_ptr, symbol_name_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("ffi_get_symbol", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("ffi_get_symbol".to_string(), id);

        // test_callback(callback, a, b) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));  // callback
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("test_callback", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("test_callback".to_string(), id);

        // map_int(callback, value) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));  // callback
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self.module.declare_function("map_int", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("map_int".to_string(), id);

        // ===== Object 函数 =====
        // object_alloc(size) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("object_alloc", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("object_alloc".to_string(), id);

        // object_release(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("object_release", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("object_release".to_string(), id);

        // object_retain(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self.module.declare_function("object_retain", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("object_retain".to_string(), id);

        // object_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self.module.declare_function("object_clone", Linkage::Import, &sig).map_err(|e| format!("{}", e))?;
        self.functions.insert("object_clone".to_string(), id);

        Ok(())
    }

    /// 编译函数（第二遍）
    fn compile_function(&mut self, func: &FuncDef) -> Result<(), String> {
        let func_id = *self.functions.get(&func.name)
            .ok_or_else(|| format!("Function {} not declared", func.name))?;

        // 预先计算参数类型
        let param_types: Vec<types::Type> = func.params.iter()
            .map(|p| self.bolide_type_to_cranelift(&p.ty))
            .collect();

        // 重建签名
        let mut sig = self.module.make_signature();
        for ty in &param_types {
            sig.params.push(AbiParam::new(*ty));
        }
        if let Some(ref ret_ty) = func.return_type {
            sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        self.ctx.func.signature = sig;
        self.ctx.func.name = cranelift_codegen::ir::UserFuncName::user(0, func_id.as_u32());

        // 创建函数构建器
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut builder_ctx);

        // 创建入口块
        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // 预先收集函数引用
        let mut func_refs = HashMap::new();
        for (name, id) in &self.functions {
            let func_ref = self.module.declare_func_in_func(*id, builder.func);
            func_refs.insert(name.clone(), func_ref);
        }

        // 收集 trampoline 引用
        let mut trampoline_refs = HashMap::new();
        let mut trampoline_param_types = HashMap::new();
        let mut trampoline_env_sizes = HashMap::new();
        for (target_func, info) in &self.trampolines {
            let func_ref = self.module.declare_func_in_func(info.func_id, builder.func);
            trampoline_refs.insert(target_func.clone(), func_ref);
            trampoline_param_types.insert(target_func.clone(), info.param_types.clone());
            trampoline_env_sizes.insert(target_func.clone(), info.env_size);
        }

        let ptr_type = self.ptr_type;
        let func_return_types = self.func_return_types.clone();
        let func_params = self.func_params.clone();
        let classes = self.classes.clone();
        let async_funcs = self.async_funcs.clone();
        let extern_funcs = self.extern_funcs.clone();
        let modules = self.modules.clone();

        let lifetime_funcs = self.lifetime_funcs.clone();

        // 创建编译上下文
        let mut compile_ctx = CompileContext::new(
            &mut builder,
            func_refs,
            func_return_types,
            func_params,
            trampoline_refs,
            trampoline_param_types,
            trampoline_env_sizes,
            ptr_type,
            classes,
            async_funcs,
            extern_funcs,
            modules,
            func.lifetime_deps.clone(),
            func.name.clone(),
            lifetime_funcs,
        );

        // 绑定参数到变量
        let params = compile_ctx.builder.block_params(entry_block).to_vec();

        for (i, param) in func.params.iter().enumerate() {
            // 记录参数的 Bolide 类型
            compile_ctx.var_types.insert(param.name.clone(), param.ty.clone());

            match param.mode {
                ParamMode::Borrow => {
                    // 借用：直接使用参数值，不负责释放
                    let var = compile_ctx.declare_variable(&param.name, param_types[i]);
                    compile_ctx.builder.def_var(var, params[i]);
                }
                ParamMode::Owned => {
                    // 所有权转移：直接使用参数值，负责释放
                    let var = compile_ctx.declare_variable(&param.name, param_types[i]);
                    compile_ctx.builder.def_var(var, params[i]);
                    // 对于需要 RC 管理的类型，注册到 rc_variables
                    if CompileContext::is_rc_type(&param.ty) {
                        compile_ctx.rc_variables.push((param.name.clone(), param.ty.clone()));
                    }
                }
                ParamMode::Ref => {
                    // Ref 参数：参数是指针地址，需要解引用
                    let ptr_addr = params[i];
                    let val = compile_ctx.builder.ins().load(ptr_type, MemFlags::new(), ptr_addr, 0);
                    let var = compile_ctx.declare_variable(&param.name, param_types[i]);
                    compile_ctx.builder.def_var(var, val);
                    // 记录 Ref 参数，以便在函数返回前写回
                    compile_ctx.ref_params.push((param.name.clone(), var, ptr_addr));
                }
            }
        }

        // 编译函数体
        let mut terminated = false;
        for stmt in &func.body {
            if terminated {
                break;
            }
            terminated = compile_ctx.compile_stmt(stmt)?;
        }

        // 如果没有显式 return，返回默认值或空
        if !terminated {
            // 生命周期模式下跳过 RC 清理
            if !compile_ctx.uses_lifetime_mode() {
                // 在隐式返回之前释放所有 RC 变量
                compile_ctx.emit_rc_cleanup();
            }

            // 写回 Ref 参数
            compile_ctx.write_back_ref_params();

            if let Some(ref ret_ty) = func.return_type {
                let zero = match ret_ty {
                    BolideType::Float => compile_ctx.builder.ins().f64const(0.0),
                    _ => compile_ctx.builder.ins().iconst(types::I64, 0),
                };
                compile_ctx.builder.ins().return_(&[zero]);
            } else {
                compile_ctx.builder.ins().return_(&[]);
            }
        }

        builder.finalize();

        // 定义函数
        self.module.define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Define function error: {}", e))?;
        self.module.clear_context(&mut self.ctx);

        Ok(())
    }

    /// 收集需要 trampoline 的 spawn 目标函数
    fn collect_spawn_targets(&self, program: &Program) -> Vec<String> {
        let mut targets = Vec::new();
        for stmt in &program.statements {
            self.collect_spawn_targets_in_stmt(stmt, &mut targets);
        }
        targets.sort();
        targets.dedup();
        targets
    }

    fn collect_spawn_targets_in_stmt(&self, stmt: &Statement, targets: &mut Vec<String>) {
        match stmt {
            Statement::FuncDef(func) => {
                for s in &func.body {
                    self.collect_spawn_targets_in_stmt(s, targets);
                }
            }
            Statement::If(if_stmt) => {
                self.collect_spawn_targets_in_expr(&if_stmt.condition, targets);
                for s in &if_stmt.then_body {
                    self.collect_spawn_targets_in_stmt(s, targets);
                }
                for (cond, body) in &if_stmt.elif_branches {
                    self.collect_spawn_targets_in_expr(cond, targets);
                    for s in body {
                        self.collect_spawn_targets_in_stmt(s, targets);
                    }
                }
                if let Some(ref else_body) = if_stmt.else_body {
                    for s in else_body {
                        self.collect_spawn_targets_in_stmt(s, targets);
                    }
                }
            }
            Statement::While(while_stmt) => {
                self.collect_spawn_targets_in_expr(&while_stmt.condition, targets);
                for s in &while_stmt.body {
                    self.collect_spawn_targets_in_stmt(s, targets);
                }
            }
            Statement::Pool(pool_stmt) => {
                self.collect_spawn_targets_in_expr(&pool_stmt.size, targets);
                for s in &pool_stmt.body {
                    self.collect_spawn_targets_in_stmt(s, targets);
                }
            }
            Statement::VarDecl(decl) => {
                if let Some(ref expr) = decl.value {
                    self.collect_spawn_targets_in_expr(expr, targets);
                }
            }
            Statement::Assign(assign) => {
                self.collect_spawn_targets_in_expr(&assign.value, targets);
            }
            Statement::Expr(expr) => {
                self.collect_spawn_targets_in_expr(expr, targets);
            }
            Statement::Return(Some(expr)) => {
                self.collect_spawn_targets_in_expr(expr, targets);
            }
            Statement::Send(send) => {
                self.collect_spawn_targets_in_expr(&send.value, targets);
            }
            Statement::AwaitScope(scope_stmt) => {
                for s in &scope_stmt.body {
                    self.collect_spawn_targets_in_stmt(s, targets);
                }
            }
            Statement::AsyncSelect(select_stmt) => {
                for branch in &select_stmt.branches {
                    let (expr, body) = match branch {
                        bolide_parser::AsyncSelectBranch::Bind { expr, body, .. } => (expr, body),
                        bolide_parser::AsyncSelectBranch::Expr { expr, body } => (expr, body),
                    };
                    self.collect_spawn_targets_in_expr(expr, targets);
                    for s in body {
                        self.collect_spawn_targets_in_stmt(s, targets);
                    }
                }
            }
            _ => {}
        }
    }

    fn collect_spawn_targets_in_expr(&self, expr: &Expr, targets: &mut Vec<String>) {
        match expr {
            Expr::Spawn(func_name, args) => {
                // 只有带参数的 spawn 需要 trampoline
                if !args.is_empty() {
                    // 检查目标函数存在且有参数
                    if self.func_params.get(func_name).map(|p| !p.is_empty()).unwrap_or(false) {
                        targets.push(func_name.clone());
                    }
                }
                for arg in args {
                    self.collect_spawn_targets_in_expr(arg, targets);
                }
            }
            Expr::BinOp(left, _, right) => {
                self.collect_spawn_targets_in_expr(left, targets);
                self.collect_spawn_targets_in_expr(right, targets);
            }
            Expr::UnaryOp(_, operand) => {
                self.collect_spawn_targets_in_expr(operand, targets);
            }
            Expr::Call(callee, args) => {
                // 检查是否是 async 函数调用
                if let Expr::Ident(func_name) = callee.as_ref() {
                    if self.async_funcs.contains(func_name) && !args.is_empty() {
                        if self.func_params.get(func_name).map(|p| !p.is_empty()).unwrap_or(false) {
                            targets.push(func_name.clone());
                        }
                    }
                }
                self.collect_spawn_targets_in_expr(callee, targets);
                for arg in args {
                    self.collect_spawn_targets_in_expr(arg, targets);
                }
            }
            Expr::Index(base, idx) => {
                self.collect_spawn_targets_in_expr(base, targets);
                self.collect_spawn_targets_in_expr(idx, targets);
            }
            Expr::Member(base, _) => {
                self.collect_spawn_targets_in_expr(base, targets);
            }
            Expr::List(items) => {
                for item in items {
                    self.collect_spawn_targets_in_expr(item, targets);
                }
            }
            _ => {}
        }
    }

    /// 为目标函数生成 trampoline
    fn generate_trampolines(&mut self, targets: &[String]) -> Result<(), String> {
        for func_name in targets {
            self.create_trampoline(func_name)?;
        }
        Ok(())
    }

    /// 创建单个 trampoline 函数
    fn create_trampoline(&mut self, target_func_name: &str) -> Result<(), String> {
        let params = self.func_params.get(target_func_name)
            .ok_or_else(|| format!("Function {} not found", target_func_name))?
            .clone();
        let return_type = self.func_return_types.get(target_func_name)
            .ok_or_else(|| format!("Function {} return type not found", target_func_name))?
            .clone();

        // 计算 env 大小（每个参数 8 字节对齐）
        let env_size = (params.len() * 8) as i64;
        let param_types: Vec<BolideType> = params.iter().map(|p| p.ty.clone()).collect();

        // 生成 trampoline 名称
        let trampoline_name = format!("__trampoline_{}_{}", target_func_name, self.trampoline_counter);
        self.trampoline_counter += 1;

        // 声明 trampoline 签名: (env: ptr) -> return_type
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(self.ptr_type));
        if let Some(ref ret_ty) = return_type {
            sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        let trampoline_id = self.module
            .declare_function(&trampoline_name, Linkage::Local, &sig)
            .map_err(|e| format!("Declare trampoline error: {}", e))?;

        // 预先计算参数的 Cranelift 类型（避免借用冲突）
        let cranelift_param_types: Vec<types::Type> = params.iter()
            .map(|p| self.bolide_type_to_cranelift(&p.ty))
            .collect();

        // 获取目标函数 ID
        let target_func_id = *self.functions.get(target_func_name)
            .ok_or_else(|| format!("Target function {} not declared", target_func_name))?;

        // 构建 trampoline 函数体
        self.ctx.func.signature = sig;
        self.ctx.func.name = cranelift_codegen::ir::UserFuncName::user(0, trampoline_id.as_u32());

        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut builder_ctx);

        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // 获取 env 指针参数
        let env_ptr = builder.block_params(entry_block)[0];

        // 获取目标函数引用
        let target_func_ref = self.module.declare_func_in_func(target_func_id, builder.func);

        // 从 env 中加载参数
        let mut call_args = Vec::new();
        for (i, cranelift_type) in cranelift_param_types.iter().enumerate() {
            let offset = (i * 8) as i32;
            let val = builder.ins().load(*cranelift_type, MemFlags::trusted(), env_ptr, offset);
            call_args.push(val);
        }

        // 调用目标函数
        let call = builder.ins().call(target_func_ref, &call_args);

        // 返回结果（先复制结果值以避免借用冲突）
        let result_val = {
            let results = builder.inst_results(call);
            if results.is_empty() { None } else { Some(results[0]) }
        };

        // 释放 RC 类型参数（spawn 时 clone 的副本）
        for (i, param) in params.iter().enumerate() {
            let release_func = match &param.ty {
                BolideType::Str => Some("string_release"),
                BolideType::BigInt => Some("bigint_release"),
                BolideType::Decimal => Some("decimal_release"),
                BolideType::List(_) => Some("list_release"),
                BolideType::Dynamic => Some("dynamic_release"),
                _ => None,
            };
            if let Some(release_name) = release_func {
                if let Some(&release_id) = self.functions.get(release_name) {
                    let release_ref = self.module.declare_func_in_func(release_id, builder.func);
                    builder.ins().call(release_ref, &[call_args[i]]);
                }
            }
        }

        if let Some(val) = result_val {
            builder.ins().return_(&[val]);
        } else {
            builder.ins().return_(&[]);
        }

        builder.finalize();

        // 定义 trampoline 函数
        self.module.define_function(trampoline_id, &mut self.ctx)
            .map_err(|e| format!("Define trampoline error: {}", e))?;
        self.module.clear_context(&mut self.ctx);

        // 存储 trampoline 信息
        self.trampolines.insert(target_func_name.to_string(), TrampolineInfo {
            func_id: trampoline_id,
            param_types,
            env_size,
        });

        self.functions.insert(trampoline_name, trampoline_id);

        Ok(())
    }

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
            BolideType::Func => self.ptr_type,  // 函数指针
            BolideType::FuncSig(_, _) => self.ptr_type,  // 带签名的函数指针
            BolideType::List(_) => self.ptr_type,
            BolideType::Dict(_, _) => self.ptr_type,  // 字典作为指针
            BolideType::Tuple(_) => self.ptr_type,  // 元组作为指针

            BolideType::Custom(_) => self.ptr_type,
            BolideType::Weak(inner) => self.bolide_type_to_cranelift(inner),
            BolideType::Unowned(inner) => self.bolide_type_to_cranelift(inner),
        }
    }

    /// 收集所有类定义（按继承顺序处理）
    fn collect_classes(&mut self, program: &Program) -> Result<(), String> {
        // 先收集所有类定义
        let mut class_defs: HashMap<String, &ClassDef> = HashMap::new();
        for stmt in &program.statements {
            if let Statement::ClassDef(class_def) = stmt {
                class_defs.insert(class_def.name.clone(), class_def);
            }
        }

        // 按继承顺序处理（父类先于子类）
        let mut processed: HashSet<String> = HashSet::new();
        let names: Vec<String> = class_defs.keys().cloned().collect();

        for name in &names {
            self.process_class_with_deps(&class_defs, &mut processed, name)?;
        }
        Ok(())
    }

    /// 递归处理类及其依赖（父类）
    fn process_class_with_deps(
        &mut self,
        class_defs: &HashMap<String, &ClassDef>,
        processed: &mut HashSet<String>,
        name: &str,
    ) -> Result<(), String> {
        if processed.contains(name) {
            return Ok(());
        }

        let class_def = class_defs.get(name)
            .ok_or_else(|| format!("Class not found: {}", name))?;

        // 先处理父类
        if let Some(ref parent) = class_def.parent {
            self.process_class_with_deps(class_defs, processed, parent)?;
        }

        // 构建并存储类信息
        let class_info = self.build_class_info(class_def)?;
        self.classes.insert(name.to_string(), class_info);
        processed.insert(name.to_string());
        Ok(())
    }

    /// 构建类信息（支持继承）
    fn build_class_info(&self, class_def: &ClassDef) -> Result<ClassInfo, String> {
        let mut fields = Vec::new();
        let mut offset = 0usize;

        // 如果有父类，先继承父类的字段
        if let Some(ref parent_name) = class_def.parent {
            if let Some(parent_info) = self.classes.get(parent_name) {
                for field in &parent_info.fields {
                    fields.push(field.clone());
                }
                offset = parent_info.size;
            } else {
                return Err(format!("Parent class '{}' not found", parent_name));
            }
        }

        // 添加子类自己的字段
        for field in &class_def.fields {
            fields.push(FieldInfo {
                name: field.name.clone(),
                ty: field.ty.clone(),
                offset,
            });
            offset += 8;
        }

        let methods: Vec<String> = class_def.methods.iter()
            .map(|m| m.name.clone())
            .collect();

        Ok(ClassInfo {
            name: class_def.name.clone(),
            parent: class_def.parent.clone(),
            fields,
            methods,
            size: offset,
        })
    }

    /// 声明类构造函数
    fn declare_class_constructor(&mut self, class_name: &str) -> Result<(), String> {
        let class_info = self.classes.get(class_name)
            .ok_or_else(|| format!("Class not found: {}", class_name))?
            .clone();

        // 构造函数签名: ClassName(field1, field2, ...) -> ptr
        let mut sig = self.module.make_signature();
        
        // 添加字段参数（按字段声明顺序）
        for field in &class_info.fields {
            let ty = self.bolide_type_to_cranelift(&field.ty);
            sig.params.push(AbiParam::new(ty));
        }
        sig.returns.push(AbiParam::new(self.ptr_type));

        let func_name = class_name.to_string();
        let func_id = self.module
            .declare_function(&func_name, Linkage::Export, &sig)
            .map_err(|e| format!("Declare constructor error: {}", e))?;

        self.functions.insert(func_name.clone(), func_id);
        self.func_return_types.insert(func_name.clone(), Some(BolideType::Custom(class_name.to_string())));
        
        // 存储构造函数参数信息
        let params: Vec<Param> = class_info.fields.iter()
            .map(|f| Param {
                name: f.name.clone(),
                ty: f.ty.clone(),
                mode: ParamMode::Borrow,
            })
            .collect();
        self.func_params.insert(func_name, params);

        Ok(())
    }

    /// 编译类构造函数
    fn compile_class_constructor(&mut self, class_name: &str) -> Result<(), String> {
        let class_info = self.classes.get(class_name)
            .ok_or_else(|| format!("Class not found: {}", class_name))?
            .clone();

        let func_id = *self.functions.get(class_name)
            .ok_or_else(|| format!("Constructor not declared: {}", class_name))?;

        // 创建函数签名（与 declare 一致）
        let mut sig = self.module.make_signature();
        for field in &class_info.fields {
            let ty = self.bolide_type_to_cranelift(&field.ty);
            sig.params.push(AbiParam::new(ty));
        }
        sig.returns.push(AbiParam::new(self.ptr_type));

        self.ctx.func.signature = sig;
        self.ctx.func.name = cranelift_codegen::ir::UserFuncName::user(0, func_id.as_u32());

        // 创建 FunctionBuilder
        let mut func_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut func_ctx);

        // 创建入口块
        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // 获取传入的参数
        let params: Vec<Value> = builder.block_params(entry_block).to_vec();

        // 导入 object_alloc 函数
        let object_alloc_id = *self.functions.get("object_alloc")
            .ok_or("object_alloc not found")?;
        let object_alloc_ref = self.module.declare_func_in_func(object_alloc_id, builder.func);

        // 调用 object_alloc(size) 分配内存
        let size_val = builder.ins().iconst(types::I64, class_info.size as i64);
        let call = builder.ins().call(object_alloc_ref, &[size_val]);
        let obj_ptr = builder.inst_results(call)[0];

        // 使用传入的参数初始化字段
        for (i, field) in class_info.fields.iter().enumerate() {
            let field_ptr = builder.ins().iadd_imm(obj_ptr, field.offset as i64);
            // 使用传入的参数值，如果没有则使用零值
            let val = if i < params.len() {
                params[i]
            } else {
                builder.ins().iconst(types::I64, 0)
            };
            builder.ins().store(MemFlags::new(), val, field_ptr, 0);
        }

        // 返回对象指针
        builder.ins().return_(&[obj_ptr]);
        builder.finalize();

        // 编译函数
        self.module.define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Define constructor error: {}", e))?;
        self.module.clear_context(&mut self.ctx);

        Ok(())
    }

    /// 声明类方法
    fn declare_class_methods(&mut self, program: &Program) -> Result<(), String> {
        for stmt in &program.statements {
            if let Statement::ClassDef(class_def) = stmt {
                for method in &class_def.methods {
                    // 方法名格式: ClassName_methodName
                    let method_name = format!("{}_{}", class_def.name, method.name);

                    let mut sig = self.module.make_signature();
                    // 第一个参数是 self (对象指针)
                    sig.params.push(AbiParam::new(self.ptr_type));
                    // 其他参数
                    for param in &method.params {
                        let ty = self.bolide_type_to_cranelift(&param.ty);
                        sig.params.push(AbiParam::new(ty));
                    }
                    // 返回类型
                    if let Some(ref ret_ty) = method.return_type {
                        sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
                    }

                    let func_id = self.module
                        .declare_function(&method_name, Linkage::Export, &sig)
                        .map_err(|e| format!("Declare method error: {}", e))?;

                    self.functions.insert(method_name.clone(), func_id);
                    self.func_return_types.insert(method_name.clone(), method.return_type.clone());

                    // 存储方法参数（包含隐式 self）
                    let mut params_with_self = vec![Param {
                        name: "self".to_string(),
                        ty: BolideType::Custom(class_def.name.clone()),
                        mode: ParamMode::Borrow,
                    }];
                    params_with_self.extend(method.params.clone());
                    self.func_params.insert(method_name, params_with_self);
                }
            }
        }
        Ok(())
    }

    /// 编译类方法
    fn compile_class_methods(&mut self, program: &Program) -> Result<(), String> {
        for stmt in &program.statements {
            if let Statement::ClassDef(class_def) = stmt {
                for method in &class_def.methods {
                    let method_name = format!("{}_{}", class_def.name, method.name);

                    // 创建带 self 参数的方法定义
                    let mut method_with_self = method.clone();
                    method_with_self.name = method_name;
                    method_with_self.params.insert(0, Param {
                        name: "self".to_string(),
                        ty: BolideType::Custom(class_def.name.clone()),
                        mode: ParamMode::Borrow,
                    });

                    self.compile_function(&method_with_self)?;
                }
            }
        }
        Ok(())
    }

    /// 注册 extern 块中的函数声明（JitCompiler 级别）
    fn register_extern_block(&mut self, eb: &ExternBlock) -> Result<(), String> {
        let lib_path = &eb.lib_path;
        for decl in &eb.declarations {
            if let bolide_parser::ExternDecl::Function(func) = decl {
                self.extern_funcs.insert(
                    func.name.clone(),
                    (lib_path.clone(), func.clone())
                );
            }
        }
        Ok(())
    }
}

impl Default for JitCompiler {
    fn default() -> Self {
        Self::new()
    }
}

/// 编译上下文，用于在编译过程中跟踪变量等状态
struct CompileContext<'a, 'b> {
    builder: &'a mut FunctionBuilder<'b>,
    func_refs: HashMap<String, FuncRef>,
    variables: HashMap<String, Variable>,
    /// 变量的 Bolide 类型（用于类型推断）
    var_types: HashMap<String, BolideType>,
    /// 函数返回类型（用于 spawn/join 类型处理）
    func_return_types: HashMap<String, Option<BolideType>>,
    /// 函数参数信息（用于参数模式处理）
    func_params: HashMap<String, Vec<Param>>,
    /// spawn 变量对应的函数名（用于 join 时获取返回类型）
    spawn_func_map: HashMap<String, String>,
    /// trampoline 函数引用
    trampoline_refs: HashMap<String, FuncRef>,
    /// trampoline 参数类型
    trampoline_param_types: HashMap<String, Vec<BolideType>>,
    /// trampoline env 大小
    trampoline_env_sizes: HashMap<String, i64>,
    /// 需要在作用域结束时释放的 RC 变量（变量名 -> 类型）
    rc_variables: Vec<(String, BolideType)>,
    /// 当前语句中产生的临时 RC 值（值 -> 类型）
    temp_rc_values: Vec<(Value, BolideType)>,
    /// 已移动的变量（Owned 传递后）
    moved_variables: HashSet<String>,
    /// Ref 参数信息（变量名, 变量, 指针地址）- 函数返回前需要写回
    ref_params: Vec<(String, Variable, Value)>,
    /// Ref 参数已被重新赋值（首次赋值后加入此集合）
    ref_params_reassigned: HashSet<String>,
    var_counter: usize,
    ptr_type: types::Type,
    /// 类信息
    classes: HashMap<String, ClassInfo>,
    /// async 函数集合
    async_funcs: HashSet<String>,
    /// extern 函数信息
    extern_funcs: HashMap<String, (String, bolide_parser::ExternFunc)>,
    /// 模块名映射
    modules: HashMap<String, String>,
    /// 生命周期依赖参数（from x, y 中的参数名）
    /// 当指定时，跳过 ARC 并执行生命周期检查
    lifetime_deps: Option<Vec<String>>,
    /// 当前函数名（用于错误信息）
    current_func_name: String,
    /// 使用生命周期模式的函数集合（返回借用而非拥有的值）
    lifetime_funcs: HashSet<String>,
    /// 变量来源追踪：变量名 -> 来源参数名（用于生命周期检查）
    var_lifetime_source: HashMap<String, String>,
    /// 当前作用域深度（用于调用者端生命周期检查）
    scope_depth: usize,
    /// 变量的作用域深度：变量名 -> 声明时的作用域深度
    var_scope_depth: HashMap<String, usize>,
    /// 借用变量追踪：变量名 -> (来源变量名, 来源作用域深度)
    borrowed_vars: HashMap<String, (String, usize)>,
    /// weak 引用变量集合（访问时需要检查是否为 nil）
    weak_variables: HashSet<String>,
}

impl<'a, 'b> CompileContext<'a, 'b> {
    fn new(
        builder: &'a mut FunctionBuilder<'b>,
        func_refs: HashMap<String, FuncRef>,
        func_return_types: HashMap<String, Option<BolideType>>,
        func_params: HashMap<String, Vec<Param>>,
        trampoline_refs: HashMap<String, FuncRef>,
        trampoline_param_types: HashMap<String, Vec<BolideType>>,
        trampoline_env_sizes: HashMap<String, i64>,
        ptr_type: types::Type,
        classes: HashMap<String, ClassInfo>,
        async_funcs: HashSet<String>,
        extern_funcs: HashMap<String, (String, bolide_parser::ExternFunc)>,
        modules: HashMap<String, String>,
        lifetime_deps: Option<Vec<String>>,
        current_func_name: String,
        lifetime_funcs: HashSet<String>,
    ) -> Self {
        Self {
            builder,
            func_refs,
            variables: HashMap::new(),
            var_types: HashMap::new(),
            func_return_types,
            func_params,
            spawn_func_map: HashMap::new(),
            trampoline_refs,
            trampoline_param_types,
            trampoline_env_sizes,
            rc_variables: Vec::new(),
            temp_rc_values: Vec::new(),
            moved_variables: HashSet::new(),
            ref_params: Vec::new(),
            ref_params_reassigned: HashSet::new(),
            var_counter: 0,
            ptr_type,
            classes,
            async_funcs,
            extern_funcs,
            modules,
            lifetime_deps,
            current_func_name,
            lifetime_funcs,
            var_lifetime_source: HashMap::new(),
            scope_depth: 0,
            var_scope_depth: HashMap::new(),
            borrowed_vars: HashMap::new(),
            weak_variables: HashSet::new(),
        }
    }

    /// 检查表达式是否来源于生命周期依赖参数
    /// 返回 Some(param_name) 如果表达式来自某个生命周期参数（直接或间接）
    fn check_lifetime_source(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(name) => {
                // 1. 检查是否直接是生命周期依赖参数
                if let Some(ref deps) = self.lifetime_deps {
                    if deps.contains(name) {
                        return Some(name.clone());
                    }
                }
                // 2. 检查是否是从生命周期参数派生的变量
                if let Some(source) = self.var_lifetime_source.get(name) {
                    return Some(source.clone());
                }
                None
            }
            Expr::Member(base, _) => {
                self.check_lifetime_source(base)
            }
            Expr::Index(base, _) => {
                self.check_lifetime_source(base)
            }
            _ => None,
        }
    }

    /// 验证返回值的生命周期依赖
    /// 如果函数声明了 from x，则返回值必须来自参数 x
    fn validate_lifetime_return(&self, expr: &Expr) -> Result<(), String> {
        if let Some(ref deps) = self.lifetime_deps {
            // 检查返回值是否来自声明的生命周期依赖参数
            if let Some(source) = self.check_lifetime_source(expr) {
                // 返回值来自某个参数，检查是否在声明的依赖列表中
                if deps.contains(&source) {
                    return Ok(());
                }
            }
            // 返回值不是来自声明的生命周期依赖参数
            return Err(format!(
                "Lifetime error in function '{}': return value must derive from parameter(s) {:?}, \
                 but the expression does not reference any of them",
                self.current_func_name, deps
            ));
        }
        Ok(())
    }

    /// 检查当前函数是否使用生命周期模式（跳过 ARC）
    fn uses_lifetime_mode(&self) -> bool {
        self.lifetime_deps.is_some()
    }

    /// 检查被调用的函数是否是生命周期函数（返回借用而非拥有的值）
    fn is_lifetime_func(&self, func_name: &str) -> bool {
        self.lifetime_funcs.contains(func_name)
    }

    /// 检查表达式是否是对生命周期函数的调用
    fn is_lifetime_func_call(&self, expr: &Expr) -> bool {
        if let Expr::Call(callee, _) = expr {
            if let Expr::Ident(func_name) = callee.as_ref() {
                return self.is_lifetime_func(func_name);
            }
        }
        false
    }

    /// 进入新作用域
    fn enter_scope(&mut self) {
        self.scope_depth += 1;
    }

    /// 离开作用域，检查借用变量是否悬空
    fn leave_scope(&mut self) -> Result<(), String> {
        // 检查是否有借用变量依赖于当前作用域的变量
        let current_depth = self.scope_depth;

        // 找出当前作用域声明的变量
        let vars_in_scope: Vec<String> = self.var_scope_depth.iter()
            .filter(|(_, &depth)| depth == current_depth)
            .map(|(name, _)| name.clone())
            .collect();

        // 检查是否有外层变量借用了当前作用域的变量
        for (borrower, (source, _)) in &self.borrowed_vars {
            let borrower_depth = self.var_scope_depth.get(borrower).copied().unwrap_or(0);
            if borrower_depth < current_depth && vars_in_scope.contains(source) {
                return Err(format!(
                    "Lifetime error: '{}' borrows from '{}' which goes out of scope",
                    borrower, source
                ));
            }
        }

        // 清理当前作用域的变量
        for var in &vars_in_scope {
            self.var_scope_depth.remove(var);
            self.borrowed_vars.remove(var);
        }

        self.scope_depth -= 1;
        Ok(())
    }

    /// 记录变量声明的作用域
    fn record_var_scope(&mut self, var_name: &str) {
        self.var_scope_depth.insert(var_name.to_string(), self.scope_depth);
    }

    /// 记录借用关系
    fn record_borrow(&mut self, borrower: &str, source: &str) {
        let source_depth = self.var_scope_depth.get(source).copied().unwrap_or(0);
        self.borrowed_vars.insert(borrower.to_string(), (source.to_string(), source_depth));
    }

    /// 获取生命周期函数调用的源变量（第一个 ref 参数）
    fn get_lifetime_call_source(&self, expr: &Expr) -> Option<String> {
        if let Expr::Call(callee, args) = expr {
            if let Expr::Ident(func_name) = callee.as_ref() {
                if self.is_lifetime_func(func_name) {
                    // 获取函数的参数信息
                    if let Some(params) = self.func_params.get(func_name) {
                        // 找第一个 ref 参数对应的实参
                        for (i, param) in params.iter().enumerate() {
                            if param.mode == ParamMode::Ref {
                                if let Some(arg) = args.get(i) {
                                    if let Expr::Ident(var_name) = arg {
                                        return Some(var_name.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// 收集语句列表中的 RC 变量声明（用于循环预初始化）
    fn collect_rc_var_decls(&self, stmts: &[Statement]) -> Vec<(String, BolideType)> {
        let mut result = Vec::new();
        for stmt in stmts {
            match stmt {
                Statement::VarDecl(decl) => {
                    let ty = if let Some(ref t) = decl.ty {
                        t.clone()
                    } else if let Some(ref value) = decl.value {
                        self.infer_expr_type(value)
                    } else {
                        BolideType::Int
                    };
                    if Self::is_rc_type(&ty) {
                        result.push((decl.name.clone(), ty));
                    }
                }
                Statement::If(if_stmt) => {
                    // 递归收集 if/else 分支中的变量
                    result.extend(self.collect_rc_var_decls(&if_stmt.then_body));
                    for elif in &if_stmt.elif_branches {
                        result.extend(self.collect_rc_var_decls(&elif.1));
                    }
                    if let Some(ref else_body) = if_stmt.else_body {
                        result.extend(self.collect_rc_var_decls(else_body));
                    }
                }
                Statement::While(while_stmt) => {
                    // 递归收集嵌套循环中的变量
                    result.extend(self.collect_rc_var_decls(&while_stmt.body));
                }
                _ => {}
            }
        }
        result
    }

    /// 检查类型是否需要 RC 管理
    fn is_rc_type(ty: &BolideType) -> bool {
        match ty {
            // weak 和 unowned 不需要 RC 管理（这是它们的核心特性）
            BolideType::Weak(_) | BolideType::Unowned(_) => false,
            _ => matches!(ty,
                BolideType::Str |
                BolideType::BigInt |
                BolideType::Decimal |
                BolideType::List(_) |
                BolideType::Dict(_, _) |
                BolideType::Dynamic |
                BolideType::Custom(_)
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
            _ => None,
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

    /// 为所有 RC 变量生成 release 调用
    fn emit_rc_cleanup(&mut self) {
        self.emit_rc_cleanup_except(None);
    }

    /// 为所有 RC 变量生成 release 调用，可以排除指定变量
    fn emit_rc_cleanup_except(&mut self, except_var: Option<&str>) {
        // 收集需要释放的变量（避免借用冲突）
        let vars_to_release: Vec<_> = self.rc_variables.iter()
            .filter_map(|(name, ty)| {
                // 跳过被排除的变量
                if let Some(except) = except_var {
                    if name == except {
                        return None;
                    }
                }
                if let Some(&var) = self.variables.get(name) {
                    return Some((name.clone(), var, ty.clone()));
                }
                None
            })
            .collect();

        // 生成 release 调用
        for (name, var, ty) in vars_to_release {
            let val = self.builder.use_var(var);

            // 如果是 Custom 类型，先释放内部的 RC 字段
            if let BolideType::Custom(ref class_name) = ty {
                self.emit_object_fields_cleanup(val, class_name);
            }

            // 释放对象本身
            if let Some(func_name) = Self::get_release_func_name(&ty) {
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

    /// 记录 RC 变量
    fn track_rc_variable(&mut self, name: &str, ty: &BolideType) {
        if Self::is_rc_type(ty) {
            self.rc_variables.push((name.to_string(), ty.clone()));
        }
    }

    /// 记录临时 RC 值（表达式中间结果）
    fn track_temp_rc_value(&mut self, val: Value, ty: &BolideType) {
        if Self::is_rc_type(ty) {
            self.temp_rc_values.push((val, ty.clone()));
        }
    }

    /// 释放所有临时 RC 值
    fn release_temp_rc_values(&mut self) {
        let temps = std::mem::take(&mut self.temp_rc_values);
        for (val, ty) in temps {
            if let Some(func_name) = Self::get_release_func_name(&ty) {
                if let Some(&func_ref) = self.func_refs.get(func_name) {
                    self.builder.ins().call(func_ref, &[val]);
                }
            }
        }
    }

    /// 从临时值列表中移除指定值（当值被存入变量时调用）
    fn remove_temp_rc_value(&mut self, val: Value) {
        self.temp_rc_values.retain(|(v, _)| *v != val);
    }

    /// 声明变量
    fn declare_variable(&mut self, name: &str, ty: types::Type) -> Variable {
        let var = Variable::new(self.var_counter);
        self.var_counter += 1;
        self.builder.declare_var(var, ty);
        self.variables.insert(name.to_string(), var);
        var
    }

    /// 定义变量 helper (Declare + Def + Type Register)
    fn define_variable(&mut self, name: &str, val: Value, ty: BolideType) -> Result<(), String> {
        let c_ty = self.bolide_type_to_cranelift(&ty);
        // 如果变量已存在，重新声明？或者复用？Compile context variables.
        // declare_variable checks if exists? 2636 implementation:
        // usually declare_variable creates NEW variable slot. If reusing name, it overwrites in HashMap.
        // This is shadowing.
        let var = self.declare_variable(name, c_ty);
        self.builder.def_var(var, val);
        self.var_types.insert(name.to_string(), ty);
        Ok(())
    }

    /// 编译语句，返回是否已终止当前块
    fn compile_stmt(&mut self, stmt: &Statement) -> Result<bool, String> {
        let result = match stmt {
            Statement::VarDecl(decl) => {
                self.compile_var_decl(decl)?;
                Ok(false)
            }
            Statement::Assign(assign) => {
                self.compile_assign(assign)?;
                Ok(false)
            }
            Statement::Return(expr) => {
                self.compile_return(expr.as_ref())?;
                Ok(true)
            }
            Statement::Expr(e) => {
                self.compile_expr(e)?;
                Ok(false)
            }
            Statement::If(if_stmt) => self.compile_if(if_stmt),
            Statement::While(while_stmt) => {
                self.compile_while(while_stmt)?;
                Ok(false)
            }
            Statement::For(for_stmt) => {
                self.compile_for(for_stmt)?;
                Ok(false)
            }
            Statement::Pool(pool_stmt) => {
                self.compile_pool(pool_stmt)?;
                Ok(false)
            }
            Statement::Send(send_stmt) => {
                self.compile_send(send_stmt)?;
                Ok(false)
            }
            Statement::Select(select_stmt) => {
                self.compile_select(select_stmt)?;
                Ok(false)
            }
            Statement::AwaitScope(scope_stmt) => {
                self.compile_await_scope(scope_stmt)?;
                Ok(false)
            }
            Statement::AsyncSelect(select_stmt) => {
                self.compile_async_select(select_stmt)?;
                Ok(false)
            }
            Statement::FuncDef(_) => Ok(false),
            Statement::ClassDef(_) => Ok(false),
            Statement::Import(_) => Ok(false),
            Statement::ExternBlock(eb) => {
                self.register_extern_block(eb)?;
                Ok(false)
            }
        };

        // 在每条语句执行后释放临时 RC 值
        self.release_temp_rc_values();

        result
    }

    /// 编译赋值语句
    fn compile_assign(&mut self, assign: &Assign) -> Result<(), String> {
        // 根据 target 类型分派
        match &assign.target {
            Expr::Ident(var_name) => self.compile_var_assign(var_name, &assign.value),
            Expr::Member(base, member) => self.compile_member_assign(base, member, &assign.value),
            Expr::Index(base, index) => self.compile_index_assign(base, index, &assign.value),
            _ => Err("Invalid assignment target".to_string()),
        }
    }

    /// 编译索引赋值 (list[i] = value)
    fn compile_index_assign(&mut self, base: &Expr, index: &Expr, value: &Expr) -> Result<(), String> {
        let base_type = self.infer_expr_type(base);
        let base_val = self.compile_expr(base)?;
        let index_val = self.compile_expr(index)?;
        let value_val = self.compile_expr(value)?;

        match base_type {
            BolideType::List(_) => {
                let list_set = *self.func_refs.get("list_set")
                    .ok_or("list_set not found")?;
                self.builder.ins().call(list_set, &[base_val, index_val, value_val]);
                Ok(())
            }
            BolideType::Dict(_, _) => {
                let dict_set = *self.func_refs.get("dict_set")
                    .ok_or("dict_set not found")?;
                self.builder.ins().call(dict_set, &[base_val, index_val, value_val]);
                Ok(())
            }

            BolideType::Tuple(_) => {
                let tuple_set = *self.func_refs.get("tuple_set")
                    .ok_or("tuple_set not found")?;
                self.builder.ins().call(tuple_set, &[base_val, index_val, value_val]);
                Ok(())
            }
            _ => Err(format!("Index assignment not supported for type: {:?}", base_type)),
        }
    }


    /// 编译变量赋值
    fn compile_var_assign(&mut self, var_name: &str, value: &Expr) -> Result<(), String> {
        let var = *self.variables.get(var_name)
            .ok_or_else(|| format!("Undefined variable: {}", var_name))?;

        // 检查是否是 Ref 参数
        let is_ref_param = self.ref_params.iter().any(|(name, _, _)| name == var_name);
        // 检查 Ref 参数是否已经被重新赋值过
        let was_reassigned = self.ref_params_reassigned.contains(var_name);

        // 决定是否释放旧值
        let should_release = !is_ref_param || was_reassigned;

        let var_ty = self.var_types.get(var_name).cloned();
        if let Some(ref ty) = var_ty {
            if Self::is_rc_type(ty) && should_release {
                if let Some(func_name) = Self::get_release_func_name(ty) {
                    if let Some(&func_ref) = self.func_refs.get(func_name) {
                        let old_val = self.builder.use_var(var);
                        self.builder.ins().call(func_ref, &[old_val]);
                    }
                }
            }
        }

        // 如果是 Ref 参数的首次赋值，标记为已重新赋值
        if is_ref_param && !was_reassigned {
            self.ref_params_reassigned.insert(var_name.to_string());
        }

        let val = self.compile_expr(value)?;

        // 如果是 RC 类型，需要处理引用计数
        if let Some(ref ty) = var_ty {
            if Self::is_rc_type(ty) {
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                if is_temp {
                    self.remove_temp_rc_value(val);
                    self.builder.def_var(var, val);
                } else {
                    let clone_func_name = Self::get_clone_func_name(ty);
                    if let Some(func_name) = clone_func_name {
                        if let Some(&func_ref) = self.func_refs.get(func_name) {
                            let call = self.builder.ins().call(func_ref, &[val]);
                            let cloned_val = self.builder.inst_results(call)[0];
                            self.builder.def_var(var, cloned_val);
                        } else {
                            self.builder.def_var(var, val);
                        }
                    } else {
                        self.builder.def_var(var, val);
                    }
                }
            } else {
                self.builder.def_var(var, val);
            }
        } else {
            self.builder.def_var(var, val);
        }

        // 调用者端借用检查：记录借用关系
        if self.is_lifetime_func_call(value) {
            if let Some(source_var) = self.get_lifetime_call_source(value) {
                self.record_borrow(var_name, &source_var);
            }
        }

        Ok(())
    }
    fn compile_member_assign(&mut self, base: &Expr, member: &str, value: &Expr) -> Result<(), String> {
        // 获取基础表达式的类型
        let class_name = self.get_expr_type(base)?;
        let class_name = match class_name {
            BolideType::Custom(name) => name,
            _ => return Err(format!("Member assign on non-class type: {:?}", class_name)),
        };

        // 获取类信息
        let class_info = self.classes.get(&class_name)
            .ok_or_else(|| format!("Class not found: {}", class_name))?
            .clone();

        // 查找字段
        let field = class_info.fields.iter()
            .find(|f| f.name == member)
            .ok_or_else(|| format!("Field '{}' not found in class '{}'", member, class_name))?;

        let field_offset = field.offset;
        let field_ty = field.ty.clone();

        // 编译基础表达式获取对象指针
        let obj_ptr = self.compile_expr(base)?;

        // 编译值表达式
        let val = self.compile_expr(value)?;

        // 计算字段地址
        let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field_offset as i64);

        // 如果字段是 RC 类型，需要处理引用计数
        if Self::is_rc_type(&field_ty) {
            let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
            if is_temp {
                // 值是临时的，移除临时标记，字段接管所有权
                self.remove_temp_rc_value(val);
                self.builder.ins().store(MemFlags::new(), val, field_ptr, 0);
            } else {
                // 值来自另一个变量，需要 clone
                if let Some(func_name) = Self::get_clone_func_name(&field_ty) {
                    if let Some(&func_ref) = self.func_refs.get(func_name) {
                        let call = self.builder.ins().call(func_ref, &[val]);
                        let cloned = self.builder.inst_results(call)[0];
                        self.builder.ins().store(MemFlags::new(), cloned, field_ptr, 0);
                    } else {
                        self.builder.ins().store(MemFlags::new(), val, field_ptr, 0);
                    }
                } else {
                    self.builder.ins().store(MemFlags::new(), val, field_ptr, 0);
                }
            }
        } else {
            self.builder.ins().store(MemFlags::new(), val, field_ptr, 0);
        }

        Ok(())
    }

    /// 编译变量声明
    fn compile_var_decl(&mut self, decl: &VarDecl) -> Result<(), String> {
        // 确定 Bolide 类型
        let bolide_ty = if let Some(ref t) = decl.ty {
            t.clone()
        } else if let Some(ref value) = decl.value {
            // 从初始化表达式推断类型
            self.infer_expr_type(value)
        } else {
            BolideType::Int
        };

        // 记录变量的 Bolide 类型
        self.var_types.insert(decl.name.clone(), bolide_ty.clone());

        // 记录变量的作用域深度
        self.record_var_scope(&decl.name);

        // 如果是 spawn 或异步函数调用，记录变量名 -> 函数名的映射
        if let Some(ref value) = decl.value {
            match value {
                Expr::Spawn(func_name, _) => {
                    self.spawn_func_map.insert(decl.name.clone(), func_name.clone());
                }
                Expr::Call(func_expr, _) => {
                    // 检查是否是异步函数调用
                    if let Expr::Ident(func_name) = func_expr.as_ref() {
                        if self.async_funcs.contains(func_name) {
                            self.spawn_func_map.insert(decl.name.clone(), func_name.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        // 转换为 Cranelift 类型
        let ty = self.bolide_type_to_cranelift(&bolide_ty);

        // 检查变量是否已存在（循环中已预初始化的变量）
        let existing_var = self.variables.get(&decl.name).copied();

        let var = if let Some(v) = existing_var {
            // 变量已存在（循环中预初始化过），release 旧值
            if Self::is_rc_type(&bolide_ty) {
                let old_val = self.builder.use_var(v);

                // 检查旧值是否为 null（第一次迭代时可能是 null）
                let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                let is_null = self.builder.ins().icmp(IntCC::Equal, old_val, null_val);

                let release_block = self.builder.create_block();
                let continue_block = self.builder.create_block();

                self.builder.ins().brif(is_null, continue_block, &[], release_block, &[]);

                // release_block: 释放旧值
                self.builder.switch_to_block(release_block);
                self.builder.seal_block(release_block);

                // 如果是 Custom 类型，先释放内部的 RC 字段
                if let BolideType::Custom(ref class_name) = bolide_ty {
                    self.emit_object_fields_cleanup(old_val, class_name);
                }
                // 释放对象本身
                if let Some(func_name) = Self::get_release_func_name(&bolide_ty) {
                    if let Some(&func_ref) = self.func_refs.get(func_name) {
                        self.builder.ins().call(func_ref, &[old_val]);
                    }
                }

                self.builder.ins().jump(continue_block, &[]);

                // continue_block: 继续执行
                self.builder.switch_to_block(continue_block);
                self.builder.seal_block(continue_block);
            }
            v
        } else {
            // 首次声明
            self.declare_variable(&decl.name, ty)
        };

        if let Some(ref value) = decl.value {
            let val = self.compile_expr(value)?;

            // 检查值是否来自生命周期函数调用（返回借用而非拥有的值）
            let is_from_lifetime_func = self.is_lifetime_func_call(value);

            // 如果是 RC 类型，需要处理引用计数
            if Self::is_rc_type(&bolide_ty) && !is_from_lifetime_func {
                // 检查值是否来自临时 RC 值（函数调用结果等）
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);

                if is_temp {
                    // 值是临时的，移除临时标记，变量接管所有权
                    self.remove_temp_rc_value(val);
                    self.builder.def_var(var, val);
                } else {
                    // 值来自另一个变量，需要 clone（retain RC）
                    let clone_func_name = Self::get_clone_func_name(&bolide_ty);
                    if let Some(func_name) = clone_func_name {
                        if let Some(&func_ref) = self.func_refs.get(func_name) {
                            let call = self.builder.ins().call(func_ref, &[val]);
                            let results = self.builder.inst_results(call);
                            let cloned_val = results[0];
                            self.builder.def_var(var, cloned_val);
                        } else {
                            // 没有 clone 函数，直接使用值
                            self.builder.def_var(var, val);
                        }
                    } else {
                        // 没有 clone 函数，直接使用值
                        self.builder.def_var(var, val);
                    }
                }
            } else {
                // 非 RC 类型或来自生命周期函数，直接使用值
                self.builder.def_var(var, val);
            }
        } else {
            // 根据类型初始化默认值
            let zero = if matches!(bolide_ty, BolideType::Float) {
                self.builder.ins().f64const(0.0)
            } else {
                self.builder.ins().iconst(ty, 0)
            };
            self.builder.def_var(var, zero);
        }

        // 数据流追踪：如果值来自生命周期参数，记录变量的来源
        if self.uses_lifetime_mode() {
            if let Some(ref value) = decl.value {
                if let Some(source) = self.check_lifetime_source(value) {
                    self.var_lifetime_source.insert(decl.name.clone(), source);
                }
            }
        }

        // 跟踪 RC 变量，用于作用域结束时释放（避免重复添加）
        // 但如果值来自生命周期函数调用，则跳过 RC 跟踪（返回的是借用而非拥有的值）
        let is_from_lifetime_func = decl.value.as_ref()
            .map(|v| self.is_lifetime_func_call(v))
            .unwrap_or(false);

        // 调用者端借用检查：记录借用关系
        if is_from_lifetime_func {
            if let Some(ref value) = decl.value {
                if let Some(source_var) = self.get_lifetime_call_source(value) {
                    self.record_borrow(&decl.name, &source_var);
                }
            }
        }

        if existing_var.is_none()
            && !self.rc_variables.iter().any(|(n, _)| n == &decl.name)
            && !is_from_lifetime_func
        {
            self.track_rc_variable(&decl.name, &bolide_ty);
        }

        // 追踪 weak 变量（访问时需要检查是否为 nil）
        if matches!(bolide_ty, BolideType::Weak(_)) {
            self.weak_variables.insert(decl.name.clone());
        }

        Ok(())
    }

    /// 编译 return 语句
    fn compile_return(&mut self, expr: Option<&Expr>) -> Result<(), String> {
        if let Some(e) = expr {
            // 生命周期模式：验证返回值来源
            if self.uses_lifetime_mode() {
                self.validate_lifetime_return(e)?;
            }

            // 先编译返回表达式
            let val = self.compile_expr(e)?;

            // 检查返回值是否是局部 RC 变量（如果是，不释放该变量）
            let return_var_name = if let Expr::Ident(name) = e {
                Some(name.clone())
            } else {
                None
            };

            // 生命周期模式下跳过 ARC 操作
            if !self.uses_lifetime_mode() {
                // 如果返回的是临时 RC 值，从临时列表中移除（调用者将接管所有权）
                self.remove_temp_rc_value(val);

                // 释放其他临时 RC 值（返回语句后不会再有机会释放）
                self.release_temp_rc_values();

                // 释放所有 RC 变量，除了返回的那个
                self.emit_rc_cleanup_except(return_var_name.as_deref());
            }

            // 写回 Ref 参数
            self.write_back_ref_params();

            self.builder.ins().return_(&[val]);
        } else {
            // 生命周期模式下跳过 ARC 操作
            if !self.uses_lifetime_mode() {
                // 释放所有临时 RC 值
                self.release_temp_rc_values();

                // 无返回值，释放所有 RC 变量
                self.emit_rc_cleanup();
            }

            // 写回 Ref 参数
            self.write_back_ref_params();

            self.builder.ins().return_(&[]);
        }
        Ok(())
    }

    /// 写回所有 Ref 参数的值
    fn write_back_ref_params(&mut self) {
        for (_, var, ptr_addr) in &self.ref_params.clone() {
            let current_val = self.builder.use_var(*var);
            self.builder.ins().store(MemFlags::new(), current_val, *ptr_addr, 0);
        }
    }

    /// 编译 if 语句
    fn compile_if(&mut self, if_stmt: &bolide_parser::IfStmt) -> Result<bool, String> {
        let cond = self.compile_expr(&if_stmt.condition)?;

        // 释放条件表达式中的临时值（在分支之前）
        self.release_temp_rc_values();

        let then_block = self.builder.create_block();
        let else_block = self.builder.create_block();
        let merge_block = self.builder.create_block();

        self.builder.ins().brif(cond, then_block, &[], else_block, &[]);

        self.builder.switch_to_block(then_block);
        self.builder.seal_block(then_block);
        self.enter_scope();  // 进入 then 作用域
        let mut then_terminated = false;
        for stmt in &if_stmt.then_body {
            if then_terminated { break; }
            then_terminated = self.compile_stmt(stmt)?;
        }
        self.leave_scope()?;  // 离开 then 作用域
        if !then_terminated {
            self.builder.ins().jump(merge_block, &[]);
        }

        self.builder.switch_to_block(else_block);
        self.builder.seal_block(else_block);

        let else_terminated = if !if_stmt.elif_branches.is_empty() {
            self.compile_elif_chain(&if_stmt.elif_branches, &if_stmt.else_body, merge_block)?
        } else if let Some(ref else_body) = if_stmt.else_body {
            self.enter_scope();  // 进入 else 作用域
            let mut terminated = false;
            for stmt in else_body {
                if terminated { break; }
                terminated = self.compile_stmt(stmt)?;
            }
            self.leave_scope()?;  // 离开 else 作用域
            if !terminated {
                self.builder.ins().jump(merge_block, &[]);
            }
            terminated
        } else {
            self.builder.ins().jump(merge_block, &[]);
            false
        };

        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);

        Ok(then_terminated && else_terminated)
    }

    fn compile_elif_chain(
        &mut self,
        elif_branches: &[(Expr, Vec<Statement>)],
        else_body: &Option<Vec<Statement>>,
        merge_block: Block,
    ) -> Result<bool, String> {
        if elif_branches.is_empty() {
            if let Some(ref body) = else_body {
                let mut terminated = false;
                for stmt in body {
                    if terminated { break; }
                    terminated = self.compile_stmt(stmt)?;
                }
                if !terminated {
                    self.builder.ins().jump(merge_block, &[]);
                }
                return Ok(terminated);
            }
            self.builder.ins().jump(merge_block, &[]);
            return Ok(false);
        }

        let (cond_expr, then_body) = &elif_branches[0];
        let rest = &elif_branches[1..];

        let cond = self.compile_expr(cond_expr)?;
        let then_block = self.builder.create_block();
        let else_block = self.builder.create_block();

        self.builder.ins().brif(cond, then_block, &[], else_block, &[]);

        self.builder.switch_to_block(then_block);
        self.builder.seal_block(then_block);
        let mut then_terminated = false;
        for stmt in then_body {
            if then_terminated { break; }
            then_terminated = self.compile_stmt(stmt)?;
        }
        if !then_terminated {
            self.builder.ins().jump(merge_block, &[]);
        }

        self.builder.switch_to_block(else_block);
        self.builder.seal_block(else_block);
        let else_terminated = self.compile_elif_chain(rest, else_body, merge_block)?;

        Ok(then_terminated && else_terminated)
    }

    /// 编译 while 语句
    fn compile_while(&mut self, while_stmt: &bolide_parser::WhileStmt) -> Result<(), String> {
        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        // 第一遍：收集循环体内的 RC 变量声明
        let loop_rc_vars = self.collect_rc_var_decls(&while_stmt.body);

        // 在进入循环前，为这些变量初始化为 null（跳过已存在的变量）
        for (var_name, var_ty) in &loop_rc_vars {
            // 如果变量已存在（外层循环已初始化），跳过
            if self.variables.contains_key(var_name) {
                continue;
            }
            let ty = self.bolide_type_to_cranelift(var_ty);
            let var = self.declare_variable(var_name, ty);
            let null_val = self.builder.ins().iconst(self.ptr_type, 0);
            self.builder.def_var(var, null_val);
            // 记录变量类型
            self.var_types.insert(var_name.clone(), var_ty.clone());
            // 跟踪 RC 变量（用于函数结束时释放最后一次迭代的值）
            self.track_rc_variable(var_name, var_ty);
        }

        self.builder.ins().jump(header_block, &[]);

        self.builder.switch_to_block(header_block);
        let cond = self.compile_expr(&while_stmt.condition)?;
        self.builder.ins().brif(cond, body_block, &[], exit_block, &[]);

        // 第二遍：正常编译循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);
        self.enter_scope();  // 进入循环体作用域
        let mut terminated = false;
        for stmt in &while_stmt.body {
            if terminated { break; }
            terminated = self.compile_stmt(stmt)?;
        }
        self.leave_scope()?;  // 离开循环体作用域
        if !terminated {
            self.builder.ins().jump(header_block, &[]);
        }

        self.builder.seal_block(header_block);

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }

    /// 编译 for 语句
    /// 支持两种形式:
    /// 1. for i in range(n) { ... } - 整数范围迭代
    /// 2. for item in list { ... } - 列表迭代
    fn compile_for(&mut self, for_stmt: &bolide_parser::ForStmt) -> Result<(), String> {
        let vars = &for_stmt.vars;
        if vars.is_empty() {
             return Err("For loop must have at least one variable".to_string());
        }

        // 检查是否是 range(n) 调用
        if let Expr::Call(callee, args) = &for_stmt.iter {
            if let Expr::Ident(func_name) = callee.as_ref() {
                if func_name == "range" {
                    if vars.len() != 1 {
                        return Err("range() loop only supports single variable".to_string());
                    }
                    return self.compile_for_range(&vars[0], args, &for_stmt.body);
                }
            }
        }
        
        // 检查是否是字典迭代
        if let BolideType::Dict(_, _) = self.infer_expr_type(&for_stmt.iter) {
             return self.compile_for_dict(vars, &for_stmt.iter, &for_stmt.body);
        }

        // 否则当作列表迭代（支持解构）
        self.compile_for_list(vars, &for_stmt.iter, &for_stmt.body)

    }

    /// 编译 for i in range(...) { ... }
    /// 支持 Python 风格的 range:
    /// - range(end): 0 到 end-1
    /// - range(start, end): start 到 end-1
    /// - range(start, end, step): start 到 end-1，步长为 step
    fn compile_for_range(&mut self, var_name: &str, args: &[Expr], body: &[Statement]) -> Result<(), String> {
        // 解析 range 参数
        let (start_val, end_val, step_val, is_negative_step) = match args.len() {
            1 => {
                let end = self.compile_expr(&args[0])?;
                let start = self.builder.ins().iconst(types::I64, 0);
                let step = self.builder.ins().iconst(types::I64, 1);
                (start, end, step, false)
            }
            2 => {
                let start = self.compile_expr(&args[0])?;
                let end = self.compile_expr(&args[1])?;
                let step = self.builder.ins().iconst(types::I64, 1);
                (start, end, step, false)
            }
            3 => {
                let start = self.compile_expr(&args[0])?;
                let end = self.compile_expr(&args[1])?;
                let step = self.compile_expr(&args[2])?;
                // 检查是否可能是负步长 (编译时无法确定，运行时处理)
                // 对于常量步长，可以优化
                let is_neg = if let Expr::Int(n) = &args[2] { *n < 0 } else { false };
                (start, end, step, is_neg)
            }
            _ => return Err("range() expects 1, 2, or 3 arguments".to_string()),
        };

        // 创建循环变量
        let loop_var = self.declare_variable(var_name, types::I64);
        self.builder.def_var(loop_var, start_val);
        self.var_types.insert(var_name.to_string(), BolideType::Int);

        // 创建基本块
        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        // 收集循环体内的 RC 变量声明
        let loop_rc_vars = self.collect_rc_var_decls(body);
        for (rc_var_name, var_ty) in &loop_rc_vars {
            if self.variables.contains_key(rc_var_name) {
                continue;
            }
            let ty = self.bolide_type_to_cranelift(var_ty);
            let var = self.declare_variable(rc_var_name, ty);
            let null_val = self.builder.ins().iconst(self.ptr_type, 0);
            self.builder.def_var(var, null_val);
            self.var_types.insert(rc_var_name.clone(), var_ty.clone());
            self.track_rc_variable(rc_var_name, var_ty);
        }

        // 跳转到循环头
        self.builder.ins().jump(header_block, &[]);

        // 循环头: 检查条件
        self.builder.switch_to_block(header_block);
        let current_val = self.builder.use_var(loop_var);
        
        // 根据步长方向选择比较条件
        let cond = if is_negative_step {
            // 负步长: i > end
            self.builder.ins().icmp(IntCC::SignedGreaterThan, current_val, end_val)
        } else {
            // 正步长: i < end
            self.builder.ins().icmp(IntCC::SignedLessThan, current_val, end_val)
        };
        self.builder.ins().brif(cond, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);
        self.enter_scope();
        let mut terminated = false;
        for stmt in body {
            if terminated { break; }
            terminated = self.compile_stmt(stmt)?;
        }
        self.leave_scope()?;
        
        if !terminated {
            // 递增/递减循环变量: i = i + step
            let current = self.builder.use_var(loop_var);
            let next = self.builder.ins().iadd(current, step_val);
            self.builder.def_var(loop_var, next);
            self.builder.ins().jump(header_block, &[]);
        }

        self.builder.seal_block(header_block);
        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }

    /// 编译 for item in list { ... }
    /// 编译列表迭代逻辑 (通用)
    fn compile_list_iteration_loop(
        &mut self, 
        vars: &[String], 
        list_ptr: Value, 
        elem_type: BolideType, 
        body: &[Statement]
    ) -> Result<(), String> {
        // 获取列表长度: list_len(list_ptr)
        let list_len_ref = *self.func_refs.get("list_len")
            .ok_or("list_len not found")?;
        let len_call = self.builder.ins().call(list_len_ref, &[list_ptr]);
        let list_length = self.builder.inst_results(len_call)[0];

        // 使用第一个变量名作为索引变量后缀
        let loop_base_name = if !vars.is_empty() { &vars[0] } else { "loop" };

        // 创建索引变量
        let idx_var_name = format!("__for_idx_{}", loop_base_name);
        let idx_var = self.declare_variable(&idx_var_name, types::I64);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.def_var(idx_var, zero);

        // 创建循环变量 (如果是单个变量)
        let loop_var = if vars.len() == 1 {
            let v = self.declare_variable(&vars[0], types::I64); // 注意: declare_variable 需要具体类型吗? 这里的declare 是JIT internal mapping.
            // Wait: declare_variable in jit.rs assigns Slot.
            // Previous code:
            // let loop_var = self.declare_variable(var_name, types::I64); -- TYPE I64?
            // Element can be Ptr or I64.
            // If elem_type is Ptr, we should declare valid Cranelift type.
            // Step 630 line 3414: types::I64. (Maybe everything is I64/Ptr=I64 in current impl).
            // I'll stick to I64.
            self.builder.def_var(v, zero);
            // 注册类型
            self.var_types.insert(vars[0].to_string(), elem_type.clone());
            Some(v)
        } else {
            None // Destructuring handled inside body
        };

        // 创建基本块
        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        // 收集循环体内的 RC 变量声明
        let loop_rc_vars = self.collect_rc_var_decls(body);
        for (rc_var_name, var_ty) in &loop_rc_vars {
            if self.variables.contains_key(rc_var_name) {
                continue;
            }
            let ty = self.bolide_type_to_cranelift(var_ty);
            let var = self.declare_variable(rc_var_name, ty);
            let null_val = self.builder.ins().iconst(self.ptr_type, 0);
            self.builder.def_var(var, null_val);
            self.var_types.insert(rc_var_name.clone(), var_ty.clone());
            self.track_rc_variable(rc_var_name, var_ty);
        }

        // 跳转到循环头
        self.builder.ins().jump(header_block, &[]);

        // 循环头: 检查条件 (idx < length)
        self.builder.switch_to_block(header_block);
        let current_idx = self.builder.use_var(idx_var);
        let cond = self.builder.ins().icmp(IntCC::SignedLessThan, current_idx, list_length);
        self.builder.ins().brif(cond, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);

        // 获取当前元素: list_get(list_ptr, idx)
        let list_get_ref = *self.func_refs.get("list_get")
            .ok_or("list_get not found")?;
        let idx_val = self.builder.use_var(idx_var);
        let get_call = self.builder.ins().call(list_get_ref, &[list_ptr, idx_val]);
        let elem_val = self.builder.inst_results(get_call)[0];
        
        if vars.len() == 1 {
             if let Some(v) = loop_var {
                 self.builder.def_var(v, elem_val);
             }
        } else {
            // 解构 (Destructuring)
            match elem_type {
                BolideType::List(inner_type) => { // List unpacking
                    let list_get_ref = *self.func_refs.get("list_get").ok_or("list_get not found")?;
                    for (i, var_name) in vars.iter().enumerate() {
                        let idx_const = self.builder.ins().iconst(types::I64, i as i64);
                        let call = self.builder.ins().call(list_get_ref, &[elem_val, idx_const]);
                        let val = self.builder.inst_results(call)[0];
                        self.define_variable(var_name, val, *inner_type.clone())?;
                    }
                }
                BolideType::Tuple(inner_types) => { // Tuple unpacking
                    let tuple_get_ref = *self.func_refs.get("tuple_get").ok_or("tuple_get not found")?;
                    // Ensure vars count matches tuple size? or min?
                    for (i, var_name) in vars.iter().enumerate() {
                         let idx_const = self.builder.ins().iconst(types::I64, i as i64);
                         let call = self.builder.ins().call(tuple_get_ref, &[elem_val, idx_const]);
                         let val = self.builder.inst_results(call)[0];
                         
                         let ty = if i < inner_types.len() { inner_types[i].clone() } else { BolideType::Int }; // Fallback
                         self.define_variable(var_name, val, ty)?;
                    }
                }
                _ => return Err(format!("Cannot unpack type {:?} in for loop", elem_type))
            }
        }

        self.enter_scope();
        let mut terminated = false;
        for stmt in body {
            if terminated { break; }
            terminated = self.compile_stmt(stmt)?;
        }
        self.leave_scope()?;
        
        if !terminated {
            // 递增索引: idx = idx + 1
            let current = self.builder.use_var(idx_var);
            let next = self.builder.ins().iadd_imm(current, 1);
            self.builder.def_var(idx_var, next);
            self.builder.ins().jump(header_block, &[]);
        }

        self.builder.seal_block(header_block);
        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }

    /// 编译 for item in list { ... }
    fn compile_for_list(&mut self, vars: &[String], iter_expr: &Expr, body: &[Statement]) -> Result<(), String> {
        let list_ptr = self.compile_expr(iter_expr)?;
        let elem_type = match self.infer_expr_type(iter_expr) {
            BolideType::List(inner) => *inner,
            _ => BolideType::Int,
        };
        self.compile_list_iteration_loop(vars, list_ptr, elem_type, body)
    }

    /// 编译 for key in dict { ... }
    fn compile_for_dict(&mut self, vars: &[String], iter_expr: &Expr, body: &[Statement]) -> Result<(), String> {
        let dict_ptr = self.compile_expr(iter_expr)?;
        
        let dict_iter = *self.func_refs.get("dict_iter").ok_or("dict_iter not found")?;
        let call = self.builder.ins().call(dict_iter, &[dict_ptr]);
        let keys_list_ptr = self.builder.inst_results(call)[0];
        
        let (key_type, val_type) = match self.infer_expr_type(iter_expr) {
            BolideType::Dict(k, v) => (*k, *v),
            _ => (BolideType::Int, BolideType::Int),
        };

        if vars.len() == 2 {
            // 优化: for k, v in d. 直接在循环中获取 value，避免创建 items 列表
            // 复用 list 迭代逻辑，但需要自定义 body 来注入 "let v = d[k]"
            
            // 我们不能直接修改 AST body，所以我们需要手动构建循环逻辑
            // 或者，我们可以生成一个新的 Statement 列表，把 v 的定义加进去
            // 但是 AST Statement 是结构体，需要构建。
            // 更简单的方法是: 手动编写 loop 逻辑 (inline)
            
            // 1. 获取 length (keys list)
            let list_len_ref = *self.func_refs.get("list_len").ok_or("list_len not found")?;
            let len_call = self.builder.ins().call(list_len_ref, &[keys_list_ptr]);
            let list_length = self.builder.inst_results(len_call)[0];

            let idx_var = self.declare_variable(&format!("__for_idx_{}", vars[0]), types::I64);
            let zero = self.builder.ins().iconst(types::I64, 0);
            self.builder.def_var(idx_var, zero);

            let header_block = self.builder.create_block();
            let body_block = self.builder.create_block();
            let exit_block = self.builder.create_block();

            self.builder.ins().jump(header_block, &[]);

            // Header
            self.builder.switch_to_block(header_block);
            let current_idx = self.builder.use_var(idx_var);
            let cond = self.builder.ins().icmp(IntCC::SignedLessThan, current_idx, list_length);
            self.builder.ins().brif(cond, body_block, &[], exit_block, &[]);

            // Body
            self.builder.switch_to_block(body_block);
            self.builder.seal_block(body_block);

            // Get Key
            let list_get_ref = *self.func_refs.get("list_get").ok_or("list_get not found")?;
            let get_key_call = self.builder.ins().call(list_get_ref, &[keys_list_ptr, current_idx]);
            let key_val = self.builder.inst_results(get_key_call)[0];
            
            self.define_variable(&vars[0], key_val, key_type.clone())?;

            // Get Value: val = dict_get(dict_ptr, key)
            let dict_get_ref = *self.func_refs.get("dict_get").ok_or("dict_get not found")?;
            let get_val_call = self.builder.ins().call(dict_get_ref, &[dict_ptr, key_val]);
            let val_val = self.builder.inst_results(get_val_call)[0];
            
            self.define_variable(&vars[1], val_val, val_type.clone())?;

            // Compile body
            self.enter_scope();
            let mut terminated = false;
            for stmt in body {
                if terminated { break; }
                terminated = self.compile_stmt(stmt)?;
            }
            self.leave_scope()?;

            if !terminated {
                 let current = self.builder.use_var(idx_var);
                 let next = self.builder.ins().iadd_imm(current, 1);
                 self.builder.def_var(idx_var, next);
                 self.builder.ins().jump(header_block, &[]);
            }

            self.builder.seal_block(header_block);
            self.builder.switch_to_block(exit_block);
            self.builder.seal_block(exit_block);

        } else {
            // 单变量迭代 (Keys)
            self.compile_list_iteration_loop(vars, keys_list_ptr, key_type, body)?;
        }

        // Release keys list
        let release_fn = *self.func_refs.get("list_release").ok_or("list_release not found")?;
        self.builder.ins().call(release_fn, &[keys_list_ptr]);

        Ok(())
    }

    /// 编译表达式
    fn compile_expr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Int(n) => Ok(self.builder.ins().iconst(types::I64, *n)),
            Expr::Float(f) => Ok(self.builder.ins().f64const(*f)),
            Expr::Bool(b) => Ok(self.builder.ins().iconst(types::I64, if *b { 1 } else { 0 })),
            Expr::String(s) => {
                // 将字符串字面量泄露到堆上，确保在程序生命周期内有效
                let bytes: Box<[u8]> = s.as_bytes().into();
                let ptr = Box::leak(bytes).as_ptr();
                let len = s.len();

                let len = s.len();

                // 获取 string_literal 函数引用 (Uses interning)
                let func_ref = *self.func_refs.get("string_literal")
                    .ok_or("string_literal not found")?;

                // 创建指针和长度的立即数
                let ptr_val = self.builder.ins().iconst(self.ptr_type, ptr as i64);
                let len_val = self.builder.ins().iconst(types::I64, len as i64);

                // 调用 string_from_slice(ptr, len) -> BolideString*
                let call = self.builder.ins().call(func_ref, &[ptr_val, len_val]);
                let results = self.builder.inst_results(call);
                Ok(results[0])
            }
            Expr::BigInt(s) => self.compile_bigint_literal(s),
            Expr::Decimal(s) => self.compile_decimal_literal(s),
            Expr::Ident(name) => self.compile_ident(name),
            Expr::BinOp(left, op, right) => self.compile_binop(left, op, right),
            Expr::UnaryOp(op, operand) => self.compile_unary(op, operand),
            Expr::Call(callee, args) => self.compile_call(callee, args),
            Expr::Index(base, index) => self.compile_index(base, index),
            Expr::Member(base, member) => self.compile_member_access(base, member),
            Expr::List(items) => self.compile_list(items),
            Expr::Spawn(func_name, args) => self.compile_spawn(func_name, args),
            Expr::Recv(channel) => self.compile_recv(channel),
            Expr::None => Ok(self.builder.ins().iconst(types::I64, 0)),
            Expr::Await(inner_expr) => self.compile_await(inner_expr),
            Expr::AwaitAll(exprs) => self.compile_await_all(exprs),
            Expr::Tuple(exprs) => self.compile_tuple(exprs),
            Expr::Dict(entries) => self.compile_dict(entries),
        }
    }


    /// 编译 BigInt 字面量
    fn compile_bigint_literal(&mut self, s: &str) -> Result<Value, String> {
        // 尝试作为 i64 解析，如果成功则用 bigint_from_i64
        let result = if let Ok(n) = s.parse::<i64>() {
            let func_ref = *self.func_refs.get("bigint_from_i64")
                .ok_or("bigint_from_i64 not found")?;
            let val = self.builder.ins().iconst(types::I64, n);
            let call = self.builder.ins().call(func_ref, &[val]);
            let results = self.builder.inst_results(call);
            results[0]
        } else {
            // 用字符串方式创建 BigInt（超出 i64 范围的大数）
            let func_ref = *self.func_refs.get("bigint_from_str")
                .ok_or("bigint_from_str not found")?;

            // 将字符串字面量泄露到堆上，确保在程序生命周期内有效
            let bytes: Box<[u8]> = s.as_bytes().into();
            let ptr = Box::leak(bytes).as_ptr();
            let len = s.len();

            let ptr_val = self.builder.ins().iconst(self.ptr_type, ptr as i64);
            let len_val = self.builder.ins().iconst(types::I64, len as i64);

            let call = self.builder.ins().call(func_ref, &[ptr_val, len_val]);
            let results = self.builder.inst_results(call);
            results[0]
        };
        // 标记为临时 RC 值
        self.track_temp_rc_value(result, &BolideType::BigInt);
        Ok(result)
    }

    /// 编译 Decimal 字面量
    fn compile_decimal_literal(&mut self, s: &str) -> Result<Value, String> {
        // 尝试作为 f64 解析
        if let Ok(f) = s.parse::<f64>() {
            let func_ref = *self.func_refs.get("decimal_from_f64")
                .ok_or("decimal_from_f64 not found")?;
            let val = self.builder.ins().f64const(f);
            let call = self.builder.ins().call(func_ref, &[val]);
            let results = self.builder.inst_results(call);
            let result = results[0];
            // 标记为临时 RC 值
            self.track_temp_rc_value(result, &BolideType::Decimal);
            Ok(result)
        } else {
            Err("Invalid decimal literal".to_string())
        }
    }

    /// 编译标识符访问
    fn compile_ident(&mut self, name: &str) -> Result<Value, String> {
        // 检查变量是否已被移动
        if self.moved_variables.contains(name) {
            return Err(format!("Variable '{}' has been moved and cannot be used", name));
        }

        // 先查找变量
        if let Some(&var) = self.variables.get(name) {
            let val = self.builder.use_var(var);

            // weak 变量访问时检查是否为 nil（运行时检查）
            // 只对指针类型（类实例等）进行 nil 检查
            if self.weak_variables.contains(name) {
                if let Some(var_ty) = self.var_types.get(name) {
                    // 获取 weak 内部的实际类型
                    let inner_ty = match var_ty {
                        BolideType::Weak(inner) => inner.as_ref(),
                        _ => var_ty,
                    };
                    // 只对 Custom 类型（类实例）进行 nil 检查
                    if matches!(inner_ty, BolideType::Custom(_)) {
                        let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                        let is_null = self.builder.ins().icmp(IntCC::Equal, val, null_val);

                        let warn_block = self.builder.create_block();
                        let continue_block = self.builder.create_block();
                        self.builder.append_block_param(continue_block, self.ptr_type);

                        self.builder.ins().brif(is_null, warn_block, &[], continue_block, &[val]);

                        // warn_block: weak 引用已失效，返回 nil
                        self.builder.switch_to_block(warn_block);
                        self.builder.seal_block(warn_block);
                        self.builder.ins().jump(continue_block, &[null_val]);

                        // continue_block: 继续执行
                        self.builder.switch_to_block(continue_block);
                        self.builder.seal_block(continue_block);

                        let result = self.builder.block_params(continue_block)[0];
                        return Ok(result);
                    }
                }
            }

            return Ok(val);
        }

        // 如果不是变量，检查是否是函数名（支持函数作为值）
        if let Some(&func_ref) = self.func_refs.get(name) {
            // 返回函数指针
            return Ok(self.builder.ins().func_addr(self.ptr_type, func_ref));
        }

        Err(format!("Undefined variable or function: {}", name))
    }

    /// 编译二元操作
    fn compile_binop(&mut self, left: &Expr, op: &BinOp, right: &Expr) -> Result<Value, String> {
        // 推断操作数类型
        let left_ty = self.infer_expr_type(left);
        let right_ty = self.infer_expr_type(right);

        // 类类型运算符重载
        if let BolideType::Custom(ref class_name) = left_ty {
            if let Some(result) = self.try_operator_overload(left, op, right, class_name)? {
                return Ok(result);
            }
        }

        let lhs = self.compile_expr(left)?;
        let rhs = self.compile_expr(right)?;

        // BigInt 运算
        if matches!(left_ty, BolideType::BigInt) || matches!(right_ty, BolideType::BigInt) {
            return self.compile_bigint_binop(lhs, op, rhs);
        }

        // Decimal 运算
        if matches!(left_ty, BolideType::Decimal) || matches!(right_ty, BolideType::Decimal) {
            return self.compile_decimal_binop(lhs, op, rhs);
        }

        // 字符串拼接
        if matches!(left_ty, BolideType::Str) && matches!(right_ty, BolideType::Str) {
            if matches!(op, BinOp::Add) {
                let func_ref = *self.func_refs.get("string_concat")
                    .ok_or("string_concat not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                return Ok(result);
            } else if matches!(op, BinOp::Eq) {
                let func_ref = *self.func_refs.get("string_eq")
                    .ok_or("string_eq not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                return Ok(self.builder.inst_results(call)[0]);
            } else if matches!(op, BinOp::Ne) {
                let func_ref = *self.func_refs.get("string_eq")
                    .ok_or("string_eq not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                let eq_result = self.builder.inst_results(call)[0];
                let one = self.builder.ins().iconst(types::I64, 1);
                return Ok(self.builder.ins().isub(one, eq_result));
            } else {
                return Err(format!("Unsupported string operation: {:?}", op));
            }
        }

        // Float 运算
        let is_float = matches!(left_ty, BolideType::Float) || matches!(right_ty, BolideType::Float);
        let result = if is_float {
            // Float 运算
            match op {
                BinOp::Add => self.builder.ins().fadd(lhs, rhs),
                BinOp::Sub => self.builder.ins().fsub(lhs, rhs),
                BinOp::Mul => self.builder.ins().fmul(lhs, rhs),
                BinOp::Div => self.builder.ins().fdiv(lhs, rhs),
                BinOp::Mod => {
                    // float mod: a - floor(a/b) * b
                    let div = self.builder.ins().fdiv(lhs, rhs);
                    let floored = self.builder.ins().floor(div);
                    let prod = self.builder.ins().fmul(floored, rhs);
                    self.builder.ins().fsub(lhs, prod)
                }
                BinOp::Eq => {
                    let cmp = self.builder.ins().fcmp(FloatCC::Equal, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Ne => {
                    let cmp = self.builder.ins().fcmp(FloatCC::NotEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Lt => {
                    let cmp = self.builder.ins().fcmp(FloatCC::LessThan, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Le => {
                    let cmp = self.builder.ins().fcmp(FloatCC::LessThanOrEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Gt => {
                    let cmp = self.builder.ins().fcmp(FloatCC::GreaterThan, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Ge => {
                    let cmp = self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::And | BinOp::Or => {
                    return Err("Logical operations not supported for float".to_string());
                }
            }
        } else {
            // Int 运算
            match op {
                BinOp::Add => self.builder.ins().iadd(lhs, rhs),
                BinOp::Sub => self.builder.ins().isub(lhs, rhs),
                BinOp::Mul => self.builder.ins().imul(lhs, rhs),
                BinOp::Div => self.builder.ins().sdiv(lhs, rhs),
                BinOp::Mod => self.builder.ins().srem(lhs, rhs),

                BinOp::Eq => {
                    let cmp = self.builder.ins().icmp(IntCC::Equal, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Ne => {
                    let cmp = self.builder.ins().icmp(IntCC::NotEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Lt => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedLessThan, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Le => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Gt => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedGreaterThan, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Ge => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }

                BinOp::And => self.builder.ins().band(lhs, rhs),
                BinOp::Or => self.builder.ins().bor(lhs, rhs),
            }
        };

        Ok(result)
    }

    /// 编译 BigInt 二元操作
    fn compile_bigint_binop(&mut self, lhs: Value, op: &BinOp, rhs: Value) -> Result<Value, String> {
        // 算术运算返回新的 BigInt，需要跟踪为临时值
        let is_arithmetic = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod);

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

        // 算术运算的结果是新分配的 BigInt，需要跟踪
        if is_arithmetic {
            self.track_temp_rc_value(result, &BolideType::BigInt);
        }

        Ok(result)
    }

    /// 编译 Decimal 二元操作
    fn compile_decimal_binop(&mut self, lhs: Value, op: &BinOp, rhs: Value) -> Result<Value, String> {
        // 算术运算返回新的 Decimal，需要跟踪为临时值
        let is_arithmetic = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod);

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

        // 算术运算的结果是新分配的 Decimal，需要跟踪
        if is_arithmetic {
            self.track_temp_rc_value(result, &BolideType::Decimal);
        }

        Ok(result)
    }

    /// 编译一元操作
    fn compile_unary(&mut self, op: &UnaryOp, operand: &Expr) -> Result<Value, String> {
        let operand_ty = self.infer_expr_type(operand);
        let is_float = matches!(operand_ty, BolideType::Float);
        let val = self.compile_expr(operand)?;

        let result = match op {
            UnaryOp::Neg => {
                if is_float {
                    self.builder.ins().fneg(val)
                } else {
                    self.builder.ins().ineg(val)
                }
            }
            UnaryOp::Not => {
                let zero = self.builder.ins().iconst(types::I64, 0);
                let is_zero = self.builder.ins().icmp(IntCC::Equal, val, zero);
                self.builder.ins().uextend(types::I64, is_zero)
            }
        };

        Ok(result)
    }

    /// 编译间接函数调用（通过函数指针调用）
    fn compile_indirect_call(
        &mut self,
        var_name: &str,
        args: &[Expr],
        func_sig: Option<(Vec<BolideType>, Option<Box<BolideType>>)>
    ) -> Result<Value, String> {
        // 获取函数指针
        let var = *self.variables.get(var_name)
            .ok_or_else(|| format!("Undefined function variable: {}", var_name))?;
        let func_ptr = self.builder.use_var(var);

        // 编译参数
        let mut arg_values = Vec::new();
        for arg in args {
            let val = self.compile_expr(arg)?;
            arg_values.push(val);
        }

        // 创建签名
        #[cfg(target_os = "windows")]
        let mut sig = Signature::new(CallConv::WindowsFastcall);
        #[cfg(not(target_os = "windows"))]
        let mut sig = Signature::new(CallConv::SystemV);

        // 使用签名中的参数类型
        if let Some((param_types, _)) = &func_sig {
            for ty in param_types {
                sig.params.push(AbiParam::new(self.bolide_type_to_cranelift(ty)));
            }
        } else {
            // 无签名时从参数推断
            for arg in args {
                let ty = self.infer_expr_type(arg);
                sig.params.push(AbiParam::new(self.bolide_type_to_cranelift(&ty)));
            }
        }

        // 使用签名中的返回类型
        if let Some((_, Some(ret_type))) = &func_sig {
            sig.returns.push(AbiParam::new(self.bolide_type_to_cranelift(ret_type)));
        } else {
            // 无返回类型时默认 i64
            sig.returns.push(AbiParam::new(types::I64));
        }

        let sig_ref = self.builder.import_signature(sig);
        let call = self.builder.ins().call_indirect(sig_ref, func_ptr, &arg_values);
        let result = self.builder.inst_results(call)[0];

        // 如果返回类型是 RC 类型，track 为临时值
        if let Some((_, Some(ret_type))) = &func_sig {
            if Self::is_rc_type(ret_type) {
                self.track_temp_rc_value(result, ret_type);
            }
        }

        Ok(result)
    }

    /// 编译函数调用
    fn compile_call(&mut self, callee: &Expr, args: &[Expr]) -> Result<Value, String> {
        // Intercept 'print' for Dynamic type
        if let Expr::Ident(name) = callee {
            if name == "print" && args.len() == 1 {
                if self.infer_expr_type(&args[0]) == BolideType::Dynamic {
                    let func = *self.func_refs.get("print_dynamic")
                        .ok_or("print_dynamic not found")?;
                    let val = self.compile_expr(&args[0])?;
                    self.builder.ins().call(func, &[val]);
                    return Ok(self.builder.ins().iconst(types::I64, 0));
                }
            }
        }

        // 检查是否是模块调用或方法调用 (obj.method(args))
        if let Expr::Member(base, member_name) = callee {
            // 先检查是否是模块调用
            if let Expr::Ident(module_name) = base.as_ref() {
                if self.modules.contains_key(module_name) {
                    // 模块调用: module.func() -> @module_func()
                    let func_name = format!("@{}_{}", module_name, member_name);
                    return self.compile_module_call(&func_name, args);
                }
            }
            // 不是模块调用，是方法调用
            return self.compile_method_call(base, member_name, args);
        }

        // 检查是否是间接调用（通过函数类型变量调用）
        if let Expr::Ident(name) = callee {
            // 检查是否是 func 类型的变量
            if let Some(var_type) = self.var_types.get(name).cloned() {
                match &var_type {
                    BolideType::Func => return self.compile_indirect_call(name, args, None),
                    BolideType::FuncSig(param_types, ret_type) => {
                        return self.compile_indirect_call(name, args, Some((param_types.clone(), ret_type.clone())));
                    }
                    _ => {}
                }
            }
        }

        let func_name = match callee {
            Expr::Ident(name) => name.clone(),
            Expr::Member(base, member) => {
                // 检查是否是模块调用: module.func()
                if let Expr::Ident(module_name) = base.as_ref() {
                    if self.modules.contains_key(module_name) {
                        // 转换为 @module_func
                        format!("@{}_{}", module_name, member)
                    } else {
                        // 不是模块，是方法调用
                        return self.compile_method_call(base, member, args);
                    }
                } else {
                    return self.compile_method_call(base, member, args);
                }
            }
            _ => return Err("Only direct function calls are supported".to_string()),
        };

        // 处理类型转换函数和特殊函数
        match func_name.as_str() {
            "int" => return self.compile_type_conversion_to_int(args),
            "float" => return self.compile_type_conversion_to_float(args),
            "str" => return self.compile_type_conversion_to_str(args),
            "bigint" => return self.compile_type_conversion_to_bigint(args),
            "decimal" => return self.compile_type_conversion_to_decimal(args),

            // 通用 print 函数 - 根据参数类型自动选择
            "print" => {
                if args.len() != 1 {
                    return Err("print expects 1 argument".to_string());
                }
                return self.compile_print(&args[0]);
            }
            // join 函数 - 等待线程/任务完成
            "join" => {
                if args.len() != 1 {
                    return Err("join expects 1 argument".to_string());
                }
                return self.compile_join(&args[0]);
            }
            // channel 函数 - 创建通道
            "channel" => {
                return self.compile_channel_create(args);
            }
            // bigint_debug_stats - 调试用
            "bigint_debug_stats" => {
                let func_ref = *self.func_refs.get("bigint_debug_stats")
                    .ok_or("bigint_debug_stats not found")?;
                self.builder.ins().call(func_ref, &[]);
                return Ok(self.builder.ins().iconst(types::I64, 0));
            }
            // input 函数 - 读取用户输入
            "input" => {
                return self.compile_input(args);
            }
            _ => {}

        }

        // 检查是否是 async 函数
        if self.async_funcs.contains(&func_name) {
            return self.compile_async_call(&func_name, args);
        }

        // 检查是否是 extern 函数
        if let Some((lib_path, extern_func)) = self.extern_funcs.get(&func_name).cloned() {
            return self.compile_extern_call(&lib_path, &extern_func, args);
        }

        let func_ref = *self.func_refs.get(&func_name)
            .ok_or_else(|| format!("Undefined function: {}", func_name))?;

        // 获取函数参数信息
        let param_modes: Vec<ParamMode> = self.func_params.get(&func_name)
            .map(|params| params.iter().map(|p| p.mode).collect())
            .unwrap_or_else(|| vec![ParamMode::Borrow; args.len()]);

        let mut arg_values = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let mode = param_modes.get(i).copied().unwrap_or(ParamMode::Borrow);

            match mode {
                ParamMode::Borrow => {
                    // 直接传值
                    arg_values.push(self.compile_expr(arg)?);
                }
                ParamMode::Owned => {
                    // 传值，然后标记变量为已移动
                    let val = self.compile_expr(arg)?;
                    arg_values.push(val);

                    // 如果参数是变量，标记为已移动并置空
                    if let Expr::Ident(var_name) = arg {
                        self.moved_variables.insert(var_name.clone());
                        // 置空变量（设为 null）
                        if let Some(&var) = self.variables.get(var_name) {
                            let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                            self.builder.def_var(var, null_val);
                        }
                        // 从 rc_variables 中移除（不再需要在作用域结束时释放）
                        self.rc_variables.retain(|(n, _)| n != var_name);
                    } else {
                        // 临时值作为 Owned 参数，所有权转移，从临时列表移除
                        self.remove_temp_rc_value(val);
                    }
                }
                ParamMode::Ref => {
                    // 传递变量的栈地址
                    if let Expr::Ident(var_name) = arg {
                        // 需要在栈上分配空间，存储变量值，然后传递地址
                        let var = *self.variables.get(var_name)
                            .ok_or_else(|| format!("Undefined variable for ref: {}", var_name))?;
                        let current_val = self.builder.use_var(var);

                        // 创建栈槽存储变量值
                        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                            StackSlotKind::ExplicitSlot,
                            8,  // 指针大小
                            0,
                        ));
                        let slot_addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);

                        // 将当前值存入栈槽
                        self.builder.ins().store(MemFlags::new(), current_val, slot_addr, 0);

                        arg_values.push(slot_addr);

                        // 注意：函数返回后需要从栈槽读回新值
                        // 这需要在 call 之后处理
                    } else {
                        return Err("ref parameter must be a variable".to_string());
                    }
                }
            }
        }

        let call = self.builder.ins().call(func_ref, &arg_values);

        // 检查是否是生命周期函数
        let is_lifetime_func = self.lifetime_funcs.contains(&func_name);

        // 处理 Ref 参数：从栈槽读回新值
        // 对于生命周期函数，跳过释放旧值（因为返回值可能就是参数本身）
        for (i, arg) in args.iter().enumerate() {
            let mode = param_modes.get(i).copied().unwrap_or(ParamMode::Borrow);
            if mode == ParamMode::Ref {
                if let Expr::Ident(var_name) = arg {
                    // arg_values[i] 是栈槽地址，从中读取新值
                    let slot_addr = arg_values[i];
                    let new_val = self.builder.ins().load(self.ptr_type, MemFlags::new(), slot_addr, 0);

                    if let Some(&var) = self.variables.get(var_name) {
                        // 释放旧值（调用者原本拥有的对象）
                        // 但对于生命周期函数，跳过释放（返回值可能就是参数本身）
                        if !is_lifetime_func {
                            if let Some(var_ty) = self.var_types.get(var_name).cloned() {
                                if Self::is_rc_type(&var_ty) {
                                    if let Some(func_name) = Self::get_release_func_name(&var_ty) {
                                        if let Some(&func_ref) = self.func_refs.get(func_name) {
                                            let old_val = self.builder.use_var(var);
                                            self.builder.ins().call(func_ref, &[old_val]);
                                        }
                                    }
                                }
                            }
                        }
                        // 更新为新值
                        self.builder.def_var(var, new_val);
                    }
                }
            }
        }

        let results = self.builder.inst_results(call);
        if results.is_empty() {
            Ok(self.builder.ins().iconst(types::I64, 0))
        } else {
            let result = results[0];

            // 如果函数返回 RC 类型，跟踪为临时值
            // 但对于生命周期函数，跳过（返回的是借用而非拥有的值）
            if !is_lifetime_func {
                if let Some(Some(ret_ty)) = self.func_return_types.get(&func_name).cloned() {
                    if Self::is_rc_type(&ret_ty) {
                        self.track_temp_rc_value(result, &ret_ty);
                    }
                }
            }

            Ok(result)
        }
    }

    /// 类型转换: int(x) - 支持 int, float, str, bigint, decimal
    fn compile_type_conversion_to_int(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("int() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        match arg_type {
            BolideType::Int => Ok(val),  // 恒等转换
            BolideType::Float => {
                // float -> int: 截断
                Ok(self.builder.ins().fcvt_to_sint(types::I64, val))
            }
            BolideType::Str => {
                // str -> int: 调用 string_to_int
                let func_ref = *self.func_refs.get("string_to_int")
                    .ok_or("string_to_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::BigInt => {
                // bigint -> int: 调用 bigint_to_i64
                let func_ref = *self.func_refs.get("bigint_to_i64")
                    .ok_or("bigint_to_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Decimal => {
                // decimal -> int: 调用 decimal_to_i64
                let func_ref = *self.func_refs.get("decimal_to_i64")
                    .ok_or("decimal_to_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Err(format!("Cannot convert {:?} to int", arg_type))
        }
    }

    /// 类型转换: float(x) - 支持 int, float, str, decimal
    fn compile_type_conversion_to_float(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("float() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        match arg_type {
            BolideType::Float => Ok(val),  // 恒等转换
            BolideType::Int => {
                // int -> float
                Ok(self.builder.ins().fcvt_from_sint(types::F64, val))
            }
            BolideType::Str => {
                // str -> float: 调用 string_to_float
                let func_ref = *self.func_refs.get("string_to_float")
                    .ok_or("string_to_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Decimal => {
                // decimal -> float: 调用 decimal_to_f64
                let func_ref = *self.func_refs.get("decimal_to_f64")
                    .ok_or("decimal_to_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Err(format!("Cannot convert {:?} to float", arg_type))
        }
    }

    /// 类型转换: str(x) - 支持 int, float, bool, str, bigint, decimal
    fn compile_type_conversion_to_str(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("str() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        let result = match arg_type {
            BolideType::Str => return Ok(val),  // 恒等转换
            BolideType::Int => {
                let func_ref = *self.func_refs.get("string_from_int")
                    .ok_or("string_from_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::Float => {
                let func_ref = *self.func_refs.get("string_from_float")
                    .ok_or("string_from_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::Bool => {
                let func_ref = *self.func_refs.get("string_from_bool")
                    .ok_or("string_from_bool not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::BigInt => {
                let func_ref = *self.func_refs.get("string_from_bigint")
                    .ok_or("string_from_bigint not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::Decimal => {
                let func_ref = *self.func_refs.get("string_from_decimal")
                    .ok_or("string_from_decimal not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            _ => return Err(format!("Cannot convert {:?} to str", arg_type))
        };

        // 返回的字符串需要 RC 跟踪
        self.track_temp_rc_value(result, &BolideType::Str);
        Ok(result)
    }

    /// 类型转换: bigint(x) - 支持 int
    fn compile_type_conversion_to_bigint(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("bigint() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        match arg_type {
            BolideType::BigInt => Ok(val),  // 恒等转换
            BolideType::Int => {
                let func_ref = *self.func_refs.get("bigint_from_i64")
                    .ok_or("bigint_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::BigInt);
                Ok(result)
            }
            _ => Err(format!("Cannot convert {:?} to bigint", arg_type))
        }
    }

    /// 类型转换: decimal(x) - 支持 int, float
    fn compile_type_conversion_to_decimal(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("decimal() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        match arg_type {
            BolideType::Decimal => Ok(val),  // 恒等转换
            BolideType::Int => {
                let func_ref = *self.func_refs.get("decimal_from_i64")
                    .ok_or("decimal_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Decimal);
                Ok(result)
            }
            BolideType::Float => {
                let func_ref = *self.func_refs.get("decimal_from_f64")
                    .ok_or("decimal_from_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Decimal);
                Ok(result)
            }
            _ => Err(format!("Cannot convert {:?} to decimal", arg_type))
        }
    }

    /// 编译通用 print 函数 - 根据表达式类型自动选择打印函数
    fn compile_print(&mut self, expr: &Expr) -> Result<Value, String> {
        let expr_type = self.infer_expr_type(expr);
        let val = self.compile_expr(expr)?;

        let func_name = match expr_type {
            BolideType::Int => "print_int",
            BolideType::Float => "print_float",
            BolideType::Bool => "print_int",  // bool 用 int 打印
            BolideType::BigInt => "print_bigint",
            BolideType::Decimal => "print_decimal",
            BolideType::Str => "print_string",
            BolideType::Dynamic => "print_dynamic",
            BolideType::Tuple(_) => "print_tuple",
            BolideType::List(_) => "print_list",
            BolideType::Dict(_, _) => "print_dict",

            _ => "print_int",  // 默认用 int 打印
        };


        let func_ref = *self.func_refs.get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        self.builder.ins().call(func_ref, &[val]);
        Ok(self.builder.ins().iconst(types::I64, 0))
    }

    /// 编译 input 函数 - 读取用户输入
    fn compile_input(&mut self, args: &[Expr]) -> Result<Value, String> {
        let result = if args.is_empty() {
            // 无参数版本: input()
            let func_ref = *self.func_refs.get("input")
                .ok_or("input not found")?;
            let call = self.builder.ins().call(func_ref, &[]);
            self.builder.inst_results(call)[0]
        } else if args.len() == 1 {
            // 带提示版本: input("prompt")
            let prompt = self.compile_expr(&args[0])?;
            let func_ref = *self.func_refs.get("input_prompt")
                .ok_or("input_prompt not found")?;
            let call = self.builder.ins().call(func_ref, &[prompt]);
            self.builder.inst_results(call)[0]
        } else {
            return Err("input expects 0 or 1 argument".to_string());
        };

        // 返回的字符串需要 RC 跟踪
        self.track_temp_rc_value(result, &BolideType::Str);
        Ok(result)
    }

    /// 推断表达式类型
    fn infer_expr_type(&self, expr: &Expr) -> BolideType {
        match expr {
            Expr::Int(_) => BolideType::Int,
            Expr::Float(_) => BolideType::Float,
            Expr::Bool(_) => BolideType::Bool,
            Expr::String(_) => BolideType::Str,
            Expr::BigInt(_) => BolideType::BigInt,
            Expr::Decimal(_) => BolideType::Decimal,
            Expr::None => BolideType::Int,
            Expr::Spawn(_, _) => BolideType::Future,
            Expr::Recv(_) => BolideType::Int,
            Expr::Ident(name) => {
                // 查找变量类型
                self.var_types.get(name).cloned().unwrap_or(BolideType::Int)
            }
            Expr::BinOp(left, op, right) => {
                let left_ty = self.infer_expr_type(left);
                let right_ty = self.infer_expr_type(right);
                // 类型提升规则
                match (&left_ty, &right_ty) {
                    (BolideType::Str, BolideType::Str) => {
                        match op {
                            BinOp::Add => BolideType::Str,
                            BinOp::Eq | BinOp::Ne => BolideType::Bool,
                            _ => BolideType::Int,
                        }
                    }
                    (BolideType::Float, _) | (_, BolideType::Float) => BolideType::Float,
                    (BolideType::BigInt, _) | (_, BolideType::BigInt) => BolideType::BigInt,
                    (BolideType::Decimal, _) | (_, BolideType::Decimal) => BolideType::Decimal,
                    _ => match op {
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                        | BinOp::And | BinOp::Or => BolideType::Bool,
                        _ => BolideType::Int,
                    }
                }
            }
            Expr::UnaryOp(op, operand) => {
                match op {
                    UnaryOp::Not => BolideType::Bool,
                    UnaryOp::Neg => self.infer_expr_type(operand),
                }
            }
            Expr::Call(callee, args) => {
                // 根据函数名推断返回类型
                if let Expr::Ident(name) = callee.as_ref() {
                    match name.as_str() {
                        "bigint" => BolideType::BigInt,
                        "decimal" => BolideType::Decimal,
                        "int" => BolideType::Int,
                        "float" => BolideType::Float,
                        "str" => BolideType::Str,  // str 函数返回字符串
                        "channel" => BolideType::Channel(Box::new(BolideType::Int)),  // 默认 int，实际类型从声明获取
                        "input" => BolideType::Str,  // input 函数返回字符串
                        "join" => {
                            // 从 spawn_func_map 获取原函数的返回类型
                            if args.len() == 1 {
                                if let Expr::Ident(var_name) = &args[0] {
                                    if let Some(func_name) = self.spawn_func_map.get(var_name) {
                                        if let Some(Some(ret_ty)) = self.func_return_types.get(func_name) {
                                            return ret_ty.clone();
                                        }
                                    }
                                }
                            }
                            BolideType::Int // 默认
                        }
                        _ => {
                            // 查找用户定义函数的返回类型
                            if let Some(Some(ret_ty)) = self.func_return_types.get(name.as_str()) {
                                ret_ty.clone()
                            } else {
                                BolideType::Int
                            }
                        }
                    }
                } else if let Expr::Member(base, method) = callee.as_ref() {
                    let base_ty = self.infer_expr_type(base);
                    match base_ty {
                        BolideType::Dict(k, v) => {
                             match method.as_str() {
                                 "keys" => BolideType::List(k),
                                 "values" => BolideType::List(v),
                                 "get" | "remove" => *v,
                                 "clone" => BolideType::Dict(k, v),
                                 "len" | "is_empty" | "contains" => BolideType::Int,
                                 _ => BolideType::Int,
                             }
                        }
                        BolideType::List(elem) => {
                             match method.as_str() {
                                 "pop" | "get" | "first" | "last" => *elem,
                                 "slice" | "copy" | "clone" => BolideType::List(elem),
                                 "len" | "index_of" | "count" | "is_empty" => BolideType::Int,
                                 _ => BolideType::Int
                             }
                        }
                        _ => BolideType::Int
                    }
                } else {
                    BolideType::Int
                }
            }
            Expr::Member(base, member) => {
                // 获取基础表达式的类型，然后查找字段类型
                let base_ty = self.infer_expr_type(base);
                if let BolideType::Custom(class_name) = base_ty {
                    if let Some(class_info) = self.classes.get(&class_name) {
                        if let Some(field) = class_info.fields.iter().find(|f| f.name == *member) {
                            return field.ty.clone();
                        }
                    }
                }
                BolideType::Int
            }
            Expr::Tuple(exprs) => {
                let elem_types: Vec<BolideType> = exprs.iter()
                    .map(|e| self.infer_expr_type(e))
                    .collect();
                BolideType::Tuple(elem_types)
            }
            Expr::Index(base, idx) => {
                let base_ty = self.infer_expr_type(base);
                match base_ty {
                    BolideType::Tuple(elem_types) => {
                        // 根据索引获取对应元素类型
                        if let Expr::Int(i) = idx.as_ref() {
                            let index = *i as usize;
                            elem_types.get(index).cloned().unwrap_or(BolideType::Int)
                        } else {
                            // 动态索引，返回第一个元素类型作为默认
                            elem_types.first().cloned().unwrap_or(BolideType::Int)
                        }
                    }
                    BolideType::List(elem_ty) => *elem_ty,
                    BolideType::Dict(_, val_ty) => *val_ty,
                    _ => BolideType::Int,
                }
            }
            Expr::Await(inner_expr) => {
                // await 表达式返回协程的返回类型
                if let Expr::Ident(var_name) = inner_expr.as_ref() {
                    // 从 spawn_func_map 查找对应的函数名
                    if let Some(func_name) = self.spawn_func_map.get(var_name) {
                        // 从 func_return_types 获取返回类型
                        self.func_return_types.get(func_name)
                            .cloned()
                            .flatten()
                            .unwrap_or(BolideType::Int)
                    } else {
                        BolideType::Int
                    }
                } else {
                    BolideType::Int
                }
            }
            Expr::List(items) => {
                let item_type = if items.is_empty() {
                    BolideType::Int
                } else {
                    let mut inferred = self.infer_expr_type(&items[0]);
                    for item in items.iter().skip(1) {
                        let next = self.infer_expr_type(item);
                        if inferred != next {
                            inferred = BolideType::Dynamic;
                        }
                    }
                    inferred
                };
                BolideType::List(Box::new(item_type))
            }
            Expr::Dict(entries) => {
                let (k_type, v_type) = if entries.is_empty() {
                    (BolideType::Int, BolideType::Int)
                } else {
                    let mut k_ty = self.infer_expr_type(&entries[0].0);
                    let mut v_ty = self.infer_expr_type(&entries[0].1);
                    for (k, v) in entries.iter().skip(1) {
                        let next_k = self.infer_expr_type(k);
                        if k_ty != next_k { k_ty = BolideType::Dynamic; }
                        let next_v = self.infer_expr_type(v);
                        if v_ty != next_v { v_ty = BolideType::Dynamic; }
                    }
                    (k_ty, v_ty)
                };
                BolideType::Dict(Box::new(k_type), Box::new(v_type))
            }
            _ => BolideType::Int,
        }
    }

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
            BolideType::Func => self.ptr_type,  // 函数指针
            BolideType::FuncSig(_, _) => self.ptr_type,  // 带签名的函数指针
            BolideType::List(_) => self.ptr_type,
            BolideType::Dict(_, _) => self.ptr_type,
            BolideType::Tuple(_) => self.ptr_type,  // 元组作为指针

            BolideType::Custom(_) => self.ptr_type,
            BolideType::Weak(inner) => self.bolide_type_to_cranelift(inner),
            BolideType::Unowned(inner) => self.bolide_type_to_cranelift(inner),
        }
    }

    /// 编译 pool 语句
    fn compile_pool(&mut self, pool_stmt: &bolide_parser::PoolStmt) -> Result<(), String> {
        // 计算线程池大小
        let size = self.compile_expr(&pool_stmt.size)?;

        // 创建线程池: pool_create(size) -> ptr
        let pool_create_ref = *self.func_refs.get("pool_create")
            .ok_or("pool_create not found")?;
        let call = self.builder.ins().call(pool_create_ref, &[size]);
        let pool_ptr = self.builder.inst_results(call)[0];

        // 进入线程池上下文: pool_enter(pool)
        let pool_enter_ref = *self.func_refs.get("pool_enter")
            .ok_or("pool_enter not found")?;
        self.builder.ins().call(pool_enter_ref, &[pool_ptr]);

        // 编译 pool 块内的语句
        for stmt in &pool_stmt.body {
            self.compile_stmt(stmt)?;
        }

        // 退出线程池上下文: pool_exit()
        let pool_exit_ref = *self.func_refs.get("pool_exit")
            .ok_or("pool_exit not found")?;
        self.builder.ins().call(pool_exit_ref, &[]);

        // 销毁线程池: pool_destroy(pool)
        let pool_destroy_ref = *self.func_refs.get("pool_destroy")
            .ok_or("pool_destroy not found")?;
        self.builder.ins().call(pool_destroy_ref, &[pool_ptr]);

        Ok(())
    }

    /// 编译 send 语句: ch <- value
    fn compile_send(&mut self, send_stmt: &bolide_parser::SendStmt) -> Result<(), String> {
        // 获取通道变量
        let channel_var = *self.variables.get(&send_stmt.channel)
            .ok_or_else(|| format!("Undefined channel: {}", send_stmt.channel))?;
        let channel_ptr = self.builder.use_var(channel_var);

        // 编译要发送的值
        let value = self.compile_expr(&send_stmt.value)?;

        // 调用 channel_send(channel, value)
        let channel_send_ref = *self.func_refs.get("channel_send")
            .ok_or("channel_send not found")?;
        self.builder.ins().call(channel_send_ref, &[channel_ptr, value]);

        Ok(())
    }

    /// 编译 select 语句
    fn compile_select(&mut self, select_stmt: &bolide_parser::SelectStmt) -> Result<(), String> {
        use bolide_parser::SelectBranch;

        // 收集 recv 分支的 channel 和相关信息
        let mut recv_branches: Vec<(&str, &str, &Vec<bolide_parser::Statement>)> = Vec::new();
        let mut timeout_branch: Option<(&Expr, &Vec<bolide_parser::Statement>)> = None;
        let mut default_branch: Option<&Vec<bolide_parser::Statement>> = None;

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

        let channel_count = recv_branches.len();
        if channel_count == 0 {
            // 没有 recv 分支，只执行 default 或 timeout
            if let Some(body) = default_branch {
                for stmt in body {
                    self.compile_stmt(stmt)?;
                }
            }
            return Ok(());
        }

        // 在栈上分配 channel 指针数组
        let array_size = (channel_count * 8) as i32;
        let stack_slot = self.builder.create_sized_stack_slot(cranelift::prelude::StackSlotData::new(
            cranelift::prelude::StackSlotKind::ExplicitSlot,
            array_size as u32,
            0,
        ));
        let array_ptr = self.builder.ins().stack_addr(self.ptr_type, stack_slot, 0);

        // 填充 channel 指针数组
        for (i, (_, channel_name, _)) in recv_branches.iter().enumerate() {
            let channel_var = *self.variables.get(*channel_name)
                .ok_or_else(|| format!("Undefined channel: {}", channel_name))?;
            let channel_ptr = self.builder.use_var(channel_var);
            let offset = (i * 8) as i32;
            self.builder.ins().store(MemFlags::new(), channel_ptr, array_ptr, offset);
        }

        // 分配接收值的栈空间
        let value_slot = self.builder.create_sized_stack_slot(cranelift::prelude::StackSlotData::new(
            cranelift::prelude::StackSlotKind::ExplicitSlot,
            8,
            0,
        ));
        let value_ptr = self.builder.ins().stack_addr(self.ptr_type, value_slot, 0);

        // 确定 timeout 值
        let timeout_val = if default_branch.is_some() {
            self.builder.ins().iconst(types::I64, -2)  // has default
        } else if let Some((duration_expr, _)) = &timeout_branch {
            self.compile_expr(duration_expr)?
        } else {
            self.builder.ins().iconst(types::I64, -1)  // no timeout
        };

        // 调用 bolide_channel_select
        let select_ref = *self.func_refs.get("channel_select")
            .ok_or("channel_select not found")?;
        let count_val = self.builder.ins().iconst(types::I64, channel_count as i64);
        let call = self.builder.ins().call(select_ref, &[array_ptr, count_val, timeout_val, value_ptr]);
        let results = self.builder.inst_results(call);
        let selected_idx = results[0];

        // 创建各分支的基本块
        let exit_block = self.builder.create_block();
        let mut branch_blocks: Vec<Block> = Vec::new();
        for _ in 0..channel_count {
            branch_blocks.push(self.builder.create_block());
        }
        let timeout_block = if timeout_branch.is_some() {
            Some(self.builder.create_block())
        } else {
            None
        };
        let default_block = if default_branch.is_some() {
            Some(self.builder.create_block())
        } else {
            None
        };

        // 生成分支跳转逻辑
        self.compile_select_dispatch(
            selected_idx,
            &branch_blocks,
            timeout_block,
            default_block,
            exit_block,
        )?;

        // 编译各 recv 分支
        for (i, (var_name, _, body)) in recv_branches.iter().enumerate() {
            self.builder.switch_to_block(branch_blocks[i]);
            self.builder.seal_block(branch_blocks[i]);

            // 从栈上读取接收到的值
            let received_val = self.builder.ins().load(types::I64, MemFlags::new(), value_ptr, 0);

            // 声明或获取变量
            let var = if let Some(&existing) = self.variables.get(*var_name) {
                self.builder.def_var(existing, received_val);
                existing
            } else {
                let new_var = self.declare_variable(var_name, types::I64);
                self.builder.def_var(new_var, received_val);
                new_var
            };
            let _ = var;

            // 编译分支体
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
        if let (Some(block), Some(body)) = (default_block, default_branch) {
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

    /// 生成 select 分支跳转逻辑
    fn compile_select_dispatch(
        &mut self,
        selected_idx: Value,
        branch_blocks: &[Block],
        timeout_block: Option<Block>,
        default_block: Option<Block>,
        exit_block: Block,
    ) -> Result<(), String> {
        // 逐个比较索引并跳转
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
        if let Some(block) = default_block {
            let default_val = self.builder.ins().iconst(types::I64, -2);
            let is_default = self.builder.ins().icmp(IntCC::Equal, selected_idx, default_val);
            self.builder.ins().brif(is_default, block, &[], exit_block, &[]);
        } else {
            self.builder.ins().jump(exit_block, &[]);
        }

        Ok(())
    }

    /// 编译 spawn 表达式
    fn compile_spawn(&mut self, func_name: &str, args: &[Expr]) -> Result<Value, String> {
        // 获取目标函数的返回类型，确定 spawn 函数后缀
        let return_type = self.func_return_types.get(func_name).cloned().unwrap_or(None);
        let type_suffix = match &return_type {
            Some(BolideType::Float) => "_float",
            Some(BolideType::Str) | Some(BolideType::BigInt) | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic) | Some(BolideType::Ptr)
            | Some(BolideType::List(_)) | Some(BolideType::Custom(_)) => "_ptr",
            _ => "_int", // Int, Bool, None 都用 int
        };

        // 根据是否有参数选择不同的路径
        let (func_addr, env_ptr) = if args.is_empty() {
            // 无参数：直接使用目标函数
            let target_func_ref = *self.func_refs.get(func_name)
                .ok_or_else(|| format!("Undefined function: {}", func_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, target_func_ref);
            let null_ptr = self.builder.ins().iconst(self.ptr_type, 0);
            (func_addr, null_ptr)
        } else {
            // 有参数：使用 trampoline
            let trampoline_ref = *self.trampoline_refs.get(func_name)
                .ok_or_else(|| format!("No trampoline for function: {}", func_name))?;
            let param_types = self.trampoline_param_types.get(func_name)
                .ok_or_else(|| format!("No param types for trampoline: {}", func_name))?
                .clone();
            let env_size = *self.trampoline_env_sizes.get(func_name)
                .ok_or_else(|| format!("No env size for trampoline: {}", func_name))?;

            // 分配 env 内存
            let alloc_ref = *self.func_refs.get("bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(alloc_call)[0];

            // 将参数存储到 env
            // 对于 RC 类型，需要 clone 后传给子线程（跨线程安全）
            for (i, arg) in args.iter().enumerate() {
                let val = self.compile_expr(arg)?;
                let offset = (i * 8) as i32;
                let bolide_type = &param_types[i];

                // 对 RC 类型进行 clone
                let val_to_store = if Self::is_rc_type(bolide_type) {
                    if let Some(clone_func) = Self::get_clone_func_name(bolide_type) {
                        if let Some(clone_ref) = self.func_refs.get(clone_func) {
                            let call = self.builder.ins().call(*clone_ref, &[val]);
                            self.builder.inst_results(call)[0]
                        } else {
                            val // 没有 clone 函数引用，直接使用
                        }
                    } else {
                        val // 没有 clone 函数名，直接使用
                    }
                } else {
                    val
                };

                self.builder.ins().store(MemFlags::trusted(), val_to_store, env_ptr, offset);
            }

            // 获取 trampoline 函数地址
            let func_addr = self.builder.ins().func_addr(self.ptr_type, trampoline_ref);
            (func_addr, env_ptr)
        };

        // 检查是否在线程池上下文中
        let pool_is_active_ref = *self.func_refs.get("pool_is_active")
            .ok_or("pool_is_active not found")?;
        let is_active_call = self.builder.ins().call(pool_is_active_ref, &[]);
        let is_active = self.builder.inst_results(is_active_call)[0];

        // 创建分支块
        let pool_block = self.builder.create_block();
        let thread_block = self.builder.create_block();
        let merge_block = self.builder.create_block();
        self.builder.append_block_param(merge_block, self.ptr_type);

        self.builder.ins().brif(is_active, pool_block, &[], thread_block, &[]);

        // 根据是否有参数选择 spawn 函数
        let spawn_suffix = if args.is_empty() {
            type_suffix.to_string()
        } else {
            format!("{}_with_env", type_suffix)
        };

        // 线程池分支
        self.builder.switch_to_block(pool_block);
        self.builder.seal_block(pool_block);
        let pool_spawn_name = format!("pool_spawn{}", spawn_suffix);
        let pool_spawn_ref = *self.func_refs.get(&pool_spawn_name)
            .ok_or_else(|| format!("{} not found", pool_spawn_name))?;
        let pool_call = if args.is_empty() {
            self.builder.ins().call(pool_spawn_ref, &[func_addr])
        } else {
            self.builder.ins().call(pool_spawn_ref, &[func_addr, env_ptr])
        };
        let pool_handle = self.builder.inst_results(pool_call)[0];
        self.builder.ins().jump(merge_block, &[pool_handle]);

        // 普通线程分支
        self.builder.switch_to_block(thread_block);
        self.builder.seal_block(thread_block);
        let thread_spawn_name = format!("thread_spawn{}", spawn_suffix);
        let thread_spawn_ref = *self.func_refs.get(&thread_spawn_name)
            .ok_or_else(|| format!("{} not found", thread_spawn_name))?;
        let thread_call = if args.is_empty() {
            self.builder.ins().call(thread_spawn_ref, &[func_addr])
        } else {
            self.builder.ins().call(thread_spawn_ref, &[func_addr, env_ptr])
        };
        let thread_handle = self.builder.inst_results(thread_call)[0];
        self.builder.ins().jump(merge_block, &[thread_handle]);

        // 合并块
        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);
        let result_handle = self.builder.block_params(merge_block)[0];

        Ok(result_handle)
    }

    /// 编译 recv 表达式: <- ch
    fn compile_recv(&mut self, channel_name: &str) -> Result<Value, String> {
        // 获取通道变量
        let channel_var = *self.variables.get(channel_name)
            .ok_or_else(|| format!("Undefined channel: {}", channel_name))?;
        let channel_ptr = self.builder.use_var(channel_var);

        // 调用 channel_recv(channel) -> i64
        let channel_recv_ref = *self.func_refs.get("channel_recv")
            .ok_or("channel_recv not found")?;
        let call = self.builder.ins().call(channel_recv_ref, &[channel_ptr]);
        let value = self.builder.inst_results(call)[0];

        Ok(value)
    }

    /// 编译 async 函数调用 - 启动协程并返回 Future
    fn compile_async_call(&mut self, func_name: &str, args: &[Expr]) -> Result<Value, String> {
        // 获取返回类型确定 spawn 函数后缀
        let return_type = self.func_return_types.get(func_name).cloned().unwrap_or(None);
        let type_suffix = match &return_type {
            Some(BolideType::Float) => "_float",
            Some(BolideType::Str) | Some(BolideType::BigInt) | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic) | Some(BolideType::Ptr)
            | Some(BolideType::List(_)) | Some(BolideType::Custom(_)) => "_ptr",
            _ => "_int",
        };

        // 获取函数地址和环境指针
        let (func_addr, env_ptr) = if args.is_empty() {
            let target_func_ref = *self.func_refs.get(func_name)
                .ok_or_else(|| format!("Undefined async function: {}", func_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, target_func_ref);
            let null_ptr = self.builder.ins().iconst(self.ptr_type, 0);
            (func_addr, null_ptr)
        } else {
            // 有参数：使用 trampoline
            let trampoline_ref = *self.trampoline_refs.get(func_name)
                .ok_or_else(|| format!("No trampoline for async function: {}", func_name))?;
            let param_types = self.trampoline_param_types.get(func_name)
                .ok_or_else(|| format!("No param types for trampoline: {}", func_name))?
                .clone();
            let env_size = *self.trampoline_env_sizes.get(func_name)
                .ok_or_else(|| format!("No env size for trampoline: {}", func_name))?;

            // 分配 env 内存
            let alloc_ref = *self.func_refs.get("bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(alloc_call)[0];

            // 存储参数到 env
            for (i, arg) in args.iter().enumerate() {
                let val = self.compile_expr(arg)?;
                let offset = (i * 8) as i32;
                self.builder.ins().store(MemFlags::trusted(), val, env_ptr, offset);
            }

            let func_addr = self.builder.ins().func_addr(self.ptr_type, trampoline_ref);
            (func_addr, env_ptr)
        };

        // 调用 coroutine_spawn_* 启动协程
        let (spawn_func_name, call) = if args.is_empty() {
            let spawn_func_name = format!("coroutine_spawn{}", type_suffix);
            let spawn_ref = *self.func_refs.get(&spawn_func_name)
                .ok_or_else(|| format!("{} not found", spawn_func_name))?;
            let call = self.builder.ins().call(spawn_ref, &[func_addr]);
            (spawn_func_name, call)
        } else {
            let spawn_func_name = format!("coroutine_spawn{}_with_env", type_suffix);
            let spawn_ref = *self.func_refs.get(&spawn_func_name)
                .ok_or_else(|| format!("{} not found", spawn_func_name))?;
            let call = self.builder.ins().call(spawn_ref, &[func_addr, env_ptr]);
            (spawn_func_name, call)
        };
        let _ = spawn_func_name; // 避免警告
        let future_ptr = self.builder.inst_results(call)[0];

        // 注册 Future 到当前 scope（如果在 scope 内）
        let scope_register = *self.func_refs.get("scope_register")
            .ok_or("scope_register not found")?;
        self.builder.ins().call(scope_register, &[future_ptr]);

        Ok(future_ptr)
    }

    /// 编译 await 表达式
    fn compile_await(&mut self, inner_expr: &Expr) -> Result<Value, String> {
        // 编译内部表达式，应该返回 Future 指针
        let future_ptr = self.compile_expr(inner_expr)?;

        // 获取协程的返回类型（不是 Future 类型，而是 await 后的结果类型）
        let await_expr = Expr::Await(Box::new(inner_expr.clone()));
        let expr_type = self.infer_expr_type(&await_expr);

        let await_func_name = match &expr_type {
            BolideType::Float => "coroutine_await_float",
            BolideType::Str | BolideType::BigInt | BolideType::Decimal
            | BolideType::List(_) | BolideType::Custom(_) => "coroutine_await_ptr",
            _ => "coroutine_await_int",
        };

        let await_ref = *self.func_refs.get(await_func_name)
            .ok_or_else(|| format!("{} not found", await_func_name))?;

        let call = self.builder.ins().call(await_ref, &[future_ptr]);
        let result = self.builder.inst_results(call)[0];

        // 释放 Future
        let free_ref = *self.func_refs.get("coroutine_free")
            .ok_or("coroutine_free not found")?;
        self.builder.ins().call(free_ref, &[future_ptr]);

        // 标记结果为临时 RC 值（调用者接管所有权）
        self.track_temp_rc_value(result, &expr_type);

        Ok(result)
    }

    /// 编译元组字面量
    fn compile_tuple(&mut self, exprs: &[Expr]) -> Result<Value, String> {
        if exprs.is_empty() {
            return Ok(self.builder.ins().iconst(self.ptr_type, 0));
        }

        // 调用 tuple_new 创建元组
        let tuple_new = *self.func_refs.get("tuple_new")
            .ok_or("tuple_new not found")?;
        let len = self.builder.ins().iconst(types::I64, exprs.len() as i64);
        let call = self.builder.ins().call(tuple_new, &[len]);
        let tuple_ptr = self.builder.inst_results(call)[0];

        // 编译并设置每个元素
        let tuple_set = *self.func_refs.get("tuple_set")
            .ok_or("tuple_set not found")?;
        for (i, expr) in exprs.iter().enumerate() {
            let val = self.compile_expr(expr)?;
            let idx = self.builder.ins().iconst(types::I64, i as i64);
            self.builder.ins().call(tuple_set, &[tuple_ptr, idx, val]);
        }

        Ok(tuple_ptr)
    }

    /// 编译列表字面量 [a, b, c]
    fn compile_list(&mut self, items: &[Expr]) -> Result<Value, String> {
        // 确定元素类型（默认 int = 0）
        let elem_type = if items.is_empty() {
            0u8 // int
        } else {
            match self.infer_expr_type(&items[0]) {
                BolideType::Int => 0,
                BolideType::Float => 1,
                BolideType::Bool => 2,
                BolideType::Str => 3,
                BolideType::BigInt => 4,
                BolideType::Decimal => 5,
                _ => 0, // default to int
            }
        };

        // 调用 list_new(elem_type) 创建列表
        let list_new = *self.func_refs.get("list_new")
            .ok_or("list_new not found")?;
        let elem_type_val = self.builder.ins().iconst(types::I8, elem_type as i64);
        let call = self.builder.ins().call(list_new, &[elem_type_val]);
        let list_ptr = self.builder.inst_results(call)[0];

        // 编译并添加每个元素
        let list_push = *self.func_refs.get("list_push")
            .ok_or("list_push not found")?;
        for expr in items {
            let val = self.compile_expr(expr)?;
            self.builder.ins().call(list_push, &[list_ptr, val]);
        }

        Ok(list_ptr)
    }



    /// 将值转换为 Dynamic 类型 (Boxing)
    fn convert_to_dynamic(&mut self, val: Value, ty: &BolideType) -> Result<Value, String> {
        let func_name = match ty {
            BolideType::Int => "dynamic_from_int",
            BolideType::Float => "dynamic_from_float",
            BolideType::Bool => "dynamic_from_bool",
            BolideType::Str => "dynamic_from_string",
            BolideType::BigInt => "dynamic_from_bigint",
            BolideType::Decimal => "dynamic_from_decimal",
            BolideType::List(_) => "dynamic_from_list",
            BolideType::Dict(_, _) => return Err("Dynamic Dict not supported yet".to_string()),
            BolideType::Dynamic => return Ok(val), // Already dynamic
            _ => return Err(format!("Cannot convert {:?} to dynamic", ty)),
        };
        let func = *self.func_refs.get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let call = self.builder.ins().call(func, &[val]);
        let res = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(res, &BolideType::Dynamic);
        Ok(res)
    }

    /// 编译字典字面量 {k: v, ...}
    fn compile_dict(&mut self, entries: &[(Expr, Expr)]) -> Result<Value, String> {
        // 确定键和值类型 (需扫描所有元素以处理 Dynamic)
        let (key_type_tag, val_type_tag) = if entries.is_empty() {
             (0u8, 0u8) // default int: int
        } else {
             // 第一次扫描：推断统一类型 (Dynamic or specific)
             let mut k_final_ty = self.infer_expr_type(&entries[0].0);
             let mut v_final_ty = self.infer_expr_type(&entries[0].1);
             
             for (k, v) in entries.iter().skip(1) {
                 let next_k = self.infer_expr_type(k);
                 if k_final_ty != next_k { k_final_ty = BolideType::Dynamic; }
                 let next_v = self.infer_expr_type(v);
                 if v_final_ty != next_v { v_final_ty = BolideType::Dynamic; }
             }

             // 映射到 type tag
             let map_tag = |ty: &BolideType| -> u8 {
                 match ty {
                    BolideType::Int => 0,
                    BolideType::Float => 1,
                    BolideType::Bool => 2,
                    BolideType::Str => 3,
                    BolideType::BigInt => 4,
                    BolideType::Decimal => 5,
                    BolideType::List(_) => 6,
                    BolideType::Ptr => 7,
                    BolideType::Dict(_, _) => 8,
                    BolideType::Dynamic => 9,
                    _ => 0 // fallback integer
                 }
             };
             (map_tag(&k_final_ty), map_tag(&v_final_ty))
        };

        // 创建字典
        let dict_new = *self.func_refs.get("dict_new")
             .ok_or("dict_new not found")?;
        let k_type_val = self.builder.ins().iconst(types::I8, key_type_tag as i64);
        let v_type_val = self.builder.ins().iconst(types::I8, val_type_tag as i64);
        let call = self.builder.ins().call(dict_new, &[k_type_val, v_type_val]);
        let dict_ptr = self.builder.inst_results(call)[0];

        // 设置元素
        let dict_set = *self.func_refs.get("dict_set")
             .ok_or("dict_set not found")?;
        
        for (key, val) in entries {
            let mut k_val = self.compile_expr(key)?;
            let mut v_val = self.compile_expr(val)?;
            
            // 如果目标是 Dynamic，但源不是，进行转换
            if key_type_tag == 9 {
                let k_ty = self.infer_expr_type(key);
                if k_ty != BolideType::Dynamic {
                    k_val = self.convert_to_dynamic(k_val, &k_ty)?;
                }
            }
            if val_type_tag == 9 {
                let v_ty = self.infer_expr_type(val);
                if v_ty != BolideType::Dynamic {
                    v_val = self.convert_to_dynamic(v_val, &v_ty)?;
                }
            }

            self.builder.ins().call(dict_set, &[dict_ptr, k_val, v_val]);
        }
        
        Ok(dict_ptr)
    }



    /// 编译索引访问 (元组或列表)
    fn compile_index(&mut self, base: &Expr, index: &Expr) -> Result<Value, String> {
        let base_type = self.infer_expr_type(base);
        let base_val = self.compile_expr(base)?;
        let index_val = self.compile_expr(index)?;

        // 根据类型选择不同的索引函数
        match base_type {
            BolideType::List(_) => {
                let list_get = *self.func_refs.get("list_get")
                    .ok_or("list_get not found")?;
                let call = self.builder.ins().call(list_get, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Dict(_, _) => {
                let dict_get = *self.func_refs.get("dict_get")
                    .ok_or("dict_get not found")?;
                let call = self.builder.ins().call(dict_get, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
            }

            _ => {
                // 默认使用元组索引
                let tuple_get = *self.func_refs.get("tuple_get")
                    .ok_or("tuple_get not found")?;
                let call = self.builder.ins().call(tuple_get, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
            }
        }
    }


    /// 编译 await all 表达式
    fn compile_await_all(&mut self, exprs: &[Expr]) -> Result<Value, String> {
        // 先启动所有协程，收集 Future 指针
        let mut futures = Vec::new();
        for expr in exprs {
            let future_ptr = self.compile_expr(expr)?;
            futures.push(future_ptr);
        }

        // 依次等待所有 Future（简单实现）
        let mut results = Vec::new();
        for (i, future_ptr) in futures.iter().enumerate() {
            let expr_type = self.infer_expr_type(&exprs[i]);
            let await_func_name = match &expr_type {
                BolideType::Float => "coroutine_await_float",
                BolideType::Str | BolideType::BigInt | BolideType::Decimal
                | BolideType::List(_) | BolideType::Custom(_) => "coroutine_await_ptr",
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

    /// 编译 await scope 语句
    fn compile_await_scope(&mut self, scope_stmt: &bolide_parser::AwaitScopeStmt) -> Result<(), String> {
        // 进入 scope
        let scope_enter = *self.func_refs.get("scope_enter")
            .ok_or("scope_enter not found")?;
        self.builder.ins().call(scope_enter, &[]);

        // 执行 scope 内的语句
        for stmt in &scope_stmt.body {
            self.compile_stmt(stmt)?;
        }

        // 退出 scope（等待所有未完成的 Future）
        let scope_exit = *self.func_refs.get("scope_exit")
            .ok_or("scope_exit not found")?;
        self.builder.ins().call(scope_exit, &[]);

        Ok(())
    }

    /// 编译 async select 语句 - 真正的竞争等待
    fn compile_async_select(&mut self, select_stmt: &bolide_parser::AsyncSelectStmt) -> Result<(), String> {
        use bolide_parser::AsyncSelectBranch;

        if select_stmt.branches.is_empty() {
            return Ok(());
        }

        let branch_count = select_stmt.branches.len();

        // 1. 启动所有异步任务，收集 futures
        let mut futures: Vec<Value> = Vec::new();
        for branch in &select_stmt.branches {
            let expr = match branch {
                AsyncSelectBranch::Bind { expr, .. } => expr,
                AsyncSelectBranch::Expr { expr, .. } => expr,
            };
            let future = self.compile_expr(expr)?;
            futures.push(future);
        }

        // 2. 在栈上分配数组存储 futures (使用 I64 作为指针类型)
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
        self.compile_select_branches(select_stmt, &futures, winner_idx)?;

        Ok(())
    }

    /// 编译 select 分支选择逻辑
    fn compile_select_branches(
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
        }

        // 最后一个 next_block 直接跳转到 merge
        self.builder.ins().jump(merge_block, &[]);
        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);

        Ok(())
    }
    /// 编译 join 函数 - 等待线程/任务完成
    fn compile_join(&mut self, handle_expr: &Expr) -> Result<Value, String> {
        let handle = self.compile_expr(handle_expr)?;

        // 从 handle 表达式获取变量名，然后查找对应的 spawn 函数返回类型
        let return_type = if let Expr::Ident(var_name) = handle_expr {
            if let Some(func_name) = self.spawn_func_map.get(var_name) {
                self.func_return_types.get(func_name).cloned().flatten()
            } else {
                None
            }
        } else {
            None
        };

        // 根据返回类型确定 join 函数后缀（只有 _int, _float, _ptr 三种）
        let type_suffix = match &return_type {
            Some(BolideType::Float) => "_float",
            Some(BolideType::Str) | Some(BolideType::BigInt) | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic) | Some(BolideType::Ptr)
            | Some(BolideType::List(_)) | Some(BolideType::Custom(_)) => "_ptr",
            _ => "_int", // Int, Bool, Channel, Future, None 都用 int
        };

        // 确定 merge_block 的参数类型（与 type_suffix 保持一致）
        let result_type = match &return_type {
            Some(BolideType::Float) => types::F64,
            Some(BolideType::Str) | Some(BolideType::BigInt) | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic) | Some(BolideType::Ptr)
            | Some(BolideType::List(_)) | Some(BolideType::Custom(_)) => self.ptr_type,
            _ => types::I64,
        };

        // 先检查是否在线程池上下文
        let pool_is_active_ref = *self.func_refs.get("pool_is_active")
            .ok_or("pool_is_active not found")?;
        let is_active_call = self.builder.ins().call(pool_is_active_ref, &[]);
        let is_active = self.builder.inst_results(is_active_call)[0];

        // 创建分支块
        let pool_block = self.builder.create_block();
        let thread_block = self.builder.create_block();
        let merge_block = self.builder.create_block();

        // 为 merge_block 添加参数（返回值）
        self.builder.append_block_param(merge_block, result_type);

        // 根据是否在线程池中选择分支
        self.builder.ins().brif(is_active, pool_block, &[], thread_block, &[]);

        // 线程池分支: 使用 pool_join
        self.builder.switch_to_block(pool_block);
        self.builder.seal_block(pool_block);
        let pool_join_name = format!("pool_join{}", type_suffix);
        let pool_join_ref = *self.func_refs.get(&pool_join_name)
            .ok_or(format!("{} not found", pool_join_name))?;
        let pool_call = self.builder.ins().call(pool_join_ref, &[handle]);
        let pool_result = self.builder.inst_results(pool_call)[0];
        self.builder.ins().jump(merge_block, &[pool_result]);

        // 普通线程分支: 使用 thread_join
        self.builder.switch_to_block(thread_block);
        self.builder.seal_block(thread_block);
        let thread_join_name = format!("thread_join{}", type_suffix);
        let thread_join_ref = *self.func_refs.get(&thread_join_name)
            .ok_or(format!("{} not found", thread_join_name))?;
        let thread_call = self.builder.ins().call(thread_join_ref, &[handle]);
        let thread_result = self.builder.inst_results(thread_call)[0];
        self.builder.ins().jump(merge_block, &[thread_result]);

        // 合并块
        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);
        let result = self.builder.block_params(merge_block)[0];

        // 如果返回类型是 RC 类型，track 为临时值
        if let Some(ref ret_ty) = return_type {
            if Self::is_rc_type(ret_ty) {
                self.track_temp_rc_value(result, ret_ty);
            }
        }

        Ok(result)
    }

    /// 编译 channel 函数 - 创建通道
    fn compile_channel_create(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.is_empty() {
            // 无缓冲通道: channel_create()
            let channel_create_ref = *self.func_refs.get("channel_create")
                .ok_or("channel_create not found")?;
            let call = self.builder.ins().call(channel_create_ref, &[]);
            let channel_ptr = self.builder.inst_results(call)[0];
            Ok(channel_ptr)
        } else if args.len() == 1 {
            // 带缓冲通道: channel_create_buffered(capacity)
            let capacity = self.compile_expr(&args[0])?;
            let channel_create_buffered_ref = *self.func_refs.get("channel_create_buffered")
                .ok_or("channel_create_buffered not found")?;
            let call = self.builder.ins().call(channel_create_buffered_ref, &[capacity]);
            let channel_ptr = self.builder.inst_results(call)[0];
            Ok(channel_ptr)
        } else {
            Err("channel() expects 0 or 1 argument".to_string())
        }
    }

    /// 编译成员访问 (obj.field)
    fn compile_member_access(&mut self, base: &Expr, member: &str) -> Result<Value, String> {
        let base_type = self.get_expr_type(base)?;
        // 处理 Weak/Unowned 类型，提取内部的 Custom 类型
        let class_name = match &base_type {
            BolideType::Custom(name) => name.clone(),
            BolideType::Weak(inner) => {
                if let BolideType::Custom(name) = inner.as_ref() {
                    name.clone()
                } else {
                    return Err(format!("Member access on non-class weak type: {:?}", inner));
                }
            }
            BolideType::Unowned(inner) => {
                if let BolideType::Custom(name) = inner.as_ref() {
                    name.clone()
                } else {
                    return Err(format!("Member access on non-class unowned type: {:?}", inner));
                }
            }
            _ => return Err(format!("Member access on non-class type: {:?}", base_type)),
        };

        let class_info = self.classes.get(&class_name)
            .ok_or_else(|| format!("Class not found: {}", class_name))?
            .clone();

        let field = class_info.fields.iter()
            .find(|f| f.name == member)
            .ok_or_else(|| format!("Field '{}' not found in class '{}'", member, class_name))?;

        let field_offset = field.offset;
        let obj_ptr = self.compile_expr(base)?;
        let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field_offset as i64);
        let value = self.builder.ins().load(types::I64, MemFlags::new(), field_ptr, 0);

        Ok(value)
    }

    /// 获取表达式的类型
    fn get_expr_type(&self, expr: &Expr) -> Result<BolideType, String> {
        match expr {
            Expr::Ident(name) => {
                self.var_types.get(name)
                    .cloned()
                    .ok_or_else(|| format!("Unknown variable type: {}", name))
            }
            Expr::Call(callee, _) => {
                if let Expr::Ident(func_name) = callee.as_ref() {
                    if self.classes.contains_key(func_name) {
                        return Ok(BolideType::Custom(func_name.clone()));
                    }
                    self.func_return_types.get(func_name)
                        .cloned()
                        .flatten()
                        .ok_or_else(|| format!("Unknown function return type: {}", func_name))
                } else {
                    Err("Cannot determine type of indirect call".to_string())
                }
            }
            Expr::Member(base, member) => {
                let class_name = self.get_expr_type(base)?;
                if let BolideType::Custom(name) = class_name {
                    let class_info = self.classes.get(&name)
                        .ok_or_else(|| format!("Class not found: {}", name))?;
                    let field = class_info.fields.iter()
                        .find(|f| f.name == *member)
                        .ok_or_else(|| format!("Field not found: {}", member))?;
                    Ok(field.ty.clone())
                } else {
                    Err("Member access on non-class type".to_string())
                }
            }
            _ => Err("Cannot determine expression type".to_string()),
        }
    }

    /// 编译模块函数调用 (module.func())
    fn compile_module_call(&mut self, func_name: &str, args: &[Expr]) -> Result<Value, String> {
        let func_ref = *self.func_refs.get(func_name)
            .ok_or_else(|| format!("Undefined function: {}", func_name))?;

        // 编译参数
        let mut arg_values = Vec::new();
        for arg in args {
            arg_values.push(self.compile_expr(arg)?);
        }

        // 调用函数
        let call = self.builder.ins().call(func_ref, &arg_values);
        let results = self.builder.inst_results(call);

        if results.is_empty() {
            Ok(self.builder.ins().iconst(types::I64, 0))
        } else {
            Ok(results[0])
        }
    }

    /// 编译方法调用 (obj.method(args))
    fn compile_method_call(&mut self, base: &Expr, method_name: &str, args: &[Expr]) -> Result<Value, String> {
        // 获取对象类型
        let class_name = self.get_expr_type(base)?;

        // 检查是否是 Future 类型的方法调用
        if matches!(class_name, BolideType::Future) {
            let handle = self.compile_expr(base)?;
            match method_name {
                "close" | "cancel" => {
                    // 调用 thread_cancel
                    let cancel_ref = *self.func_refs.get("thread_cancel")
                        .ok_or("thread_cancel not found")?;
                    self.builder.ins().call(cancel_ref, &[handle]);
                    return Ok(self.builder.ins().iconst(types::I64, 0));
                }
                "is_cancelled" => {
                    // 调用 thread_is_cancelled
                    let is_cancelled_ref = *self.func_refs.get("thread_is_cancelled")
                        .ok_or("thread_is_cancelled not found")?;
                    let call = self.builder.ins().call(is_cancelled_ref, &[handle]);
                    return Ok(self.builder.inst_results(call)[0]);
                }
                _ => return Err(format!("Unknown Future method: {}", method_name)),
            }
        }

        // 检查是否是 List 类型的方法调用
        if matches!(class_name, BolideType::List(_)) {
            let list_ptr = self.compile_expr(base)?;
            return self.compile_list_method_call(list_ptr, method_name, args);
        }

        // 检查是否是 Dict 类型的方法调用
        if matches!(class_name, BolideType::Dict(_, _)) {
            let dict_ptr = self.compile_expr(base)?;
            return self.compile_dict_method_call(dict_ptr, method_name, args);
        }


        let class_name = match class_name {
            BolideType::Custom(name) => name,
            _ => return Err(format!("Method call on non-class type: {:?}", class_name)),
        };


        // 查找方法（支持继承链）
        let full_method_name = self.find_method(&class_name, method_name)?;

        // 获取方法引用
        let func_ref = *self.func_refs.get(&full_method_name)
            .ok_or_else(|| format!("Method '{}' not found", full_method_name))?;

        // 编译 self 参数（对象指针）
        let self_val = self.compile_expr(base)?;

        // 编译其他参数
        let mut arg_values = vec![self_val];
        for arg in args {
            arg_values.push(self.compile_expr(arg)?);
        }

        // 调用方法
        let call = self.builder.ins().call(func_ref, &arg_values);
        let results = self.builder.inst_results(call);

        if results.is_empty() {
            Ok(self.builder.ins().iconst(types::I64, 0))
        } else {
            Ok(results[0])
        }
    }

    /// 编译列表方法调用
    fn compile_list_method_call(&mut self, list_ptr: Value, method_name: &str, args: &[Expr]) -> Result<Value, String> {
        match method_name {
            // push(value) -> void
            "push" | "append" => {
                if args.len() != 1 {
                    return Err(format!("{} expects 1 argument", method_name));
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self.func_refs.get("list_push").ok_or("list_push not found")?;
                self.builder.ins().call(func_ref, &[list_ptr, value]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // pop() -> value
            "pop" => {
                let func_ref = *self.func_refs.get("list_pop").ok_or("list_pop not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // len() -> int
            "len" | "length" | "size" => {
                let func_ref = *self.func_refs.get("list_len").ok_or("list_len not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // get(index) -> value
            "get" => {
                if args.len() != 1 {
                    return Err("get expects 1 argument".to_string());
                }
                let index = self.compile_expr(&args[0])?;
                let func_ref = *self.func_refs.get("list_get").ok_or("list_get not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, index]);
                Ok(self.builder.inst_results(call)[0])
            }
            // set(index, value) -> bool
            "set" => {
                if args.len() != 2 {
                    return Err("set expects 2 arguments".to_string());
                }
                let index = self.compile_expr(&args[0])?;
                let value = self.compile_expr(&args[1])?;
                let func_ref = *self.func_refs.get("list_set").ok_or("list_set not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, index, value]);
                Ok(self.builder.inst_results(call)[0])
            }
            // insert(index, value) -> void
            "insert" => {
                if args.len() != 2 {
                    return Err("insert expects 2 arguments".to_string());
                }
                let index = self.compile_expr(&args[0])?;
                let value = self.compile_expr(&args[1])?;
                let func_ref = *self.func_refs.get("list_insert").ok_or("list_insert not found")?;
                self.builder.ins().call(func_ref, &[list_ptr, index, value]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // remove(index) -> value
            "remove" => {
                if args.len() != 1 {
                    return Err("remove expects 1 argument".to_string());
                }
                let index = self.compile_expr(&args[0])?;
                let func_ref = *self.func_refs.get("list_remove").ok_or("list_remove not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, index]);
                Ok(self.builder.inst_results(call)[0])
            }
            // clear() -> void
            "clear" => {
                let func_ref = *self.func_refs.get("list_clear").ok_or("list_clear not found")?;
                self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // reverse() -> void
            "reverse" => {
                let func_ref = *self.func_refs.get("list_reverse").ok_or("list_reverse not found")?;
                self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // extend(other_list) -> void
            "extend" => {
                if args.len() != 1 {
                    return Err("extend expects 1 argument".to_string());
                }
                let other = self.compile_expr(&args[0])?;
                let func_ref = *self.func_refs.get("list_extend").ok_or("list_extend not found")?;
                self.builder.ins().call(func_ref, &[list_ptr, other]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // contains(value) -> bool
            "contains" | "includes" => {
                if args.len() != 1 {
                    return Err(format!("{} expects 1 argument", method_name));
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self.func_refs.get("list_contains").ok_or("list_contains not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, value]);
                Ok(self.builder.inst_results(call)[0])
            }
            // index_of(value) -> int (-1 if not found)
            "index_of" | "index" | "find" => {
                if args.len() != 1 {
                    return Err(format!("{} expects 1 argument", method_name));
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self.func_refs.get("list_index_of").ok_or("list_index_of not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, value]);
                Ok(self.builder.inst_results(call)[0])
            }
            // count(value) -> int
            "count" => {
                if args.len() != 1 {
                    return Err("count expects 1 argument".to_string());
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self.func_refs.get("list_count").ok_or("list_count not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, value]);
                Ok(self.builder.inst_results(call)[0])
            }
            // sort() -> void
            "sort" => {
                let func_ref = *self.func_refs.get("list_sort").ok_or("list_sort not found")?;
                self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // slice(start, end) -> list
            "slice" => {
                if args.len() != 2 {
                    return Err("slice expects 2 arguments".to_string());
                }
                let start = self.compile_expr(&args[0])?;
                let end = self.compile_expr(&args[1])?;
                let func_ref = *self.func_refs.get("list_slice").ok_or("list_slice not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, start, end]);
                Ok(self.builder.inst_results(call)[0])
            }
            // is_empty() -> bool
            "is_empty" | "empty" => {
                let func_ref = *self.func_refs.get("list_is_empty").ok_or("list_is_empty not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // first() -> value
            "first" => {
                let func_ref = *self.func_refs.get("list_first").ok_or("list_first not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // last() -> value
            "last" => {
                let func_ref = *self.func_refs.get("list_last").ok_or("list_last not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // copy() -> list (shallow copy, same as clone)
            "copy" | "clone" => {
                let func_ref = *self.func_refs.get("list_clone").ok_or("list_clone not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Err(format!("Unknown list method: {}", method_name)),
        }
    }

    /// 编译字典方法调用
    fn compile_dict_method_call(&mut self, dict_ptr: Value, method_name: &str, args: &[Expr]) -> Result<Value, String> {
        match method_name {
            "set" => {
                 let set_fn = *self.func_refs.get("dict_set").ok_or("dict_set failed")?;
                 let k = self.compile_expr(&args[0])?;
                 let v = self.compile_expr(&args[1])?;
                 self.builder.ins().call(set_fn, &[dict_ptr, k, v]);
                 Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "get" => {
                let get_fn = *self.func_refs.get("dict_get").ok_or("dict_get failed")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(get_fn, &[dict_ptr, k]);
                Ok(self.builder.inst_results(call)[0])
            }
            "contains" => {
                let contains_fn = *self.func_refs.get("dict_contains").ok_or("dict_contains failed")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(contains_fn, &[dict_ptr, k]);
                Ok(self.builder.inst_results(call)[0])
            }
            "remove" => {
                let remove_fn = *self.func_refs.get("dict_remove").ok_or("dict_remove failed")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(remove_fn, &[dict_ptr, k]);
                Ok(self.builder.inst_results(call)[0])
            }
             "len" => {
                let len_fn = *self.func_refs.get("dict_len").ok_or("dict_len failed")?;
                let call = self.builder.ins().call(len_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
             "is_empty" => {
                let is_empty_fn = *self.func_refs.get("dict_is_empty").ok_or("dict_is_empty failed")?;
                let call = self.builder.ins().call(is_empty_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            "clear" => {
                let clear_fn = *self.func_refs.get("dict_clear").ok_or("dict_clear failed")?;
                self.builder.ins().call(clear_fn, &[dict_ptr]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
             "keys" => {
                let keys_fn = *self.func_refs.get("dict_keys").ok_or("dict_keys failed")?;
                let call = self.builder.ins().call(keys_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
             "values" => {
                let values_fn = *self.func_refs.get("dict_values").ok_or("dict_values failed")?;
                let call = self.builder.ins().call(values_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
             "clone" => {
                let clone_fn = *self.func_refs.get("dict_clone").ok_or("dict_clone failed")?;
                let call = self.builder.ins().call(clone_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Err(format!("Unknown dictionary method: {}", method_name)),
        }
    }


    /// 在继承链中查找方法

    fn find_method(&self, class_name: &str, method_name: &str) -> Result<String, String> {
        let mut current = class_name.to_string();
        loop {
            let full_name = format!("{}_{}", current, method_name);
            if self.func_refs.contains_key(&full_name) {
                return Ok(full_name);
            }
            // 查找父类
            if let Some(class_info) = self.classes.get(&current) {
                if let Some(ref parent) = class_info.parent {
                    current = parent.clone();
                    continue;
                }
            }
            return Err(format!("Method '{}' not found in class '{}' or its parents", method_name, class_name));
        }
    }

    /// 尝试运算符重载
    fn try_operator_overload(&mut self, left: &Expr, op: &BinOp, right: &Expr, class_name: &str) -> Result<Option<Value>, String> {
        let method_name = match op {
            BinOp::Add => "__add__",
            BinOp::Sub => "__sub__",
            BinOp::Mul => "__mul__",
            BinOp::Div => "__div__",
            BinOp::Mod => "__mod__",
            BinOp::Eq => "__eq__",
            BinOp::Ne => "__ne__",
            BinOp::Lt => "__lt__",
            BinOp::Le => "__le__",
            BinOp::Gt => "__gt__",
            BinOp::Ge => "__ge__",
            _ => return Ok(None),
        };

        // 检查是否有运算符方法
        if self.find_method(class_name, method_name).is_ok() {
            let result = self.compile_method_call(left, method_name, &[right.clone()])?;
            return Ok(Some(result));
        }
        Ok(None)
    }

    // ============ FFI extern 支持 ============

    /// 注册 extern 块中的函数声明
    fn register_extern_block(&mut self, eb: &bolide_parser::ExternBlock) -> Result<(), String> {
        let lib_path = &eb.lib_path;

        // 遍历所有声明
        for decl in &eb.declarations {
            match decl {
                bolide_parser::ExternDecl::Function(func) => {
                    // 记录 extern 函数信息
                    self.extern_funcs.insert(
                        func.name.clone(),
                        (lib_path.clone(), func.clone())
                    );
                }
                bolide_parser::ExternDecl::Struct(_) => {
                    // TODO: 处理结构体声明
                }
                bolide_parser::ExternDecl::TypeAlias(_, _) => {
                    // TODO: 处理类型别名
                }
            }
        }
        Ok(())
    }

    /// 创建字符串常量并返回指针
    fn create_string_constant(&mut self, s: &str) -> Result<Value, String> {
        // 创建以 null 结尾的字符串
        let mut bytes: Vec<u8> = s.bytes().collect();
        bytes.push(0);

        // 在栈上分配空间
        let slot = self.builder.create_sized_stack_slot(
            cranelift_codegen::ir::StackSlotData::new(
                cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                bytes.len() as u32,
                0,
            )
        );
        let ptr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);

        // 写入字符串数据
        for (i, byte) in bytes.iter().enumerate() {
            let val = self.builder.ins().iconst(types::I8, *byte as i64);
            self.builder.ins().store(
                cranelift_codegen::ir::MemFlags::new(),
                val,
                ptr,
                i as i32,
            );
        }

        Ok(ptr)
    }

    /// 编译 extern 函数调用
    fn compile_extern_call(
        &mut self,
        lib_path: &str,
        extern_func: &bolide_parser::ExternFunc,
        args: &[Expr],
    ) -> Result<Value, String> {
        // 1. 创建库路径字符串常量
        let lib_path_ptr = self.create_string_constant(lib_path)?;

        // 2. 加载库
        let load_lib_ref = *self.func_refs.get("ffi_load_library")
            .ok_or("ffi_load_library not found")?;
        self.builder.ins().call(load_lib_ref, &[lib_path_ptr]);

        // 3. 创建函数名字符串常量
        let func_name_ptr = self.create_string_constant(&extern_func.name)?;

        // 4. 获取函数指针
        let get_symbol_ref = *self.func_refs.get("ffi_get_symbol")
            .ok_or("ffi_get_symbol not found")?;
        let call = self.builder.ins().call(get_symbol_ref, &[lib_path_ptr, func_name_ptr]);
        let func_ptr = self.builder.inst_results(call)[0];

        // 5. 编译参数并进行类型转换
        let mut arg_values = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            // 检查是否是函数指针参数（回调）
            if let Some(param) = extern_func.params.get(i) {
                if matches!(param.ty, bolide_parser::CType::FuncPtr { .. }) {
                    // 参数是函数指针类型，检查是否传递了函数名
                    if let Expr::Ident(func_name) = arg {
                        // 获取函数地址
                        if let Some(&func_ref) = self.func_refs.get(func_name) {
                            let func_addr = self.builder.ins().func_addr(self.ptr_type, func_ref);
                            arg_values.push(func_addr);
                            continue;
                        }
                    }
                }
            }

            let val = self.compile_expr(arg)?;

            // 检查是否需要将 BolideString* 转换为 char*
            if let Some(param) = extern_func.params.get(i) {
                if let bolide_parser::CType::Ptr(inner) = &param.ty {
                    if matches!(inner.as_ref(), bolide_parser::CType::Char) {
                        // 参数类型是 *char，需要转换 BolideString* -> char*
                        let as_cstr_ref = *self.func_refs.get("string_as_cstr")
                            .ok_or("string_as_cstr not found")?;
                        let call = self.builder.ins().call(as_cstr_ref, &[val]);
                        let cstr_ptr = self.builder.inst_results(call)[0];
                        arg_values.push(cstr_ptr);
                        continue;
                    }
                }
            }

            // 获取期望的 C 类型
            if let Some(param) = extern_func.params.get(i) {
                let expected_ty = self.ctype_to_cranelift(&param.ty);
                let actual_ty = self.builder.func.dfg.value_type(val);

                // 类型转换
                let converted = if actual_ty == types::I64 && expected_ty == types::I32 {
                    self.builder.ins().ireduce(types::I32, val)
                } else if actual_ty == types::I64 && expected_ty == types::I16 {
                    self.builder.ins().ireduce(types::I16, val)
                } else if actual_ty == types::I64 && expected_ty == types::I8 {
                    self.builder.ins().ireduce(types::I8, val)
                } else if actual_ty == types::F64 && expected_ty == types::F32 {
                    self.builder.ins().fdemote(types::F32, val)
                } else {
                    val
                };
                arg_values.push(converted);
            } else {
                arg_values.push(val);
            }
        }

        // 6. 构建函数签名
        let sig = self.build_extern_signature(extern_func)?;
        let sig_ref = self.builder.import_signature(sig);

        // 7. 间接调用
        let call = self.builder.ins().call_indirect(sig_ref, func_ptr, &arg_values);
        let results = self.builder.inst_results(call);

        if results.is_empty() {
            Ok(self.builder.ins().iconst(types::I64, 0))
        } else {
            // 转换返回值类型到 Bolide 类型
            let result = results[0];
            let result_ty = self.builder.func.dfg.value_type(result);

            // 如果是 I32，扩展到 I64
            if result_ty == types::I32 {
                Ok(self.builder.ins().sextend(types::I64, result))
            } else if result_ty == types::I8 || result_ty == types::I16 {
                Ok(self.builder.ins().sextend(types::I64, result))
            } else if result_ty == types::F32 {
                Ok(self.builder.ins().fpromote(types::F64, result))
            } else {
                Ok(result)
            }
        }
    }

    /// 构建 extern 函数签名
    fn build_extern_signature(&self, func: &bolide_parser::ExternFunc) -> Result<Signature, String> {
        use cranelift_codegen::isa::CallConv;

        // Windows 使用 WindowsFastcall，其他平台使用 SystemV
        #[cfg(target_os = "windows")]
        let call_conv = CallConv::WindowsFastcall;
        #[cfg(not(target_os = "windows"))]
        let call_conv = CallConv::SystemV;

        let mut sig = Signature::new(call_conv);

        // 添加参数
        for param in &func.params {
            let ty = self.ctype_to_cranelift(&param.ty);
            sig.params.push(AbiParam::new(ty));
        }

        // 添加返回类型
        if let Some(ref ret_ty) = func.return_type {
            if !matches!(ret_ty, bolide_parser::CType::Void) {
                let ty = self.ctype_to_cranelift(ret_ty);
                sig.returns.push(AbiParam::new(ty));
            }
        }

        Ok(sig)
    }

    /// C 类型转换为 Cranelift 类型
    fn ctype_to_cranelift(&self, ctype: &bolide_parser::CType) -> types::Type {
        use bolide_parser::CType;
        match ctype {
            CType::Void => types::I64,  // void 用 i64 占位
            CType::Char | CType::I8 => types::I8,
            CType::UChar | CType::U8 => types::I8,
            CType::Short | CType::I16 => types::I16,
            CType::UShort | CType::U16 => types::I16,
            CType::Int | CType::I32 => types::I32,
            CType::UInt | CType::U32 => types::I32,
            CType::Long | CType::LongLong | CType::I64 => types::I64,
            CType::ULong | CType::ULongLong | CType::U64 => types::I64,
            CType::Float => types::F32,
            CType::Double => types::F64,
            CType::Bool => types::I8,
            CType::SizeT | CType::PtrDiffT => types::I64,
            CType::Ptr(_) => self.ptr_type,
            CType::Array(_, _) => self.ptr_type,
            CType::FuncPtr { .. } => self.ptr_type,
            CType::Struct(_) => self.ptr_type,
        }
    }
}
