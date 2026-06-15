//! AOT 编译器
//!
//! 使用 Cranelift 实现的提前编译器，生成目标文件

use crate::ffi_spec::{is_dynamic_lib_spec, resolve_dynamic_lib_spec, validate_extern_lib_spec};
use crate::inject_builtin_classes;
use bolide_parser::{
    BinOp, CType, Expr, ExternBlock, ExternDecl, ForStmt, FuncDef, IfStmt, Param, ParamMode,
    Program, Statement, Type as BolideType, UnaryOp, VarDecl,
};
use cranelift::prelude::isa::{CallConv, TargetIsa};
use cranelift::prelude::*;
use cranelift_codegen::ir::{FuncRef, StackSlotData, StackSlotKind, TrapCode};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// AOT 编译结果
#[derive(Debug)]
pub struct AotCompileResult {
    /// 目标文件字节码
    pub object_code: Vec<u8>,
    /// 外部库列表 (库路径)
    pub extern_libs: Vec<String>,
    /// 导出函数的 C 头文件声明（仅当存在 `export fn` 时为 Some）
    pub c_header: Option<String>,
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
    default_value: Option<Expr>,
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

#[derive(Clone)]
struct AdtFieldInfo {
    name: Option<String>,
    ty: BolideType,
    offset: usize,
}

#[derive(Clone)]
struct AdtVariantInfo {
    name: String,
    tag: i64,
    fields: Vec<AdtFieldInfo>,
}

#[derive(Clone)]
struct AdtInfo {
    name: String,
    type_params: Vec<String>,
    variants: Vec<AdtVariantInfo>,
    size: usize,
}

#[derive(Clone)]
enum PreparedArg {
    Expr(Expr),
    PackedArgs {
        elem_ty: BolideType,
        items: Vec<PackedArgItem>,
    },
    PackedKwargs {
        value_ty: BolideType,
        items: Vec<PackedKwargItem>,
    },
}

#[derive(Clone)]
enum PackedArgItem {
    Expr(Expr),
    Spread(Expr),
}

#[derive(Clone)]
enum PackedKwargItem {
    Entry(String, Expr),
    Spread(Expr),
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
    adts: HashMap<String, AdtInfo>,
    /// 类名 -> 异常类型标签（>=100，按声明顺序分配，用于 catch 类型过滤）
    class_tags: HashMap<String, i64>,
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
    /// 程序快照（用于编译函数时扫描类字段默认值中的字符串）
    program_snapshot: Program,
    /// 全局变量名 -> DataId
    global_data_ids: HashMap<String, DataId>,
    /// 全局变量名 -> Bolide 类型
    global_var_types: HashMap<String, BolideType>,
    /// 全局 future 变量名 -> 对应 async 函数名（用于两步式 await 的类型推断）
    global_spawn_funcs: HashMap<String, String>,
    /// 标记了 export 的用户函数（裸名导出，供 C 互调 + 头文件生成）
    export_funcs: Vec<FuncDef>,
    /// 库模式：抑制合成入口 `main`，使产物可作为静态库被 C 链接
    lib_mode: bool,
    /// 源文件所在目录（import 相对路径的解析基准）
    base_dir: Option<String>,
    /// 闭包计数器（生成唯一 lifted 函数名）
    closure_counter: usize,
    /// 待编译的 lifted 闭包函数
    pending_closures: Vec<ClosureJob>,
}

/// 一个待编译的 lifted 闭包函数（AOT）
#[derive(Clone)]
struct ClosureJob {
    func_id: FuncId,
    name: String,
    params: Vec<Param>,
    return_type: Option<BolideType>,
    body: Vec<Statement>,
    captures: Vec<(String, BolideType)>,
}

/// 运行时符号列表
pub const RUNTIME_SYMBOLS: &[&str] = &[
    // 基本类型打印
    "print_int",
    "print_float",
    "print_bool",
    "print_bigint",
    "print_decimal",
    "print_string",
    "print_bytes",
    "print_dynamic",
    "print_int_inline",
    "print_float_inline",
    "print_bool_inline",
    "print_bigint_inline",
    "print_decimal_inline",
    "print_string_inline",
    "print_bytes_inline",
    "print_dynamic_inline",
    "print_tuple_start",
    "print_tuple_separator",
    "print_tuple_end_inline",
    "println",
    // 用户输入
    "input",
    "input_prompt",
    // BigInt
    "bigint_from_i64",
    "bigint_from_str",
    "bigint_add",
    "bigint_sub",
    "bigint_mul",
    "bigint_div",
    "bigint_rem",
    "bigint_neg",
    "bigint_eq",
    "bigint_lt",
    "bigint_le",
    "bigint_gt",
    "bigint_ge",
    "bigint_to_i64",
    "bigint_clone",
    "bigint_debug_stats",
    // Decimal
    "decimal_from_i64",
    "decimal_from_f64",
    "decimal_from_str",
    "decimal_add",
    "decimal_sub",
    "decimal_mul",
    "decimal_div",
    "decimal_neg",
    "decimal_eq",
    "decimal_lt",
    "decimal_to_i64",
    "decimal_to_f64",
    "decimal_clone",
    // Dynamic
    "dynamic_from_int",
    "dynamic_from_float",
    "dynamic_from_bool",
    "dynamic_from_string",
    "dynamic_from_list",
    "dynamic_from_bytes",
    "dynamic_from_dict",
    "dynamic_from_bigint",
    "dynamic_from_decimal",
    "dynamic_to_int",
    "dynamic_to_float",
    "dynamic_to_string",
    "dynamic_add",
    "dynamic_sub",
    "dynamic_mul",
    "dynamic_div",
    "dynamic_neg",
    "dynamic_eq",
    "dynamic_lt",
    "dynamic_clone",
    // String
    "string_from_slice",
    "string_literal",
    "string_as_cstr",
    "string_concat",
    "string_concat_many",
    "string_eq",
    "string_from_int",
    "string_from_float",
    "string_from_bool",
    "string_from_bigint",
    "string_from_decimal",
    "string_to_int",
    "string_to_float",
    // String 方法 + 切片 + 索引
    "string_slice",
    "string_char_at",
    "string_upper",
    "string_lower",
    "string_trim",
    "string_replace",
    "string_repeat",
    "string_find",
    "string_contains",
    "string_starts_with",
    "string_ends_with",
    "string_count",
    "string_split",
    // Bytes
    "bytes_new",
    "bytes_retain",
    "bytes_release",
    "bytes_clone",
    "bytes_len",
    "bytes_get",
    "bytes_set",
    "bytes_push",
    "bytes_to_string_lossy",
    // Memory
    "bolide_alloc",
    "bolide_free",
    // Object
    "object_alloc",
    "object_retain",
    "object_release",
    "object_clone",
    "object_weak_retain",
    "object_weak_release",
    "object_weak_clone",
    "object_assert_alive",
    "object_is_alive",
    "object_ref_count",
    // Closure
    "bolide_closure_new",
    "bolide_closure_fn_ptr",
    "bolide_closure_env_ptr",
    "bolide_closure_retain",
    "bolide_closure_release",
    // Thread
    "thread_spawn_int",
    "thread_spawn_float",
    "thread_spawn_ptr",
    "thread_spawn_int_with_env",
    "thread_spawn_float_with_env",
    "thread_spawn_ptr_with_env",
    "thread_join_int",
    "thread_join_float",
    "thread_join_ptr",
    "thread_handle_free",
    "thread_cancel",
    "thread_is_cancelled",
    // Pool
    "pool_create",
    "pool_enter",
    "pool_exit",
    "pool_is_active",
    "pool_spawn_int",
    "pool_spawn_float",
    "pool_spawn_ptr",
    "pool_spawn_int_with_env",
    "pool_spawn_float_with_env",
    "pool_spawn_ptr_with_env",
    "pool_join_int",
    "pool_join_float",
    "pool_join_ptr",
    "pool_handle_free",
    "pool_select_wait_first",
    "pool_destroy",
    // Channel
    "channel_create",
    "channel_create_buffered",
    "channel_send",
    "channel_recv",
    "channel_close",
    "channel_free",
    "channel_select",
    // Coroutine
    "coroutine_spawn_int",
    "coroutine_spawn_float",
    "coroutine_spawn_ptr",
    "coroutine_await_int",
    "coroutine_await_float",
    "coroutine_await_ptr",
    "coroutine_cancel",
    "coroutine_free",
    "coroutine_spawn_int_with_env",
    "coroutine_spawn_float_with_env",
    "coroutine_spawn_ptr_with_env",
    "scope_enter",
    "scope_register",
    "scope_exit",
    // Select
    "select_wait_first",
    // Tuple
    "tuple_new",
    "tuple_new_typed",
    "tuple_free",
    "tuple_set",
    "tuple_set_typed",
    "tuple_get",
    "tuple_get_type",
    "tuple_len",
    "tuple_retain",
    "tuple_clone",
    "tuple_release",
    "tuple_slice_step",
    "print_tuple",
    // FFI
    "ffi_load_library",
    "ffi_get_symbol",
    "test_callback",
    "map_int",
    // RC
    "string_retain",
    "string_release",
    "string_clone",
    "string_len",
    // String 方法 + 切片 + 索引
    "string_slice",
    "string_char_at",
    "string_upper",
    "string_lower",
    "string_trim",
    "string_replace",
    "string_repeat",
    "string_find",
    "string_contains",
    "string_starts_with",
    "string_ends_with",
    "string_count",
    "string_split",
    "bigint_retain",
    "bigint_release",
    "decimal_retain",
    "decimal_release",
    "list_retain",
    "list_release",
    "list_clone",
    "list_new",
    "list_push",
    "list_pop",
    "list_len",
    "list_get",
    "list_set",
    "list_insert",
    "list_remove",
    "list_clear",
    "list_reverse",
    "list_extend",
    "list_contains",
    "list_index_of",
    "list_count",
    "list_sort",
    "list_slice",
    "list_slice_step",
    "list_is_empty",
    "list_first",
    "list_last",
    "list_map",
    "list_filter",
    "print_list",
    // Dict
    "dict_new",
    "dict_retain",
    "dict_release",
    "dict_clone",
    "dict_extend",
    "dict_set",
    "dict_get",
    "dict_contains",
    "dict_remove",
    "dict_len",
    "dict_is_empty",
    "dict_clear",
    "dict_keys",
    "dict_values",
    "dict_iter",
    "print_dict",
    "dynamic_retain",
    "dynamic_release",
    // Web
    "bolide_web_app_new",
    "bolide_web_app_free",
    "bolide_web_app_set_workers",
    "bolide_web_app_set_max_body",
    "bolide_web_route",
    "bolide_web_route_handler",
    "bolide_web_route_async_handler",
    "bolide_web_static",
    "bolide_web_get",
    "bolide_web_get_handler",
    "bolide_web_get_async_handler",
    "bolide_web_post",
    "bolide_web_post_handler",
    "bolide_web_post_async_handler",
    "bolide_web_put",
    "bolide_web_put_handler",
    "bolide_web_put_async_handler",
    "bolide_web_patch",
    "bolide_web_patch_handler",
    "bolide_web_patch_async_handler",
    "bolide_web_delete",
    "bolide_web_delete_handler",
    "bolide_web_delete_async_handler",
    "bolide_web_head",
    "bolide_web_head_handler",
    "bolide_web_head_async_handler",
    "bolide_web_options",
    "bolide_web_options_handler",
    "bolide_web_options_async_handler",
    "bolide_web_trace",
    "bolide_web_trace_handler",
    "bolide_web_trace_async_handler",
    "bolide_web_connect",
    "bolide_web_connect_handler",
    "bolide_web_connect_async_handler",
    "bolide_web_run",
    "bolide_web_serve",
    "bolide_web_app_handle",
    "bolide_web_app_handle_with_headers",
    "bolide_web_cookie_pair",
    "bolide_web_request_method",
    "bolide_web_request_target",
    "bolide_web_request_path",
    "bolide_web_request_query",
    "bolide_web_request_version",
    "bolide_web_request_header",
    "bolide_web_request_header_str",
    "bolide_web_request_cookie",
    "bolide_web_request_cookie_str",
    "bolide_web_request_query_param",
    "bolide_web_request_query_param_str",
    "bolide_web_request_form_param",
    "bolide_web_request_form_param_str",
    "bolide_web_request_path_param",
    "bolide_web_request_path_param_str",
    "bolide_web_request_body_text",
    "bolide_web_request_body_bytes",
    "bolide_web_request_body_len",
    "bolide_web_response_new",
    "bolide_web_response_new_str",
    "bolide_web_text",
    "bolide_web_text_str",
    "bolide_web_html",
    "bolide_web_html_str",
    "bolide_web_json",
    "bolide_web_json_str",
    "bolide_web_bytes",
    "bolide_web_empty",
    "bolide_web_redirect",
    "bolide_web_redirect_str",
    "bolide_web_response_set_status",
    "bolide_web_response_set_header",
    "bolide_web_response_set_header_str",
    "bolide_web_response_set_cookie",
    "bolide_web_response_delete_cookie",
    "bolide_web_response_status",
    "bolide_web_response_header",
    "bolide_web_response_header_str",
    "bolide_web_response_cookie_pair",
    "bolide_web_response_body_text",
    "bolide_web_response_body_bytes",
    "bolide_web_response_free",
    "bolide_web_session",
    "bolide_web_session_id",
    "bolide_web_session_get",
    "bolide_web_session_set",
    "bolide_web_session_contains",
    "bolide_web_session_remove",
    "bolide_web_session_clear",
    "bolide_web_session_destroy",
    "bolide_web_session_regenerate",
    "bolide_web_session_free",
    // Template
    "bolide_template_escape_html",
    "bolide_template_render",
    "bolide_template_render_file",
    // DB
    "bolide_db_open",
    "bolide_db_close",
    "bolide_db_last_error",
    "bolide_db_create_table",
    "bolide_db_insert",
    "bolide_db_update",
    "bolide_db_delete",
    "bolide_db_get",
    "bolide_db_all",
    "bolide_db_where_eq",
    "bolide_db_count",
    // GUI
    "bolide_gui_backend",
    "bolide_gui_run",
    "bolide_gui_label",
    "bolide_gui_heading",
    "bolide_gui_small",
    "bolide_gui_strong",
    "bolide_gui_separator",
    "bolide_gui_space",
    "bolide_gui_button",
    "bolide_gui_link",
    "bolide_gui_text_input",
    "bolide_gui_password_input",
    "bolide_gui_multiline_input",
    "bolide_gui_checkbox",
    "bolide_gui_slider",
    "bolide_gui_progress",
    "bolide_gui_pack",
    "bolide_gui_row",
    "bolide_gui_column",
    "bolide_gui_group",
    "bolide_gui_grid",
    "bolide_gui_end_row",
    "bolide_gui_frame",
    "bolide_gui_scroll",
    "bolide_gui_indent",
    "bolide_gui_centered",
    "bolide_gui_align",
    "bolide_gui_pad",
    "bolide_gui_width",
    "bolide_gui_height",
    "bolide_gui_size",
    "bolide_gui_fill_width",
    "bolide_gui_fill_height",
    "bolide_gui_fill",
    "bolide_gui_place",
    "bolide_gui_collapsing",
    "bolide_gui_available_width",
    "bolide_gui_available_height",
    "bolide_gui_request_repaint",
];

impl AotCompiler {
    /// 创建新的 AOT 编译器
    pub fn new() -> Result<Self, String> {
        let isa_builder = cranelift_native::builder()
            .map_err(|e| format!("Failed to create ISA builder: {}", e))?;

        // 开启 Cranelift 优化（默认 opt_level=none 不做任何优化）
        let mut flag_builder = settings::builder();
        flag_builder
            .set("opt_level", "speed")
            .map_err(|e| format!("Failed to set opt_level: {}", e))?;
        let flags = settings::Flags::new(flag_builder);
        let isa = isa_builder
            .finish(flags)
            .map_err(|e| format!("Failed to create ISA: {}", e))?;

        let builder = ObjectBuilder::new(
            isa,
            "bolide_program",
            cranelift_module::default_libcall_names(),
        )
        .map_err(|e| format!("Failed to create object builder: {}", e))?;

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
            adts: HashMap::new(),
            class_tags: HashMap::new(),
            async_funcs: HashSet::new(),
            extern_funcs: HashMap::new(),
            modules: HashMap::new(),
            lifetime_funcs: HashSet::new(),
            string_data: HashMap::new(),
            global_data_ids: HashMap::new(),
            global_var_types: HashMap::new(),
            global_spawn_funcs: HashMap::new(),
            export_funcs: Vec::new(),
            lib_mode: false,
            base_dir: None,
            program_snapshot: Program { statements: vec![] },
            closure_counter: 0,
            pending_closures: Vec::new(),
        })
    }

    /// 设置源文件所在目录（import 相对路径的解析基准）
    pub fn set_base_dir(&mut self, dir: &str) {
        self.base_dir = Some(dir.to_string());
    }

    /// 库模式：不生成合成入口 `main`，使产物可作为静态库被 C 程序链接。
    pub fn set_lib_mode(&mut self, lib: bool) {
        self.lib_mode = lib;
    }

    /// Get or create a data object for a string literal
    fn get_or_create_string_data(&mut self, s: &str) -> Result<DataId, String> {
        if let Some(&data_id) = self.string_data.get(s) {
            return Ok(data_id);
        }

        // Create a unique name for this string data
        let name = format!("str_{}", self.string_data.len());

        // Declare the data object
        let data_id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .map_err(|e| format!("Failed to declare string data: {}", e))?;

        // Define the data with the string bytes
        self.data_desc.clear();
        self.data_desc
            .define(s.as_bytes().to_vec().into_boxed_slice());

        self.module
            .define_data(data_id, &self.data_desc)
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
                for s in &if_stmt.then_body {
                    self.collect_strings_from_stmt(s, strings);
                }
                for (cond, body) in &if_stmt.elif_branches {
                    self.collect_strings_from_expr(cond, strings);
                    for s in body {
                        self.collect_strings_from_stmt(s, strings);
                    }
                }
                if let Some(ref eb) = if_stmt.else_body {
                    for s in eb {
                        self.collect_strings_from_stmt(s, strings);
                    }
                }
            }
            Statement::While(while_stmt) => {
                self.collect_strings_from_expr(&while_stmt.condition, strings);
                for s in &while_stmt.body {
                    self.collect_strings_from_stmt(s, strings);
                }
            }
            Statement::For(for_stmt) => {
                self.collect_strings_from_expr(&for_stmt.iter, strings);
                for s in &for_stmt.body {
                    self.collect_strings_from_stmt(s, strings);
                }
            }
            Statement::Return(Some(e)) => self.collect_strings_from_expr(e, strings),
            Statement::Throw(e) => self.collect_strings_from_expr(e, strings),
            Statement::Try(try_stmt) => {
                for s in &try_stmt.try_body {
                    self.collect_strings_from_stmt(s, strings);
                }
                for clause in &try_stmt.catch_clauses {
                    for s in &clause.body {
                        self.collect_strings_from_stmt(s, strings);
                    }
                }
                if let Some(ref fin) = try_stmt.finally {
                    for s in fin {
                        self.collect_strings_from_stmt(s, strings);
                    }
                }
            }
            Statement::SpawnSelect(async_stmt) => {
                for branch in &async_stmt.branches {
                    match branch {
                        bolide_parser::SpawnSelectBranch::Bind { expr, body, .. } => {
                            self.collect_strings_from_expr(expr, strings);
                            for s in body {
                                self.collect_strings_from_stmt(s, strings);
                            }
                        }
                        bolide_parser::SpawnSelectBranch::Expr { expr, body } => {
                            self.collect_strings_from_expr(expr, strings);
                            for s in body {
                                self.collect_strings_from_stmt(s, strings);
                            }
                        }
                    }
                }
            }
            Statement::Pool(pool_stmt) => {
                self.collect_strings_from_expr(&pool_stmt.size, strings);
                for s in &pool_stmt.body {
                    self.collect_strings_from_stmt(s, strings);
                }
            }
            Statement::AwaitScope(scope) => {
                for s in &scope.body {
                    self.collect_strings_from_stmt(s, strings);
                }
            }
            Statement::Select(select_stmt) => {
                for branch in &select_stmt.branches {
                    match branch {
                        bolide_parser::SelectBranch::Recv { body, .. } => {
                            for s in body {
                                self.collect_strings_from_stmt(s, strings);
                            }
                        }
                        bolide_parser::SelectBranch::Timeout { duration, body } => {
                            self.collect_strings_from_expr(duration, strings);
                            for s in body {
                                self.collect_strings_from_stmt(s, strings);
                            }
                        }
                        bolide_parser::SelectBranch::Default { body } => {
                            for s in body {
                                self.collect_strings_from_stmt(s, strings);
                            }
                        }
                    }
                }
            }
            Statement::Send(send_stmt) => {
                self.collect_strings_from_expr(&send_stmt.value, strings);
            }
            _ => {}
        }
    }

    fn collect_strings_from_expr(&self, expr: &Expr, strings: &mut HashSet<String>) {
        match expr {
            Expr::String(s) => {
                strings.insert(s.clone());
            }
            // bigint/decimal 大字面量需经 *_from_str 构造，数字串必须进数据段
            Expr::BigInt(s) | Expr::Decimal(s) => {
                strings.insert(s.clone());
            }
            Expr::Call(callee, args) => {
                self.collect_strings_from_expr(callee, strings);
                for a in args {
                    self.collect_strings_from_expr(a, strings);
                }
            }
            Expr::NamedArg(_, value) | Expr::SpreadArg(value) | Expr::KwSpreadArg(value) => {
                self.collect_strings_from_expr(value, strings);
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
                for i in items {
                    self.collect_strings_from_expr(i, strings);
                }
            }
            Expr::Tuple(items) => {
                for i in items {
                    self.collect_strings_from_expr(i, strings);
                }
            }
            Expr::Dict(entries) => {
                for (k, v) in entries {
                    self.collect_strings_from_expr(k, strings);
                    self.collect_strings_from_expr(v, strings);
                }
            }
            Expr::Spawn(_, args) => {
                for arg in args {
                    self.collect_strings_from_expr(arg, strings);
                }
            }
            Expr::Await(inner) => self.collect_strings_from_expr(inner, strings),
            Expr::ListComprehension {
                expr, iter, filter, ..
            } => {
                self.collect_strings_from_expr(expr, strings);
                self.collect_strings_from_expr(iter, strings);
                if let Some(f) = filter {
                    self.collect_strings_from_expr(f, strings);
                }
            }
            Expr::Closure { body, .. } => {
                // 闭包体中的字符串字面量也需注册到 data segment
                for s in body {
                    self.collect_strings_from_stmt(s, strings);
                }
            }
            _ => {}
        }
    }

    /// 收集类字段默认值中的字符串字面量（AOT 入口函数不一定能扫描到）
    fn collect_class_default_strings_from_program(&self, program: &Program) -> HashSet<String> {
        let mut strings = HashSet::new();
        for stmt in &program.statements {
            if let Statement::ClassDef(class) = stmt {
                for field in &class.fields {
                    if let Some(ref default_expr) = field.default_value {
                        self.collect_strings_from_expr(default_expr, &mut strings);
                    }
                }
            }
        }
        strings
    }

    fn collect_param_default_strings_from_program(&self, program: &Program) -> HashSet<String> {
        let mut strings = HashSet::new();
        for stmt in &program.statements {
            match stmt {
                Statement::FuncDef(func) => {
                    for param in &func.params {
                        if let Some(default_expr) = &param.default_value {
                            self.collect_strings_from_expr(default_expr, &mut strings);
                        }
                    }
                }
                Statement::ClassDef(class) => {
                    for method in &class.methods {
                        for param in &method.params {
                            if let Some(default_expr) = &param.default_value {
                                self.collect_strings_from_expr(default_expr, &mut strings);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        strings
    }

    /// 编译程序并返回目标文件字节
    pub fn compile(mut self, program: &Program) -> Result<AotCompileResult, String> {
        // 预处理 import 语句
        let program = self.process_imports(program)?;
        // 注入内置类（Error 等），供 try/catch 使用
        let program = inject_builtin_classes(program);
        // 泛型函数单态化
        let program = crate::monomorphize(program)?;
        // 保存程序快照用于后续字符串扫描（类字段默认值）
        self.program_snapshot = program.clone();

        // 注册内置函数
        self.register_builtins()?;

        // 处理 extern 块
        for stmt in &program.statements {
            if let Statement::ExternBlock(eb) = stmt {
                self.register_extern_block(eb)?;
            }
        }

        // 收集 ADT 和类定义
        self.collect_adts(&program)?;
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

        // 类字段默认值中的字符串字面量也需要创建数据段（compile_class_constructor 不扫描）
        for s in self.collect_class_default_strings_from_program(&program) {
            self.get_or_create_string_data(&s)?;
        }

        // 扫描顶层 VarDecl，声明全局数据对象（必须在编译函数之前）
        // 预扫描：记录 future 全局变量 -> async 函数名，供两步式 await 类型推断
        for stmt in &program.statements {
            if let Statement::VarDecl(decl) = stmt {
                if let Some(Expr::Call(callee, _)) = decl.value.as_ref() {
                    if let Expr::Ident(fname) = callee.as_ref() {
                        if self.async_funcs.contains(fname.as_str()) {
                            self.global_spawn_funcs
                                .insert(decl.name.clone(), fname.clone());
                        }
                    }
                }
                if let Some(Expr::Spawn(fname, _)) = decl.value.as_ref() {
                    self.global_spawn_funcs
                        .insert(decl.name.clone(), fname.clone());
                }
            }
        }
        for stmt in &program.statements {
            if let Statement::VarDecl(decl) = stmt {
                let data_name = format!("_g_{}", decl.name);
                let data_id = self
                    .module
                    .declare_data(&data_name, Linkage::Local, true, false)
                    .map_err(|e| format!("Declare global data error: {}", e))?;
                // 初始化为 0 (null pointer)
                self.data_desc.clear();
                self.data_desc.define_zeroinit(8); // 8 bytes = pointer size
                self.module
                    .define_data(data_id, &self.data_desc)
                    .map_err(|e| format!("Define global data error: {}", e))?;
                self.global_data_ids.insert(decl.name.clone(), data_id);
                let gvar_ty = decl
                    .ty
                    .as_ref()
                    .map(|ty| self.normalize_bolide_type(ty))
                    .or_else(|| {
                        decl.value.as_ref().and_then(|v| {
                            self.infer_expr_type_static(v)
                                .map(|ty| self.normalize_bolide_type(&ty))
                        })
                    })
                    .unwrap_or(BolideType::Int);
                self.global_var_types.insert(decl.name.clone(), gvar_ty);
            }
        }

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

        // 包装顶层代码为合成入口函数（保留键 __bolide_entry__，链接符号为 C 入口 main）
        // 库模式不生成 main：产物作为静态库被 C 链接，入口由 C 端提供。
        if !self.lib_mode {
            let main_func = FuncDef {
                name: "__bolide_entry__".to_string(),
                is_async: false,
                is_export: false,
                type_params: vec![],
                params: vec![],
                return_type: Some(BolideType::Int),
                lifetime_deps: None,
                body: toplevel_stmts,
            };
            self.declare_function(&main_func)?;
            self.compile_function(&main_func)?;
        }

        // 收集外部库列表 (去重)
        let extern_libs: Vec<String> = self
            .extern_funcs
            .values()
            .map(|(lib_path, _)| lib_path.clone())
            .filter(|lib_path| lib_path != "bolide")
            .filter(|lib_path| !is_dynamic_lib_spec(lib_path))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // 生成 C 头文件（仅含 export 函数；无导出则为 None）
        let c_header = self.generate_c_header();

        // 生成目标文件
        let product = self.module.finish();
        let object_code = product.emit().map_err(|e| format!("Emit error: {}", e))?;

        Ok(AotCompileResult {
            object_code,
            extern_libs,
            c_header,
        })
    }

    /// 为 `export fn` 生成 C 头文件内容。仅支持数值/指针签名（与用户约定一致）。
    /// 复合类型（Str/List/Custom 等）映射为 `void*`，C 端需自行了解 ABI。
    fn generate_c_header(&self) -> Option<String> {
        if self.export_funcs.is_empty() {
            return None;
        }
        let c_ty = |ty: &Option<BolideType>| -> &'static str {
            match ty {
                Some(BolideType::Int) | Some(BolideType::Bool) => "long long",
                Some(BolideType::Float) => "double",
                None => "void",
                // 复合类型按指针传递（运行时内部表示）
                _ => "void*",
            }
        };
        let c_param_ty = |ty: &BolideType| -> &'static str {
            match ty {
                BolideType::Int | BolideType::Bool => "long long",
                BolideType::Float => "double",
                _ => "void*",
            }
        };

        let mut out = String::new();
        out.push_str("/* Auto-generated by Bolide compiler. Do not edit. */\n");
        out.push_str("#ifndef BOLIDE_EXPORTS_H\n#define BOLIDE_EXPORTS_H\n\n");
        out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
        for func in &self.export_funcs {
            let ret = c_ty(&func.return_type);
            out.push_str(ret);
            out.push(' ');
            out.push_str(&func.name);
            out.push('(');
            if func.params.is_empty() {
                out.push_str("void");
            } else {
                let params: Vec<String> = func
                    .params
                    .iter()
                    .map(|p| format!("{} {}", c_param_ty(&p.ty), p.name))
                    .collect();
                out.push_str(&params.join(", "));
            }
            out.push_str(");\n");
        }
        out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n#endif /* BOLIDE_EXPORTS_H */\n");
        Some(out)
    }

    /// 规范化类型名称，将 module.Type 转成内部模块符号。
    fn normalize_type_name(&self, name: &str) -> String {
        if name.contains('.') {
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() == 2 {
                let module = parts[0];
                let type_name = parts[1];
                if self.modules.contains_key(module) {
                    return format!("@{}_{}", module, type_name);
                }
            }
        }
        name.to_string()
    }

    /// 规范化 BolideType：class 保持 Custom，enum/union 统一成 Adt。
    fn normalize_bolide_type(&self, ty: &BolideType) -> BolideType {
        match ty {
            BolideType::Custom(name) => {
                let normalized = self.normalize_type_name(name);
                if self.adts.contains_key(&normalized) {
                    BolideType::Adt(normalized, vec![])
                } else {
                    BolideType::Custom(normalized)
                }
            }
            BolideType::Adt(name, args) => BolideType::Adt(
                self.normalize_type_name(name),
                args.iter().map(|t| self.normalize_bolide_type(t)).collect(),
            ),
            BolideType::List(inner) => {
                BolideType::List(Box::new(self.normalize_bolide_type(inner)))
            }
            BolideType::Dict(k, v) => BolideType::Dict(
                Box::new(self.normalize_bolide_type(k)),
                Box::new(self.normalize_bolide_type(v)),
            ),
            BolideType::Tuple(types) => BolideType::Tuple(
                types
                    .iter()
                    .map(|t| self.normalize_bolide_type(t))
                    .collect(),
            ),
            BolideType::FuncSig(params, ret) => BolideType::FuncSig(
                params
                    .iter()
                    .map(|t| self.normalize_bolide_type(t))
                    .collect(),
                ret.as_ref()
                    .map(|t| Box::new(self.normalize_bolide_type(t))),
            ),
            BolideType::Weak(inner) => {
                BolideType::Weak(Box::new(self.normalize_bolide_type(inner)))
            }
            BolideType::Unowned(inner) => {
                BolideType::Unowned(Box::new(self.normalize_bolide_type(inner)))
            }
            BolideType::Channel(inner) => {
                BolideType::Channel(Box::new(self.normalize_bolide_type(inner)))
            }
            _ => ty.clone(),
        }
    }

    /// Bolide 类型转换为 Cranelift 类型
    fn bolide_type_to_cranelift(&self, ty: &BolideType) -> types::Type {
        match ty {
            BolideType::Int => types::I64,
            BolideType::Float => types::F64,
            BolideType::Bool => types::I64,
            BolideType::Str => self.ptr_type,
            BolideType::Bytes => self.ptr_type,
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
            BolideType::Generic(_) => self.ptr_type,
            BolideType::Adt(_, _) => self.ptr_type,
            BolideType::Custom(_) => self.ptr_type,
            BolideType::Weak(_) => self.ptr_type,
            BolideType::Unowned(_) => self.ptr_type,
        }
    }

    /// 静态推断表达式类型（无编译上下文阶段，如全局变量扫描用）。
    fn infer_expr_type_static(&self, expr: &Expr) -> Option<BolideType> {
        match expr {
            Expr::Int(_) => Some(BolideType::Int),
            Expr::Float(_) => Some(BolideType::Float),
            Expr::Bool(_) => Some(BolideType::Bool),
            Expr::String(_) => Some(BolideType::Str),
            Expr::BigInt(_) => Some(BolideType::BigInt),
            Expr::Decimal(_) => Some(BolideType::Decimal),
            Expr::List(items) => {
                // 跨元素加宽：类型不一致则退化为 Dynamic（与运行时 compile_list 一致）
                let elem = if let Some(first) = items.first() {
                    let mut t = self
                        .infer_expr_type_static(first)
                        .unwrap_or(BolideType::Dynamic);
                    for item in items.iter().skip(1) {
                        let next = self
                            .infer_expr_type_static(item)
                            .unwrap_or(BolideType::Dynamic);
                        if t != next {
                            t = BolideType::Dynamic;
                        }
                    }
                    t
                } else {
                    BolideType::Dynamic
                };
                Some(BolideType::List(Box::new(elem)))
            }
            Expr::Dict(entries) => {
                // 跨条目加宽键/值类型（与运行时 compile_dict 一致）
                let (k, v) = if let Some((k0, v0)) = entries.first() {
                    let mut kt = self
                        .infer_expr_type_static(k0)
                        .unwrap_or(BolideType::Dynamic);
                    let mut vt = self
                        .infer_expr_type_static(v0)
                        .unwrap_or(BolideType::Dynamic);
                    for (k, v) in entries.iter().skip(1) {
                        let nk = self
                            .infer_expr_type_static(k)
                            .unwrap_or(BolideType::Dynamic);
                        if kt != nk {
                            kt = BolideType::Dynamic;
                        }
                        let nv = self
                            .infer_expr_type_static(v)
                            .unwrap_or(BolideType::Dynamic);
                        if vt != nv {
                            vt = BolideType::Dynamic;
                        }
                    }
                    (kt, vt)
                } else {
                    (BolideType::Dynamic, BolideType::Dynamic)
                };
                Some(BolideType::Dict(Box::new(k), Box::new(v)))
            }
            Expr::Tuple(items) => {
                let types: Vec<_> = items
                    .iter()
                    .map(|e| {
                        self.infer_expr_type_static(e)
                            .unwrap_or(BolideType::Dynamic)
                    })
                    .collect();
                Some(BolideType::Tuple(types))
            }
            Expr::Call(callee, args) => {
                if let Expr::Member(base, variant_name) = callee.as_ref() {
                    if let Expr::Ident(adt_name) = base.as_ref() {
                        if let Some(adt_info) = self.adts.get(adt_name) {
                            if let Some(variant) =
                                adt_info.variants.iter().find(|v| v.name == *variant_name)
                            {
                                let type_args =
                                    self.infer_adt_type_args_static(adt_info, variant, args);
                                return Some(BolideType::Adt(adt_name.clone(), type_args));
                            }
                        }
                    }
                }
                if let Expr::Ident(name) = callee.as_ref() {
                    match name.as_str() {
                        "int" | "input" => Some(BolideType::Int),
                        "float" => Some(BolideType::Float),
                        "str" => Some(BolideType::Str),
                        "bytes" => Some(BolideType::Bytes),
                        "bigint" => Some(BolideType::BigInt),
                        "decimal" => Some(BolideType::Decimal),
                        _ => {
                            if let Some(Some(ret_ty)) = self.func_return_types.get(name) {
                                Some(ret_ty.clone())
                            } else if self.classes.contains_key(name) {
                                Some(BolideType::Custom(name.clone()))
                            } else {
                                Some(BolideType::Int)
                            }
                        }
                    }
                } else {
                    Some(BolideType::Int)
                }
            }
            Expr::Spawn(_, _) => Some(BolideType::Future),
            Expr::Recv(_) => Some(BolideType::Int),
            Expr::None => Some(BolideType::Int),
            // await fn() → 协程返回类型；spawn all {..} → 元组
            Expr::Await(inner) => Some(self.static_awaited_type(inner)),
            Expr::SpawnAll(exprs) => {
                let elem_types: Vec<BolideType> = exprs
                    .iter()
                    .map(|e| self.spawn_item_type(e).unwrap_or(BolideType::Int))
                    .collect();
                Some(BolideType::Tuple(elem_types))
            }
            Expr::Closure {
                params,
                return_type,
                ..
            } => Some(BolideType::FuncSig(
                params.iter().map(|p| p.ty.clone()).collect(),
                return_type.clone().map(Box::new),
            )),
            // 裸函数名作为值：合成 FuncSig（一等函数支持）
            Expr::Ident(name) => {
                if self.functions.contains_key(name) {
                    let param_types: Vec<BolideType> = self
                        .func_params
                        .get(name)
                        .map(|ps| ps.iter().map(|p| p.ty.clone()).collect())
                        .unwrap_or_default();
                    let ret = self
                        .func_return_types
                        .get(name)
                        .cloned()
                        .flatten()
                        .map(Box::new);
                    Some(BolideType::FuncSig(param_types, ret))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn infer_adt_type_args_static(
        &self,
        adt_info: &AdtInfo,
        variant: &AdtVariantInfo,
        args: &[Expr],
    ) -> Vec<BolideType> {
        let mut bindings = HashMap::new();
        for (field, arg) in variant.fields.iter().zip(args.iter()) {
            let actual = self
                .infer_expr_type_static(arg)
                .unwrap_or(BolideType::Dynamic);
            Self::unify_generic_type_static(&field.ty, &actual, &mut bindings);
        }
        adt_info
            .type_params
            .iter()
            .map(|name| bindings.get(name).cloned().unwrap_or(BolideType::Dynamic))
            .collect()
    }

    fn unify_generic_type_static(
        pattern: &BolideType,
        actual: &BolideType,
        bindings: &mut HashMap<String, BolideType>,
    ) {
        match pattern {
            BolideType::Generic(name) => {
                bindings
                    .entry(name.clone())
                    .or_insert_with(|| actual.clone());
            }
            BolideType::List(p) => {
                if let BolideType::List(a) = actual {
                    Self::unify_generic_type_static(p, a, bindings);
                }
            }
            BolideType::Dict(pk, pv) => {
                if let BolideType::Dict(ak, av) = actual {
                    Self::unify_generic_type_static(pk, ak, bindings);
                    Self::unify_generic_type_static(pv, av, bindings);
                }
            }
            BolideType::Tuple(ps) => {
                if let BolideType::Tuple(as_) = actual {
                    for (p, a) in ps.iter().zip(as_.iter()) {
                        Self::unify_generic_type_static(p, a, bindings);
                    }
                }
            }
            BolideType::Adt(pn, ps) => {
                if let BolideType::Adt(an, as_) = actual {
                    if pn == an {
                        for (p, a) in ps.iter().zip(as_.iter()) {
                            Self::unify_generic_type_static(p, a, bindings);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn spawn_call_parts<'expr>(
        &self,
        expr: &'expr Expr,
    ) -> Result<(&'expr str, &'expr [Expr]), String> {
        match expr {
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    Ok((name.as_str(), args.as_slice()))
                } else {
                    Err("spawn all/select only supports direct function calls".to_string())
                }
            }
            Expr::Spawn(name, args) => Ok((name.as_str(), args.as_slice())),
            _ => Err("spawn all/select expects tasks like foo(...)".to_string()),
        }
    }

    fn spawn_item_type(&self, expr: &Expr) -> Result<BolideType, String> {
        let (func_name, _) = self.spawn_call_parts(expr)?;
        Ok(self
            .func_return_types
            .get(func_name)
            .cloned()
            .flatten()
            .unwrap_or(BolideType::Int))
    }

    /// 静态推断 await 目标的结果类型（全局变量扫描阶段用）。
    fn static_awaited_type(&self, expr: &Expr) -> BolideType {
        match expr {
            Expr::Call(callee, _) => {
                if let Expr::Ident(func_name) = callee.as_ref() {
                    return self
                        .func_return_types
                        .get(func_name)
                        .cloned()
                        .flatten()
                        .unwrap_or(BolideType::Int);
                }
                BolideType::Int
            }
            Expr::Spawn(func_name, _) => self
                .func_return_types
                .get(func_name)
                .cloned()
                .flatten()
                .unwrap_or(BolideType::Int),
            // future 变量：查 global_spawn_funcs 解析对应 async 函数返回类型
            Expr::Ident(var_name) => {
                if let Some(func_name) = self.global_spawn_funcs.get(var_name) {
                    self.func_return_types
                        .get(func_name)
                        .cloned()
                        .flatten()
                        .unwrap_or(BolideType::Int)
                } else {
                    BolideType::Int
                }
            }
            _ => BolideType::Int,
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

                    // 有别名时用别名作为模块命名空间（如 `import "x.bl" as mu` → mu.f）
                    let module_name = import
                        .alias
                        .clone()
                        .unwrap_or_else(|| Self::extract_module_name(file_path));
                    self.modules.insert(module_name.clone(), file_path.clone());

                    let imported = self.load_module(file_path)?;

                    let mut class_names: HashSet<String> = HashSet::new();
                    for imp_stmt in &imported.statements {
                        if let Statement::ClassDef(class) = imp_stmt {
                            class_names.insert(class.name.clone());
                        }
                    }

                    for imp_stmt in imported.statements {
                        match imp_stmt {
                            Statement::FuncDef(mut func) => {
                                func.name = format!("@{}_{}", module_name, func.name);
                                Self::rewrite_func_class_refs(
                                    &mut func,
                                    &module_name,
                                    &class_names,
                                );
                                merged_statements.push(Statement::FuncDef(func));
                            }
                            Statement::ClassDef(mut class) => {
                                let old_name = class.name.clone();
                                class.name = format!("@{}_{}", module_name, old_name);
                                for method in &mut class.methods {
                                    Self::rewrite_func_class_refs(
                                        method,
                                        &module_name,
                                        &class_names,
                                    );
                                }
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

        Ok(Program {
            statements: merged_statements,
        })
    }

    fn extract_module_name(file_path: &str) -> String {
        Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module")
            .to_string()
    }

    fn load_module(&self, file_path: &str) -> Result<Program, String> {
        let resolved = self.resolve_import_path(file_path);
        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| format!("Failed to load module '{}': {}", resolved, e))?;
        bolide_parser::parse_source(&content)
            .map_err(|e| format!("Failed to parse module '{}': {}", resolved, e))
    }

    fn rewrite_func_class_refs(
        func: &mut FuncDef,
        module_name: &str,
        class_names: &HashSet<String>,
    ) {
        if let Some(ref mut ret_ty) = func.return_type {
            Self::rewrite_type_class_refs(ret_ty, module_name, class_names);
        }
        for param in &mut func.params {
            Self::rewrite_type_class_refs(&mut param.ty, module_name, class_names);
        }
        for stmt in &mut func.body {
            Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
        }
    }

    fn rewrite_type_class_refs(
        ty: &mut BolideType,
        module_name: &str,
        class_names: &HashSet<String>,
    ) {
        match ty {
            BolideType::Custom(name) => {
                if class_names.contains(name) {
                    *name = format!("@{}_{}", module_name, name);
                }
            }
            BolideType::Adt(name, args) => {
                if class_names.contains(name) {
                    *name = format!("@{}_{}", module_name, name);
                }
                for arg in args {
                    Self::rewrite_type_class_refs(arg, module_name, class_names);
                }
            }
            BolideType::List(inner)
            | BolideType::Channel(inner)
            | BolideType::Weak(inner)
            | BolideType::Unowned(inner) => {
                Self::rewrite_type_class_refs(inner, module_name, class_names);
            }
            BolideType::Dict(k, v) => {
                Self::rewrite_type_class_refs(k, module_name, class_names);
                Self::rewrite_type_class_refs(v, module_name, class_names);
            }
            BolideType::Tuple(types) => {
                for ty in types {
                    Self::rewrite_type_class_refs(ty, module_name, class_names);
                }
            }
            BolideType::FuncSig(params, ret) => {
                for param in params {
                    Self::rewrite_type_class_refs(param, module_name, class_names);
                }
                if let Some(ret) = ret {
                    Self::rewrite_type_class_refs(ret, module_name, class_names);
                }
            }
            _ => {}
        }
    }

    fn rewrite_var_decl_class_refs(
        decl: &mut VarDecl,
        module_name: &str,
        class_names: &HashSet<String>,
    ) {
        if let Some(ref mut ty) = decl.ty {
            Self::rewrite_type_class_refs(ty, module_name, class_names);
        }
        if let Some(ref mut value) = decl.value {
            Self::rewrite_expr_class_refs(value, module_name, class_names);
        }
    }

    fn rewrite_stmt_class_refs(
        stmt: &mut Statement,
        module_name: &str,
        class_names: &HashSet<String>,
    ) {
        match stmt {
            Statement::VarDecl(decl) => {
                Self::rewrite_var_decl_class_refs(decl, module_name, class_names);
            }
            Statement::Assign(assign) => {
                Self::rewrite_expr_class_refs(&mut assign.target, module_name, class_names);
                Self::rewrite_expr_class_refs(&mut assign.value, module_name, class_names);
            }
            Statement::Expr(expr) | Statement::Return(Some(expr)) | Statement::Throw(expr) => {
                Self::rewrite_expr_class_refs(expr, module_name, class_names);
            }
            Statement::FuncDef(func) => {
                Self::rewrite_func_class_refs(func, module_name, class_names);
            }
            Statement::ClassDef(class) => {
                for field in &mut class.fields {
                    Self::rewrite_type_class_refs(&mut field.ty, module_name, class_names);
                    if let Some(ref mut value) = field.default_value {
                        Self::rewrite_expr_class_refs(value, module_name, class_names);
                    }
                }
                for method in &mut class.methods {
                    Self::rewrite_func_class_refs(method, module_name, class_names);
                }
            }
            Statement::If(if_stmt) => {
                Self::rewrite_expr_class_refs(&mut if_stmt.condition, module_name, class_names);
                for stmt in &mut if_stmt.then_body {
                    Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                }
                for (cond, body) in &mut if_stmt.elif_branches {
                    Self::rewrite_expr_class_refs(cond, module_name, class_names);
                    for stmt in body {
                        Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                    }
                }
                if let Some(ref mut body) = if_stmt.else_body {
                    for stmt in body {
                        Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                    }
                }
            }
            Statement::While(while_stmt) => {
                Self::rewrite_expr_class_refs(&mut while_stmt.condition, module_name, class_names);
                for stmt in &mut while_stmt.body {
                    Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                }
            }
            Statement::For(for_stmt) => {
                Self::rewrite_expr_class_refs(&mut for_stmt.iter, module_name, class_names);
                for stmt in &mut for_stmt.body {
                    Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                }
            }
            Statement::Pool(pool_stmt) => {
                Self::rewrite_expr_class_refs(&mut pool_stmt.size, module_name, class_names);
                for stmt in &mut pool_stmt.body {
                    Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                }
            }
            Statement::AwaitScope(scope) => {
                for stmt in &mut scope.body {
                    Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                }
            }
            Statement::SpawnSelect(select_stmt) => {
                for branch in &mut select_stmt.branches {
                    match branch {
                        bolide_parser::SpawnSelectBranch::Bind { expr, body, .. }
                        | bolide_parser::SpawnSelectBranch::Expr { expr, body } => {
                            Self::rewrite_expr_class_refs(expr, module_name, class_names);
                            for stmt in body {
                                Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                            }
                        }
                    }
                }
            }
            Statement::Send(send_stmt) => {
                Self::rewrite_expr_class_refs(&mut send_stmt.value, module_name, class_names);
            }
            Statement::Try(try_stmt) => {
                for stmt in &mut try_stmt.try_body {
                    Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                }
                for clause in &mut try_stmt.catch_clauses {
                    for stmt in &mut clause.body {
                        Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                    }
                }
                if let Some(ref mut body) = try_stmt.finally {
                    for stmt in body {
                        Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                    }
                }
            }
            Statement::Match(match_stmt) => {
                Self::rewrite_expr_class_refs(&mut match_stmt.expr, module_name, class_names);
                for arm in &mut match_stmt.arms {
                    for stmt in &mut arm.body {
                        Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                    }
                }
            }
            _ => {}
        }
    }

    fn rewrite_expr_class_refs(expr: &mut Expr, module_name: &str, class_names: &HashSet<String>) {
        match expr {
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_mut() {
                    if class_names.contains(name.as_str()) {
                        *name = format!("@{}_{}", module_name, name);
                    }
                }
                Self::rewrite_expr_class_refs(callee, module_name, class_names);
                for arg in args {
                    Self::rewrite_expr_class_refs(arg, module_name, class_names);
                }
            }
            Expr::NamedArg(_, value) | Expr::SpreadArg(value) | Expr::KwSpreadArg(value) => {
                Self::rewrite_expr_class_refs(value, module_name, class_names);
            }
            Expr::BinOp(left, _, right) => {
                Self::rewrite_expr_class_refs(left, module_name, class_names);
                Self::rewrite_expr_class_refs(right, module_name, class_names);
            }
            Expr::UnaryOp(_, operand) | Expr::Await(operand) => {
                Self::rewrite_expr_class_refs(operand, module_name, class_names);
            }
            Expr::Index(base, index) => {
                Self::rewrite_expr_class_refs(base, module_name, class_names);
                Self::rewrite_expr_class_refs(index, module_name, class_names);
            }
            Expr::Slice(base, start, end, step) => {
                Self::rewrite_expr_class_refs(base, module_name, class_names);
                if let Some(expr) = start {
                    Self::rewrite_expr_class_refs(expr, module_name, class_names);
                }
                if let Some(expr) = end {
                    Self::rewrite_expr_class_refs(expr, module_name, class_names);
                }
                if let Some(expr) = step {
                    Self::rewrite_expr_class_refs(expr, module_name, class_names);
                }
            }
            Expr::Member(base, _) => {
                Self::rewrite_expr_class_refs(base, module_name, class_names);
            }
            Expr::List(items) | Expr::Tuple(items) | Expr::SpawnAll(items) => {
                for item in items {
                    Self::rewrite_expr_class_refs(item, module_name, class_names);
                }
            }
            Expr::Dict(entries) => {
                for (key, value) in entries {
                    Self::rewrite_expr_class_refs(key, module_name, class_names);
                    Self::rewrite_expr_class_refs(value, module_name, class_names);
                }
            }
            Expr::Spawn(_, args) => {
                for arg in args {
                    Self::rewrite_expr_class_refs(arg, module_name, class_names);
                }
            }
            Expr::Closure {
                params,
                return_type,
                body,
            } => {
                for param in params {
                    Self::rewrite_type_class_refs(&mut param.ty, module_name, class_names);
                }
                if let Some(ret) = return_type {
                    Self::rewrite_type_class_refs(ret, module_name, class_names);
                }
                for stmt in body {
                    Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
                }
            }
            Expr::ListComprehension {
                expr, iter, filter, ..
            } => {
                Self::rewrite_expr_class_refs(expr, module_name, class_names);
                Self::rewrite_expr_class_refs(iter, module_name, class_names);
                if let Some(filter) = filter {
                    Self::rewrite_expr_class_refs(filter, module_name, class_names);
                }
            }
            _ => {}
        }
    }

    /// 解析 import 路径（确定性顺序，不依赖进程工作目录）：
    /// 1. 绝对路径按原样使用
    /// 2. 相对路径基于导入方源文件所在目录
    /// 3. BOLIDE_HOME 环境变量（开发期指向仓库根）
    /// 4. 可执行文件所在目录（发行版布局：std/ 与 bolide 可执行文件同级）
    fn resolve_import_path(&self, file_path: &str) -> String {
        let p = Path::new(file_path);
        if p.is_absolute() {
            return file_path.to_string();
        }
        if let Some(ref base) = self.base_dir {
            let joined = Path::new(base).join(p);
            if joined.exists() {
                return joined.to_string_lossy().to_string();
            }
        }
        if let Ok(home) = std::env::var("BOLIDE_HOME") {
            let joined = Path::new(&home).join(p);
            if joined.exists() {
                return joined.to_string_lossy().to_string();
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let joined = exe_dir.join(p);
                if joined.exists() {
                    return joined.to_string_lossy().to_string();
                }
            }
        }
        file_path.to_string()
    }

    /// 注册内置函数（表驱动）。
    ///
    /// 签名以 runtime crate 实际导出的 C 符号为唯一来源，与 JIT 保持一致。
    /// 表项格式：(内部查找键, 链接符号名, 参数类型, 返回类型)。
    /// 内部键约定：去掉 `bolide_` 前缀；`object_*` 系列直接使用裸名。
    /// 未被实际调用的 Import 声明不会在目标文件中产生未定义符号，因此可安全全量声明。
    fn register_builtins(&mut self) -> Result<(), String> {
        let p = self.ptr_type;
        let i64t = types::I64;
        let f64t = types::F64;
        let i32t = types::I32;
        let i8t = types::I8;

        let table: Vec<(&str, &str, Vec<types::Type>, Option<types::Type>)> = vec![
            // ---- 打印 ----
            ("@_print_int", "bolide_print_int", vec![i64t], None),
            ("@_print_float", "bolide_print_float", vec![f64t], None),
            ("@_print_bool", "bolide_print_bool", vec![i64t], None),
            ("@_print_bigint", "bolide_print_bigint", vec![p], None),
            ("@_print_decimal", "bolide_print_decimal", vec![p], None),
            ("@_print_string", "bolide_print_string", vec![p], None),
            ("@_print_bytes", "bolide_print_bytes", vec![p], None),
            ("@_print_dynamic", "bolide_print_dynamic", vec![p], None),
            ("@_print_list", "bolide_print_list", vec![p], None),
            ("@_print_dict", "bolide_print_dict", vec![p], None),
            ("@_print_tuple", "bolide_print_tuple", vec![p], None),
            ("@_println", "bolide_println", vec![], None),
            (
                "@_print_int_inline",
                "bolide_print_int_inline",
                vec![i64t],
                None,
            ),
            (
                "@_print_float_inline",
                "bolide_print_float_inline",
                vec![f64t],
                None,
            ),
            (
                "@_print_bool_inline",
                "bolide_print_bool_inline",
                vec![i64t],
                None,
            ),
            (
                "@_print_bigint_inline",
                "bolide_print_bigint_inline",
                vec![p],
                None,
            ),
            (
                "@_print_decimal_inline",
                "bolide_print_decimal_inline",
                vec![p],
                None,
            ),
            (
                "@_print_string_inline",
                "bolide_print_string_inline",
                vec![p],
                None,
            ),
            (
                "@_print_bytes_inline",
                "bolide_print_bytes_inline",
                vec![p],
                None,
            ),
            (
                "@_print_dynamic_inline",
                "bolide_print_dynamic_inline",
                vec![p],
                None,
            ),
            (
                "@_print_tuple_start",
                "bolide_print_tuple_start",
                vec![],
                None,
            ),
            (
                "@_print_tuple_separator",
                "bolide_print_tuple_separator",
                vec![],
                None,
            ),
            (
                "@_print_tuple_end_inline",
                "bolide_print_tuple_end_inline",
                vec![],
                None,
            ),
            // ---- 用户输入 ----
            ("@_input", "bolide_input", vec![], Some(p)),
            ("@_input_prompt", "bolide_input_prompt", vec![p], Some(p)),
            // ---- 字符串 ----
            (
                "@_string_from_slice",
                "bolide_string_from_slice",
                vec![p, i64t],
                Some(p),
            ),
            (
                "@_string_literal",
                "bolide_string_literal",
                vec![p, i64t],
                Some(p),
            ),
            ("@_string_new", "bolide_string_new", vec![p], Some(p)),
            (
                "@_string_as_cstr",
                "bolide_string_as_cstr",
                vec![p],
                Some(p),
            ),
            (
                "@_string_concat",
                "bolide_string_concat",
                vec![p, p],
                Some(p),
            ),
            (
                "@_string_concat_many",
                "bolide_string_concat_many",
                vec![p, i64t],
                Some(p),
            ),
            ("@_string_eq", "bolide_string_eq", vec![p, p], Some(i64t)),
            (
                "@_string_from_int",
                "bolide_string_from_int",
                vec![i64t],
                Some(p),
            ),
            (
                "@_string_from_float",
                "bolide_string_from_float",
                vec![f64t],
                Some(p),
            ),
            (
                "@_string_from_bool",
                "bolide_string_from_bool",
                vec![i64t],
                Some(p),
            ),
            (
                "@_string_from_bigint",
                "bolide_string_from_bigint",
                vec![p],
                Some(p),
            ),
            (
                "@_string_from_decimal",
                "bolide_string_from_decimal",
                vec![p],
                Some(p),
            ),
            (
                "@_string_to_int",
                "bolide_string_to_int",
                vec![p],
                Some(i64t),
            ),
            (
                "@_string_to_float",
                "bolide_string_to_float",
                vec![p],
                Some(f64t),
            ),
            ("@_string_len", "bolide_string_len", vec![p], Some(i64t)),
            ("@_string_retain", "bolide_string_retain", vec![p], Some(p)),
            ("@_string_release", "bolide_string_release", vec![p], None),
            ("@_string_clone", "bolide_string_clone", vec![p], Some(p)),
            // ---- bytes ----
            ("@_bytes_new", "bolide_bytes_new", vec![], Some(p)),
            ("@_bytes_retain", "bolide_bytes_retain", vec![p], Some(p)),
            ("@_bytes_release", "bolide_bytes_release", vec![p], None),
            ("@_bytes_clone", "bolide_bytes_clone", vec![p], Some(p)),
            ("@_bytes_len", "bolide_bytes_len", vec![p], Some(i64t)),
            ("@_bytes_get", "bolide_bytes_get", vec![p, i64t], Some(i64t)),
            (
                "@_bytes_set",
                "bolide_bytes_set",
                vec![p, i64t, i64t],
                Some(i64t),
            ),
            ("@_bytes_push", "bolide_bytes_push", vec![p, i64t], None),
            (
                "@_bytes_to_string_lossy",
                "bolide_bytes_to_string_lossy",
                vec![p],
                Some(p),
            ),
            // ---- 字符串方法 + 切片 + 索引 ----
            (
                "@_string_slice",
                "bolide_string_slice",
                vec![p, i64t, i64t, i64t, i64t],
                Some(p),
            ),
            (
                "@_string_char_at",
                "bolide_string_char_at",
                vec![p, i64t],
                Some(p),
            ),
            ("@_string_upper", "bolide_string_upper", vec![p], Some(p)),
            ("@_string_lower", "bolide_string_lower", vec![p], Some(p)),
            ("@_string_trim", "bolide_string_trim", vec![p], Some(p)),
            (
                "@_string_replace",
                "bolide_string_replace",
                vec![p, p, p],
                Some(p),
            ),
            (
                "@_string_repeat",
                "bolide_string_repeat",
                vec![p, i64t],
                Some(p),
            ),
            (
                "@_string_find",
                "bolide_string_find",
                vec![p, p],
                Some(i64t),
            ),
            (
                "@_string_contains",
                "bolide_string_contains",
                vec![p, p],
                Some(i64t),
            ),
            (
                "@_string_starts_with",
                "bolide_string_starts_with",
                vec![p, p],
                Some(i64t),
            ),
            (
                "@_string_ends_with",
                "bolide_string_ends_with",
                vec![p, p],
                Some(i64t),
            ),
            (
                "@_string_count",
                "bolide_string_count",
                vec![p, p],
                Some(i64t),
            ),
            ("@_string_split", "bolide_string_split", vec![p, p], Some(p)),
            // ---- 字符串方法 + 切片 + 索引 ----
            (
                "@_string_slice",
                "bolide_string_slice",
                vec![p, i64t, i64t, i64t, i64t],
                Some(p),
            ),
            (
                "@_string_char_at",
                "bolide_string_char_at",
                vec![p, i64t],
                Some(p),
            ),
            ("@_string_upper", "bolide_string_upper", vec![p], Some(p)),
            ("@_string_lower", "bolide_string_lower", vec![p], Some(p)),
            ("@_string_trim", "bolide_string_trim", vec![p], Some(p)),
            (
                "@_string_replace",
                "bolide_string_replace",
                vec![p, p, p],
                Some(p),
            ),
            (
                "@_string_repeat",
                "bolide_string_repeat",
                vec![p, i64t],
                Some(p),
            ),
            (
                "@_string_find",
                "bolide_string_find",
                vec![p, p],
                Some(i64t),
            ),
            (
                "@_string_contains",
                "bolide_string_contains",
                vec![p, p],
                Some(i64t),
            ),
            (
                "@_string_starts_with",
                "bolide_string_starts_with",
                vec![p, p],
                Some(i64t),
            ),
            (
                "@_string_ends_with",
                "bolide_string_ends_with",
                vec![p, p],
                Some(i64t),
            ),
            (
                "@_string_count",
                "bolide_string_count",
                vec![p, p],
                Some(i64t),
            ),
            ("@_string_split", "bolide_string_split", vec![p, p], Some(p)),
            (
                "@_list_slice_step",
                "bolide_list_slice_step",
                vec![p, i64t, i64t, i64t, i64t],
                Some(p),
            ),
            (
                "@_tuple_slice_step",
                "bolide_tuple_slice_step",
                vec![p, i64t, i64t, i64t, i64t],
                Some(p),
            ),
            // ---- BigInt ----
            (
                "@_bigint_from_i64",
                "bolide_bigint_from_i64",
                vec![i64t],
                Some(p),
            ),
            (
                "@_bigint_from_str",
                "bolide_bigint_from_str",
                vec![p, i64t],
                Some(p),
            ),
            ("@_bigint_add", "bolide_bigint_add", vec![p, p], Some(p)),
            ("@_bigint_sub", "bolide_bigint_sub", vec![p, p], Some(p)),
            ("@_bigint_mul", "bolide_bigint_mul", vec![p, p], Some(p)),
            ("@_bigint_div", "bolide_bigint_div", vec![p, p], Some(p)),
            ("@_bigint_rem", "bolide_bigint_rem", vec![p, p], Some(p)),
            ("@_bigint_neg", "bolide_bigint_neg", vec![p], Some(p)),
            ("@_bigint_eq", "bolide_bigint_eq", vec![p, p], Some(i64t)),
            ("@_bigint_ne", "bolide_bigint_ne", vec![p, p], Some(i64t)),
            ("@_bigint_lt", "bolide_bigint_lt", vec![p, p], Some(i64t)),
            ("@_bigint_le", "bolide_bigint_le", vec![p, p], Some(i64t)),
            ("@_bigint_gt", "bolide_bigint_gt", vec![p, p], Some(i64t)),
            ("@_bigint_ge", "bolide_bigint_ge", vec![p, p], Some(i64t)),
            (
                "@_bigint_to_i64",
                "bolide_bigint_to_i64",
                vec![p],
                Some(i64t),
            ),
            (
                "@_bigint_to_f64",
                "bolide_bigint_to_f64",
                vec![p],
                Some(f64t),
            ),
            ("@_bigint_clone", "bolide_bigint_clone", vec![p], Some(p)),
            ("@_bigint_retain", "bolide_bigint_retain", vec![p], Some(p)),
            ("@_bigint_release", "bolide_bigint_release", vec![p], None),
            (
                "@_bigint_debug_stats",
                "bolide_bigint_debug_stats",
                vec![],
                None,
            ),
            // ---- Decimal ----
            (
                "@_decimal_from_i64",
                "bolide_decimal_from_i64",
                vec![i64t],
                Some(p),
            ),
            (
                "@_decimal_from_f64",
                "bolide_decimal_from_f64",
                vec![f64t],
                Some(p),
            ),
            (
                "@_decimal_from_str",
                "bolide_decimal_from_str",
                vec![p, i64t],
                Some(p),
            ),
            ("@_decimal_add", "bolide_decimal_add", vec![p, p], Some(p)),
            ("@_decimal_sub", "bolide_decimal_sub", vec![p, p], Some(p)),
            ("@_decimal_mul", "bolide_decimal_mul", vec![p, p], Some(p)),
            ("@_decimal_div", "bolide_decimal_div", vec![p, p], Some(p)),
            ("@_decimal_rem", "bolide_decimal_rem", vec![p, p], Some(p)),
            ("@_decimal_neg", "bolide_decimal_neg", vec![p], Some(p)),
            ("@_decimal_eq", "bolide_decimal_eq", vec![p, p], Some(i64t)),
            ("@_decimal_ne", "bolide_decimal_ne", vec![p, p], Some(i64t)),
            ("@_decimal_lt", "bolide_decimal_lt", vec![p, p], Some(i64t)),
            ("@_decimal_le", "bolide_decimal_le", vec![p, p], Some(i64t)),
            ("@_decimal_gt", "bolide_decimal_gt", vec![p, p], Some(i64t)),
            ("@_decimal_ge", "bolide_decimal_ge", vec![p, p], Some(i64t)),
            (
                "@_decimal_to_i64",
                "bolide_decimal_to_i64",
                vec![p],
                Some(i64t),
            ),
            (
                "@_decimal_to_f64",
                "bolide_decimal_to_f64",
                vec![p],
                Some(f64t),
            ),
            ("@_decimal_clone", "bolide_decimal_clone", vec![p], Some(p)),
            (
                "@_decimal_retain",
                "bolide_decimal_retain",
                vec![p],
                Some(p),
            ),
            ("@_decimal_release", "bolide_decimal_release", vec![p], None),
            ("@_decimal_abs", "bolide_decimal_abs", vec![p], Some(p)),
            ("@_decimal_ceil", "bolide_decimal_ceil", vec![p], Some(p)),
            ("@_decimal_floor", "bolide_decimal_floor", vec![p], Some(p)),
            ("@_decimal_round", "bolide_decimal_round", vec![p], Some(p)),
            (
                "@_decimal_round_dp",
                "bolide_decimal_round_dp",
                vec![p, i32t],
                Some(p),
            ),
            // ---- Dynamic ----
            (
                "@_dynamic_from_int",
                "bolide_dynamic_from_int",
                vec![i64t],
                Some(p),
            ),
            (
                "@_dynamic_from_float",
                "bolide_dynamic_from_float",
                vec![f64t],
                Some(p),
            ),
            (
                "@_dynamic_from_bool",
                "bolide_dynamic_from_bool",
                vec![i64t],
                Some(p),
            ),
            (
                "@_dynamic_from_string",
                "bolide_dynamic_from_string",
                vec![p],
                Some(p),
            ),
            (
                "@_dynamic_from_list",
                "bolide_dynamic_from_list",
                vec![p],
                Some(p),
            ),
            (
                "@_dynamic_from_bytes",
                "bolide_dynamic_from_bytes",
                vec![p],
                Some(p),
            ),
            (
                "@_dynamic_from_dict",
                "bolide_dynamic_from_dict",
                vec![p],
                Some(p),
            ),
            (
                "@_dynamic_from_bigint",
                "bolide_dynamic_from_bigint",
                vec![p],
                Some(p),
            ),
            (
                "@_dynamic_from_decimal",
                "bolide_dynamic_from_decimal",
                vec![p],
                Some(p),
            ),
            ("@_dynamic_add", "bolide_dynamic_add", vec![p, p], Some(p)),
            ("@_dynamic_sub", "bolide_dynamic_sub", vec![p, p], Some(p)),
            ("@_dynamic_mul", "bolide_dynamic_mul", vec![p, p], Some(p)),
            ("@_dynamic_div", "bolide_dynamic_div", vec![p, p], Some(p)),
            ("@_dynamic_neg", "bolide_dynamic_neg", vec![p], Some(p)),
            ("@_dynamic_eq", "bolide_dynamic_eq", vec![p, p], Some(i64t)),
            ("@_dynamic_lt", "bolide_dynamic_lt", vec![p, p], Some(i64t)),
            ("@_dynamic_le", "bolide_dynamic_le", vec![p, p], Some(i64t)),
            ("@_dynamic_gt", "bolide_dynamic_gt", vec![p, p], Some(i64t)),
            ("@_dynamic_ge", "bolide_dynamic_ge", vec![p, p], Some(i64t)),
            ("@_dynamic_clone", "bolide_dynamic_clone", vec![p], Some(p)),
            (
                "@_dynamic_retain",
                "bolide_dynamic_retain",
                vec![p],
                Some(p),
            ),
            ("@_dynamic_release", "bolide_dynamic_release", vec![p], None),
            (
                "@_exception_set",
                "bolide_exception_set",
                vec![p, i64t],
                None,
            ),
            ("@_exception_get", "bolide_exception_get", vec![], Some(p)),
            (
                "@_exception_tag",
                "bolide_exception_tag",
                vec![],
                Some(i64t),
            ),
            ("@_throw_uncaught", "bolide_throw_uncaught", vec![p], None),
            (
                "@_dynamic_get_type",
                "bolide_dynamic_get_type",
                vec![p],
                Some(i64t),
            ),
            (
                "@_dynamic_to_int",
                "bolide_dynamic_to_int",
                vec![p],
                Some(i64t),
            ),
            (
                "@_dynamic_to_float",
                "bolide_dynamic_to_float",
                vec![p],
                Some(f64t),
            ),
            (
                "@_dynamic_to_string",
                "bolide_dynamic_to_string",
                vec![p],
                Some(p),
            ),
            (
                "@_dynamic_is_truthy",
                "bolide_dynamic_is_truthy",
                vec![p],
                Some(i64t),
            ),
            ("@_dynamic_none", "bolide_dynamic_none", vec![], Some(p)),
            // ---- List ----
            ("@_list_new", "bolide_list_new", vec![i8t], Some(p)),
            (
                "@_list_with_capacity",
                "bolide_list_with_capacity",
                vec![i8t, i64t],
                Some(p),
            ),
            ("@_list_push", "bolide_list_push", vec![p, i64t], None),
            ("@_list_pop", "bolide_list_pop", vec![p], Some(i64t)),
            ("@_list_len", "bolide_list_len", vec![p], Some(i64t)),
            ("@_list_get", "bolide_list_get", vec![p, i64t], Some(i64t)),
            (
                "@_list_set",
                "bolide_list_set",
                vec![p, i64t, i64t],
                Some(i64t),
            ),
            (
                "@_list_insert",
                "bolide_list_insert",
                vec![p, i64t, i64t],
                None,
            ),
            (
                "@_list_remove",
                "bolide_list_remove",
                vec![p, i64t],
                Some(i64t),
            ),
            ("@_list_clear", "bolide_list_clear", vec![p], None),
            ("@_list_reverse", "bolide_list_reverse", vec![p], None),
            ("@_list_extend", "bolide_list_extend", vec![p, p], None),
            (
                "@_list_contains",
                "bolide_list_contains",
                vec![p, i64t],
                Some(i64t),
            ),
            (
                "@_list_index_of",
                "bolide_list_index_of",
                vec![p, i64t],
                Some(i64t),
            ),
            (
                "@_list_count",
                "bolide_list_count",
                vec![p, i64t],
                Some(i64t),
            ),
            ("@_list_sort", "bolide_list_sort", vec![p], None),
            (
                "@_list_slice",
                "bolide_list_slice",
                vec![p, i64t, i64t],
                Some(p),
            ),
            (
                "@_list_slice_step",
                "bolide_list_slice_step",
                vec![p, i64t, i64t, i64t, i64t],
                Some(p),
            ),
            (
                "@_list_is_empty",
                "bolide_list_is_empty",
                vec![p],
                Some(i64t),
            ),
            ("@_list_first", "bolide_list_first", vec![p], Some(i64t)),
            ("@_list_last", "bolide_list_last", vec![p], Some(i64t)),
            ("@_list_map", "bolide_list_map", vec![p, p, i8t], Some(p)),
            ("@_list_filter", "bolide_list_filter", vec![p, p], Some(p)),
            (
                "@_list_elem_type",
                "bolide_list_elem_type",
                vec![p],
                Some(i8t),
            ),
            ("@_list_retain", "bolide_list_retain", vec![p], Some(p)),
            ("@_list_release", "bolide_list_release", vec![p], None),
            ("@_list_clone", "bolide_list_clone", vec![p], Some(p)),
            ("@_list_free", "bolide_list_free", vec![p], None),
            // ---- Dict ----
            ("@_dict_new", "bolide_dict_new", vec![i8t, i8t], Some(p)),
            ("@_dict_set", "bolide_dict_set", vec![p, i64t, i64t], None),
            ("@_dict_get", "bolide_dict_get", vec![p, i64t], Some(i64t)),
            (
                "@_dict_contains",
                "bolide_dict_contains",
                vec![p, i64t],
                Some(i64t),
            ),
            (
                "@_dict_remove",
                "bolide_dict_remove",
                vec![p, i64t],
                Some(i64t),
            ),
            ("@_dict_len", "bolide_dict_len", vec![p], Some(i64t)),
            (
                "@_dict_is_empty",
                "bolide_dict_is_empty",
                vec![p],
                Some(i64t),
            ),
            ("@_dict_clear", "bolide_dict_clear", vec![p], None),
            ("@_dict_keys", "bolide_dict_keys", vec![p], Some(p)),
            ("@_dict_values", "bolide_dict_values", vec![p], Some(p)),
            ("@_dict_iter", "bolide_dict_iter", vec![p], Some(p)),
            ("@_dict_retain", "bolide_dict_retain", vec![p], None),
            ("@_dict_release", "bolide_dict_release", vec![p], None),
            ("@_dict_clone", "bolide_dict_clone", vec![p], Some(p)),
            ("@_dict_extend", "bolide_dict_extend", vec![p, p], None),
            (
                "@_dict_key_type",
                "bolide_dict_key_type",
                vec![p],
                Some(i8t),
            ),
            (
                "@_dict_value_type",
                "bolide_dict_value_type",
                vec![p],
                Some(i8t),
            ),
            // ---- Tuple ----
            ("@_tuple_new", "bolide_tuple_new", vec![i64t], Some(p)),
            (
                "@_tuple_new_typed",
                "bolide_tuple_new_typed",
                vec![i64t, p],
                Some(p),
            ),
            ("@_tuple_set", "bolide_tuple_set", vec![p, i64t, i64t], None),
            (
                "@_tuple_set_typed",
                "bolide_tuple_set_typed",
                vec![p, i64t, i64t, i8t],
                None,
            ),
            ("@_tuple_get", "bolide_tuple_get", vec![p, i64t], Some(i64t)),
            (
                "@_tuple_get_type",
                "bolide_tuple_get_type",
                vec![p, i64t],
                Some(i8t),
            ),
            ("@_tuple_len", "bolide_tuple_len", vec![p], Some(i64t)),
            ("@_tuple_free", "bolide_tuple_free", vec![p], None),
            ("@_tuple_retain", "bolide_tuple_retain", vec![p], None),
            ("@_tuple_clone", "bolide_tuple_clone", vec![p], Some(p)),
            (
                "@_tuple_release",
                "bolide_tuple_release",
                vec![p],
                Some(i64t),
            ),
            (
                "@_tuple_debug_stats",
                "bolide_tuple_debug_stats",
                vec![],
                None,
            ),
            (
                "@_tuple_slice_step",
                "bolide_tuple_slice_step",
                vec![p, i64t, i64t, i64t, i64t],
                Some(p),
            ),
            // ---- 内存 ----
            ("@_bolide_alloc", "bolide_alloc", vec![i64t], Some(p)),
            ("@_bolide_free", "bolide_free", vec![p, i64t], None),
            // ---- Object (RC 对象，裸链接名) ----
            ("@_object_alloc", "object_alloc", vec![i64t], Some(p)),
            ("@_object_release", "object_release", vec![p], None),
            ("@_object_retain", "object_retain", vec![p], None),
            ("@_object_clone", "object_clone", vec![p], Some(p)),
            ("@_object_weak_retain", "object_weak_retain", vec![p], None),
            (
                "@_object_weak_release",
                "object_weak_release",
                vec![p],
                None,
            ),
            ("@_object_weak_clone", "object_weak_clone", vec![p], Some(p)),
            (
                "@_object_assert_alive",
                "object_assert_alive",
                vec![p],
                None,
            ),
            ("@_object_is_alive", "object_is_alive", vec![p], Some(i64t)),
            (
                "@_object_ref_count",
                "object_ref_count",
                vec![p],
                Some(i64t),
            ),
            // ---- Closure ----
            (
                "@_closure_new",
                "bolide_closure_new",
                vec![p, p, i64t, p],
                Some(p),
            ),
            (
                "@_closure_fn_ptr",
                "bolide_closure_fn_ptr",
                vec![p],
                Some(p),
            ),
            (
                "@_closure_env_ptr",
                "bolide_closure_env_ptr",
                vec![p],
                Some(p),
            ),
            ("@_closure_retain", "bolide_closure_retain", vec![p], None),
            ("@_closure_release", "bolide_closure_release", vec![p], None),
            // ---- 线程 ----
            (
                "@_thread_spawn_int",
                "bolide_thread_spawn_int",
                vec![p],
                Some(p),
            ),
            (
                "@_thread_spawn_float",
                "bolide_thread_spawn_float",
                vec![p],
                Some(p),
            ),
            (
                "@_thread_spawn_ptr",
                "bolide_thread_spawn_ptr",
                vec![p],
                Some(p),
            ),
            (
                "@_thread_spawn_int_with_env",
                "bolide_thread_spawn_int_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_thread_spawn_float_with_env",
                "bolide_thread_spawn_float_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_thread_spawn_ptr_with_env",
                "bolide_thread_spawn_ptr_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_thread_join_int",
                "bolide_thread_join_int",
                vec![p],
                Some(i64t),
            ),
            (
                "@_thread_join_float",
                "bolide_thread_join_float",
                vec![p],
                Some(f64t),
            ),
            (
                "@_thread_join_ptr",
                "bolide_thread_join_ptr",
                vec![p],
                Some(p),
            ),
            (
                "@_thread_handle_free",
                "bolide_thread_handle_free",
                vec![p],
                None,
            ),
            ("@_thread_cancel", "bolide_thread_cancel", vec![p], None),
            (
                "@_thread_is_cancelled",
                "bolide_thread_is_cancelled",
                vec![p],
                Some(i64t),
            ),
            // ---- 线程池 ----
            ("@_pool_create", "bolide_pool_create", vec![i64t], Some(p)),
            ("@_pool_enter", "bolide_pool_enter", vec![p], None),
            ("@_pool_exit", "bolide_pool_exit", vec![], None),
            (
                "@_pool_is_active",
                "bolide_pool_is_active",
                vec![],
                Some(i64t),
            ),
            (
                "@_pool_spawn_int",
                "bolide_pool_spawn_int",
                vec![p],
                Some(p),
            ),
            (
                "@_pool_spawn_float",
                "bolide_pool_spawn_float",
                vec![p],
                Some(p),
            ),
            (
                "@_pool_spawn_ptr",
                "bolide_pool_spawn_ptr",
                vec![p],
                Some(p),
            ),
            (
                "@_pool_spawn_int_with_env",
                "bolide_pool_spawn_int_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_pool_spawn_float_with_env",
                "bolide_pool_spawn_float_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_pool_spawn_ptr_with_env",
                "bolide_pool_spawn_ptr_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_pool_join_int",
                "bolide_pool_join_int",
                vec![p],
                Some(i64t),
            ),
            (
                "@_pool_join_float",
                "bolide_pool_join_float",
                vec![p],
                Some(f64t),
            ),
            ("@_pool_join_ptr", "bolide_pool_join_ptr", vec![p], Some(p)),
            (
                "@_pool_handle_free",
                "bolide_pool_handle_free",
                vec![p],
                None,
            ),
            (
                "@_pool_select_wait_first",
                "bolide_pool_select_wait_first",
                vec![p, i64t],
                Some(i64t),
            ),
            ("@_pool_destroy", "bolide_pool_destroy", vec![p], None),
            // ---- 通道 ----
            ("@_channel_create", "bolide_channel_create", vec![], Some(p)),
            (
                "@_channel_create_buffered",
                "bolide_channel_create_buffered",
                vec![i64t],
                Some(p),
            ),
            (
                "@_channel_send",
                "bolide_channel_send",
                vec![p, i64t],
                Some(i64t),
            ),
            ("@_channel_recv", "bolide_channel_recv", vec![p], Some(i64t)),
            (
                "@_channel_try_recv",
                "bolide_channel_try_recv",
                vec![p, p],
                Some(i64t),
            ),
            ("@_channel_close", "bolide_channel_close", vec![p], None),
            ("@_channel_free", "bolide_channel_free", vec![p], None),
            (
                "@_channel_is_closed",
                "bolide_channel_is_closed",
                vec![p],
                Some(i64t),
            ),
            (
                "@_channel_select",
                "bolide_channel_select",
                vec![p, i64t, i64t, p],
                Some(i64t),
            ),
            // ---- 协程 ----
            (
                "@_coroutine_spawn_int",
                "bolide_coroutine_spawn_int",
                vec![p],
                Some(p),
            ),
            (
                "@_coroutine_spawn_float",
                "bolide_coroutine_spawn_float",
                vec![p],
                Some(p),
            ),
            (
                "@_coroutine_spawn_ptr",
                "bolide_coroutine_spawn_ptr",
                vec![p],
                Some(p),
            ),
            (
                "@_coroutine_spawn_int_with_env",
                "bolide_coroutine_spawn_int_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_coroutine_spawn_float_with_env",
                "bolide_coroutine_spawn_float_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_coroutine_spawn_ptr_with_env",
                "bolide_coroutine_spawn_ptr_with_env",
                vec![p, p],
                Some(p),
            ),
            (
                "@_coroutine_await_int",
                "bolide_coroutine_await_int",
                vec![p],
                Some(i64t),
            ),
            (
                "@_coroutine_await_float",
                "bolide_coroutine_await_float",
                vec![p],
                Some(f64t),
            ),
            (
                "@_coroutine_await_ptr",
                "bolide_coroutine_await_ptr",
                vec![p],
                Some(p),
            ),
            (
                "@_coroutine_cancel",
                "bolide_coroutine_cancel",
                vec![p],
                None,
            ),
            ("@_coroutine_free", "bolide_coroutine_free", vec![p], None),
            // ---- 作用域 / select ----
            ("@_scope_enter", "bolide_scope_enter", vec![], None),
            ("@_scope_exit", "bolide_scope_exit", vec![], None),
            ("@_scope_register", "bolide_scope_register", vec![p], None),
            (
                "@_select_wait_first",
                "bolide_select_wait_first",
                vec![p, i64t],
                Some(i64t),
            ),
            // ---- FFI ----
            (
                "@_ffi_load_library",
                "bolide_ffi_load_library",
                vec![p],
                Some(i64t),
            ),
            (
                "@_ffi_get_symbol",
                "bolide_ffi_get_symbol",
                vec![p, p],
                Some(p),
            ),
            (
                "@_test_callback",
                "bolide_test_callback",
                vec![p, i64t, i64t],
                Some(i64t),
            ),
            ("@_map_int", "bolide_map_int", vec![p, i64t], Some(i64t)),
        ];

        for (internal, linker, params, ret) in &table {
            let mut sig = self.module.make_signature();
            for pt in params {
                sig.params.push(AbiParam::new(*pt));
            }
            if let Some(r) = ret {
                sig.returns.push(AbiParam::new(*r));
            }
            let id = self
                .module
                .declare_function(linker, Linkage::Import, &sig)
                .map_err(|e| format!("declare runtime {} ({}): {}", internal, linker, e))?;
            self.functions.insert(internal.to_string(), id);
        }

        Ok(())
    }

    /// 注册 extern 块中的函数
    fn register_extern_block(&mut self, eb: &ExternBlock) -> Result<(), String> {
        validate_extern_lib_spec(&eb.lib_path)?;
        let is_dynamic = is_dynamic_lib_spec(&eb.lib_path);

        for decl in &eb.declarations {
            if let ExternDecl::Function(func) = decl {
                if !is_dynamic {
                    let mut sig = self.module.make_signature();
                    for param in &func.params {
                        sig.params
                            .push(AbiParam::new(self.ctype_to_cranelift(&param.ty)));
                    }
                    if let Some(ref ret_ty) = func.return_type {
                        sig.returns
                            .push(AbiParam::new(self.ctype_to_cranelift(ret_ty)));
                    }
                    let id = self
                        .module
                        .declare_function(&func.name, Linkage::Import, &sig)
                        .map_err(|e| format!("{}", e))?;
                    self.functions.insert(func.name.clone(), id);
                }
                self.extern_funcs
                    .insert(func.name.clone(), (eb.lib_path.clone(), func.clone()));
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
            CType::Long
            | CType::ULong
            | CType::LongLong
            | CType::ULongLong
            | CType::I64
            | CType::U64
            | CType::SizeT
            | CType::PtrDiffT => types::I64,
            CType::Float => types::F32,
            CType::Double => types::F64,
            CType::Bool => types::I8,
            CType::Ptr(_) | CType::Array(_, _) | CType::FuncPtr { .. } => self.ptr_type,
            CType::Struct(_) => self.ptr_type,
        }
    }

    fn collect_adts(&mut self, program: &Program) -> Result<(), String> {
        for stmt in &program.statements {
            if let Statement::EnumDef(def) = stmt {
                if self.adts.contains_key(&def.name) {
                    return Err(format!("Duplicate enum/union '{}'", def.name));
                }
                let mut seen_variants = HashSet::new();
                let mut variants = Vec::new();
                let mut max_fields = 0usize;
                for (idx, variant) in def.variants.iter().enumerate() {
                    if !seen_variants.insert(variant.name.clone()) {
                        return Err(format!("Duplicate variant '{}.{}'", def.name, variant.name));
                    }
                    max_fields = max_fields.max(variant.fields.len());
                    let fields = variant
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(field_idx, field)| AdtFieldInfo {
                            name: field.name.clone(),
                            ty: field.ty.clone(),
                            offset: 8 + field_idx * 8,
                        })
                        .collect();
                    variants.push(AdtVariantInfo {
                        name: variant.name.clone(),
                        tag: idx as i64,
                        fields,
                    });
                }
                self.adts.insert(
                    def.name.clone(),
                    AdtInfo {
                        name: def.name.clone(),
                        type_params: def.type_params.clone(),
                        variants,
                        size: 8 + max_fields * 8,
                    },
                );
            }
        }
        Ok(())
    }

    fn find_adt_variant(&self, adt_name: &str, variant_name: &str) -> Option<&AdtVariantInfo> {
        self.adts
            .get(adt_name)
            .and_then(|info| info.variants.iter().find(|v| v.name == variant_name))
    }

    /// 收集类定义
    fn collect_classes(&mut self, program: &Program) -> Result<(), String> {
        // 按程序声明顺序分配异常类型标签（>=100），保证 JIT/AOT 一致
        for stmt in &program.statements {
            if let Statement::ClassDef(class) = stmt {
                if !self.class_tags.contains_key(&class.name) {
                    let tag = 100 + self.class_tags.len() as i64;
                    self.class_tags.insert(class.name.clone(), tag);
                }
            }
        }

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
                        default_value: field.default_value.clone(),
                    });
                    offset += size;
                }

                let methods: Vec<String> = class.methods.iter().map(|m| m.name.clone()).collect();

                self.classes.insert(
                    class.name.clone(),
                    ClassInfo {
                        name: class.name.clone(),
                        parent: class.parent.clone(),
                        fields,
                        methods,
                        size: offset,
                    },
                );
            }
        }
        Ok(())
    }

    /// 声明函数
    /// 用户函数/类符号的链接名：加 `bolide_user_` 命名空间，避免与运行时 C 符号、
    /// 合成入口 `main`、以及 CRT/libc 符号冲突。内部查找键仍用裸名（用户写得出的名字）。
    /// 合成顶层入口使用保留键 `__bolide_entry__`，其链接符号固定为 C 入口名 `main`。
    fn user_link_name(name: &str) -> String {
        if name == "__bolide_entry__" {
            "main".to_string()
        } else {
            format!("bolide_user_{}", name)
        }
    }

    /// 导出函数（`export fn`）的链接名：使用裸名，供 C 端按声明名直接链接，
    /// 不加 `bolide_user_` 前缀。
    fn export_link_name(name: &str) -> String {
        name.to_string()
    }

    fn declare_function(&mut self, func: &FuncDef) -> Result<(), String> {
        let mut sig = self.module.make_signature();

        for param in &func.params {
            let param_ty = self.normalize_bolide_type(&param.ty);
            let ty = self.bolide_type_to_cranelift(&param_ty);
            sig.params.push(AbiParam::new(ty));
        }

        if let Some(ref ret_ty) = func.return_type {
            let ret_ty = self.normalize_bolide_type(ret_ty);
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(&ret_ty)));
        }

        let link_name = if func.is_export {
            self.export_funcs.push(func.clone());
            Self::export_link_name(&func.name)
        } else {
            Self::user_link_name(&func.name)
        };
        let func_id = self
            .module
            .declare_function(&link_name, Linkage::Export, &sig)
            .map_err(|e| format!("Declare function error: {}", e))?;

        self.functions.insert(func.name.clone(), func_id);
        self.func_return_types.insert(
            func.name.clone(),
            func.return_type
                .as_ref()
                .map(|ty| self.normalize_bolide_type(ty)),
        );
        let mut params = func.params.clone();
        for param in &mut params {
            param.ty = self.normalize_bolide_type(&param.ty);
        }
        self.func_params.insert(func.name.clone(), params);

        if func.lifetime_deps.is_some() {
            self.lifetime_funcs.insert(func.name.clone());
        }
        Ok(())
    }

    /// 声明类构造函数
    fn declare_class_constructor(&mut self, class_name: &str) -> Result<(), String> {
        let class_info = self
            .classes
            .get(class_name)
            .ok_or_else(|| format!("Class {} not found", class_name))?
            .clone();

        let mut sig = self.module.make_signature();
        // 构造函数参数：每个字段一个参数
        for field in &class_info.fields {
            sig.params
                .push(AbiParam::new(self.bolide_type_to_cranelift(&field.ty)));
        }
        // 返回对象指针
        sig.returns.push(AbiParam::new(self.ptr_type));

        let link_name = Self::user_link_name(class_name);
        let func_id = self
            .module
            .declare_function(&link_name, Linkage::Export, &sig)
            .map_err(|e| format!("Declare constructor error: {}", e))?;

        self.functions.insert(class_name.to_string(), func_id);
        self.func_return_types.insert(
            class_name.to_string(),
            Some(BolideType::Custom(class_name.to_string())),
        );

        // 存储构造器参数信息（用于缺参填充）
        let params: Vec<Param> = class_info
            .fields
            .iter()
            .map(|f| Param {
                name: f.name.clone(),
                ty: f.ty.clone(),
                mode: ParamMode::Borrow,
                default_value: f.default_value.clone(),
                is_variadic: false,
                is_kw_variadic: false,
            })
            .collect();
        self.func_params.insert(class_name.to_string(), params);

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
                        sig.params
                            .push(AbiParam::new(self.bolide_type_to_cranelift(&param.ty)));
                    }
                    if let Some(ref ret_ty) = method.return_type {
                        sig.returns
                            .push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
                    }

                    let link_name = Self::user_link_name(&method_name);
                    let func_id = self
                        .module
                        .declare_function(&link_name, Linkage::Export, &sig)
                        .map_err(|e| format!("Declare method error: {}", e))?;

                    self.functions.insert(method_name.clone(), func_id);
                    self.func_return_types
                        .insert(method_name.clone(), method.return_type.clone());

                    let mut params_with_self = vec![Param {
                        name: "self".to_string(),
                        ty: BolideType::Custom(class.name.clone()),
                        mode: ParamMode::Borrow,
                        default_value: None,
                        is_variadic: false,
                        is_kw_variadic: false,
                    }];
                    params_with_self.extend(method.params.clone());
                    self.func_params.insert(method_name, params_with_self);
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
                for (cond, body) in &i.elif_branches {
                    self.collect_spawn_in_expr(cond, targets);
                    self.collect_spawn_in_stmts(body, targets);
                }
                if let Some(ref else_body) = i.else_body {
                    self.collect_spawn_in_stmts(else_body, targets);
                }
            }
            Statement::While(w) => {
                self.collect_spawn_in_expr(&w.condition, targets);
                self.collect_spawn_in_stmts(&w.body, targets);
            }
            Statement::For(f) => {
                self.collect_spawn_in_expr(&f.iter, targets);
                self.collect_spawn_in_stmts(&f.body, targets);
            }
            Statement::Pool(p) => {
                self.collect_spawn_in_expr(&p.size, targets);
                self.collect_spawn_in_stmts(&p.body, targets);
            }
            Statement::FuncDef(f) => {
                self.collect_spawn_in_stmts(&f.body, targets);
            }
            Statement::Try(t) => {
                self.collect_spawn_in_stmts(&t.try_body, targets);
                for clause in &t.catch_clauses {
                    self.collect_spawn_in_stmts(&clause.body, targets);
                }
                if let Some(ref finally_body) = t.finally {
                    self.collect_spawn_in_stmts(finally_body, targets);
                }
            }
            Statement::Select(s) => {
                for branch in &s.branches {
                    if let bolide_parser::SelectBranch::Recv { body, .. } = branch {
                        self.collect_spawn_in_stmts(body, targets);
                    }
                }
            }
            Statement::SpawnSelect(s) => {
                for branch in &s.branches {
                    let (expr, body) = match branch {
                        bolide_parser::SpawnSelectBranch::Bind { expr, body, .. } => (expr, body),
                        bolide_parser::SpawnSelectBranch::Expr { expr, body } => (expr, body),
                    };
                    if let Ok((func_name, _)) = self.spawn_call_parts(expr) {
                        if self
                            .func_params
                            .get(func_name)
                            .map(|p| !p.is_empty())
                            .unwrap_or(false)
                        {
                            targets.insert(func_name.to_string());
                        }
                    }
                    self.collect_spawn_in_expr(expr, targets);
                    self.collect_spawn_in_stmts(body, targets);
                }
            }
            Statement::Send(s) => self.collect_spawn_in_expr(&s.value, targets),
            Statement::Throw(e) => self.collect_spawn_in_expr(e, targets),
            Statement::AwaitScope(a) => {
                self.collect_spawn_in_stmts(&a.body, targets);
            }
            Statement::Return(Some(e)) => self.collect_spawn_in_expr(e, targets),
            _ => {}
        }
    }

    fn collect_spawn_in_expr(&self, expr: &Expr, targets: &mut HashSet<String>) {
        match expr {
            Expr::Spawn(name, args) => {
                if self
                    .func_params
                    .get(name)
                    .map(|p| !p.is_empty())
                    .unwrap_or(false)
                {
                    targets.insert(name.clone());
                }
                for arg in args {
                    self.collect_spawn_in_expr(arg, targets);
                }
            }
            Expr::SpawnAll(exprs) => {
                for expr in exprs {
                    if let Ok((func_name, _)) = self.spawn_call_parts(expr) {
                        if self
                            .func_params
                            .get(func_name)
                            .map(|p| !p.is_empty())
                            .unwrap_or(false)
                        {
                            targets.insert(func_name.to_string());
                        }
                    }
                    self.collect_spawn_in_expr(expr, targets);
                }
            }
            Expr::NamedArg(_, value) | Expr::SpreadArg(value) | Expr::KwSpreadArg(value) => {
                self.collect_spawn_in_expr(value, targets);
            }
            Expr::BinOp(l, _, r) => {
                self.collect_spawn_in_expr(l, targets);
                self.collect_spawn_in_expr(r, targets);
            }
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    if self.async_funcs.contains(name.as_str())
                        && self
                            .func_params
                            .get(name)
                            .map(|p| !p.is_empty())
                            .unwrap_or(false)
                    {
                        targets.insert(name.clone());
                    }
                }
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
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        let trampoline_id = self
            .module
            .declare_function(&trampoline_name, Linkage::Export, &sig)
            .map_err(|e| format!("{}", e))?;

        // 获取目标函数 ID
        let target_func_id = *self
            .functions
            .get(func_name)
            .ok_or_else(|| format!("Target function {} not declared", func_name))?;

        // 预计算参数类型
        let cranelift_types: Vec<types::Type> = params
            .iter()
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
        let target_ref = self
            .module
            .declare_func_in_func(target_func_id, builder.func);

        // 从 env 加载参数
        let mut call_args = Vec::new();
        for (i, ty) in cranelift_types.iter().enumerate() {
            let offset = (i * 8) as i32;
            let val = builder
                .ins()
                .load(*ty, MemFlags::trusted(), env_ptr, offset);
            call_args.push(val);
        }

        // 调用目标函数
        let call = builder.ins().call(target_ref, &call_args);
        let result_val = {
            let results = builder.inst_results(call);
            if results.is_empty() {
                None
            } else {
                Some(results[0])
            }
        };

        // 释放 RC 类型参数（spawn/async 时 clone 的副本）
        for (i, param) in params.iter().enumerate() {
            let release_name = match &param.ty {
                BolideType::Str => Some("@_string_release"),
                BolideType::BigInt => Some("@_bigint_release"),
                BolideType::Decimal => Some("@_decimal_release"),
                BolideType::List(_) => Some("@_list_release"),
                BolideType::Dynamic => Some("@_dynamic_release"),
                _ => None,
            };
            if let Some(rel_name) = release_name {
                if let Some(&rel_id) = self.functions.get(rel_name) {
                    let rel_ref = self.module.declare_func_in_func(rel_id, builder.func);
                    builder.ins().call(rel_ref, &[call_args[i]]);
                }
            }
        }

        if let Some(val) = result_val {
            builder.ins().return_(&[val]);
        } else {
            builder.ins().return_(&[]);
        }

        builder.finalize();

        self.module
            .define_function(trampoline_id, &mut self.ctx)
            .map_err(|e| format!("Define trampoline error: {}", e))?;
        self.module.clear_context(&mut self.ctx);

        self.trampolines.insert(
            func_name.to_string(),
            TrampolineInfo {
                func_id: trampoline_id,
                param_types,
                env_size,
            },
        );
        self.functions.insert(trampoline_name, trampoline_id);

        Ok(())
    }

    /// 编译类构造函数
    fn compile_class_constructor(&mut self, class_name: &str) -> Result<(), String> {
        let class_info = self
            .classes
            .get(class_name)
            .ok_or_else(|| format!("Class {} not found", class_name))?
            .clone();

        let func_id = *self
            .functions
            .get(class_name)
            .ok_or_else(|| format!("Constructor {} not declared", class_name))?;

        let mut sig = self.module.make_signature();
        for field in &class_info.fields {
            sig.params
                .push(AbiParam::new(self.bolide_type_to_cranelift(&field.ty)));
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
        let alloc_id = *self
            .functions
            .get("@_object_alloc")
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

        self.module
            .define_function(func_id, &mut self.ctx)
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

    fn add_dynamic_extern_c_strings(
        &mut self,
        string_data_ids: &mut HashMap<String, DataId>,
    ) -> Result<(), String> {
        let mut cstrings = Vec::new();
        for (lib_path, func) in self.extern_funcs.values() {
            if is_dynamic_lib_spec(lib_path) {
                cstrings.push(format!("{}\0", resolve_dynamic_lib_spec(lib_path)?));
                cstrings.push(format!("{}\0", func.name));
            }
        }

        for s in cstrings {
            if !string_data_ids.contains_key(&s) {
                let data_id = self.get_or_create_string_data(&s)?;
                string_data_ids.insert(s, data_id);
            }
        }

        Ok(())
    }

    fn compile_class_method(&mut self, class_name: &str, method: &FuncDef) -> Result<(), String> {
        let method_name = format!("{}_{}", class_name, method.name);
        let func_id = *self
            .functions
            .get(&method_name)
            .ok_or_else(|| format!("Method {} not declared", method_name))?;

        // Collect string literals and create data objects
        let strings = self.collect_strings_from_stmts(&method.body);
        let mut string_data_ids: HashMap<String, DataId> = HashMap::new();
        for s in &strings {
            let data_id = self.get_or_create_string_data(s)?;
            string_data_ids.insert(s.clone(), data_id);
        }
        for s in self.collect_class_default_strings_from_program(&self.program_snapshot) {
            if !string_data_ids.contains_key(&s) {
                let data_id = self.get_or_create_string_data(&s)?;
                string_data_ids.insert(s.clone(), data_id);
            }
        }
        for s in self.collect_param_default_strings_from_program(&self.program_snapshot) {
            if !string_data_ids.contains_key(&s) {
                let data_id = self.get_or_create_string_data(&s)?;
                string_data_ids.insert(s.clone(), data_id);
            }
        }
        self.add_dynamic_extern_c_strings(&mut string_data_ids)?;

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(self.ptr_type)); // self
        for param in &method.params {
            let param_ty = self.normalize_bolide_type(&param.ty);
            sig.params
                .push(AbiParam::new(self.bolide_type_to_cranelift(&param_ty)));
        }
        if let Some(ref ret_ty) = method.return_type {
            let ret_ty = self.normalize_bolide_type(ret_ty);
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(&ret_ty)));
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

        // 在函数中预声明全局变量的 GlobalValue
        let mut global_refs = HashMap::new();
        for (name, data_id) in &self.global_data_ids {
            let gv = self.module.declare_data_in_func(*data_id, builder.func);
            global_refs.insert(name.clone(), gv);
        }

        // 使用作用域来确保 ctx 在 finalize 之前被释放
        {
            let mut ctx = AotCompileContext::new(
                &mut builder,
                &mut self.module,
                func_refs,
                self.ptr_type,
                self.classes.clone(),
                self.adts.clone(),
                self.class_tags.clone(),
                self.async_funcs.clone(),
                self.func_return_types.clone(),
                string_globals,
                self.modules.clone(),
                self.func_params.clone(),
                self.extern_funcs.clone(),
                global_refs,
                self.global_var_types.clone(),
                method_name.to_string(),
                method.lifetime_deps.clone(),
                self.lifetime_funcs.clone(),
            );
            let params: Vec<_> = ctx.builder.block_params(entry).to_vec();
            let self_var = ctx.declare_variable("self", self.ptr_type);
            ctx.builder.def_var(self_var, params[0]);
            ctx.var_types.insert(
                "self".to_string(),
                BolideType::Custom(class_name.to_string()),
            );

            // 绑定 super：与 self 共享同一对象指针，但类型记为父类。
            // 静态派发 + 父类字段在前的布局兼容，使父类方法/字段解析正确。
            if let Some(parent) = self.classes.get(class_name).and_then(|c| c.parent.clone()) {
                ctx.variables.insert("super".to_string(), self_var);
                ctx.var_types
                    .insert("super".to_string(), BolideType::Custom(parent));
            }

            // 设置其他参数变量
            for (i, param) in method.params.iter().enumerate() {
                let param_ty = ctx.normalize_bolide_type(&param.ty);
                let ty = ctx.bolide_type_to_cranelift(&param_ty);
                let var = ctx.declare_variable(&param.name, ty);
                ctx.builder.def_var(var, params[i + 1]); // +1 因为 self 是第一个参数
                ctx.var_types.insert(param.name.clone(), param_ty.clone());
                // 仅 owned 参数由被调方负责释放；borrow 参数只借用不释放（与 JIT 一致）
                if param.mode == ParamMode::Owned {
                    ctx.track_rc_variable(&param.name, &param_ty);
                } else {
                    ctx.caller_owned_params.insert(param.name.clone());
                }
            }

            // 编译方法体
            let mut returned = false;
            for stmt in &method.body {
                if ctx.compile_stmt(stmt)? {
                    returned = true;
                    break;
                }
            }

            // 如果没有显式返回，添加默认返回（先释放局部 RC 变量）
            if !returned {
                ctx.emit_rc_cleanup();
                if method.return_type.is_some() {
                    let zero = ctx.builder.ins().iconst(types::I64, 0);
                    ctx.builder.ins().return_(&[zero]);
                } else {
                    ctx.builder.ins().return_(&[]);
                }
            }
        }

        builder.finalize();
        self.module
            .define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Define method error: {}", e))?;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }

    /// 编译函数
    fn compile_function(&mut self, func: &FuncDef) -> Result<(), String> {
        let mut pending_closures: Vec<ClosureJob> = Vec::new();
        self.compile_function_internal(func, &mut pending_closures)?;
        // 编译所有待处理的 lifted 闭包函数（可能产生嵌套闭包，循环直到清空）
        while let Some(job) = pending_closures.pop() {
            self.compile_closure_job(&job, &mut pending_closures)?;
        }
        Ok(())
    }

    /// 编译函数本体（不处理闭包工作队列）
    fn compile_function_internal(
        &mut self,
        func: &FuncDef,
        pending_closures: &mut Vec<ClosureJob>,
    ) -> Result<(), String> {
        let func_id = *self
            .functions
            .get(&func.name)
            .ok_or_else(|| format!("Function {} not declared", func.name))?;

        // Collect string literals and create data objects
        let strings = self.collect_strings_from_stmts(&func.body);
        let mut string_data_ids: HashMap<String, DataId> = HashMap::new();
        for s in &strings {
            let data_id = self.get_or_create_string_data(s)?;
            string_data_ids.insert(s.clone(), data_id);
        }

        // 类字段默认值中的字符串字面量也需要注册（入口函数编译时可能未扫描到）
        for s in self.collect_class_default_strings_from_program(&self.program_snapshot) {
            if !string_data_ids.contains_key(&s) {
                let data_id = self.get_or_create_string_data(&s)?;
                string_data_ids.insert(s.clone(), data_id);
            }
        }
        for s in self.collect_param_default_strings_from_program(&self.program_snapshot) {
            if !string_data_ids.contains_key(&s) {
                let data_id = self.get_or_create_string_data(&s)?;
                string_data_ids.insert(s.clone(), data_id);
            }
        }
        self.add_dynamic_extern_c_strings(&mut string_data_ids)?;

        let mut sig = self.module.make_signature();
        for param in &func.params {
            let param_ty = self.normalize_bolide_type(&param.ty);
            sig.params
                .push(AbiParam::new(self.bolide_type_to_cranelift(&param_ty)));
        }
        if let Some(ref ret_ty) = func.return_type {
            let ret_ty = self.normalize_bolide_type(ret_ty);
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(&ret_ty)));
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

        // 在函数中预声明全局变量的 GlobalValue
        let mut global_refs = HashMap::new();
        for (name, data_id) in &self.global_data_ids {
            let gv = self.module.declare_data_in_func(*data_id, builder.func);
            global_refs.insert(name.clone(), gv);
        }

        // 使用作用域来确保 ctx 在 finalize 之前被释放
        {
            let mut ctx = AotCompileContext::new(
                &mut builder,
                &mut self.module,
                func_refs,
                self.ptr_type,
                self.classes.clone(),
                self.adts.clone(),
                self.class_tags.clone(),
                self.async_funcs.clone(),
                self.func_return_types.clone(),
                string_globals,
                self.modules.clone(),
                self.func_params.clone(),
                self.extern_funcs.clone(),
                global_refs,
                self.global_var_types.clone(),
                func.name.clone(),
                func.lifetime_deps.clone(),
                self.lifetime_funcs.clone(),
            );

            // 设置参数变量
            let params: Vec<_> = ctx.builder.block_params(entry).to_vec();
            for (i, param) in func.params.iter().enumerate() {
                let param_ty = ctx.normalize_bolide_type(&param.ty);
                let ty = ctx.bolide_type_to_cranelift(&param_ty);
                let var = ctx.declare_variable(&param.name, ty);
                ctx.var_types.insert(param.name.clone(), param_ty.clone());
                if matches!(param_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                    ctx.closure_param_vars.insert(param.name.clone());
                }
                match param.mode {
                    ParamMode::Ref => {
                        // ref 参数：传入的是指针地址，解引用取真实值；返回前写回
                        let ptr_addr = params[i];
                        let val = ctx.builder.ins().load(ty, MemFlags::new(), ptr_addr, 0);
                        ctx.builder.def_var(var, val);
                        ctx.ref_params.push((param.name.clone(), var, ptr_addr));
                        ctx.caller_owned_params.insert(param.name.clone());
                    }
                    ParamMode::Owned => {
                        ctx.builder.def_var(var, params[i]);
                        ctx.track_rc_variable(&param.name, &param_ty);
                    }
                    ParamMode::Borrow => {
                        // 借用：直接使用，不负责释放
                        ctx.builder.def_var(var, params[i]);
                        ctx.caller_owned_params.insert(param.name.clone());
                    }
                }
            }

            // 编译函数体
            let mut returned = false;
            for stmt in &func.body {
                if ctx.compile_stmt(stmt)? {
                    returned = true;
                    break;
                }
            }

            // 如果没有显式返回，添加默认返回（先写回 ref 参数，再释放局部 RC）
            if !returned {
                ctx.write_back_ref_params();
                ctx.emit_rc_cleanup();
                if func.return_type.is_some() {
                    let zero = ctx.builder.ins().iconst(types::I64, 0);
                    ctx.builder.ins().return_(&[zero]);
                } else {
                    ctx.builder.ins().return_(&[]);
                }
            }

            // 收集本函数内创建的待编译闭包
            let pending = std::mem::take(&mut ctx.pending_closures);
            pending_closures.extend(pending);
        } // ctx 在这里被释放

        builder.finalize();
        // println!("Compiling Aot function: {}", func.name);
        if let Err(e) = self.ctx.verify_if(&*self.module.isa()) {
            println!("Verify Error for {}: {:?}", func.name, e);
            println!("{}", self.ctx.func.display());
        }

        self.module
            .define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Define function error in {}: {}", func.name, e))?;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }

    /// 编译一个 lifted 闭包函数：签名 (env_ptr, ...params) -> ret，
    /// 入口处从 env 恢复捕获变量为局部（借用，不参与 RC 清理）。
    fn compile_closure_job(
        &mut self,
        job: &ClosureJob,
        pending_closures: &mut Vec<ClosureJob>,
    ) -> Result<(), String> {
        // 收集闭包体中的字符串字面量并注册到 data segment
        let strings = self.collect_strings_from_stmts(&job.body);
        let mut string_data_ids: HashMap<String, DataId> = HashMap::new();
        for s in &strings {
            let data_id = self.get_or_create_string_data(s)?;
            string_data_ids.insert(s.clone(), data_id);
        }
        self.add_dynamic_extern_c_strings(&mut string_data_ids)?;

        let param_types: Vec<types::Type> = job
            .params
            .iter()
            .map(|p| self.bolide_type_to_cranelift(&self.normalize_bolide_type(&p.ty)))
            .collect();

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(self.ptr_type)); // env
        for ty in &param_types {
            sig.params.push(AbiParam::new(*ty));
        }
        if let Some(ref ret_ty) = job.return_type {
            let ret_ty = self.normalize_bolide_type(ret_ty);
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(&ret_ty)));
        }

        self.ctx.func.signature = sig;
        let mut fbc = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut fbc);

        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        let mut func_refs = HashMap::new();
        for (name, &fid) in &self.functions {
            let fref = self.module.declare_func_in_func(fid, builder.func);
            func_refs.insert(name.clone(), fref);
        }

        let mut string_globals = HashMap::new();
        for (s, data_id) in &self.string_data {
            let gv = self.module.declare_data_in_func(*data_id, builder.func);
            string_globals.insert(s.clone(), (gv, s.len()));
        }

        let mut global_refs = HashMap::new();
        for (name, data_id) in &self.global_data_ids {
            let gv = self.module.declare_data_in_func(*data_id, builder.func);
            global_refs.insert(name.clone(), gv);
        }

        {
            let mut ctx = AotCompileContext::new(
                &mut builder,
                &mut self.module,
                func_refs,
                self.ptr_type,
                self.classes.clone(),
                self.adts.clone(),
                self.class_tags.clone(),
                self.async_funcs.clone(),
                self.func_return_types.clone(),
                string_globals,
                self.modules.clone(),
                self.func_params.clone(),
                self.extern_funcs.clone(),
                global_refs,
                self.global_var_types.clone(),
                job.name.clone(),
                None,
                self.lifetime_funcs.clone(),
            );

            let block_params = ctx.builder.block_params(entry).to_vec();
            let env_ptr = block_params[0];

            for (i, (name, ty)) in job.captures.iter().enumerate() {
                let cty = ctx.bolide_type_to_cranelift(ty);
                let offset = (i * 8) as i32;
                let raw = ctx
                    .builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), env_ptr, offset);
                let val = if matches!(ty, BolideType::Float) {
                    ctx.builder.ins().bitcast(types::F64, MemFlags::new(), raw)
                } else {
                    raw
                };
                let var = ctx.declare_variable(name, cty);
                ctx.builder.def_var(var, val);
                ctx.var_types.insert(name.clone(), ty.clone());
            }

            for (i, param) in job.params.iter().enumerate() {
                let param_ty = ctx.normalize_bolide_type(&param.ty);
                ctx.var_types.insert(param.name.clone(), param_ty.clone());
                if matches!(param_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                    ctx.closure_param_vars.insert(param.name.clone());
                }
                let var = ctx.declare_variable(&param.name, param_types[i]);
                ctx.builder.def_var(var, block_params[i + 1]);
                ctx.caller_owned_params.insert(param.name.clone());
            }

            let mut returned = false;
            for stmt in &job.body {
                if ctx.compile_stmt(stmt)? {
                    returned = true;
                    break;
                }
            }

            if !returned {
                ctx.write_back_ref_params();
                ctx.emit_rc_cleanup();
                if job.return_type.is_some() {
                    let zero = ctx.builder.ins().iconst(types::I64, 0);
                    ctx.builder.ins().return_(&[zero]);
                } else {
                    ctx.builder.ins().return_(&[]);
                }
            }

            let pending = std::mem::take(&mut ctx.pending_closures);
            pending_closures.extend(pending);
        }

        builder.finalize();
        if let Err(e) = self.ctx.verify_if(&*self.module.isa()) {
            println!("Verify Error for closure {}: {:?}", job.name, e);
            println!("{}", self.ctx.func.display());
        }

        self.module
            .define_function(job.func_id, &mut self.ctx)
            .map_err(|e| format!("Define closure error in {}: {}", job.name, e))?;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }
}

/// AOT 编译上下文
struct AotCompileContext<'a, 'b> {
    builder: &'a mut FunctionBuilder<'b>,
    module: &'a mut ObjectModule,
    func_refs: HashMap<String, FuncRef>,
    variables: HashMap<String, Variable>,
    var_types: HashMap<String, BolideType>,
    var_counter: usize,
    ptr_type: types::Type,
    classes: HashMap<String, ClassInfo>,
    adts: HashMap<String, AdtInfo>,
    class_tags: HashMap<String, i64>,
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
    /// 循环块栈：(continue 目标块, break 目标块, 循环作用域基索引)
    loop_stack: Vec<(Block, Block, usize)>,
    /// catch 落点栈：每个 try 块的 catch_block，用于编译 throw（同函数内直接跳转）
    catch_stack: Vec<Block>,
    /// weak/unowned 引用变量集合（访问时需要检查对象是否存活）
    weak_variables: HashSet<String>,
    /// spawn/async 句柄变量 -> 目标函数名（join 时据此推断返回类型后缀）
    spawn_func_map: HashMap<String, String>,
    /// 当前正在编译的函数名（只有 __main__ 中才写入全局变量）
    current_func: String,
    /// 已通过 owned 实参移动出去的变量（不再在作用域结束时释放）
    moved_variables: HashSet<String>,
    /// 函数参数信息（含 ParamMode），用于调用点按 borrow/owned/ref 处理实参
    func_params: HashMap<String, Vec<Param>>,
    /// ref 参数：(参数名, 变量, 调用方传入的指针地址)，返回前写回
    ref_params: Vec<(String, Variable, Value)>,
    /// ref 参数是否已被重新赋值（首次不释放旧值）
    ref_params_reassigned: HashSet<String>,
    /// extern 函数信息: 函数名 -> (库路径, 函数声明)
    extern_funcs: HashMap<String, (String, bolide_parser::ExternFunc)>,
    /// 当前函数中归调用方所有的参数名（borrow/ref 模式），返回它们时需 clone
    caller_owned_params: HashSet<String>,
    /// 全局变量 GlobalValue 映射（已在函数中预声明）
    global_refs: HashMap<String, cranelift_codegen::ir::GlobalValue>,
    /// 全局变量类型映射
    global_var_types: HashMap<String, BolideType>,
    /// 当前函数的生命周期依赖（from x, y）；None 表示非生命周期函数
    lifetime_deps: Option<Vec<String>>,
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
    /// 本函数内创建的待编译 lifted 闭包函数
    pending_closures: Vec<ClosureJob>,
    /// 闭包局部计数
    closure_local_counter: usize,
    /// 当前语句产生的未吸收闭包临时值
    closure_temps: Vec<Value>,
    /// 持有闭包对象的局部变量名
    closure_vars: HashSet<String>,
    /// 函数类型参数名（按借用处理，作用域结束不释放）
    closure_param_vars: HashSet<String>,
}

impl<'a, 'b> AotCompileContext<'a, 'b> {
    fn new(
        builder: &'a mut FunctionBuilder<'b>,
        module: &'a mut ObjectModule,
        func_refs: HashMap<String, FuncRef>,
        ptr_type: types::Type,
        classes: HashMap<String, ClassInfo>,
        adts: HashMap<String, AdtInfo>,
        class_tags: HashMap<String, i64>,
        async_funcs: HashSet<String>,
        func_return_types: HashMap<String, Option<BolideType>>,
        string_globals: HashMap<String, (cranelift_codegen::ir::GlobalValue, usize)>,
        modules: HashMap<String, String>,
        func_params: HashMap<String, Vec<Param>>,
        extern_funcs: HashMap<String, (String, bolide_parser::ExternFunc)>,
        global_refs: HashMap<String, cranelift_codegen::ir::GlobalValue>,
        global_var_types: HashMap<String, BolideType>,
        current_func: String,
        lifetime_deps: Option<Vec<String>>,
        lifetime_funcs: HashSet<String>,
    ) -> Self {
        Self {
            builder,
            module,
            func_refs,
            variables: HashMap::new(),
            var_types: HashMap::new(),
            var_counter: 0,
            ptr_type,
            classes,
            adts,
            class_tags,
            async_funcs,
            func_return_types,
            string_globals,
            modules,
            rc_variables: Vec::new(),
            temp_rc_values: Vec::new(),
            loop_stack: Vec::new(),
            catch_stack: Vec::new(),
            weak_variables: HashSet::new(),
            spawn_func_map: HashMap::new(),
            current_func,
            moved_variables: HashSet::new(),
            func_params,
            ref_params: Vec::new(),
            ref_params_reassigned: HashSet::new(),
            extern_funcs,
            caller_owned_params: HashSet::new(),
            global_refs,
            global_var_types,
            lifetime_deps,
            lifetime_funcs,
            var_lifetime_source: HashMap::new(),
            scope_depth: 0,
            var_scope_depth: HashMap::new(),
            borrowed_vars: HashMap::new(),
            pending_closures: Vec::new(),
            closure_local_counter: 0,
            closure_temps: Vec::new(),
            closure_vars: HashSet::new(),
            closure_param_vars: HashSet::new(),
        }
    }

    fn normalize_type_name(&self, name: &str) -> String {
        if name.contains('.') {
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() == 2 {
                let module = parts[0];
                let type_name = parts[1];
                if self.modules.contains_key(module) {
                    return format!("@{}_{}", module, type_name);
                }
            }
        }
        name.to_string()
    }

    fn normalize_bolide_type(&self, ty: &BolideType) -> BolideType {
        match ty {
            BolideType::Custom(name) => {
                let normalized = self.normalize_type_name(name);
                if self.adts.contains_key(&normalized) {
                    BolideType::Adt(normalized, vec![])
                } else {
                    BolideType::Custom(normalized)
                }
            }
            BolideType::Adt(name, args) => BolideType::Adt(
                self.normalize_type_name(name),
                args.iter().map(|t| self.normalize_bolide_type(t)).collect(),
            ),
            BolideType::List(inner) => {
                BolideType::List(Box::new(self.normalize_bolide_type(inner)))
            }
            BolideType::Dict(k, v) => BolideType::Dict(
                Box::new(self.normalize_bolide_type(k)),
                Box::new(self.normalize_bolide_type(v)),
            ),
            BolideType::Tuple(types) => BolideType::Tuple(
                types
                    .iter()
                    .map(|t| self.normalize_bolide_type(t))
                    .collect(),
            ),
            BolideType::FuncSig(params, ret) => BolideType::FuncSig(
                params
                    .iter()
                    .map(|t| self.normalize_bolide_type(t))
                    .collect(),
                ret.as_ref()
                    .map(|t| Box::new(self.normalize_bolide_type(t))),
            ),
            BolideType::Weak(inner) => {
                BolideType::Weak(Box::new(self.normalize_bolide_type(inner)))
            }
            BolideType::Unowned(inner) => {
                BolideType::Unowned(Box::new(self.normalize_bolide_type(inner)))
            }
            BolideType::Channel(inner) => {
                BolideType::Channel(Box::new(self.normalize_bolide_type(inner)))
            }
            _ => ty.clone(),
        }
    }

    /// 将所有 ref 参数的当前值写回调用方传入的指针地址
    fn write_back_ref_params(&mut self) {
        for (_, var, ptr_addr) in self.ref_params.clone() {
            let current = self.builder.use_var(var);
            self.builder
                .ins()
                .store(MemFlags::new(), current, ptr_addr, 0);
        }
    }

    fn enter_scope(&mut self) -> usize {
        self.scope_depth += 1;
        self.rc_variables.len()
    }

    fn leave_scope(&mut self, start_index: usize) -> Result<(), String> {
        // 生命周期检查：当前作用域离开时，检查是否有外层借用变量
        // 依赖于本作用域内声明的变量（悬空借用）。
        let current_depth = self.scope_depth;
        let vars_in_scope: Vec<String> = self
            .var_scope_depth
            .iter()
            .filter(|(_, &depth)| depth == current_depth)
            .map(|(name, _)| name.clone())
            .collect();
        for (borrower, (source, _)) in &self.borrowed_vars {
            let borrower_depth = self.var_scope_depth.get(borrower).copied().unwrap_or(0);
            if borrower_depth < current_depth && vars_in_scope.contains(source) {
                return Err(format!(
                    "Lifetime error: '{}' borrows from '{}' which goes out of scope",
                    borrower, source
                ));
            }
        }
        for var in &vars_in_scope {
            self.var_scope_depth.remove(var);
            self.borrowed_vars.remove(var);
        }
        if self.scope_depth > 0 {
            self.scope_depth -= 1;
        }

        // Release vars declared in this scope (stack-like)
        for i in (start_index..self.rc_variables.len()).rev() {
            let (var, ty) = self.rc_variables[i].clone();
            let val = self.builder.use_var(var);
            self.emit_release(val, &ty);
        }
        // Truncate
        self.rc_variables.truncate(start_index);
        Ok(())
    }

    /// 在不截断 rc_variables 的情况下释放从 start_index 起的作用域变量。
    /// 用于 break/continue 的提前跳出路径；正常路径仍由 leave_scope 处理。
    fn emit_scope_releases_from(&mut self, start_index: usize) {
        for i in (start_index..self.rc_variables.len()).rev() {
            let (var, ty) = self.rc_variables[i].clone();
            let val = self.builder.use_var(var);
            self.emit_release(val, &ty);
        }
    }

    // ==================== 生命周期 / 借用检查（与 JIT 对齐） ====================

    /// 当前函数是否使用生命周期模式（声明了 from 依赖，跳过 ARC）
    fn uses_lifetime_mode(&self) -> bool {
        self.lifetime_deps.is_some()
    }

    /// 被调函数是否为生命周期函数（返回借用而非拥有的值）
    fn is_lifetime_func(&self, func_name: &str) -> bool {
        self.lifetime_funcs.contains(func_name)
    }

    /// 表达式是否是对生命周期函数的调用
    fn is_lifetime_func_call(&self, expr: &Expr) -> bool {
        if let Expr::Call(callee, _) = expr {
            if let Expr::Ident(func_name) = callee.as_ref() {
                return self.is_lifetime_func(func_name);
            }
        }
        false
    }

    /// 检查表达式的生命周期来源（直接是依赖参数，或从依赖参数派生的变量）
    fn check_lifetime_source(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(name) => {
                if let Some(ref deps) = self.lifetime_deps {
                    if deps.contains(name) {
                        return Some(name.clone());
                    }
                }
                if let Some(source) = self.var_lifetime_source.get(name) {
                    return Some(source.clone());
                }
                None
            }
            Expr::Member(base, _) => self.check_lifetime_source(base),
            Expr::Index(base, _) => self.check_lifetime_source(base),
            _ => None,
        }
    }

    /// 验证返回值的生命周期依赖：声明 from x 时返回值必须来自 x
    fn validate_lifetime_return(&self, expr: &Expr) -> Result<(), String> {
        if let Some(ref deps) = self.lifetime_deps {
            if let Some(source) = self.check_lifetime_source(expr) {
                if deps.contains(&source) {
                    return Ok(());
                }
            }
            return Err(format!(
                "Lifetime error in function '{}': return value must derive from parameter(s) {:?}, \
                 but the expression does not reference any of them",
                self.current_func, deps
            ));
        }
        Ok(())
    }

    /// 记录变量声明时的作用域深度
    fn record_var_scope(&mut self, var_name: &str) {
        self.var_scope_depth
            .insert(var_name.to_string(), self.scope_depth);
    }

    /// 记录借用关系：borrower 借用了 source
    fn record_borrow(&mut self, borrower: &str, source: &str) {
        let source_depth = self.var_scope_depth.get(source).copied().unwrap_or(0);
        self.borrowed_vars
            .insert(borrower.to_string(), (source.to_string(), source_depth));
    }

    /// 借用逃逸检查：借用值不拥有对象，禁止存入容器/字段/通道或跨线程逃逸
    fn check_borrow_escape(&self, expr: &Expr, context: &str) -> Result<(), String> {
        if let Expr::Ident(name) = expr {
            if let Some((src, _)) = self.borrowed_vars.get(name) {
                return Err(format!(
                    "Lifetime error: '{}' borrows from '{}' and cannot be stored via {} \
                     (a borrowed value does not own the object)",
                    name, src, context
                ));
            }
        }
        Ok(())
    }

    /// 借用来源检查：借用存活期间禁止对来源变量重新赋值
    fn check_borrow_source_assign(&self, var_name: &str) -> Result<(), String> {
        if let Some((borrower, _)) = self
            .borrowed_vars
            .iter()
            .find(|(_, (src, _))| src == var_name)
        {
            return Err(format!(
                "Lifetime error: cannot assign to '{}' while it is borrowed by '{}'",
                var_name, borrower
            ));
        }
        Ok(())
    }

    /// 获取生命周期函数调用的源变量（第一个 ref 参数对应的实参）
    fn get_lifetime_call_source(&self, expr: &Expr) -> Option<String> {
        if let Expr::Call(callee, args) = expr {
            if let Expr::Ident(func_name) = callee.as_ref() {
                if self.is_lifetime_func(func_name) {
                    if let Some(params) = self.func_params.get(func_name) {
                        for (i, param) in params.iter().enumerate() {
                            if param.mode == ParamMode::Ref {
                                if let Some(Expr::Ident(var_name)) = args.get(i) {
                                    return Some(var_name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        None
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
            BolideType::Bytes => self.ptr_type,
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
            BolideType::Generic(_) => self.ptr_type,
            BolideType::Adt(_, _) => self.ptr_type,
            BolideType::Custom(_) => self.ptr_type,
            BolideType::Weak(_) => self.ptr_type,
            BolideType::Unowned(_) => self.ptr_type,
        }
    }

    /// 基本异常类型标签（与 JIT 一致）。自定义类标签由 class_tags 提供（>=100）。
    fn basic_throw_tag(ty: &BolideType) -> i64 {
        match ty {
            BolideType::Int => 1,
            BolideType::Bool => 2,
            BolideType::Float => 3,
            BolideType::Str => 4,
            BolideType::BigInt => 5,
            BolideType::Decimal => 6,
            _ => 0,
        }
    }

    /// 计算抛出表达式静态类型对应的异常标签。
    fn type_to_throw_tag(&self, ty: &BolideType) -> i64 {
        match ty {
            BolideType::Custom(name) => self.class_tags.get(name).copied().unwrap_or(0),
            other => Self::basic_throw_tag(other),
        }
    }

    /// 计算 catch (e: T) 应匹配的标签集合（T 自身 + 所有以 T 为祖先的子类）。
    fn catch_match_tags(&self, catch_ty: &BolideType) -> Vec<i64> {
        match catch_ty {
            BolideType::Custom(target) => {
                let mut tags = Vec::new();
                for (cls_name, &tag) in &self.class_tags {
                    let mut cur = cls_name.clone();
                    loop {
                        if &cur == target {
                            tags.push(tag);
                            break;
                        }
                        match self.classes.get(&cur).and_then(|c| c.parent.clone()) {
                            Some(parent) => cur = parent,
                            None => break,
                        }
                    }
                }
                tags
            }
            other => vec![Self::basic_throw_tag(other)],
        }
    }

    /// 检查类型是否需要 RC 管理
    fn is_rc_type(ty: &BolideType) -> bool {
        match ty {
            // weak/unowned 类引用需要弱引用计数管理（保住对象头以便存活检查）
            BolideType::Weak(inner) | BolideType::Unowned(inner) => {
                matches!(inner.as_ref(), BolideType::Custom(_))
            }
            _ => matches!(
                ty,
                BolideType::Str
                    | BolideType::Bytes
                    | BolideType::BigInt
                    | BolideType::Decimal
                    | BolideType::List(_)
                    | BolideType::Dict(_, _)
                    | BolideType::Dynamic
                    | BolideType::Adt(_, _)
                    | BolideType::Custom(_)
                    | BolideType::Tuple(_)
            ),
        }
    }

    /// 检查类型是否是指向类实例的 weak/unowned 引用
    fn is_weak_ref_type(ty: &BolideType) -> bool {
        matches!(ty,
            BolideType::Weak(inner) | BolideType::Unowned(inner)
                if matches!(inner.as_ref(), BolideType::Custom(_)))
    }

    /// 获取类型对应的 release 函数名
    fn get_release_func_name(ty: &BolideType) -> Option<&'static str> {
        match ty {
            BolideType::Str => Some("@_string_release"),
            BolideType::Bytes => Some("@_bytes_release"),
            BolideType::BigInt => Some("@_bigint_release"),
            BolideType::Decimal => Some("@_decimal_release"),
            BolideType::List(_) => Some("@_list_release"),
            BolideType::Dict(_, _) => Some("@_dict_release"),
            BolideType::Dynamic => Some("@_dynamic_release"),
            BolideType::Adt(_, _) => Some("@_object_release"),
            BolideType::Custom(_) => Some("@_object_release"),
            BolideType::Tuple(_) => Some("@_tuple_release"),
            // weak/unowned 释放的是弱引用计数
            BolideType::Weak(inner) | BolideType::Unowned(inner)
                if matches!(inner.as_ref(), BolideType::Custom(_)) =>
            {
                Some("@_object_weak_release")
            }
            _ => None,
        }
    }

    /// 获取类型对应的 retain 函数名
    fn get_retain_func_name(ty: &BolideType) -> Option<&'static str> {
        match ty {
            BolideType::Str => Some("@_string_retain"),
            BolideType::Bytes => Some("@_bytes_retain"),
            BolideType::BigInt => Some("@_bigint_retain"),
            BolideType::Decimal => Some("@_decimal_retain"),
            BolideType::List(_) => Some("@_list_retain"),
            BolideType::Dict(_, _) => Some("@_dict_retain"),
            BolideType::Dynamic => Some("@_dynamic_retain"),
            BolideType::Adt(_, _) | BolideType::Custom(_) => Some("@_object_retain"),
            BolideType::Tuple(_) => Some("@_tuple_retain"),
            BolideType::Weak(inner) | BolideType::Unowned(inner)
                if matches!(inner.as_ref(), BolideType::Custom(_)) =>
            {
                Some("@_object_weak_retain")
            }
            _ => None,
        }
    }

    /// RC 捕获释放 tag（与 runtime closure.rs release_capture 对齐）
    fn capture_release_tag(ty: &BolideType) -> i64 {
        match ty {
            BolideType::Str => 1,
            BolideType::BigInt => 2,
            BolideType::Decimal => 3,
            BolideType::List(_) => 4,
            BolideType::Adt(_, _) | BolideType::Custom(_) => 5,
            BolideType::Dict(_, _) => 8,
            BolideType::Dynamic => 9,
            BolideType::Tuple(_) => 10,
            BolideType::Bytes => 12,
            BolideType::Weak(inner) | BolideType::Unowned(inner)
                if matches!(inner.as_ref(), BolideType::Custom(_)) =>
            {
                11
            }
            _ => 0,
        }
    }

    /// Define closure capture metadata in the emitted object and return its runtime address.
    fn define_closure_meta_data(
        &mut self,
        lifted_name: &str,
        tags: &[i64],
    ) -> Result<Value, String> {
        let safe_name: String = lifted_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let data_name = format!("{}_meta", safe_name);

        let data_id = self
            .module
            .declare_data(&data_name, Linkage::Local, false, false)
            .map_err(|e| format!("Failed to declare closure meta data: {}", e))?;

        let mut bytes = Vec::with_capacity(tags.len() * std::mem::size_of::<i64>());
        for tag in tags {
            bytes.extend_from_slice(&tag.to_le_bytes());
        }

        let mut data_desc = DataDescription::new();
        data_desc.define(bytes.into_boxed_slice());
        self.module
            .define_data(data_id, &data_desc)
            .map_err(|e| format!("Failed to define closure meta data: {}", e))?;

        let gv = self.module.declare_data_in_func(data_id, self.builder.func);
        Ok(self.builder.ins().global_value(self.ptr_type, gv))
    }

    /// 对闭包对象生成 @_closure_release 调用
    fn emit_closure_release(&mut self, val: Value) {
        if let Some(&rref) = self.func_refs.get("@_closure_release") {
            self.builder.ins().call(rref, &[val]);
        }
    }

    /// 对闭包对象生成 @_closure_retain 调用
    fn emit_closure_retain(&mut self, val: Value) {
        if let Some(&rref) = self.func_refs.get("@_closure_retain") {
            self.builder.ins().call(rref, &[val]);
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
        self.emit_rc_cleanup_except(None);
    }

    /// 释放所有 RC 变量，except_var 指定的变量除外（用于 `return 变量` 时移交所有权）
    fn emit_rc_cleanup_except(&mut self, except_var: Option<Variable>) {
        let vars_to_release = self.rc_variables.clone();
        for (var, ty) in vars_to_release {
            if Some(var) == except_var {
                continue;
            }
            let val = self.builder.use_var(var);
            self.emit_release(val, &ty);
        }

        // 释放持有闭包的局部变量
        let closure_names: Vec<String> = self.closure_vars.iter().cloned().collect();
        for name in closure_names {
            if let Some(&var) = self.variables.get(&name) {
                if Some(var) == except_var {
                    continue;
                }
                let val = self.builder.use_var(var);
                self.emit_closure_release(val);
            }
        }
    }

    /// __main__ 返回前释放全局 RC 变量 + 全局闭包变量
    fn emit_global_cleanup(&mut self) {
        let global_pairs: Vec<(String, BolideType)> = self
            .global_var_types
            .iter()
            .filter(|(_, ty)| Self::is_rc_type(ty))
            .map(|(n, t)| (n.clone(), t.clone()))
            .collect();
        for (name, ty) in global_pairs {
            if let Some(&gv) = self.global_refs.get(&name) {
                let addr = self.builder.ins().global_value(self.ptr_type, gv);
                let val = self
                    .builder
                    .ins()
                    .load(self.ptr_type, MemFlags::new(), addr, 0);
                let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                let is_null = self.builder.ins().icmp(IntCC::Equal, val, null_val);
                let release_block = self.builder.create_block();
                let skip_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(is_null, skip_block, &[], release_block, &[]);
                self.builder.switch_to_block(release_block);
                self.builder.seal_block(release_block);
                self.emit_release(val, &ty);
                self.builder.ins().jump(skip_block, &[]);
                self.builder.switch_to_block(skip_block);
                self.builder.seal_block(skip_block);
            }
        }
        let closure_global_names: Vec<String> = self.closure_vars.iter().cloned().collect();
        for name in closure_global_names {
            if let Some(&gv) = self.global_refs.get(&name) {
                let addr = self.builder.ins().global_value(self.ptr_type, gv);
                let val = self
                    .builder
                    .ins()
                    .load(self.ptr_type, MemFlags::new(), addr, 0);
                let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                let is_null = self.builder.ins().icmp(IntCC::Equal, val, null_val);
                let release_block = self.builder.create_block();
                let skip_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(is_null, skip_block, &[], release_block, &[]);
                self.builder.switch_to_block(release_block);
                self.builder.seal_block(release_block);
                self.emit_closure_release(val);
                self.builder.ins().jump(skip_block, &[]);
                self.builder.switch_to_block(skip_block);
                self.builder.seal_block(skip_block);
            }
        }
    }

    /// 统一的 release 辅助函数
    fn emit_release(&mut self, val: Value, ty: &BolideType) {
        if let BolideType::Tuple(_) = ty {
            if let Some(&release_func) = self.func_refs.get("@_tuple_release") {
                self.builder.ins().call(release_func, &[val]);
            }
        } else if let BolideType::Adt(ref adt_name, ref type_args) = ty {
            let null_val = self.builder.ins().iconst(self.ptr_type, 0);
            let is_null = self.builder.ins().icmp(IntCC::Equal, val, null_val);

            let check_block = self.builder.create_block();
            let fields_block = self.builder.create_block();
            let release_block = self.builder.create_block();
            let continue_block = self.builder.create_block();

            self.builder
                .ins()
                .brif(is_null, continue_block, &[], check_block, &[]);

            self.builder.switch_to_block(check_block);
            self.builder.seal_block(check_block);
            if let Some(&rc_func) = self.func_refs.get("@_object_ref_count") {
                let call = self.builder.ins().call(rc_func, &[val]);
                let count = self.builder.inst_results(call)[0];
                let one = self.builder.ins().iconst(types::I64, 1);
                let is_last = self.builder.ins().icmp(IntCC::Equal, count, one);
                self.builder
                    .ins()
                    .brif(is_last, fields_block, &[], release_block, &[]);
            } else {
                self.builder.ins().jump(release_block, &[]);
            }

            self.builder.switch_to_block(fields_block);
            self.builder.seal_block(fields_block);
            self.emit_adt_fields_cleanup(val, adt_name, type_args);
            self.builder.ins().jump(release_block, &[]);

            self.builder.switch_to_block(release_block);
            self.builder.seal_block(release_block);
            if let Some(&release_func) = self.func_refs.get("@_object_release") {
                self.builder.ins().call(release_func, &[val]);
            }
            self.builder.ins().jump(continue_block, &[]);

            self.builder.switch_to_block(continue_block);
            self.builder.seal_block(continue_block);
        } else if let BolideType::Custom(ref class_name) = ty {
            // 对于 Custom 类型，需要先检查是否为 null
            // 字段清理仅在最后一个强引用（refcount == 1）时执行，
            // 否则共享对象的字段会被重复释放
            let null_val = self.builder.ins().iconst(self.ptr_type, 0);
            let is_null = self.builder.ins().icmp(IntCC::Equal, val, null_val);

            let check_block = self.builder.create_block();
            let fields_block = self.builder.create_block();
            let release_block = self.builder.create_block();
            let continue_block = self.builder.create_block();

            self.builder
                .ins()
                .brif(is_null, continue_block, &[], check_block, &[]);

            // check_block: 仅当 strong_count == 1 时清理字段
            self.builder.switch_to_block(check_block);
            self.builder.seal_block(check_block);
            if let Some(&rc_func) = self.func_refs.get("@_object_ref_count") {
                let call = self.builder.ins().call(rc_func, &[val]);
                let count = self.builder.inst_results(call)[0];
                let one = self.builder.ins().iconst(types::I64, 1);
                let is_last = self.builder.ins().icmp(IntCC::Equal, count, one);
                self.builder
                    .ins()
                    .brif(is_last, fields_block, &[], release_block, &[]);
            } else {
                self.builder.ins().jump(release_block, &[]);
            }

            // fields_block: 释放对象内部 RC 字段
            self.builder.switch_to_block(fields_block);
            self.builder.seal_block(fields_block);
            self.emit_object_fields_cleanup(val, class_name);
            self.builder.ins().jump(release_block, &[]);

            // release_block: 释放对象本身
            self.builder.switch_to_block(release_block);
            self.builder.seal_block(release_block);
            if let Some(&release_func) = self.func_refs.get("@_object_release") {
                self.builder.ins().call(release_func, &[val]);
            }
            self.builder.ins().jump(continue_block, &[]);

            // continue_block: 继续执行
            self.builder.switch_to_block(continue_block);
            self.builder.seal_block(continue_block);
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
                            let field_ptr =
                                self.builder.ins().iadd_imm(obj_ptr, field.offset as i64);
                            let field_val =
                                self.builder
                                    .ins()
                                    .load(types::I64, MemFlags::new(), field_ptr, 0);
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
            Expr::Bool(b) => Ok(self
                .builder
                .ins()
                .iconst(types::I64, if *b { 1 } else { 0 })),
            Expr::String(s) => self.compile_string_literal(s),
            Expr::BigInt(s) => self.compile_bigint_literal(s),
            Expr::Decimal(s) => self.compile_decimal_literal(s),
            Expr::Ident(name) => self.compile_ident(name),
            Expr::BinOp(left, op, right) => self.compile_binop(left, op, right),
            Expr::UnaryOp(op, operand) => self.compile_unary(op, operand),
            Expr::Call(callee, args) => self.compile_call(callee, args),
            Expr::NamedArg(..) | Expr::SpreadArg(_) | Expr::KwSpreadArg(_) => {
                Err("argument modifiers are only valid inside call argument lists".to_string())
            }
            Expr::None => Ok(self.builder.ins().iconst(types::I64, 0)),
            Expr::Index(base, index) => self.compile_index(base, index),
            Expr::Slice(base, start, end, step) => self.compile_slice(base, start, end, step),
            Expr::Member(base, member) => self.compile_member(base, member),
            Expr::List(items) => self.compile_list(items),
            Expr::ListComprehension {
                expr,
                vars,
                iter,
                filter,
            } => self.compile_list_comprehension(expr, vars, iter, filter.as_deref()),
            Expr::Tuple(items) => self.compile_tuple(items),
            Expr::Dict(entries) => self.compile_dict(entries),
            Expr::Spawn(name, args) => self.compile_spawn(name, args),
            Expr::Await(inner) => self.compile_await(inner),
            Expr::Recv(channel) => self.compile_recv_channel(channel),
            Expr::SpawnAll(exprs) => self.compile_spawn_all(exprs),
            Expr::Closure {
                params,
                return_type,
                body,
            } => self.compile_closure(params, return_type.as_ref(), body),
        }
    }

    /// 闭包编译（AOT）：暂未实现，占位。
    fn compile_closure(
        &mut self,
        params: &[Param],
        return_type: Option<&BolideType>,
        body: &[Statement],
    ) -> Result<Value, String> {
        // 1. 自由变量分析 -> 捕获列表（仅捕获本函数局部变量）
        let free = crate::closure_capture::free_variables(params, body);
        let mut captures: Vec<(String, BolideType)> = Vec::new();
        for name in &free {
            if self.variables.contains_key(name) {
                let ty = self.var_types.get(name).cloned().unwrap_or(BolideType::Int);
                captures.push((name.clone(), ty));
            }
        }

        // 2. 生成 lifted 函数名 + 声明签名 (env_ptr, ...params) -> ret
        let lifted_name = format!(
            "__closure_{}_{}",
            self.current_func, self.closure_local_counter
        );
        self.closure_local_counter += 1;

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(self.ptr_type)); // env
        for p in params {
            let pty = self.normalize_bolide_type(&p.ty);
            sig.params
                .push(AbiParam::new(self.bolide_type_to_cranelift(&pty)));
        }
        if let Some(ret) = return_type {
            let ret = self.normalize_bolide_type(ret);
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(&ret)));
        }
        let func_id = self
            .module
            .declare_function(&lifted_name, Linkage::Local, &sig)
            .map_err(|e| format!("declare closure error: {}", e))?;

        let func_ref = self.module.declare_func_in_func(func_id, self.builder.func);
        let fn_ptr = self.builder.ins().func_addr(self.ptr_type, func_ref);

        // 3. 构造 env + meta
        let env_size = (captures.len() * 8) as i64;
        let (env_ptr, meta_ptr) = if captures.is_empty() {
            let null = self.builder.ins().iconst(self.ptr_type, 0);
            (null, self.builder.ins().iconst(self.ptr_type, 0))
        } else {
            let alloc_ref = *self
                .func_refs
                .get("@_bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(alloc_call)[0];

            let mut tags: Vec<i64> = vec![captures.len() as i64];
            for (i, (name, ty)) in captures.clone().iter().enumerate() {
                let mut val = self.compile_ident(name)?;
                if let Some(retain) = Self::get_retain_func_name(ty) {
                    if let Some(&rref) = self.func_refs.get(retain) {
                        self.builder.ins().call(rref, &[val]);
                    }
                }
                if matches!(ty, BolideType::Float) {
                    val = self.builder.ins().bitcast(types::I64, MemFlags::new(), val);
                }
                let offset = (i * 8) as i32;
                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val, env_ptr, offset);
                tags.push(Self::capture_release_tag(ty));
            }

            let meta_ptr = self.define_closure_meta_data(&lifted_name, &tags)?;
            (env_ptr, meta_ptr)
        };

        // 4. 调用 closure_new(fn_ptr, env_ptr, env_size, meta_ptr)
        let new_ref = *self
            .func_refs
            .get("@_closure_new")
            .ok_or("closure_new not found")?;
        let size_val = self.builder.ins().iconst(types::I64, env_size);
        let call = self
            .builder
            .ins()
            .call(new_ref, &[fn_ptr, env_ptr, size_val, meta_ptr]);
        let closure_val = self.builder.inst_results(call)[0];

        self.pending_closures.push(ClosureJob {
            func_id,
            name: lifted_name,
            params: params.to_vec(),
            return_type: return_type.cloned(),
            body: body.to_vec(),
            captures,
        });

        self.closure_temps.push(closure_val);
        Ok(closure_val)
    }

    /// 编译列表推导式: [expr for var in iter if filter]
    fn compile_list_comprehension(
        &mut self,
        expr: &Expr,
        vars: &[String],
        iter: &Expr,
        filter: Option<&Expr>,
    ) -> Result<Value, String> {
        if vars.len() != 1 {
            return Err(
                "list comprehension with multiple loop variables is not yet supported".to_string(),
            );
        }
        let loop_var_name = &vars[0];

        let iter_elem_ty = self.infer_iter_elem_type(iter);

        // 临时绑定循环变量以推断推导式元素类型
        let old_ty = self.var_types.get(loop_var_name).cloned();
        self.var_types
            .insert(loop_var_name.clone(), iter_elem_ty.clone());
        let elem_ty = self.infer_expr_type(expr).unwrap_or(BolideType::Int);
        if let Some(old) = old_ty {
            self.var_types.insert(loop_var_name.clone(), old);
        } else {
            self.var_types.remove(loop_var_name);
        }

        // 创建结果列表
        let elem_tag = match elem_ty {
            BolideType::Float => 1,
            BolideType::Bool => 2,
            BolideType::Str => 3,
            BolideType::BigInt => 4,
            BolideType::Decimal => 5,
            BolideType::List(_) => 6,
            BolideType::Dict(_, _) => 8,
            BolideType::Dynamic => 9,
            _ => 0,
        };

        let list_new = *self
            .func_refs
            .get("@_list_new")
            .ok_or("list_new not found")?;
        let elem_type_val = self.builder.ins().iconst(types::I8, elem_tag as i64);
        let call = self.builder.ins().call(list_new, &[elem_type_val]);
        let list_ptr = self.builder.inst_results(call)[0];

        let result_name = format!("@_lc_result_{}", self.var_counter);
        let result_var = self.declare_variable(&result_name, self.ptr_type);
        self.builder.def_var(result_var, list_ptr);
        self.var_types
            .insert(result_name.clone(), BolideType::List(Box::new(elem_ty)));

        let push_expr = Expr::Call(
            Box::new(Expr::Member(
                Box::new(Expr::Ident(result_name)),
                "push".to_string(),
            )),
            vec![expr.clone()],
        );
        let body = if let Some(filter_expr) = filter {
            vec![Statement::If(IfStmt {
                condition: filter_expr.clone(),
                then_body: vec![Statement::Expr(push_expr)],
                elif_branches: vec![],
                else_body: None,
            })]
        } else {
            vec![Statement::Expr(push_expr)]
        };

        let for_stmt = ForStmt {
            vars: vars.to_vec(),
            iter: iter.clone(),
            body,
        };
        self.compile_for(&for_stmt)?;

        Ok(list_ptr)
    }

    /// 推断迭代器元素类型（辅助列表推导式）
    fn infer_iter_elem_type(&self, iter: &Expr) -> BolideType {
        if let Expr::Call(callee, _) = iter {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "range" {
                    return BolideType::Int;
                }
            }
        }
        match self.infer_expr_type(iter) {
            Some(BolideType::List(inner)) => *inner,
            _ => BolideType::Int,
        }
    }

    /// 编译字符串字面量
    fn compile_string_literal(&mut self, s: &str) -> Result<Value, String> {
        let func_ref = *self
            .func_refs
            .get("@_string_literal")
            .ok_or("string_literal not found")?;

        // Get the GlobalValue for this string from string_globals
        let (gv, len) = *self
            .string_globals
            .get(s)
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
            let func_ref = *self
                .func_refs
                .get("@_bigint_from_i64")
                .ok_or("bigint_from_i64 not found")?;
            let arg = self.builder.ins().iconst(types::I64, n);
            let call = self.builder.ins().call(func_ref, &[arg]);
            val = self.builder.inst_results(call)[0];
        } else {
            let func_ref = *self
                .func_refs
                .get("@_bigint_from_str")
                .ok_or("bigint_from_str not found")?;
            // 数字串作为数据段发射（AOT 产物是独立进程，不能嵌入编译期主机指针）
            let (gv, len) = *self
                .string_globals
                .get(s)
                .ok_or_else(|| format!("BigInt literal data not found for: {}", s))?;
            let ptr_val = self.builder.ins().global_value(self.ptr_type, gv);
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
            let func_ref = *self
                .func_refs
                .get("@_decimal_from_f64")
                .ok_or("decimal_from_f64 not found")?;
            let arg = self.builder.ins().f64const(f);
            let call = self.builder.ins().call(func_ref, &[arg]);
            val = self.builder.inst_results(call)[0];
        } else {
            // 经 decimal_from_str 构造：数字串作为数据段发射，避免嵌入编译期主机指针
            let func_ref = *self
                .func_refs
                .get("@_decimal_from_str")
                .ok_or("decimal_from_str not found")?;
            let (gv, len) = *self
                .string_globals
                .get(s)
                .ok_or_else(|| format!("Decimal literal data not found for: {}", s))?;
            let ptr_val = self.builder.ins().global_value(self.ptr_type, gv);
            let len_val = self.builder.ins().iconst(types::I64, len as i64);
            let call = self.builder.ins().call(func_ref, &[ptr_val, len_val]);
            val = self.builder.inst_results(call)[0];
        }
        self.track_temp_rc_value(val, &BolideType::Decimal);
        Ok(val)
    }

    /// 记录临时 RC 值（表达式中间结果）
    fn track_temp_rc_value(&mut self, val: Value, ty: &BolideType) {
        if Self::is_rc_type(ty) && !self.temp_rc_values.iter().any(|(v, _)| *v == val) {
            self.temp_rc_values.push((val, ty.clone()));
        }
    }

    /// 移除临时 RC 值（所有权转移）
    fn remove_temp_rc_value(&mut self, val: Value) {
        self.temp_rc_values.retain(|(v, _)| *v != val);
    }

    /// 释放所有临时 RC 值
    fn release_temp_rc_values(&mut self) {
        let temps = std::mem::take(&mut self.temp_rc_values);
        for (val, ty) in temps {
            self.emit_release(val, &ty);
        }
        // 同时释放未吸收的闭包临时值
        let closure_temps = std::mem::take(&mut self.closure_temps);
        for val in closure_temps {
            self.emit_closure_release(val);
        }
    }

    /// 从闭包临时列表移除（被变量吸收或返回时）
    fn remove_temp_closure(&mut self, val: Value) {
        self.closure_temps.retain(|v| *v != val);
    }

    /// 获取类型对应的 clone 函数名
    fn get_clone_func_name(ty: &BolideType) -> Option<&'static str> {
        match ty {
            BolideType::Str => Some("@_string_clone"),
            BolideType::Bytes => Some("@_bytes_clone"),
            BolideType::BigInt => Some("@_bigint_clone"),
            BolideType::Decimal => Some("@_decimal_clone"),
            BolideType::List(_) => Some("@_list_clone"),
            BolideType::Dict(_, _) => Some("@_dict_clone"),
            BolideType::Dynamic => Some("@_dynamic_clone"),
            BolideType::Tuple(_) => Some("@_tuple_clone"),
            BolideType::Adt(_, _) => Some("@_object_clone"),
            BolideType::Custom(_) => Some("@_object_clone"),
            // weak/unowned 克隆只增加弱引用计数
            BolideType::Weak(inner) | BolideType::Unowned(inner)
                if matches!(inner.as_ref(), BolideType::Custom(_)) =>
            {
                Some("@_object_weak_clone")
            }
            _ => None,
        }
    }

    /// 统一的 retain (clone) 辅助函数
    fn emit_retain(&mut self, val: Value, ty: &BolideType) -> Value {
        if matches!(ty, BolideType::Tuple(_)) {
            if let Some(&clone_func) = self.func_refs.get("@_tuple_clone") {
                let call = self.builder.ins().call(clone_func, &[val]);
                return self.builder.inst_results(call)[0];
            }
            if let Some(&retain_func) = self.func_refs.get("@_tuple_retain") {
                self.builder.ins().call(retain_func, &[val]);
            }
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
            // weak/unowned 变量访问前检查对象是否存活（死对象访问确定性 abort）
            if self.weak_variables.contains(name) {
                if let Some(&assert_ref) = self.func_refs.get("@_object_assert_alive") {
                    self.builder.ins().call(assert_ref, &[val]);
                }
            }
            // 借用语义：读变量返回裸值，不 retain、不计入临时（与 JIT 一致）。
            // 所有权只在 let / 赋值 / Owned 实参 / 返回等转移点处理。
            return Ok(val);
        }
        // 全局变量
        if let Some(&gv) = self.global_refs.get(name) {
            let addr = self.builder.ins().global_value(self.ptr_type, gv);
            let load_ty = self
                .global_var_types
                .get(name)
                .map(|t| self.bolide_type_to_cranelift(t))
                .unwrap_or(self.ptr_type);
            let val = self.builder.ins().load(load_ty, MemFlags::new(), addr, 0);
            // weak/unowned 检查
            if self.weak_variables.contains(name) {
                if let Some(&assert_ref) = self.func_refs.get("@_object_assert_alive") {
                    self.builder.ins().call(assert_ref, &[val]);
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
        let left_type = self.infer_expr_type(left);
        let right_type = self.infer_expr_type(right);

        // 类类型运算符重载
        if let Some(BolideType::Custom(ref class_name)) = left_type {
            if let Some(result) = self.try_operator_overload(left, op, right, class_name)? {
                return Ok(result);
            }
        }

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
                    let cmp = self
                        .builder
                        .ins()
                        .fcmp(FloatCC::GreaterThanOrEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::And | BinOp::Or => {
                    Err("Logical operations not supported for floats".to_string())
                }
                BinOp::Shl | BinOp::Shr | BinOp::BitAnd | BinOp::BitOr | BinOp::Xor => {
                    Err("Bit operations not supported for float".to_string())
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
                    let cmp = self
                        .builder
                        .ins()
                        .icmp(IntCC::SignedLessThanOrEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Gt => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedGreaterThan, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::Ge => {
                    let cmp = self
                        .builder
                        .ins()
                        .icmp(IntCC::SignedGreaterThanOrEqual, lhs, rhs);
                    Ok(self.builder.ins().uextend(types::I64, cmp))
                }
                BinOp::And => Ok(self.builder.ins().band(lhs, rhs)),
                BinOp::Or => Ok(self.builder.ins().bor(lhs, rhs)),
                BinOp::Shl => Ok(self.builder.ins().ishl(lhs, rhs)),
                BinOp::Shr => Ok(self.builder.ins().sshr(lhs, rhs)),
                BinOp::BitAnd => Ok(self.builder.ins().band(lhs, rhs)),
                BinOp::BitOr => Ok(self.builder.ins().bor(lhs, rhs)),
                BinOp::Xor => Ok(self.builder.ins().bxor(lhs, rhs)),
            }
        }
    }

    fn try_operator_overload(
        &mut self,
        left: &Expr,
        op: &BinOp,
        right: &Expr,
        class_name: &str,
    ) -> Result<Option<Value>, String> {
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
        let full_name = format!("{}_{}", class_name, method_name);
        if self.func_return_types.contains_key(&full_name) {
            let result = self.compile_method_call(left, method_name, &[right.clone()])?;
            return Ok(Some(result));
        }
        Ok(None)
    }

    /// 编译字符串二元运算
    fn compile_string_binop(
        &mut self,
        left: &Expr,
        op: &BinOp,
        right: &Expr,
    ) -> Result<Value, String> {
        match op {
            BinOp::Add => {
                let mut parts = Vec::new();
                self.collect_string_concat_operands(left, &mut parts);
                self.collect_string_concat_operands(right, &mut parts);
                if parts.len() > 2 {
                    return self.compile_string_concat_many(&parts);
                }

                let lhs = self.compile_expr(left)?;
                let rhs = self.compile_expr(right)?;
                // 字符串连接
                let func_ref = *self
                    .func_refs
                    .get("@_string_concat")
                    .ok_or("string_concat not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            BinOp::Eq => {
                let lhs = self.compile_expr(left)?;
                let rhs = self.compile_expr(right)?;
                // 字符串相等比较
                let func_ref = *self
                    .func_refs
                    .get("@_string_eq")
                    .ok_or("string_eq not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                Ok(self.builder.inst_results(call)[0])
            }
            BinOp::Ne => {
                let lhs = self.compile_expr(left)?;
                let rhs = self.compile_expr(right)?;
                // 字符串不等比较
                let func_ref = *self
                    .func_refs
                    .get("@_string_eq")
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

    fn collect_string_concat_operands<'expr>(&self, expr: &'expr Expr, out: &mut Vec<&'expr Expr>) {
        if let Expr::BinOp(left, BinOp::Add, right) = expr {
            let left_is_str = matches!(self.infer_expr_type(left), Some(BolideType::Str));
            let right_is_str = matches!(self.infer_expr_type(right), Some(BolideType::Str));
            if left_is_str && right_is_str {
                self.collect_string_concat_operands(left, out);
                self.collect_string_concat_operands(right, out);
                return;
            }
        }
        out.push(expr);
    }

    fn compile_string_concat_many(&mut self, parts: &[&Expr]) -> Result<Value, String> {
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            values.push(self.compile_expr(part)?);
        }

        let array_size = parts
            .len()
            .checked_mul(8)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("string concat chain is too large")?;
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            array_size,
            0,
        ));
        let array_ptr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
        for (i, value) in values.iter().enumerate() {
            self.builder
                .ins()
                .store(MemFlags::new(), *value, array_ptr, (i * 8) as i32);
        }

        let func_ref = *self
            .func_refs
            .get("@_string_concat_many")
            .ok_or("string_concat_many not found")?;
        let count = self.builder.ins().iconst(types::I64, parts.len() as i64);
        let call = self.builder.ins().call(func_ref, &[array_ptr, count]);
        let result = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(result, &BolideType::Str);
        Ok(result)
    }

    /// 编译 BigInt 二元运算
    fn compile_bigint_binop(
        &mut self,
        lhs: Value,
        op: &BinOp,
        rhs: Value,
    ) -> Result<Value, String> {
        let func_name = match op {
            BinOp::Add => "@_bigint_add",
            BinOp::Sub => "@_bigint_sub",
            BinOp::Mul => "@_bigint_mul",
            BinOp::Div => "@_bigint_div",
            BinOp::Mod => "@_bigint_rem",
            BinOp::Eq => "@_bigint_eq",
            BinOp::Ne => {
                // ne = !eq
                let eq_ref = *self
                    .func_refs
                    .get("@_bigint_eq")
                    .ok_or("bigint_eq not found")?;
                let call = self.builder.ins().call(eq_ref, &[lhs, rhs]);
                let eq_result = self.builder.inst_results(call)[0];
                let one = self.builder.ins().iconst(types::I64, 1);
                return Ok(self.builder.ins().isub(one, eq_result));
            }
            BinOp::Lt => "@_bigint_lt",
            BinOp::Le => "@_bigint_le",
            BinOp::Gt => "@_bigint_gt",
            BinOp::Ge => "@_bigint_ge",
            BinOp::And | BinOp::Or => {
                return Err("Logical operations not supported for BigInt".to_string());
            }
            BinOp::Shl | BinOp::Shr | BinOp::BitAnd | BinOp::BitOr | BinOp::Xor => {
                return Err("Bit operations not yet supported for BigInt".to_string());
            }
        };

        let func_ref = *self
            .func_refs
            .get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
        let result = self.builder.inst_results(call)[0];

        // Track arithmetic results as temps
        if matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        ) {
            self.track_temp_rc_value(result, &BolideType::BigInt);
        }

        Ok(result)
    }

    /// 编译 Decimal 二元运算
    fn compile_decimal_binop(
        &mut self,
        lhs: Value,
        op: &BinOp,
        rhs: Value,
    ) -> Result<Value, String> {
        let func_name = match op {
            BinOp::Add => "@_decimal_add",
            BinOp::Sub => "@_decimal_sub",
            BinOp::Mul => "@_decimal_mul",
            BinOp::Div => "@_decimal_div",
            BinOp::Mod => "@_decimal_rem",
            BinOp::Eq => "@_decimal_eq",
            BinOp::Ne => {
                // ne = !eq
                let eq_ref = *self
                    .func_refs
                    .get("@_decimal_eq")
                    .ok_or("decimal_eq not found")?;
                let call = self.builder.ins().call(eq_ref, &[lhs, rhs]);
                let eq_result = self.builder.inst_results(call)[0];
                let one = self.builder.ins().iconst(types::I64, 1);
                return Ok(self.builder.ins().isub(one, eq_result));
            }
            BinOp::Lt => "@_decimal_lt",
            BinOp::Le => "@_decimal_le",
            BinOp::Gt => "@_decimal_gt",
            BinOp::Ge => "@_decimal_ge",
            BinOp::And | BinOp::Or => {
                return Err("Logical operations not supported for Decimal".to_string());
            }
            BinOp::Shl | BinOp::Shr | BinOp::BitAnd | BinOp::BitOr | BinOp::Xor => {
                return Err("Bit operations not supported for Decimal".to_string());
            }
        };

        let func_ref = *self
            .func_refs
            .get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
        let result = self.builder.inst_results(call)[0];

        // Track arithmetic results as temps
        if matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        ) {
            self.track_temp_rc_value(result, &BolideType::Decimal);
        }

        Ok(result)
    }

    /// 编译一元运算
    fn compile_unary(&mut self, op: &UnaryOp, operand: &Expr) -> Result<Value, String> {
        let operand_type = self.infer_expr_type(operand);
        let val = self.compile_expr(operand)?;

        match op {
            UnaryOp::Neg => match operand_type {
                Some(BolideType::Float) => Ok(self.builder.ins().fneg(val)),
                Some(BolideType::BigInt) => {
                    let func_ref = *self
                        .func_refs
                        .get("@_bigint_neg")
                        .ok_or("bigint_neg not found")?;
                    let call = self.builder.ins().call(func_ref, &[val]);
                    let result = self.builder.inst_results(call)[0];
                    self.track_temp_rc_value(result, &BolideType::BigInt);
                    Ok(result)
                }
                Some(BolideType::Decimal) => {
                    let func_ref = *self
                        .func_refs
                        .get("@_decimal_neg")
                        .ok_or("decimal_neg not found")?;
                    let call = self.builder.ins().call(func_ref, &[val]);
                    let result = self.builder.inst_results(call)[0];
                    self.track_temp_rc_value(result, &BolideType::Decimal);
                    Ok(result)
                }
                _ => Ok(self.builder.ins().ineg(val)),
            },
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
            Expr::Ident(name) => {
                if self.closure_vars.contains(name) || self.closure_param_vars.contains(name) {
                    return self.compile_closure_call_ident(name, args);
                }
                self.compile_named_call(name, args)
            }
            Expr::Member(base, method_name) => {
                if let Expr::Ident(adt_name) = base.as_ref() {
                    if self.adts.contains_key(adt_name) {
                        return self.compile_adt_variant(adt_name, method_name, args);
                    }
                }
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
            // 任意 callee 表达式（fns[0](x) / getFn()(x) / make_adder(5)(10) 等）
            other => {
                // 闭包字面量直接调用
                if let Expr::Closure {
                    params,
                    return_type,
                    ..
                } = other
                {
                    let closure_val = self.compile_expr(other)?;
                    let psig: Vec<BolideType> = params.iter().map(|p| p.ty.clone()).collect();
                    let rsig = return_type.clone().map(Box::new);
                    return self.compile_closure_call_ptr(closure_val, args, &psig, &rsig);
                }
                // 返回闭包的表达式调用：走闭包 ABI
                if let Some(BolideType::FuncSig(params, ret)) = self.infer_expr_type(other) {
                    let closure_val = self.compile_expr(other)?;
                    let was_temp = self.closure_temps.contains(&closure_val);
                    if was_temp {
                        self.remove_temp_closure(closure_val);
                    }
                    let result = self.compile_closure_call_ptr(closure_val, args, &params, &ret)?;
                    if was_temp {
                        self.closure_temps.push(closure_val);
                    }
                    return Ok(result);
                }
                let func_ptr = self.compile_expr(other)?;
                let func_sig = match self.infer_expr_type(other) {
                    Some(BolideType::FuncSig(p, r)) => Some((p, r)),
                    _ => None,
                };
                self.compile_indirect_call_ptr(func_ptr, args, func_sig)
            }
        }
    }

    fn compile_adt_variant(
        &mut self,
        adt_name: &str,
        variant_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        let adt_info = self
            .adts
            .get(adt_name)
            .ok_or_else(|| format!("Unknown enum/union '{}'", adt_name))?
            .clone();
        let variant = adt_info
            .variants
            .iter()
            .find(|v| v.name == variant_name)
            .ok_or_else(|| format!("Unknown variant '{}.{}'", adt_name, variant_name))?
            .clone();

        if args.len() != variant.fields.len() {
            return Err(format!(
                "{}.{} expects {} argument(s), got {}",
                adt_name,
                variant_name,
                variant.fields.len(),
                args.len()
            ));
        }

        let type_args = self.infer_adt_type_args(&adt_info, &variant, args);
        let type_map = Self::adt_type_map(&adt_info, &type_args);
        let object_alloc = *self
            .func_refs
            .get("@_object_alloc")
            .ok_or("object_alloc not found")?;
        let size_val = self.builder.ins().iconst(types::I64, adt_info.size as i64);
        let call = self.builder.ins().call(object_alloc, &[size_val]);
        let obj_ptr = self.builder.inst_results(call)[0];

        let tag_val = self.builder.ins().iconst(types::I64, variant.tag);
        self.builder
            .ins()
            .store(MemFlags::new(), tag_val, obj_ptr, 0);

        for (field, arg) in variant.fields.iter().zip(args.iter()) {
            self.check_borrow_escape(arg, "enum variant")?;
            let field_ty = Self::substitute_type(&field.ty, &type_map);
            let mut val = self.compile_expr(arg)?;
            if matches!(field_ty, BolideType::Dynamic) {
                let actual_ty = self.infer_expr_type(arg).unwrap_or(BolideType::Dynamic);
                if actual_ty != BolideType::Dynamic {
                    val = self.convert_to_dynamic(val, &actual_ty)?;
                }
            }
            if Self::is_rc_type(&field_ty) {
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                if is_temp && !Self::is_weak_ref_type(&field_ty) {
                    self.remove_temp_rc_value(val);
                } else if let Some(func_name) = Self::get_clone_func_name(&field_ty) {
                    if let Some(&func_ref) = self.func_refs.get(func_name) {
                        let call = self.builder.ins().call(func_ref, &[val]);
                        val = self.builder.inst_results(call)[0];
                    }
                }
            }
            let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field.offset as i64);
            self.builder.ins().store(MemFlags::new(), val, field_ptr, 0);
        }

        let result_ty = BolideType::Adt(adt_name.to_string(), type_args);
        self.track_temp_rc_value(obj_ptr, &result_ty);
        Ok(obj_ptr)
    }

    /// 编译方法调用
    fn compile_method_call(
        &mut self,
        base: &Expr,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        let base_type = self.infer_expr_type(base);

        // 处理列表方法
        if let Some(BolideType::List(_)) = &base_type {
            return self.compile_list_method(base, method_name, args);
        }

        // 处理字典方法
        if let Some(BolideType::Dict(_, _)) = &base_type {
            return self.compile_dict_method(base, method_name, args);
        }

        // 处理字符串方法
        if let Some(BolideType::Str) = &base_type {
            return self.compile_string_method(base, method_name, args);
        }

        // 处理 bytes 方法
        if let Some(BolideType::Bytes) = &base_type {
            return self.compile_bytes_method(base, method_name, args);
        }

        // 处理类方法（沿继承链查找）
        if let Some(BolideType::Custom(class_name)) = base_type {
            let base_val = self.compile_expr(base)?;
            let method_full_name = self
                .find_class_method(&class_name, method_name)
                .unwrap_or_else(|| format!("{}_{}", class_name, method_name));

            if let Some(&func_ref) = self.func_refs.get(&method_full_name) {
                // self 与方法参数均为借用：不转移所有权，临时值在语句末释放
                let mut arg_vals = vec![base_val]; // self 作为第一个参数
                let prepared_args = if let Some(params) = self.func_params.get(&method_full_name) {
                    let user_params = params.get(1..).unwrap_or(&[]);
                    self.normalize_args_for_params(&method_full_name, user_params, args)?
                } else {
                    self.prepare_plain_args(&method_full_name, args)?
                };
                let user_params = self
                    .func_params
                    .get(&method_full_name)
                    .map(|params| params.get(1..).unwrap_or(&[]).to_vec())
                    .unwrap_or_default();
                let user_arg_vals =
                    self.compile_prepared_args_for_params(&prepared_args, &user_params)?;
                arg_vals.extend(user_arg_vals);
                let call = self.builder.ins().call(func_ref, &arg_vals);
                let results = self.builder.inst_results(call);
                if results.is_empty() {
                    return Ok(self.builder.ins().iconst(types::I64, 0));
                }
                let result = results[0];
                let ret_ty_opt = self
                    .func_return_types
                    .get(&method_full_name)
                    .cloned()
                    .flatten();
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
    fn compile_list_method(
        &mut self,
        base: &Expr,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        let list_val = self.compile_expr(base)?;

        // 直接调用单参/无参运行时函数返回 i64 的小工具
        macro_rules! call0 {
            ($key:expr) => {{
                let f = *self
                    .func_refs
                    .get($key)
                    .ok_or(concat!($key, " not found"))?;
                let call = self.builder.ins().call(f, &[list_val]);
                Ok(self.builder.inst_results(call)[0])
            }};
        }

        match method_name {
            "len" | "length" | "size" => call0!("@_list_len"),
            "pop" => call0!("@_list_pop"),
            "is_empty" | "empty" => call0!("@_list_is_empty"),
            "first" => call0!("@_list_first"),
            "last" => call0!("@_list_last"),
            "sort" => {
                let f = *self
                    .func_refs
                    .get("@_list_sort")
                    .ok_or("list_sort not found")?;
                self.builder.ins().call(f, &[list_val]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "reverse" => {
                let f = *self
                    .func_refs
                    .get("@_list_reverse")
                    .ok_or("list_reverse not found")?;
                self.builder.ins().call(f, &[list_val]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "clear" => {
                let f = *self
                    .func_refs
                    .get("@_list_clear")
                    .ok_or("list_clear not found")?;
                self.builder.ins().call(f, &[list_val]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "copy" | "clone" => {
                let f = *self
                    .func_refs
                    .get("@_list_clone")
                    .ok_or("list_clone not found")?;
                let call = self.builder.ins().call(f, &[list_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "push" | "append" => {
                self.check_borrow_escape(&args[0], "list method")?;
                let f = *self
                    .func_refs
                    .get("@_list_push")
                    .ok_or("list_push not found")?;
                let val = self.compile_expr(&args[0])?;
                self.builder.ins().call(f, &[list_val, val]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "get" => {
                let f = *self
                    .func_refs
                    .get("@_list_get")
                    .ok_or("list_get not found")?;
                let idx = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(f, &[list_val, idx]);
                Ok(self.builder.inst_results(call)[0])
            }
            "set" => {
                let f = *self
                    .func_refs
                    .get("@_list_set")
                    .ok_or("list_set not found")?;
                let idx = self.compile_expr(&args[0])?;
                let val = self.compile_expr(&args[1])?;
                let call = self.builder.ins().call(f, &[list_val, idx, val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "insert" => {
                self.check_borrow_escape(&args[1], "list method")?;
                let f = *self
                    .func_refs
                    .get("@_list_insert")
                    .ok_or("list_insert not found")?;
                let idx = self.compile_expr(&args[0])?;
                let val = self.compile_expr(&args[1])?;
                self.builder.ins().call(f, &[list_val, idx, val]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "remove" => {
                let f = *self
                    .func_refs
                    .get("@_list_remove")
                    .ok_or("list_remove not found")?;
                let idx = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(f, &[list_val, idx]);
                Ok(self.builder.inst_results(call)[0])
            }
            "extend" => {
                let f = *self
                    .func_refs
                    .get("@_list_extend")
                    .ok_or("list_extend not found")?;
                let other = self.compile_expr(&args[0])?;
                self.builder.ins().call(f, &[list_val, other]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "contains" | "includes" => {
                let f = *self
                    .func_refs
                    .get("@_list_contains")
                    .ok_or("list_contains not found")?;
                let val = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(f, &[list_val, val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "index_of" | "index" | "find" => {
                let f = *self
                    .func_refs
                    .get("@_list_index_of")
                    .ok_or("list_index_of not found")?;
                let val = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(f, &[list_val, val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "count" => {
                let f = *self
                    .func_refs
                    .get("@_list_count")
                    .ok_or("list_count not found")?;
                let val = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(f, &[list_val, val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "slice" => {
                let f = *self
                    .func_refs
                    .get("@_list_slice")
                    .ok_or("list_slice not found")?;
                let start = self.compile_expr(&args[0])?;
                let end = self.compile_expr(&args[1])?;
                let call = self.builder.ins().call(f, &[list_val, start, end]);
                Ok(self.builder.inst_results(call)[0])
            }
            "map" => {
                if args.len() != 1 {
                    return Err("map expects 1 argument (function)".to_string());
                }
                // 结果元素类型 = 回调返回类型；缺省退化为源元素类型
                let src_elem = self.infer_expr_type_from_list(base);
                let ret_ty = self
                    .func_ptr_return_type(&args[0])
                    .unwrap_or_else(|| src_elem.clone());
                let result_tag = Self::bolide_type_to_element_tag(&ret_ty);
                let func_ptr = self.compile_expr_as_func_ptr(&args[0])?;
                let f = *self
                    .func_refs
                    .get("@_list_map")
                    .ok_or("list_map not found")?;
                let tag_val = self.builder.ins().iconst(types::I8, result_tag as i64);
                let call = self.builder.ins().call(f, &[list_val, func_ptr, tag_val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::List(Box::new(ret_ty)));
                Ok(result)
            }
            "filter" => {
                if args.len() != 1 {
                    return Err("filter expects 1 argument (function)".to_string());
                }
                let func_ptr = self.compile_expr_as_func_ptr(&args[0])?;
                let f = *self
                    .func_refs
                    .get("@_list_filter")
                    .ok_or("list_filter not found")?;
                let call = self.builder.ins().call(f, &[list_val, func_ptr]);
                let result = self.builder.inst_results(call)[0];
                let elem_ty = self.infer_expr_type_from_list(base);
                self.track_temp_rc_value(result, &BolideType::List(Box::new(elem_ty)));
                Ok(result)
            }
            _ => Err(format!("Unknown list method: {}", method_name)),
        }
    }

    /// 将函数引用编译为函数指针 Value。
    fn compile_expr_as_func_ptr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Ident(name) => {
                if let Some(&func_ref) = self.func_refs.get(name.as_str()) {
                    Ok(self.builder.ins().func_addr(self.ptr_type, func_ref))
                } else if let Some(&var) = self.variables.get(name.as_str()) {
                    Ok(self.builder.use_var(var))
                } else {
                    Err(format!("Function not found: {}", name))
                }
            }
            _ => {
                let val = self.compile_expr(expr)?;
                Ok(val)
            }
        }
    }

    /// 从 List 类型 base expr 推断元素类型。
    fn infer_expr_type_from_list(&self, base: &Expr) -> BolideType {
        let base_ty = self.infer_expr_type(base);
        if let Some(BolideType::List(elem)) = base_ty {
            *elem
        } else {
            BolideType::Int
        }
    }

    /// 编译字符串方法
    fn compile_string_method(
        &mut self,
        base: &Expr,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        let str_val = self.compile_expr(base)?;

        // 返回字符串的方法（结果需 RC 跟踪）
        let str_ret = |m: &str| -> Option<&'static str> {
            match m {
                "upper" => Some("@_string_upper"),
                "lower" => Some("@_string_lower"),
                "trim" | "strip" => Some("@_string_trim"),
                _ => None,
            }
        };
        // 返回 i64 的单参（字符串参）方法
        let i64_ret = |m: &str| -> Option<&'static str> {
            match m {
                "find" | "index_of" => Some("@_string_find"),
                "contains" | "includes" => Some("@_string_contains"),
                "starts_with" => Some("@_string_starts_with"),
                "ends_with" => Some("@_string_ends_with"),
                "count" => Some("@_string_count"),
                _ => None,
            }
        };

        match method_name {
            "len" | "length" | "size" => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_len")
                    .ok_or("string_len not found")?;
                let call = self.builder.ins().call(func_ref, &[str_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            m if str_ret(m).is_some() => {
                let func_ref = *self.func_refs.get(str_ret(m).unwrap()).unwrap();
                let call = self.builder.ins().call(func_ref, &[str_val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            m if i64_ret(m).is_some() => {
                let arg =
                    self.compile_expr(args.first().ok_or(format!("{} expects 1 argument", m))?)?;
                let func_ref = *self.func_refs.get(i64_ret(m).unwrap()).unwrap();
                let call = self.builder.ins().call(func_ref, &[str_val, arg]);
                Ok(self.builder.inst_results(call)[0])
            }
            "replace" => {
                let old = self.compile_expr(args.first().ok_or("replace expects 2 arguments")?)?;
                let new = self.compile_expr(args.get(1).ok_or("replace expects 2 arguments")?)?;
                let func_ref = *self.func_refs.get("@_string_replace").unwrap();
                let call = self.builder.ins().call(func_ref, &[str_val, old, new]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            "repeat" => {
                let n = self.compile_expr(args.first().ok_or("repeat expects 1 argument")?)?;
                let func_ref = *self.func_refs.get("@_string_repeat").unwrap();
                let call = self.builder.ins().call(func_ref, &[str_val, n]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            "substring" | "substr" => {
                // substring(a, b) 复用切片：step=1, flags=both
                let a = self.compile_expr(args.first().ok_or("substring expects 2 arguments")?)?;
                let b = self.compile_expr(args.get(1).ok_or("substring expects 2 arguments")?)?;
                let one = self.builder.ins().iconst(types::I64, 1);
                let flags = self.builder.ins().iconst(types::I64, 3);
                let func_ref = *self.func_refs.get("@_string_slice").unwrap();
                let call = self
                    .builder
                    .ins()
                    .call(func_ref, &[str_val, a, b, one, flags]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            "char_at" | "at" => {
                let idx = self.compile_expr(args.first().ok_or("char_at expects 1 argument")?)?;
                let func_ref = *self.func_refs.get("@_string_char_at").unwrap();
                let call = self.builder.ins().call(func_ref, &[str_val, idx]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            "split" => {
                let sep = self.compile_expr(args.first().ok_or("split expects 1 argument")?)?;
                let func_ref = *self.func_refs.get("@_string_split").unwrap();
                let call = self.builder.ins().call(func_ref, &[str_val, sep]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::List(Box::new(BolideType::Str)));
                Ok(result)
            }
            _ => Err(format!("Unknown string method: {}", method_name)),
        }
    }

    fn compile_bytes_method(
        &mut self,
        base: &Expr,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        let bytes_val = self.compile_expr(base)?;
        match method_name {
            "len" | "length" | "size" => {
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_len")
                    .ok_or("bytes_len not found")?;
                let call = self.builder.ins().call(func_ref, &[bytes_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "get" => {
                if args.len() != 1 {
                    return Err("bytes.get expects 1 argument".to_string());
                }
                let index = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_get")
                    .ok_or("bytes_get not found")?;
                let call = self.builder.ins().call(func_ref, &[bytes_val, index]);
                Ok(self.builder.inst_results(call)[0])
            }
            "set" => {
                if args.len() != 2 {
                    return Err("bytes.set expects 2 arguments".to_string());
                }
                let index = self.compile_expr(&args[0])?;
                let value = self.compile_expr(&args[1])?;
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_set")
                    .ok_or("bytes_set not found")?;
                let call = self
                    .builder
                    .ins()
                    .call(func_ref, &[bytes_val, index, value]);
                Ok(self.builder.inst_results(call)[0])
            }
            "push" | "append" => {
                if args.len() != 1 {
                    return Err(format!("{} expects 1 argument", method_name));
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_push")
                    .ok_or("bytes_push not found")?;
                self.builder.ins().call(func_ref, &[bytes_val, value]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "copy" | "clone" => {
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_clone")
                    .ok_or("bytes_clone not found")?;
                let call = self.builder.ins().call(func_ref, &[bytes_val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Bytes);
                Ok(result)
            }
            "to_string_lossy" => {
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_to_string_lossy")
                    .ok_or("bytes_to_string_lossy not found")?;
                let call = self.builder.ins().call(func_ref, &[bytes_val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            _ => Err(format!("Unknown bytes method: {}", method_name)),
        }
    }

    /// 沿继承链查找类方法，返回已声明的全名（如 `Parent_get_age`）
    fn find_class_method(&self, class_name: &str, method_name: &str) -> Option<String> {
        let mut current = class_name.to_string();
        loop {
            let full = format!("{}_{}", current, method_name);
            if self.func_refs.contains_key(&full) {
                return Some(full);
            }
            match self.classes.get(&current).and_then(|c| c.parent.clone()) {
                Some(parent) => current = parent,
                None => return None,
            }
        }
    }

    /// 沿继承链查找方法返回类型（用于类方法调用的类型推断）。
    fn lookup_method_return_type(&self, class_name: &str, method_name: &str) -> Option<BolideType> {
        let mut current = class_name.to_string();
        loop {
            let full = format!("{}_{}", current, method_name);
            if let Some(ret) = self.func_return_types.get(&full) {
                return ret.clone();
            }
            match self.classes.get(&current).and_then(|c| c.parent.clone()) {
                Some(parent) => current = parent,
                None => return None,
            }
        }
    }

    /// 编译字典方法
    fn compile_dict_method(
        &mut self,
        base: &Expr,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        let dict_val = self.compile_expr(base)?;
        match method_name {
            "set" => {
                let f = *self
                    .func_refs
                    .get("@_dict_set")
                    .ok_or("dict_set not found")?;
                let k = self.compile_expr(&args[0])?;
                let v = self.compile_expr(&args[1])?;
                self.builder.ins().call(f, &[dict_val, k, v]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "get" => {
                let f = *self
                    .func_refs
                    .get("@_dict_get")
                    .ok_or("dict_get not found")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(f, &[dict_val, k]);
                Ok(self.builder.inst_results(call)[0])
            }
            "contains" => {
                let f = *self
                    .func_refs
                    .get("@_dict_contains")
                    .ok_or("dict_contains not found")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(f, &[dict_val, k]);
                Ok(self.builder.inst_results(call)[0])
            }
            "remove" => {
                let f = *self
                    .func_refs
                    .get("@_dict_remove")
                    .ok_or("dict_remove not found")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(f, &[dict_val, k]);
                Ok(self.builder.inst_results(call)[0])
            }
            "len" | "size" => {
                let f = *self
                    .func_refs
                    .get("@_dict_len")
                    .ok_or("dict_len not found")?;
                let call = self.builder.ins().call(f, &[dict_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "is_empty" => {
                let f = *self
                    .func_refs
                    .get("@_dict_is_empty")
                    .ok_or("dict_is_empty not found")?;
                let call = self.builder.ins().call(f, &[dict_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "clear" => {
                let f = *self
                    .func_refs
                    .get("@_dict_clear")
                    .ok_or("dict_clear not found")?;
                self.builder.ins().call(f, &[dict_val]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "keys" => {
                let f = *self
                    .func_refs
                    .get("@_dict_keys")
                    .ok_or("dict_keys not found")?;
                let call = self.builder.ins().call(f, &[dict_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "values" => {
                let f = *self
                    .func_refs
                    .get("@_dict_values")
                    .ok_or("dict_values not found")?;
                let call = self.builder.ins().call(f, &[dict_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            "clone" => {
                let f = *self
                    .func_refs
                    .get("@_dict_clone")
                    .ok_or("dict_clone not found")?;
                let call = self.builder.ins().call(f, &[dict_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Err(format!("Unknown dictionary method: {}", method_name)),
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
            "bytes" => return self.compile_bytes_new(args),
            "bigint" => return self.compile_to_bigint(args),
            "decimal" => return self.compile_to_decimal(args),
            "input" => return self.compile_input(args),
            "join" => return self.compile_join(args),
            "channel" => return self.compile_channel_create(args),
            // 调试用内置自由函数（运行时符号经 @_ 隔离，需显式分发）
            "bigint_debug_stats" => {
                let func_ref = *self
                    .func_refs
                    .get("@_bigint_debug_stats")
                    .ok_or("bigint_debug_stats not found")?;
                self.builder.ins().call(func_ref, &[]);
                return Ok(self.builder.ins().iconst(types::I64, 0));
            }
            "tuple_debug_stats" => {
                let func_ref = *self
                    .func_refs
                    .get("@_tuple_debug_stats")
                    .ok_or("tuple_debug_stats not found")?;
                self.builder.ins().call(func_ref, &[]);
                return Ok(self.builder.ins().iconst(types::I64, 0));
            }
            _ => {}
        }

        // 检查是否是 async 函数调用
        if self.async_funcs.contains(name) {
            return self.compile_async_call(name, args);
        }

        // 检查是否是 extern (FFI) 函数调用
        if let Some((lib_path, extern_func)) = self.extern_funcs.get(name).cloned() {
            return self.compile_extern_call(&lib_path, &extern_func, args);
        }

        // 若 name 是持有函数指针的变量（func / func(...) 类型），走间接调用
        // 包括局部变量与全局变量（顶层 let f = add1）
        let var_func_ty = self
            .var_types
            .get(name)
            .or_else(|| self.global_var_types.get(name))
            .cloned();
        let is_func_var = (self.variables.contains_key(name)
            || self.global_refs.contains_key(name))
            && matches!(
                var_func_ty,
                Some(BolideType::Func) | Some(BolideType::FuncSig(_, _))
            );
        if is_func_var {
            let func_sig = match var_func_ty {
                Some(BolideType::FuncSig(p, r)) => Some((p, r)),
                _ => None,
            };
            return self.compile_indirect_call(name, args, func_sig);
        }

        // 查找函数引用
        let func_ref = *self
            .func_refs
            .get(name)
            .ok_or_else(|| format!("Function not found: {}", name))?;

        let prepared_args = self.prepare_call_args(name, args)?;

        // 按参数模式处理实参（与 JIT 一致：borrow 借用 / owned 移动 / ref 传址）
        let param_modes: Vec<ParamMode> = self
            .func_params
            .get(name)
            .map(|ps| ps.iter().map(|p| p.mode).collect())
            .unwrap_or_else(|| vec![ParamMode::Borrow; prepared_args.len()]);

        let mut arg_vals = Vec::new();
        let mut ref_slots: Vec<(String, Value)> = Vec::new();

        for (i, arg) in prepared_args.iter().enumerate() {
            let mode = param_modes.get(i).copied().unwrap_or(ParamMode::Borrow);
            match mode {
                ParamMode::Borrow => {
                    // 借用：直接传值，调用方保留所有权（临时值仍在语句末释放）
                    let val = self.compile_prepared_arg(arg)?;
                    let target_ty = self
                        .func_params
                        .get(name)
                        .and_then(|params| params.get(i))
                        .map(|param| param.ty.clone());
                    let val = if let (Some(target_ty), PreparedArg::Expr(expr)) = (target_ty, arg) {
                        let actual_ty = self
                            .infer_expr_type(expr)
                            .map(|ty| self.normalize_bolide_type(&ty))
                            .unwrap_or(BolideType::Dynamic);
                        self.prepare_value_for_storage(val, &actual_ty, &target_ty)?
                    } else {
                        val
                    };
                    arg_vals.push(val);
                }
                ParamMode::Owned => {
                    let expr = match arg {
                        PreparedArg::Expr(expr) => expr,
                        _ => {
                            return Err(
                                "owned parameter cannot receive packed arguments".to_string()
                            )
                        }
                    };
                    let raw_val = self.compile_expr(expr)?;
                    let target_ty = self
                        .func_params
                        .get(name)
                        .and_then(|params| params.get(i))
                        .map(|param| param.ty.clone());
                    let val = if let Some(target_ty) = target_ty {
                        let actual_ty = self
                            .infer_expr_type(expr)
                            .map(|ty| self.normalize_bolide_type(&ty))
                            .unwrap_or(BolideType::Dynamic);
                        self.prepare_value_for_storage(raw_val, &actual_ty, &target_ty)?
                    } else {
                        raw_val
                    };
                    arg_vals.push(val);
                    if let Expr::Ident(var_name) = expr {
                        // 变量所有权移交被调方：置空并停止在本作用域释放
                        self.moved_variables.insert(var_name.clone());
                        if let Some(&var) = self.variables.get(var_name) {
                            let null = self.builder.ins().iconst(self.ptr_type, 0);
                            self.builder.def_var(var, null);
                            self.rc_variables.retain(|(v, _)| *v != var);
                        } else if let Some(&gv) = self.global_refs.get(var_name) {
                            let addr = self.builder.ins().global_value(self.ptr_type, gv);
                            let null = self.builder.ins().iconst(self.ptr_type, 0);
                            self.builder.ins().store(MemFlags::new(), null, addr, 0);
                        }
                    } else {
                        // 临时值作为 owned 实参：所有权转移，移出临时列表
                        self.remove_temp_rc_value(val);
                    }
                }
                ParamMode::Ref => {
                    let expr = match arg {
                        PreparedArg::Expr(expr) => expr,
                        _ => {
                            return Err("ref parameter cannot receive packed arguments".to_string())
                        }
                    };
                    if let Expr::Ident(var_name) = expr {
                        if let Some(&var) = self.variables.get(var_name) {
                            let current = self.builder.use_var(var);
                            let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                                StackSlotKind::ExplicitSlot,
                                8,
                                0,
                            ));
                            let slot_addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                            self.builder
                                .ins()
                                .store(MemFlags::new(), current, slot_addr, 0);
                            arg_vals.push(slot_addr);
                            ref_slots.push((var_name.clone(), slot_addr));
                        } else if let Some(&gv) = self.global_refs.get(var_name) {
                            // 全局变量：直接传递其数据段地址，被调函数原地读写
                            let addr = self.builder.ins().global_value(self.ptr_type, gv);
                            arg_vals.push(addr);
                            ref_slots.push((var_name.clone(), addr));
                        } else {
                            return Err(format!("Undefined variable for ref: {}", var_name));
                        }
                    } else {
                        return Err("ref parameter must be a variable".to_string());
                    }
                }
            }
        }

        // 调用函数
        let call = self.builder.ins().call(func_ref, &arg_vals);

        // ref 参数：从栈槽读回新值写回变量
        for (var_name, slot_addr) in &ref_slots {
            if let Some(&var) = self.variables.get(var_name) {
                // 局部变量：读回新值，释放旧值
                let new_val =
                    self.builder
                        .ins()
                        .load(self.ptr_type, MemFlags::new(), *slot_addr, 0);
                let var_ty = self.var_types.get(var_name).cloned();
                if let Some(ref ty) = var_ty {
                    if Self::is_rc_type(ty) {
                        let old_val = self.builder.use_var(var);
                        self.emit_release(old_val, ty);
                    }
                }
                self.builder.def_var(var, new_val);
            } else if self.global_refs.contains_key(var_name) {
                // 全局变量：被调函数已原地写入新值（ref 参数传递了数据段地址）。
                // swap 操作交换指针，总引用数不变，无需释放旧值。
            }
        }

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
                if matches!(ret_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                    self.closure_temps.push(result);
                }
            }
            Ok(result)
        }
    }

    /// 通过变量持有的函数指针进行间接调用（func / func(...) 类型）
    fn compile_indirect_call(
        &mut self,
        var_name: &str,
        args: &[Expr],
        func_sig: Option<(Vec<BolideType>, Option<Box<BolideType>>)>,
    ) -> Result<Value, String> {
        // 函数指针来源：局部变量 / 全局变量 / 裸函数名，统一经 compile_ident 求值
        let func_ptr = self.compile_ident(var_name)?;
        self.compile_indirect_call_ptr(func_ptr, args, func_sig)
    }

    /// 通过闭包变量名调用闭包
    fn compile_closure_call_ident(&mut self, name: &str, args: &[Expr]) -> Result<Value, String> {
        let (params, ret) = match self
            .var_types
            .get(name)
            .or_else(|| self.global_var_types.get(name))
            .cloned()
        {
            Some(BolideType::FuncSig(p, r)) => (p, r),
            _ => (Vec::new(), None),
        };
        let closure_val = self.compile_ident(name)?;
        self.compile_closure_call_ptr(closure_val, args, &params, &ret)
    }

    /// 调用闭包对象：加载 fn_ptr/env_ptr，按 (env, ...args) 间接调用。
    fn compile_closure_call_ptr(
        &mut self,
        closure_val: Value,
        args: &[Expr],
        param_types: &[BolideType],
        ret_type: &Option<Box<BolideType>>,
    ) -> Result<Value, String> {
        let fn_ref = *self
            .func_refs
            .get("@_closure_fn_ptr")
            .ok_or("closure_fn_ptr not found")?;
        let env_ref = *self
            .func_refs
            .get("@_closure_env_ptr")
            .ok_or("closure_env_ptr not found")?;
        let fc = self.builder.ins().call(fn_ref, &[closure_val]);
        let fn_ptr = self.builder.inst_results(fc)[0];
        let ec = self.builder.ins().call(env_ref, &[closure_val]);
        let env_ptr = self.builder.inst_results(ec)[0];

        let mut arg_values = vec![env_ptr];
        for arg in args {
            arg_values.push(self.compile_expr(arg)?);
        }

        let call_conv = self.builder.func.signature.call_conv;
        let mut sig = Signature::new(call_conv);
        sig.params.push(AbiParam::new(self.ptr_type)); // env
        if param_types.is_empty() {
            for arg in args {
                let ty = self.infer_expr_type(arg).unwrap_or(BolideType::Int);
                sig.params
                    .push(AbiParam::new(self.bolide_type_to_cranelift(&ty)));
            }
        } else {
            for ty in param_types {
                sig.params
                    .push(AbiParam::new(self.bolide_type_to_cranelift(ty)));
            }
        }
        let ret_b = ret_type
            .as_ref()
            .map(|t| (**t).clone())
            .unwrap_or(BolideType::Int);
        sig.returns
            .push(AbiParam::new(self.bolide_type_to_cranelift(&ret_b)));

        let sig_ref = self.builder.import_signature(sig);
        let call = self
            .builder
            .ins()
            .call_indirect(sig_ref, fn_ptr, &arg_values);
        let result = self.builder.inst_results(call)[0];

        if Self::is_rc_type(&ret_b) {
            self.track_temp_rc_value(result, &ret_b);
        }
        if matches!(ret_b, BolideType::FuncSig(_, _) | BolideType::Func) {
            self.closure_temps.push(result);
        }
        Ok(result)
    }

    /// 通过已求值的函数指针 Value 进行间接调用（支持任意 callee 表达式）
    fn compile_indirect_call_ptr(
        &mut self,
        func_ptr: Value,
        args: &[Expr],
        func_sig: Option<(Vec<BolideType>, Option<Box<BolideType>>)>,
    ) -> Result<Value, String> {
        let mut arg_values = Vec::new();
        for arg in args {
            arg_values.push(self.compile_expr(arg)?);
        }

        let call_conv = self.builder.func.signature.call_conv;
        let mut sig = Signature::new(call_conv);
        if let Some((ref param_types, _)) = func_sig {
            for ty in param_types {
                sig.params
                    .push(AbiParam::new(self.bolide_type_to_cranelift(ty)));
            }
        } else {
            for arg in args {
                let ty = self.infer_expr_type(arg).unwrap_or(BolideType::Int);
                sig.params
                    .push(AbiParam::new(self.bolide_type_to_cranelift(&ty)));
            }
        }
        let ret_ty = func_sig.as_ref().and_then(|(_, r)| r.clone());
        if let Some(ref rt) = ret_ty {
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(rt)));
        } else {
            sig.returns.push(AbiParam::new(types::I64));
        }

        let sig_ref = self.builder.import_signature(sig);
        let call = self
            .builder
            .ins()
            .call_indirect(sig_ref, func_ptr, &arg_values);
        let result = self.builder.inst_results(call)[0];

        if let Some(rt) = ret_ty {
            if Self::is_rc_type(&rt) {
                self.track_temp_rc_value(result, &rt);
            }
        }
        Ok(result)
    }

    fn emit_adt_fields_cleanup(
        &mut self,
        obj_ptr: Value,
        adt_name: &str,
        type_args: &[BolideType],
    ) {
        let Some(adt_info) = self.adts.get(adt_name).cloned() else {
            return;
        };
        let tag_val = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), obj_ptr, 0);
        let done_block = self.builder.create_block();
        let type_map = Self::adt_type_map(&adt_info, type_args);

        for variant in &adt_info.variants {
            let body_block = self.builder.create_block();
            let next_block = self.builder.create_block();
            let expected = self.builder.ins().iconst(types::I64, variant.tag);
            let matched = self.builder.ins().icmp(IntCC::Equal, tag_val, expected);
            self.builder
                .ins()
                .brif(matched, body_block, &[], next_block, &[]);

            self.builder.switch_to_block(body_block);
            self.builder.seal_block(body_block);
            for field in &variant.fields {
                let field_ty = Self::substitute_type(&field.ty, &type_map);
                if Self::is_rc_type(&field_ty) {
                    let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field.offset as i64);
                    let cl_ty = self.bolide_type_to_cranelift(&field_ty);
                    let field_val = self
                        .builder
                        .ins()
                        .load(cl_ty, MemFlags::new(), field_ptr, 0);
                    self.emit_release(field_val, &field_ty);
                }
            }
            self.builder.ins().jump(done_block, &[]);

            self.builder.switch_to_block(next_block);
            self.builder.seal_block(next_block);
        }

        self.builder.ins().jump(done_block, &[]);
        self.builder.switch_to_block(done_block);
        self.builder.seal_block(done_block);
    }

    fn adt_type_map(adt_info: &AdtInfo, type_args: &[BolideType]) -> HashMap<String, BolideType> {
        adt_info
            .type_params
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                (
                    name.clone(),
                    type_args.get(idx).cloned().unwrap_or(BolideType::Dynamic),
                )
            })
            .collect()
    }

    fn substitute_type(ty: &BolideType, type_map: &HashMap<String, BolideType>) -> BolideType {
        match ty {
            BolideType::Generic(name) => type_map.get(name).cloned().unwrap_or(BolideType::Dynamic),
            BolideType::List(inner) => {
                BolideType::List(Box::new(Self::substitute_type(inner, type_map)))
            }
            BolideType::Dict(k, v) => BolideType::Dict(
                Box::new(Self::substitute_type(k, type_map)),
                Box::new(Self::substitute_type(v, type_map)),
            ),
            BolideType::Tuple(items) => BolideType::Tuple(
                items
                    .iter()
                    .map(|item| Self::substitute_type(item, type_map))
                    .collect(),
            ),
            BolideType::Channel(inner) => {
                BolideType::Channel(Box::new(Self::substitute_type(inner, type_map)))
            }
            BolideType::FuncSig(params, ret) => BolideType::FuncSig(
                params
                    .iter()
                    .map(|param| Self::substitute_type(param, type_map))
                    .collect(),
                ret.as_ref()
                    .map(|ret| Box::new(Self::substitute_type(ret, type_map))),
            ),
            BolideType::Adt(name, args) => BolideType::Adt(
                name.clone(),
                args.iter()
                    .map(|arg| Self::substitute_type(arg, type_map))
                    .collect(),
            ),
            BolideType::Weak(inner) => {
                BolideType::Weak(Box::new(Self::substitute_type(inner, type_map)))
            }
            BolideType::Unowned(inner) => {
                BolideType::Unowned(Box::new(Self::substitute_type(inner, type_map)))
            }
            other => other.clone(),
        }
    }

    fn unify_generic_type(
        pattern: &BolideType,
        actual: &BolideType,
        bindings: &mut HashMap<String, BolideType>,
    ) {
        match pattern {
            BolideType::Generic(name) => {
                bindings
                    .entry(name.clone())
                    .or_insert_with(|| actual.clone());
            }
            BolideType::List(p) => {
                if let BolideType::List(a) = actual {
                    Self::unify_generic_type(p, a, bindings);
                }
            }
            BolideType::Dict(pk, pv) => {
                if let BolideType::Dict(ak, av) = actual {
                    Self::unify_generic_type(pk, ak, bindings);
                    Self::unify_generic_type(pv, av, bindings);
                }
            }
            BolideType::Tuple(ps) => {
                if let BolideType::Tuple(as_) = actual {
                    for (p, a) in ps.iter().zip(as_.iter()) {
                        Self::unify_generic_type(p, a, bindings);
                    }
                }
            }
            BolideType::Adt(pn, ps) => {
                if let BolideType::Adt(an, as_) = actual {
                    if pn == an {
                        for (p, a) in ps.iter().zip(as_.iter()) {
                            Self::unify_generic_type(p, a, bindings);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn infer_adt_type_args(
        &self,
        adt_info: &AdtInfo,
        variant: &AdtVariantInfo,
        args: &[Expr],
    ) -> Vec<BolideType> {
        let mut bindings = HashMap::new();
        for (field, arg) in variant.fields.iter().zip(args.iter()) {
            let actual = self.infer_expr_type(arg).unwrap_or(BolideType::Dynamic);
            Self::unify_generic_type(&field.ty, &actual, &mut bindings);
        }
        adt_info
            .type_params
            .iter()
            .map(|name| bindings.get(name).cloned().unwrap_or(BolideType::Dynamic))
            .collect()
    }

    fn normalize_args_for_params(
        &self,
        call_name: &str,
        params: &[Param],
        raw_args: &[Expr],
    ) -> Result<Vec<PreparedArg>, String> {
        let args_index = params.iter().position(|p| p.is_variadic);
        let kwargs_index = params.iter().position(|p| p.is_kw_variadic);
        let mut slots: Vec<Option<Expr>> = vec![None; params.len()];
        let mut packed_args = Vec::new();
        let mut packed_kwargs = Vec::new();
        let mut next_pos = 0usize;
        let mut named_or_spread_seen = false;

        for raw in raw_args {
            match raw {
                Expr::NamedArg(name, value) => {
                    named_or_spread_seen = true;
                    if let Some(i) = params
                        .iter()
                        .position(|p| !p.is_variadic && !p.is_kw_variadic && p.name == *name)
                    {
                        if slots[i].is_some() {
                            return Err(format!(
                                "{} got multiple values for argument '{}'",
                                call_name, name
                            ));
                        }
                        slots[i] = Some((**value).clone());
                    } else if kwargs_index.is_some() {
                        packed_kwargs.push(PackedKwargItem::Entry(name.clone(), (**value).clone()));
                    } else {
                        return Err(format!(
                            "{} got unexpected keyword argument '{}'",
                            call_name, name
                        ));
                    }
                }
                Expr::SpreadArg(value) => {
                    named_or_spread_seen = true;
                    if args_index.is_none() {
                        return Err(format!("{} does not accept *args", call_name));
                    }
                    packed_args.push(PackedArgItem::Spread((**value).clone()));
                }
                Expr::KwSpreadArg(value) => {
                    named_or_spread_seen = true;
                    if kwargs_index.is_none() {
                        return Err(format!("{} does not accept **kwargs", call_name));
                    }
                    packed_kwargs.push(PackedKwargItem::Spread((**value).clone()));
                }
                expr => {
                    if named_or_spread_seen {
                        return Err(format!(
                            "{} positional argument cannot follow named or spread arguments",
                            call_name
                        ));
                    }
                    while next_pos < params.len()
                        && (params[next_pos].is_variadic
                            || params[next_pos].is_kw_variadic
                            || slots[next_pos].is_some())
                    {
                        next_pos += 1;
                    }
                    if next_pos < params.len() {
                        slots[next_pos] = Some(expr.clone());
                        next_pos += 1;
                    } else if args_index.is_some() {
                        packed_args.push(PackedArgItem::Expr(expr.clone()));
                    } else {
                        return Err(format!("{} got too many positional arguments", call_name));
                    }
                }
            }
        }

        for (i, param) in params.iter().enumerate() {
            if param.is_variadic || param.is_kw_variadic {
                continue;
            }
            if slots[i].is_none() {
                if let Some(default_value) = &param.default_value {
                    slots[i] = Some(default_value.clone());
                } else {
                    return Err(format!(
                        "{} missing required argument '{}'",
                        call_name, param.name
                    ));
                }
            }
        }

        let mut prepared = Vec::with_capacity(params.len());
        for (i, param) in params.iter().enumerate() {
            if param.is_variadic {
                let elem_ty = match &param.ty {
                    BolideType::List(inner) => inner.as_ref().clone(),
                    _ => BolideType::Dynamic,
                };
                prepared.push(PreparedArg::PackedArgs {
                    elem_ty,
                    items: packed_args.clone(),
                });
            } else if param.is_kw_variadic {
                let value_ty = match &param.ty {
                    BolideType::Dict(_, value) => value.as_ref().clone(),
                    _ => BolideType::Dynamic,
                };
                prepared.push(PreparedArg::PackedKwargs {
                    value_ty,
                    items: packed_kwargs.clone(),
                });
            } else {
                prepared.push(PreparedArg::Expr(slots[i].take().unwrap()));
            }
        }

        Ok(prepared)
    }

    fn prepare_plain_args(
        &self,
        call_name: &str,
        raw_args: &[Expr],
    ) -> Result<Vec<PreparedArg>, String> {
        let mut prepared = Vec::with_capacity(raw_args.len());
        for arg in raw_args {
            match arg {
                Expr::NamedArg(..) => {
                    return Err(format!("{} does not accept named arguments", call_name));
                }
                Expr::SpreadArg(..) => {
                    return Err(format!("{} does not accept *args", call_name));
                }
                Expr::KwSpreadArg(..) => {
                    return Err(format!("{} does not accept **kwargs", call_name));
                }
                expr => prepared.push(PreparedArg::Expr(expr.clone())),
            }
        }
        Ok(prepared)
    }

    fn prepare_call_args(
        &self,
        call_name: &str,
        raw_args: &[Expr],
    ) -> Result<Vec<PreparedArg>, String> {
        if let Some(params) = self.func_params.get(call_name) {
            self.normalize_args_for_params(call_name, params, raw_args)
        } else {
            self.prepare_plain_args(call_name, raw_args)
        }
    }

    fn compile_prepared_arg(&mut self, arg: &PreparedArg) -> Result<Value, String> {
        match arg {
            PreparedArg::Expr(expr) => self.compile_expr(expr),
            PreparedArg::PackedArgs { elem_ty, items } => self.compile_packed_args(elem_ty, items),
            PreparedArg::PackedKwargs { value_ty, items } => {
                self.compile_packed_kwargs(value_ty, items)
            }
        }
    }

    fn compile_prepared_args_for_params(
        &mut self,
        prepared_args: &[PreparedArg],
        params: &[Param],
    ) -> Result<Vec<Value>, String> {
        let mut values = Vec::with_capacity(prepared_args.len());
        for (i, arg) in prepared_args.iter().enumerate() {
            let raw_val = self.compile_prepared_arg(arg)?;
            let val = if let (Some(param), PreparedArg::Expr(expr)) = (params.get(i), arg) {
                let actual_ty = self
                    .infer_expr_type(expr)
                    .map(|ty| self.normalize_bolide_type(&ty))
                    .unwrap_or(BolideType::Dynamic);
                self.prepare_value_for_storage(raw_val, &actual_ty, &param.ty)?
            } else {
                raw_val
            };
            values.push(val);
        }
        Ok(values)
    }

    fn compile_packed_args(
        &mut self,
        elem_ty: &BolideType,
        items: &[PackedArgItem],
    ) -> Result<Value, String> {
        let list_new = *self
            .func_refs
            .get("@_list_new")
            .ok_or("list_new not found")?;
        let list_push = *self
            .func_refs
            .get("@_list_push")
            .ok_or("list_push not found")?;
        let list_extend = *self
            .func_refs
            .get("@_list_extend")
            .ok_or("list_extend not found")?;
        let elem_tag = Self::bolide_type_to_element_tag(elem_ty);
        let elem_tag_val = self.builder.ins().iconst(types::I8, elem_tag as i64);
        let call = self.builder.ins().call(list_new, &[elem_tag_val]);
        let list_ptr = self.builder.inst_results(call)[0];

        for item in items {
            match item {
                PackedArgItem::Expr(expr) => {
                    self.check_borrow_escape(expr, "*args")?;
                    let mut val = self.compile_expr(expr)?;
                    if elem_tag == 1 && self.builder.func.dfg.value_type(val) == types::F64 {
                        val = self.builder.ins().bitcast(types::I64, MemFlags::new(), val);
                    }
                    self.builder.ins().call(list_push, &[list_ptr, val]);
                }
                PackedArgItem::Spread(expr) => {
                    self.check_borrow_escape(expr, "*args")?;
                    let spread = self.compile_expr(expr)?;
                    self.builder.ins().call(list_extend, &[list_ptr, spread]);
                }
            }
        }

        self.track_temp_rc_value(list_ptr, &BolideType::List(Box::new(elem_ty.clone())));
        Ok(list_ptr)
    }

    fn compile_packed_kwargs(
        &mut self,
        value_ty: &BolideType,
        items: &[PackedKwargItem],
    ) -> Result<Value, String> {
        let dict_new = *self
            .func_refs
            .get("@_dict_new")
            .ok_or("dict_new not found")?;
        let dict_set = *self
            .func_refs
            .get("@_dict_set")
            .ok_or("dict_set not found")?;
        let dict_extend = *self
            .func_refs
            .get("@_dict_extend")
            .ok_or("dict_extend not found")?;
        let key_tag = self.builder.ins().iconst(types::I8, 3);
        let value_tag = self
            .builder
            .ins()
            .iconst(types::I8, Self::bolide_type_to_element_tag(value_ty) as i64);
        let call = self.builder.ins().call(dict_new, &[key_tag, value_tag]);
        let dict_ptr = self.builder.inst_results(call)[0];

        for item in items {
            match item {
                PackedKwargItem::Entry(name, expr) => {
                    self.check_borrow_escape(expr, "**kwargs")?;
                    let key = self.compile_expr(&Expr::String(name.clone()))?;
                    let mut val = self.compile_expr(expr)?;
                    if matches!(value_ty, BolideType::Dynamic) {
                        let actual_ty = self.infer_expr_type(expr).unwrap_or(BolideType::Dynamic);
                        if actual_ty != BolideType::Dynamic {
                            val = self.convert_to_dynamic(val, &actual_ty)?;
                        }
                    }
                    self.builder.ins().call(dict_set, &[dict_ptr, key, val]);
                }
                PackedKwargItem::Spread(expr) => {
                    self.check_borrow_escape(expr, "**kwargs")?;
                    let spread = self.compile_expr(expr)?;
                    self.builder.ins().call(dict_extend, &[dict_ptr, spread]);
                }
            }
        }

        let dict_ty = BolideType::Dict(Box::new(BolideType::Str), Box::new(value_ty.clone()));
        self.track_temp_rc_value(dict_ptr, &dict_ty);
        Ok(dict_ptr)
    }

    /// 编译 async 函数调用 - 启动协程并返回 Future
    fn compile_async_call(&mut self, func_name: &str, args: &[Expr]) -> Result<Value, String> {
        let prepared_args = self.prepare_call_args(func_name, args)?;
        let param_types: Vec<BolideType> = self
            .func_params
            .get(func_name)
            .map(|params| params.iter().map(|p| p.ty.clone()).collect())
            .unwrap_or_default();

        // 获取返回类型确定 spawn 函数后缀
        let return_type = self
            .func_return_types
            .get(func_name)
            .cloned()
            .unwrap_or(None);
        let type_suffix = Self::spawn_type_suffix(&return_type);

        // 计算函数地址与 env 指针
        let (func_addr, env_ptr) = if prepared_args.is_empty() {
            let target_func_ref = *self
                .func_refs
                .get(func_name)
                .ok_or_else(|| format!("Undefined async function: {}", func_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, target_func_ref);
            let null_env = self.builder.ins().iconst(self.ptr_type, 0);
            (func_addr, null_env)
        } else {
            // 有参数：分配 env，存入参数，使用 trampoline
            let env_size = (prepared_args.len() * 8) as i64;
            let alloc_ref = *self
                .func_refs
                .get("@_bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(alloc_call)[0];

            for (i, arg) in prepared_args.iter().enumerate() {
                let raw_val = self.compile_prepared_arg(arg)?;
                let val =
                    if let (Some(target_ty), PreparedArg::Expr(expr)) = (param_types.get(i), arg) {
                        let actual_ty = self
                            .infer_expr_type(expr)
                            .map(|ty| self.normalize_bolide_type(&ty))
                            .unwrap_or(BolideType::Dynamic);
                        self.prepare_value_for_storage(raw_val, &actual_ty, target_ty)?
                    } else {
                        raw_val
                    };
                let offset = (i * 8) as i32;
                // 对 RC 类型参数 clone 一份交给协程（跨协程生命周期安全）
                let arg_ty = param_types.get(i);
                let val_to_store = match arg_ty.and_then(Self::get_clone_func_name) {
                    Some(clone_func) => {
                        if let Some(&clone_ref) = self.func_refs.get(clone_func) {
                            let call = self.builder.ins().call(clone_ref, &[val]);
                            self.builder.inst_results(call)[0]
                        } else {
                            val
                        }
                    }
                    None => val,
                };
                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val_to_store, env_ptr, offset);
            }

            let trampoline_name = self.get_trampoline_name(func_name);
            let trampoline_ref = *self
                .func_refs
                .get(&trampoline_name)
                .ok_or_else(|| format!("Trampoline not found: {}", trampoline_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, trampoline_ref);
            (func_addr, env_ptr)
        };

        // 调用 coroutine_spawn_{suffix} 启动协程
        let spawn_func_name = if prepared_args.is_empty() {
            format!("@_coroutine_spawn{}", type_suffix)
        } else {
            format!("@_coroutine_spawn{}_with_env", type_suffix)
        };
        let spawn_ref = *self
            .func_refs
            .get(&spawn_func_name)
            .ok_or_else(|| format!("{} not found", spawn_func_name))?;
        let call = if prepared_args.is_empty() {
            self.builder.ins().call(spawn_ref, &[func_addr])
        } else {
            self.builder.ins().call(spawn_ref, &[func_addr, env_ptr])
        };
        let future_ptr = self.builder.inst_results(call)[0];

        // 注册 Future 到当前 scope（如果在 scope 内）
        if let Some(&scope_register) = self.func_refs.get("@_scope_register") {
            self.builder.ins().call(scope_register, &[future_ptr]);
        }

        Ok(future_ptr)
    }

    /// C 类型转换为 Cranelift 类型（extern 调用用）
    fn ctype_to_cranelift(&self, ty: &CType) -> types::Type {
        match ty {
            CType::Void => types::I64,
            CType::Char | CType::UChar | CType::I8 | CType::U8 => types::I8,
            CType::Short | CType::UShort | CType::I16 | CType::U16 => types::I16,
            CType::Int | CType::UInt | CType::I32 | CType::U32 => types::I32,
            CType::Long
            | CType::ULong
            | CType::LongLong
            | CType::ULongLong
            | CType::I64
            | CType::U64
            | CType::SizeT
            | CType::PtrDiffT => types::I64,
            CType::Float => types::F32,
            CType::Double => types::F64,
            CType::Bool => types::I8,
            CType::Ptr(_) | CType::Array(_, _) | CType::FuncPtr { .. } => self.ptr_type,
            CType::Struct(_) => self.ptr_type,
        }
    }

    fn managed_extern_return_type(ty: &CType) -> Option<BolideType> {
        match ty {
            CType::Struct(name) => match name.as_str() {
                "str" | "string" => Some(BolideType::Str),
                "bytes" => Some(BolideType::Bytes),
                "list" => Some(BolideType::List(Box::new(BolideType::Dynamic))),
                "dict" => Some(BolideType::Dict(
                    Box::new(BolideType::Dynamic),
                    Box::new(BolideType::Dynamic),
                )),
                "dynamic" => Some(BolideType::Dynamic),
                _ => None,
            },
            CType::Ptr(inner) => match inner.as_ref() {
                CType::Struct(name) if name == "dynamic" => Some(BolideType::Dynamic),
                _ => None,
            },
            _ => None,
        }
    }

    fn extern_return_type_to_bolide(ty: &CType) -> BolideType {
        if let Some(managed_ty) = Self::managed_extern_return_type(ty) {
            return managed_ty;
        }
        match ty {
            CType::Float | CType::Double => BolideType::Float,
            CType::Ptr(inner) => match inner.as_ref() {
                CType::Char => BolideType::Str,
                CType::Struct(name) if name == "dynamic" => BolideType::Dynamic,
                _ => BolideType::Ptr,
            },
            _ => BolideType::Int,
        }
    }

    /// 编译 extern (FFI) 函数调用：直接链接或动态加载 + C↔Bolide 类型转换
    fn compile_extern_call(
        &mut self,
        lib_path: &str,
        extern_func: &bolide_parser::ExternFunc,
        args: &[Expr],
    ) -> Result<Value, String> {
        if is_dynamic_lib_spec(lib_path) {
            return self.compile_dynamic_extern_call(lib_path, extern_func, args);
        }

        let func_ref = *self
            .func_refs
            .get(&extern_func.name)
            .ok_or_else(|| format!("Extern function not declared: {}", extern_func.name))?;

        let arg_values = self.compile_extern_args(extern_func, args)?;
        let call = self.builder.ins().call(func_ref, &arg_values);
        let results = self.builder.inst_results(call).to_vec();
        self.convert_extern_result(extern_func, &results)
    }

    fn compile_dynamic_extern_call(
        &mut self,
        lib_path: &str,
        extern_func: &bolide_parser::ExternFunc,
        args: &[Expr],
    ) -> Result<Value, String> {
        let resolved_lib = resolve_dynamic_lib_spec(lib_path)?;
        let lib_path_ptr = self.create_c_string_constant(&resolved_lib)?;
        let func_name_ptr = self.create_c_string_constant(&extern_func.name)?;

        let load_lib_ref = *self
            .func_refs
            .get("@_ffi_load_library")
            .ok_or("ffi_load_library not found")?;
        self.builder.ins().call(load_lib_ref, &[lib_path_ptr]);

        let get_symbol_ref = *self
            .func_refs
            .get("@_ffi_get_symbol")
            .ok_or("ffi_get_symbol not found")?;
        let symbol_call = self
            .builder
            .ins()
            .call(get_symbol_ref, &[lib_path_ptr, func_name_ptr]);
        let func_ptr = self.builder.inst_results(symbol_call)[0];

        let arg_values = self.compile_extern_args(extern_func, args)?;
        let sig = self.build_extern_signature(extern_func);
        let sig_ref = self.builder.import_signature(sig);
        let call = self
            .builder
            .ins()
            .call_indirect(sig_ref, func_ptr, &arg_values);
        let results = self.builder.inst_results(call).to_vec();
        self.convert_extern_result(extern_func, &results)
    }

    fn create_c_string_constant(&mut self, s: &str) -> Result<Value, String> {
        let mut nul_terminated = String::with_capacity(s.len() + 1);
        nul_terminated.push_str(s);
        nul_terminated.push('\0');

        let (gv, _) = *self
            .string_globals
            .get(&nul_terminated)
            .ok_or_else(|| format!("C string data not found for extern FFI: {}", s))?;
        Ok(self.builder.ins().global_value(self.ptr_type, gv))
    }

    fn build_extern_signature(&mut self, extern_func: &bolide_parser::ExternFunc) -> Signature {
        let mut sig = self.module.make_signature();
        for param in &extern_func.params {
            sig.params
                .push(AbiParam::new(self.ctype_to_cranelift(&param.ty)));
        }
        if let Some(ref ret_ty) = extern_func.return_type {
            sig.returns
                .push(AbiParam::new(self.ctype_to_cranelift(ret_ty)));
        }
        sig
    }

    fn compile_extern_args(
        &mut self,
        extern_func: &bolide_parser::ExternFunc,
        args: &[Expr],
    ) -> Result<Vec<Value>, String> {
        let mut arg_values = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            // 函数指针参数（回调）：传函数地址
            if let Some(param) = extern_func.params.get(i) {
                if matches!(param.ty, CType::FuncPtr { .. }) {
                    if let Expr::Ident(func_name) = arg {
                        if let Some(&cb_ref) = self.func_refs.get(func_name.as_str()) {
                            let func_addr = self.builder.ins().func_addr(self.ptr_type, cb_ref);
                            arg_values.push(func_addr);
                            continue;
                        }
                    }
                }
            }

            let val = self.compile_expr(arg)?;

            // *char 参数：BolideString* -> char*
            if let Some(param) = extern_func.params.get(i) {
                if let CType::Ptr(inner) = &param.ty {
                    if matches!(inner.as_ref(), CType::Char) {
                        let as_cstr_ref = *self
                            .func_refs
                            .get("@_string_as_cstr")
                            .ok_or("string_as_cstr not found")?;
                        let call = self.builder.ins().call(as_cstr_ref, &[val]);
                        arg_values.push(self.builder.inst_results(call)[0]);
                        continue;
                    }
                }
            }

            // 数值类型按 C 签名收窄
            if let Some(param) = extern_func.params.get(i) {
                let expected_ty = self.ctype_to_cranelift(&param.ty);
                let actual_ty = self.builder.func.dfg.value_type(val);
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

        Ok(arg_values)
    }

    fn convert_extern_result(
        &mut self,
        extern_func: &bolide_parser::ExternFunc,
        results: &[Value],
    ) -> Result<Value, String> {
        if results.is_empty() {
            return Ok(self.builder.ins().iconst(types::I64, 0));
        }

        let result = results[0];

        // *char 返回值：char* -> BolideString*
        if let Some(ref ret_ty) = extern_func.return_type {
            if let Some(managed_ty) = Self::managed_extern_return_type(ret_ty) {
                self.track_temp_rc_value(result, &managed_ty);
                return Ok(result);
            }

            if let CType::Ptr(inner) = ret_ty {
                if matches!(inner.as_ref(), CType::Char) {
                    let string_new_ref = *self
                        .func_refs
                        .get("@_string_new")
                        .ok_or("string_new not found")?;
                    let call = self.builder.ins().call(string_new_ref, &[result]);
                    let bolide_string = self.builder.inst_results(call)[0];
                    self.track_temp_rc_value(bolide_string, &BolideType::Str);
                    return Ok(bolide_string);
                }
            }
        }

        // 数值返回值拓宽到 Bolide 类型
        let result_ty = self.builder.func.dfg.value_type(result);
        if result_ty == types::I32 || result_ty == types::I16 || result_ty == types::I8 {
            Ok(self.builder.ins().sextend(types::I64, result))
        } else if result_ty == types::F32 {
            Ok(self.builder.ins().fpromote(types::F64, result))
        } else {
            Ok(result)
        }
    }

    /// 编译 print 函数
    fn compile_print(&mut self, arg: &Expr) -> Result<Value, String> {
        let val = self.compile_expr(arg)?;

        let inferred_type = self.infer_expr_type(arg);
        if let Some(BolideType::Tuple(elem_types)) = &inferred_type {
            self.compile_print_tuple_inline(val, elem_types)?;
            let println_ref = *self.func_refs.get("@_println").ok_or("println not found")?;
            self.builder.ins().call(println_ref, &[]);
            return Ok(self.builder.ins().iconst(types::I64, 0));
        }

        // 容器索引 / 元组元素取出时值为 i64，Float 需 bitcast 回 f64
        let val = if matches!(inferred_type, Some(BolideType::Float)) {
            self.builder.ins().bitcast(types::F64, MemFlags::new(), val)
        } else {
            val
        };

        let func_name = self.get_print_func_name(&inferred_type);

        let func_ref = *self
            .func_refs
            .get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        self.builder.ins().call(func_ref, &[val]);
        Ok(self.builder.ins().iconst(types::I64, 0))
    }

    fn compile_print_tuple_inline(
        &mut self,
        tuple_val: Value,
        elem_types: &[BolideType],
    ) -> Result<(), String> {
        let start_ref = *self
            .func_refs
            .get("@_print_tuple_start")
            .ok_or("print_tuple_start not found")?;
        self.builder.ins().call(start_ref, &[]);

        let tuple_get = *self
            .func_refs
            .get("@_tuple_get")
            .ok_or("tuple_get not found")?;
        let separator_ref = *self
            .func_refs
            .get("@_print_tuple_separator")
            .ok_or("print_tuple_separator not found")?;

        for (i, elem_ty) in elem_types.iter().enumerate() {
            if i > 0 {
                self.builder.ins().call(separator_ref, &[]);
            }
            let idx = self.builder.ins().iconst(types::I64, i as i64);
            let call = self.builder.ins().call(tuple_get, &[tuple_val, idx]);
            let elem_val = self.builder.inst_results(call)[0];
            self.compile_print_value_inline(elem_val, elem_ty)?;
        }

        let end_ref = *self
            .func_refs
            .get("@_print_tuple_end_inline")
            .ok_or("print_tuple_end_inline not found")?;
        self.builder.ins().call(end_ref, &[]);
        Ok(())
    }

    fn compile_print_value_inline(&mut self, val: Value, ty: &BolideType) -> Result<(), String> {
        if let BolideType::Tuple(elem_types) = ty {
            return self.compile_print_tuple_inline(val, elem_types);
        }

        // Float 从元组取出时为 i64 槽位，需要 bitcast 回 f64
        let val = if matches!(ty, BolideType::Float) {
            self.builder.ins().bitcast(types::F64, MemFlags::new(), val)
        } else {
            val
        };

        let func_name = match ty {
            BolideType::Int => "@_print_int_inline",
            BolideType::Float => "@_print_float_inline",
            BolideType::Bool => "@_print_bool_inline",
            BolideType::BigInt => "@_print_bigint_inline",
            BolideType::Decimal => "@_print_decimal_inline",
            BolideType::Str => "@_print_string_inline",
            BolideType::Bytes => "@_print_bytes_inline",
            BolideType::Dynamic => "@_print_dynamic_inline",
            BolideType::List(_) => "@_print_list",
            BolideType::Dict(_, _) => "@_print_dict",
            _ => "@_print_int_inline",
        };
        let func_ref = *self
            .func_refs
            .get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        self.builder.ins().call(func_ref, &[val]);
        Ok(())
    }

    /// 根据类型获取打印函数名
    fn get_print_func_name(&self, ty: &Option<BolideType>) -> &'static str {
        match ty {
            Some(BolideType::Int) => "@_print_int",
            Some(BolideType::Float) => "@_print_float",
            Some(BolideType::Bool) => "@_print_bool",
            Some(BolideType::Str) => "@_print_string",
            Some(BolideType::Bytes) => "@_print_bytes",
            Some(BolideType::BigInt) => "@_print_bigint",
            Some(BolideType::Decimal) => "@_print_decimal",
            Some(BolideType::Dynamic) => "@_print_dynamic",
            Some(BolideType::List(_)) => "@_print_list",
            Some(BolideType::Dict(_, _)) => "@_print_dict",
            Some(BolideType::Tuple(_)) => "@_print_tuple",
            _ => "@_print_int",
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
            Some(BolideType::Float) => Ok(self.builder.ins().fcvt_to_sint(types::I64, val)),
            Some(BolideType::Str) => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_to_int")
                    .ok_or("string_to_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::BigInt) => {
                let func_ref = *self
                    .func_refs
                    .get("@_bigint_to_i64")
                    .ok_or("bigint_to_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Decimal) => {
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_to_i64")
                    .ok_or("decimal_to_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Dynamic) => {
                let func_ref = *self
                    .func_refs
                    .get("@_dynamic_to_int")
                    .ok_or("dynamic_to_int not found")?;
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
            Some(BolideType::Int) => Ok(self.builder.ins().fcvt_from_sint(types::F64, val)),
            Some(BolideType::Str) => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_to_float")
                    .ok_or("string_to_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Decimal) => {
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_to_f64")
                    .ok_or("decimal_to_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Dynamic) => {
                let func_ref = *self
                    .func_refs
                    .get("@_dynamic_to_float")
                    .ok_or("dynamic_to_float not found")?;
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
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_int")
                    .ok_or("string_from_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Float) => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_float")
                    .ok_or("string_from_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Bool) => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_bool")
                    .ok_or("string_from_bool not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::BigInt) => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_bigint")
                    .ok_or("string_from_bigint not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Decimal) => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_decimal")
                    .ok_or("string_from_decimal not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Dynamic) => {
                let func_ref = *self
                    .func_refs
                    .get("@_dynamic_to_string")
                    .ok_or("dynamic_to_string not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_int")
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

    fn compile_bytes_new(&mut self, args: &[Expr]) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("bytes() expects 0 arguments".to_string());
        }
        let func_ref = *self
            .func_refs
            .get("@_bytes_new")
            .ok_or("bytes_new not found")?;
        let call = self.builder.ins().call(func_ref, &[]);
        let result = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(result, &BolideType::Bytes);
        Ok(result)
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
                let func_ref = *self
                    .func_refs
                    .get("@_bigint_from_i64")
                    .ok_or("bigint_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Str) => {
                let func_ref = *self
                    .func_refs
                    .get("@_bigint_from_str")
                    .ok_or("bigint_from_str not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => {
                let func_ref = *self
                    .func_refs
                    .get("@_bigint_from_i64")
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
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_from_i64")
                    .ok_or("decimal_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Float) => {
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_from_f64")
                    .ok_or("decimal_from_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Str) => {
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_from_str")
                    .ok_or("decimal_from_str not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => {
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_from_f64")
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
            let func_ref = *self.func_refs.get("@_input").ok_or("input not found")?;
            let call = self.builder.ins().call(func_ref, &[]);
            let result = self.builder.inst_results(call)[0];
            self.track_temp_rc_value(result, &BolideType::Str);
            Ok(result)
        } else if args.len() == 1 {
            let prompt = self.compile_expr(&args[0])?;
            let func_ref = *self
                .func_refs
                .get("@_input_prompt")
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

        // 通过句柄变量名查出 spawn 的目标函数，从而推断返回类型后缀
        let return_type = if let Expr::Ident(var_name) = &args[0] {
            if let Some(func_name) = self.spawn_func_map.get(var_name) {
                self.func_return_types.get(func_name).cloned().flatten()
            } else {
                None
            }
        } else {
            None
        };

        let type_suffix = Self::spawn_type_suffix(&return_type);
        let result_type = match &return_type {
            Some(BolideType::Float) => types::F64,
            Some(BolideType::Str)
            | Some(BolideType::BigInt)
            | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic)
            | Some(BolideType::Ptr)
            | Some(BolideType::List(_))
            | Some(BolideType::Tuple(_))
            | Some(BolideType::Custom(_)) => self.ptr_type,
            _ => types::I64,
        };

        // 运行时判断 pool/thread 分支
        let pool_is_active_ref = *self
            .func_refs
            .get("@_pool_is_active")
            .ok_or("pool_is_active not found")?;
        let is_active_call = self.builder.ins().call(pool_is_active_ref, &[]);
        let is_active = self.builder.inst_results(is_active_call)[0];

        let pool_block = self.builder.create_block();
        let thread_block = self.builder.create_block();
        let merge_block = self.builder.create_block();
        self.builder.append_block_param(merge_block, result_type);

        self.builder
            .ins()
            .brif(is_active, pool_block, &[], thread_block, &[]);

        // 线程池分支
        self.builder.switch_to_block(pool_block);
        self.builder.seal_block(pool_block);
        let pool_join_name = format!("@_pool_join{}", type_suffix);
        let pool_join_ref = *self
            .func_refs
            .get(&pool_join_name)
            .ok_or_else(|| format!("{} not found", pool_join_name))?;
        let pool_call = self.builder.ins().call(pool_join_ref, &[handle]);
        let pool_result = self.builder.inst_results(pool_call)[0];
        let pool_free_ref = *self
            .func_refs
            .get("@_pool_handle_free")
            .ok_or("pool_handle_free not found")?;
        self.builder.ins().call(pool_free_ref, &[handle]);
        self.builder.ins().jump(merge_block, &[pool_result]);

        // 普通线程分支
        self.builder.switch_to_block(thread_block);
        self.builder.seal_block(thread_block);
        let thread_join_name = format!("@_thread_join{}", type_suffix);
        let thread_join_ref = *self
            .func_refs
            .get(&thread_join_name)
            .ok_or_else(|| format!("{} not found", thread_join_name))?;
        let thread_call = self.builder.ins().call(thread_join_ref, &[handle]);
        let thread_result = self.builder.inst_results(thread_call)[0];
        let thread_free_ref = *self
            .func_refs
            .get("@_thread_handle_free")
            .ok_or("thread_handle_free not found")?;
        self.builder.ins().call(thread_free_ref, &[handle]);
        self.builder.ins().jump(merge_block, &[thread_result]);

        // 合并块
        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);
        let result = self.builder.block_params(merge_block)[0];

        // RC 类型结果作为临时值跟踪，语句末释放
        if let Some(ref ret_ty) = return_type {
            if Self::is_rc_type(ret_ty) {
                self.track_temp_rc_value(result, ret_ty);
            }
        }

        Ok(result)
    }

    /// 编译 channel() 函数
    fn compile_channel_create(&mut self, args: &[Expr]) -> Result<Value, String> {
        let func_ref = *self
            .func_refs
            .get("@_channel_create")
            .ok_or("channel_create not found")?;
        if args.is_empty() {
            let call = self.builder.ins().call(func_ref, &[]);
            Ok(self.builder.inst_results(call)[0])
        } else if args.len() == 1 {
            let size = self.compile_expr(&args[0])?;
            let buffered_ref = *self
                .func_refs
                .get("@_channel_create_buffered")
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
        let index_type = self.infer_expr_type(index);
        let base_val = self.compile_expr(base)?;
        let index_val = self.compile_expr(index)?;

        // 根据类型选择不同的索引函数
        // 根据类型选择不同的索引函数
        match base_type {
            Some(BolideType::List(ref elem_ty)) => {
                if matches!(
                    elem_ty.as_ref(),
                    BolideType::Int | BolideType::Float | BolideType::Bool
                ) {
                    return self.emit_list_get_inline(base_val, index_val, elem_ty.as_ref());
                }
                let func_ref = *self
                    .func_refs
                    .get("@_list_get")
                    .ok_or("list_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                let val = self.builder.inst_results(call)[0];
                // 返回容器内的借用引用，不 clone（对齐 JIT）
                // 调用方若需要独立所有权，compile_var_decl 会 clone
                Ok(val)
            }
            Some(BolideType::Dict(key_ty, _)) => {
                if !Self::dict_key_type_accepts(key_ty.as_ref(), index_type.as_ref()) {
                    return Err(format!(
                        "Dict key type mismatch: expected {:?}, got {:?}",
                        key_ty, index_type
                    ));
                }
                // dict_get 返回 i64 标签值（可能是整数或指针），与 JIT 一致，不 retain
                let func_ref = *self
                    .func_refs
                    .get("@_dict_get")
                    .ok_or("dict_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Bytes) => {
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_get")
                    .ok_or("bytes_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            Some(BolideType::Tuple(_inner_types)) => {
                let func_ref = *self
                    .func_refs
                    .get("@_tuple_get")
                    .ok_or("tuple_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                let val = self.builder.inst_results(call)[0];
                // 返回容器内的借用引用，不 clone（对齐 JIT）
                Ok(val)
            }
            Some(BolideType::Str) => {
                // 字符串索引按码点，返回单码点新串
                let char_at = *self
                    .func_refs
                    .get("@_string_char_at")
                    .ok_or("string_char_at not found")?;
                let call = self.builder.ins().call(char_at, &[base_val, index_val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            _ => {
                // If type unknown, assume tuple or dynamic
                let func_ref = *self
                    .func_refs
                    .get("@_tuple_get")
                    .ok_or("tuple_get not found")?;
                let call = self.builder.ins().call(func_ref, &[base_val, index_val]);
                let val = self.builder.inst_results(call)[0];

                // Without type info, we can't safely retain.
                // This might be a limitation for untyped/dynamic code.
                Ok(val)
            }
        }
    }

    /// 编译切片 base[start:end:step]。缺省 start/end 用 flags 标记，step 缺省 1。
    fn compile_slice(
        &mut self,
        base: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        step: &Option<Box<Expr>>,
    ) -> Result<Value, String> {
        let base_type = self.infer_expr_type(base);
        let base_val = self.compile_expr(base)?;

        let mut flags: i64 = 0;
        let start_val = if let Some(e) = start {
            flags |= 1;
            self.compile_expr(e)?
        } else {
            self.builder.ins().iconst(types::I64, 0)
        };
        let end_val = if let Some(e) = end {
            flags |= 2;
            self.compile_expr(e)?
        } else {
            self.builder.ins().iconst(types::I64, 0)
        };
        let step_val = if let Some(e) = step {
            self.compile_expr(e)?
        } else {
            self.builder.ins().iconst(types::I64, 1)
        };
        let flags_val = self.builder.ins().iconst(types::I64, flags);

        let (func_name, result_ty) = match &base_type {
            Some(BolideType::Str) => ("@_string_slice", BolideType::Str),
            Some(BolideType::List(_)) => ("@_list_slice_step", base_type.clone().unwrap()),
            Some(BolideType::Tuple(_)) => ("@_tuple_slice_step", base_type.clone().unwrap()),
            other => return Err(format!("Cannot slice non-sequence type: {:?}", other)),
        };
        let func_ref = *self
            .func_refs
            .get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let call = self.builder.ins().call(
            func_ref,
            &[base_val, start_val, end_val, step_val, flags_val],
        );
        let result = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(result, &result_ty);
        Ok(result)
    }

    fn dict_key_type_accepts(expected: &BolideType, actual: Option<&BolideType>) -> bool {
        matches!(expected, BolideType::Dynamic) || actual.is_some_and(|actual| expected == actual)
    }

    // ==================== List 内联索引（Int/Float/Bool） ====================
    const LIST_DATA_OFFSET: i64 = 16;
    const LIST_LEN_OFFSET: i64 = 24;

    fn emit_list_get_inline(
        &mut self,
        list_ptr: Value,
        index: Value,
        elem_ty: &BolideType,
    ) -> Result<Value, String> {
        let (elem_byte_width, load_ty) = if matches!(elem_ty, BolideType::Bool) {
            (1, types::I8)
        } else {
            (8, types::I64)
        };
        let len_ptr = self.builder.ins().iadd_imm(list_ptr, Self::LIST_LEN_OFFSET);
        let len = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), len_ptr, 0);
        let in_bounds = self.builder.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let data_ptr_addr = self
            .builder
            .ins()
            .iadd_imm(list_ptr, Self::LIST_DATA_OFFSET);
        let data_ptr = self
            .builder
            .ins()
            .load(self.ptr_type, MemFlags::new(), data_ptr_addr, 0);
        let offset = self.builder.ins().imul_imm(index, elem_byte_width);
        let elem_addr = self.builder.ins().iadd(data_ptr, offset);
        let loaded = self
            .builder
            .ins()
            .load(load_ty, MemFlags::new(), elem_addr, 0);
        let value = if load_ty == types::I8 {
            self.builder.ins().uextend(types::I64, loaded)
        } else {
            loaded
        };
        let zero = self.builder.ins().iconst(types::I64, 0);
        Ok(self.builder.ins().select(in_bounds, value, zero))
    }

    fn emit_list_set_inline(
        &mut self,
        list_ptr: Value,
        index: Value,
        value: Value,
        elem_ty: &BolideType,
    ) -> Result<(), String> {
        let (elem_byte_width, store_ty) = if matches!(elem_ty, BolideType::Bool) {
            (1, types::I8)
        } else {
            (8, types::I64)
        };
        let len_ptr = self.builder.ins().iadd_imm(list_ptr, Self::LIST_LEN_OFFSET);
        let _len = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), len_ptr, 0);
        let data_ptr_addr = self
            .builder
            .ins()
            .iadd_imm(list_ptr, Self::LIST_DATA_OFFSET);
        let data_ptr = self
            .builder
            .ins()
            .load(self.ptr_type, MemFlags::new(), data_ptr_addr, 0);
        let offset = self.builder.ins().imul_imm(index, elem_byte_width);
        let elem_addr = self.builder.ins().iadd(data_ptr, offset);
        // 窄存储时必须 ireduce，否则 store 按 value 类型宽度写出
        let store_val = if store_ty == types::I8 {
            self.builder.ins().ireduce(types::I8, value)
        } else {
            value
        };
        self.builder
            .ins()
            .store(MemFlags::new(), store_val, elem_addr, 0);
        Ok(())
    }

    /// 编译成员访问
    fn compile_member(&mut self, base: &Expr, member: &str) -> Result<Value, String> {
        let base_val = self.compile_expr(base)?;

        // 尝试获取基础表达式的类型
        let base_type = self.infer_expr_type(base);

        // 处理 Weak/Unowned 类型，提取内部的 Custom 类型
        let class_name = match &base_type {
            Some(BolideType::Custom(name)) => Some(name.clone()),
            Some(BolideType::Weak(inner)) => {
                if let BolideType::Custom(name) = inner.as_ref() {
                    Some(name.clone())
                } else {
                    None
                }
            }
            Some(BolideType::Unowned(inner)) => {
                if let BolideType::Custom(name) = inner.as_ref() {
                    Some(name.clone())
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(class_name) = class_name {
            // 类成员访问
            if let Some(class_info) = self.classes.get(&class_name).cloned() {
                for field in &class_info.fields {
                    if field.name == member {
                        let offset = field.offset as i32;
                        let field_ty = self.bolide_type_to_cranelift(&field.ty);
                        let val =
                            self.builder
                                .ins()
                                .load(field_ty, MemFlags::new(), base_val, offset);
                        // 返回借用引用，不 clone（对齐 JIT）
                        // 调用方若需要独立所有权，compile_var_decl 会 clone
                        return Ok(val);
                    }
                }
                return Err(format!(
                    "Field '{}' not found in class '{}'",
                    member, class_name
                ));
            }
        }

        // 默认返回 0（用于未知类型）
        Ok(self.builder.ins().iconst(types::I64, 0))
    }

    /// 推断表达式类型
    fn infer_expr_type(&self, expr: &Expr) -> Option<BolideType> {
        match expr {
            Expr::Ident(name) => self
                .var_types
                .get(name)
                .cloned()
                .or_else(|| self.global_var_types.get(name).cloned())
                .or_else(|| {
                    // 裸函数名作为值：合成 FuncSig 类型（参数类型 + 返回类型）
                    if self.func_params.contains_key(name)
                        || self.func_return_types.contains_key(name)
                    {
                        let params = self
                            .func_params
                            .get(name)
                            .map(|ps| ps.iter().map(|p| p.ty.clone()).collect())
                            .unwrap_or_default();
                        let ret = self
                            .func_return_types
                            .get(name)
                            .cloned()
                            .flatten()
                            .map(Box::new);
                        Some(BolideType::FuncSig(params, ret))
                    } else {
                        None
                    }
                }),
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
            Expr::ListComprehension { .. } => Some(BolideType::List(Box::new(BolideType::Dynamic))),
            Expr::Dict(entries) => {
                let (k_type, v_type) = if entries.is_empty() {
                    (BolideType::Dynamic, BolideType::Dynamic)
                } else {
                    let mut k_ty = self
                        .infer_expr_type(&entries[0].0)
                        .unwrap_or(BolideType::Dynamic);
                    let mut v_ty = self
                        .infer_expr_type(&entries[0].1)
                        .unwrap_or(BolideType::Dynamic);
                    for (k, v) in entries.iter().skip(1) {
                        let next_k = self.infer_expr_type(k).unwrap_or(BolideType::Dynamic);
                        if k_ty != next_k {
                            k_ty = BolideType::Dynamic;
                        }
                        let next_v = self.infer_expr_type(v).unwrap_or(BolideType::Dynamic);
                        if v_ty != next_v {
                            v_ty = BolideType::Dynamic;
                        }
                    }
                    (k_ty, v_ty)
                };
                Some(BolideType::Dict(Box::new(k_type), Box::new(v_type)))
            }
            Expr::Tuple(exprs) => {
                let elem_types: Vec<BolideType> = exprs
                    .iter()
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
                    BolideType::Bytes => Some(BolideType::Int),
                    // 字符串索引按码点，返回单码点新串
                    BolideType::Str => Some(BolideType::Str),
                    _ => Some(BolideType::Dynamic),
                }
            }
            Expr::Slice(base, _, _, _) => {
                // 切片保持容器类型：Str->Str, List(e)->List(e), Tuple->Tuple
                match self.infer_expr_type(base) {
                    Some(BolideType::Str) => Some(BolideType::Str),
                    Some(BolideType::List(e)) => Some(BolideType::List(e)),
                    Some(BolideType::Tuple(t)) => Some(BolideType::Tuple(t)),
                    other => other,
                }
            }
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    match name.as_str() {
                        "bigint" => Some(BolideType::BigInt),
                        "decimal" => Some(BolideType::Decimal),
                        "int" => Some(BolideType::Int),
                        "float" => Some(BolideType::Float),
                        "str" => Some(BolideType::Str),
                        "bytes" => Some(BolideType::Bytes),
                        "input" => Some(BolideType::Str),
                        _ => {
                            // Check user-defined function return types
                            self.func_return_types
                                .get(name.as_str())
                                .cloned()
                                .flatten()
                                .or_else(|| {
                                    // Check function variables with FuncSig type (indirect calls)
                                    self.var_types.get(name.as_str()).and_then(|vt| match vt {
                                        BolideType::FuncSig(_, ret) => ret.clone().map(|b| *b),
                                        _ => None,
                                    })
                                })
                                .or_else(|| {
                                    // Check global function variables with FuncSig type (closures)
                                    self.global_var_types
                                        .get(name.as_str())
                                        .and_then(|vt| match vt {
                                            BolideType::FuncSig(_, ret) => ret.clone().map(|b| *b),
                                            _ => None,
                                        })
                                })
                                .or_else(|| {
                                    // Check extern (FFI) function return types
                                    self.extern_funcs.get(name.as_str()).and_then(|(_, ef)| {
                                        ef.return_type
                                            .as_ref()
                                            .map(Self::extern_return_type_to_bolide)
                                    })
                                })
                        }
                    }
                } else if let Expr::Member(base, method) = callee.as_ref() {
                    if let Expr::Ident(module_name) = base.as_ref() {
                        if self.modules.contains_key(module_name) {
                            let func_name = format!("@{}_{}", module_name, method);
                            return self.func_return_types.get(&func_name).cloned().flatten();
                        }
                    }

                    // 方法调用返回类型推断（List / Dict）
                    let base_ty = self.infer_expr_type(base);
                    match base_ty {
                        Some(BolideType::List(elem)) => match method.as_str() {
                            "pop" | "get" | "first" | "last" | "remove" => Some(*elem),
                            "slice" | "copy" | "clone" | "filter" => Some(BolideType::List(elem)),
                            // map 结果元素类型 = 回调返回类型（无则回退源类型）
                            "map" => {
                                let ret = args
                                    .first()
                                    .and_then(|cb| self.func_ptr_return_type(cb))
                                    .map(Box::new)
                                    .unwrap_or(elem);
                                Some(BolideType::List(ret))
                            }
                            "len" | "length" | "size" | "index_of" | "index" | "find" | "count"
                            | "is_empty" | "empty" => Some(BolideType::Int),
                            _ => Some(BolideType::Int),
                        },
                        Some(BolideType::Dict(k, v)) => match method.as_str() {
                            "keys" => Some(BolideType::List(k)),
                            "values" => Some(BolideType::List(v)),
                            "get" | "remove" => Some(*v),
                            "clone" => Some(BolideType::Dict(k, v)),
                            "len" | "is_empty" | "contains" => Some(BolideType::Int),
                            _ => Some(BolideType::Int),
                        },
                        // 字符串方法返回类型
                        Some(BolideType::Str) => match method.as_str() {
                            "upper" | "lower" | "trim" | "strip" | "replace" | "repeat"
                            | "substring" | "char_at" => Some(BolideType::Str),
                            "split" => Some(BolideType::List(Box::new(BolideType::Str))),
                            "find" | "index_of" | "contains" | "includes" | "starts_with"
                            | "ends_with" | "count" | "len" | "length" | "size" => {
                                Some(BolideType::Int)
                            }
                            _ => Some(BolideType::Int),
                        },
                        Some(BolideType::Bytes) => match method.as_str() {
                            "copy" | "clone" => Some(BolideType::Bytes),
                            "to_string_lossy" => Some(BolideType::Str),
                            _ => Some(BolideType::Int),
                        },
                        // 用户类方法：沿继承链查方法返回类型
                        Some(BolideType::Custom(class_name)) => {
                            self.lookup_method_return_type(&class_name, method)
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
            Expr::BinOp(left, op, right) => {
                let left_ty = self.infer_expr_type(left);
                let right_ty = self.infer_expr_type(right);
                match (&left_ty, &right_ty) {
                    (Some(BolideType::Str), Some(BolideType::Str)) => match op {
                        BinOp::Add => Some(BolideType::Str),
                        BinOp::Eq | BinOp::Ne => Some(BolideType::Bool),
                        _ => Some(BolideType::Int),
                    },
                    (Some(BolideType::BigInt), _) | (_, Some(BolideType::BigInt)) => match op {
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                            Some(BolideType::Bool)
                        }
                        _ => Some(BolideType::BigInt),
                    },
                    (Some(BolideType::Decimal), _) | (_, Some(BolideType::Decimal)) => match op {
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                            Some(BolideType::Bool)
                        }
                        _ => Some(BolideType::Decimal),
                    },
                    (Some(BolideType::Float), _) | (_, Some(BolideType::Float)) => {
                        Some(BolideType::Float)
                    }
                    _ => match op {
                        BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                        | BinOp::And
                        | BinOp::Or => Some(BolideType::Bool),
                        _ => Some(BolideType::Int),
                    },
                }
            }
            Expr::None => None,
            Expr::Member(base, member) => {
                // 获取基础表达式的类型，然后查找字段类型
                let base_ty = self.infer_expr_type(base)?;
                // 处理 Weak/Unowned 类型，提取内部的 Custom 类型
                let class_name = match &base_ty {
                    BolideType::Custom(name) => Some(name.clone()),
                    BolideType::Weak(inner) => {
                        if let BolideType::Custom(name) = inner.as_ref() {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }
                    BolideType::Unowned(inner) => {
                        if let BolideType::Custom(name) = inner.as_ref() {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(class_name) = class_name {
                    if let Some(class_info) = self.classes.get(&class_name) {
                        if let Some(field) = class_info.fields.iter().find(|f| f.name == *member) {
                            return Some(field.ty.clone());
                        }
                    }
                }
                None
            }
            Expr::Await(inner) => Some(self.infer_awaited_type(inner)),
            Expr::SpawnAll(exprs) => {
                let elem_types: Vec<BolideType> = exprs
                    .iter()
                    .map(|e| self.spawn_item_type(e).unwrap_or(BolideType::Int))
                    .collect();
                Some(BolideType::Tuple(elem_types))
            }
            Expr::Closure {
                params,
                return_type,
                ..
            } => Some(BolideType::FuncSig(
                params.iter().map(|p| p.ty.clone()).collect(),
                return_type.clone().map(Box::new),
            )),
            _ => None,
        }
    }

    /// 推断 await 表达式的类型
    fn infer_awaited_type(&self, expr: &Expr) -> BolideType {
        match expr {
            Expr::Call(callee, _) => {
                if let Expr::Ident(func_name) = callee.as_ref() {
                    return self
                        .func_return_types
                        .get(func_name)
                        .cloned()
                        .flatten()
                        .unwrap_or(BolideType::Int);
                }
                BolideType::Int
            }
            Expr::Ident(var_name) => {
                // 只有当当前作用域中该变量类型为 Future 时，才用 spawn_func_map
                // 推断返回类型，避免局部变量遮蔽全局同名变量时误判类型。
                if self
                    .var_types
                    .get(var_name)
                    .map(|t| matches!(t, BolideType::Future))
                    == Some(true)
                {
                    if let Some(func_name) = self.spawn_func_map.get(var_name) {
                        return self
                            .func_return_types
                            .get(func_name)
                            .cloned()
                            .flatten()
                            .unwrap_or(BolideType::Int);
                    }
                }
                BolideType::Int
            }
            Expr::Spawn(func_name, _) => self
                .func_return_types
                .get(func_name)
                .cloned()
                .flatten()
                .unwrap_or(BolideType::Int),
            Expr::Int(_) => BolideType::Int,
            Expr::Float(_) => BolideType::Float,
            Expr::String(_) => BolideType::Str,
            Expr::Bool(_) => BolideType::Bool,
            Expr::BigInt(_) => BolideType::BigInt,
            Expr::Decimal(_) => BolideType::Decimal,
            _ => self.infer_expr_type(expr).unwrap_or(BolideType::Int),
        }
    }

    /// 编译列表字面量
    fn compile_list(&mut self, items: &[Expr]) -> Result<Value, String> {
        self.compile_list_with_hint(items, None)
    }

    fn compile_list_with_hint(
        &mut self,
        items: &[Expr],
        hint: Option<&BolideType>,
    ) -> Result<Value, String> {
        // 确定元素类型：优先用标注，否则从元素推断（空列表默认 int）
        let elem_ty = if let Some(BolideType::List(inner)) = hint {
            inner.as_ref().clone()
        } else if items.is_empty() {
            BolideType::Int
        } else {
            self.infer_expr_type(&items[0]).unwrap_or(BolideType::Int)
        };
        let elem_tag = Self::bolide_type_to_element_tag(&elem_ty);

        let func_ref = *self
            .func_refs
            .get("@_list_new")
            .ok_or("list_new not found")?;
        let elem_type = self.builder.ins().iconst(types::I8, elem_tag as i64);
        let call = self.builder.ins().call(func_ref, &[elem_type]);
        let list_ptr = self.builder.inst_results(call)[0];

        let push_ref = *self
            .func_refs
            .get("@_list_push")
            .ok_or("list_push not found")?;
        for item in items {
            self.check_borrow_escape(item, "list literal")?;
            let mut val = self.compile_expr(item)?;
            // Float 元素：list_push 期望 i64 槽位，bitcast 保留位模式
            if matches!(self.infer_expr_type(item), Some(BolideType::Float)) {
                val = self.builder.ins().bitcast(types::I64, MemFlags::new(), val);
            }
            self.builder.ins().call(push_ref, &[list_ptr, val]);
        }

        self.track_temp_rc_value(list_ptr, &BolideType::List(Box::new(elem_ty)));
        Ok(list_ptr)
    }

    /// 编译 Tuple 字面量
    fn compile_tuple(&mut self, items: &[Expr]) -> Result<Value, String> {
        // 收集元素类型
        let mut elem_types = Vec::new();
        for expr in items {
            elem_types.push(self.infer_expr_type(expr).unwrap_or(BolideType::Int));
        }
        let tuple_type = BolideType::Tuple(elem_types.clone());

        let tag_bytes: Vec<u8> = elem_types
            .iter()
            .map(|ty| match ty {
                BolideType::Int => 0u8,
                BolideType::Float => 1,
                BolideType::Bool => 2,
                BolideType::Str => 3,
                BolideType::BigInt => 4,
                BolideType::Decimal => 5,
                BolideType::List(_) => 6,
                BolideType::Dict(_, _) => 8,
                BolideType::Dynamic => 9,
                BolideType::Bytes => 10,
                _ => 7,
            })
            .collect();

        // 栈上类型标签数组
        let tags_slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            tag_bytes.len() as u32,
            1,
        ));
        let tags_ptr = self.builder.ins().stack_addr(self.ptr_type, tags_slot, 0);
        for (i, &b) in tag_bytes.iter().enumerate() {
            let byte_val = self.builder.ins().iconst(types::I8, b as i64);
            let addr = self.builder.ins().iadd_imm(tags_ptr, i as i64);
            self.builder.ins().store(MemFlags::new(), byte_val, addr, 0);
        }

        // 类型感知创建元组
        let func_ref = *self
            .func_refs
            .get("@_tuple_new_typed")
            .ok_or("tuple_new_typed not found")?;
        let len = self.builder.ins().iconst(types::I64, items.len() as i64);
        let call = self.builder.ins().call(func_ref, &[len, tags_ptr]);
        let tuple_ptr = self.builder.inst_results(call)[0];

        let set_ref = *self
            .func_refs
            .get("@_tuple_set_typed")
            .ok_or("tuple_set_typed not found")?;
        for (i, item) in items.iter().enumerate() {
            self.check_borrow_escape(item, "tuple literal")?;
            let val = self.compile_expr(item)?;
            let ty = self.infer_expr_type(item).unwrap_or(BolideType::Int);
            let val_to_store = if Self::is_rc_type(&ty) {
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                if is_temp {
                    self.remove_temp_rc_value(val);
                    val
                } else {
                    self.emit_retain(val, &ty)
                }
            } else if matches!(ty, BolideType::Float) {
                self.builder.ins().bitcast(types::I64, MemFlags::new(), val)
            } else {
                val
            };
            let idx = self.builder.ins().iconst(types::I64, i as i64);
            let tag = self.builder.ins().iconst(types::I8, tag_bytes[i] as i64);
            self.builder
                .ins()
                .call(set_ref, &[tuple_ptr, idx, val_to_store, tag]);
        }

        self.track_temp_rc_value(tuple_ptr, &tuple_type);
        Ok(tuple_ptr)
    }

    /// 编译 Dict 字面量
    fn compile_dict(&mut self, entries: &[(Expr, Expr)]) -> Result<Value, String> {
        let (key_type_tag, val_type_tag, key_final_ty, val_final_ty) = if entries.is_empty() {
            (0u8, 0u8, BolideType::Int, BolideType::Int)
        } else {
            let mut k_final_ty = self
                .infer_expr_type(&entries[0].0)
                .unwrap_or(BolideType::Dynamic);
            let mut v_final_ty = self
                .infer_expr_type(&entries[0].1)
                .unwrap_or(BolideType::Dynamic);
            for (k, v) in entries.iter().skip(1) {
                let next_k = self.infer_expr_type(k).unwrap_or(BolideType::Dynamic);
                if k_final_ty != next_k {
                    k_final_ty = BolideType::Dynamic;
                }
                let next_v = self.infer_expr_type(v).unwrap_or(BolideType::Dynamic);
                if v_final_ty != next_v {
                    v_final_ty = BolideType::Dynamic;
                }
            }
            (
                Self::bolide_type_to_element_tag(&k_final_ty),
                Self::bolide_type_to_element_tag(&v_final_ty),
                k_final_ty,
                v_final_ty,
            )
        };

        let func_ref = *self
            .func_refs
            .get("@_dict_new")
            .ok_or("dict_new not found")?;
        let key_type = self.builder.ins().iconst(types::I8, key_type_tag as i64);
        let val_type = self.builder.ins().iconst(types::I8, val_type_tag as i64);
        let call = self.builder.ins().call(func_ref, &[key_type, val_type]);
        let dict_ptr = self.builder.inst_results(call)[0];

        let set_ref = *self
            .func_refs
            .get("@_dict_set")
            .ok_or("dict_set not found")?;
        for (key, value) in entries {
            self.check_borrow_escape(key, "dict literal")?;
            self.check_borrow_escape(value, "dict literal")?;
            let mut k = self.compile_expr(key)?;
            let mut v = self.compile_expr(value)?;
            if key_type_tag == 9 {
                let k_ty = self.infer_expr_type(key).unwrap_or(BolideType::Dynamic);
                if k_ty != BolideType::Dynamic {
                    k = self.convert_to_dynamic(k, &k_ty)?;
                }
            }
            if val_type_tag == 9 {
                let v_ty = self.infer_expr_type(value).unwrap_or(BolideType::Dynamic);
                if v_ty != BolideType::Dynamic {
                    v = self.convert_to_dynamic(v, &v_ty)?;
                }
            }
            self.builder.ins().call(set_ref, &[dict_ptr, k, v]);
        }

        self.track_temp_rc_value(
            dict_ptr,
            &BolideType::Dict(Box::new(key_final_ty), Box::new(val_final_ty)),
        );
        Ok(dict_ptr)
    }

    fn bolide_type_to_element_tag(ty: &BolideType) -> u8 {
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
            BolideType::Bytes => 10,
            _ => 0,
        }
    }

    /// 推断作为 map/filter 回调的函数表达式的返回类型。
    fn func_ptr_return_type(&self, expr: &Expr) -> Option<BolideType> {
        if let Expr::Ident(name) = expr {
            if let Some(Some(ret_ty)) = self.func_return_types.get(name) {
                return Some(ret_ty.clone());
            }
            if let Some(BolideType::FuncSig(_, Some(ret))) = self
                .var_types
                .get(name)
                .or_else(|| self.global_var_types.get(name))
            {
                return Some(ret.as_ref().clone());
            }
        }
        None
    }

    fn convert_to_dynamic(&mut self, val: Value, ty: &BolideType) -> Result<Value, String> {
        let func_name = match ty {
            BolideType::Int => "@_dynamic_from_int",
            BolideType::Float => "@_dynamic_from_float",
            BolideType::Bool => "@_dynamic_from_bool",
            BolideType::Str => "@_dynamic_from_string",
            BolideType::BigInt => "@_dynamic_from_bigint",
            BolideType::Decimal => "@_dynamic_from_decimal",
            BolideType::List(_) => "@_dynamic_from_list",
            BolideType::Bytes => "@_dynamic_from_bytes",
            BolideType::Dict(_, _) => "@_dynamic_from_dict",
            BolideType::Dynamic => return Ok(val),
            _ => return Err(format!("Cannot convert {:?} to dynamic", ty)),
        };
        let func = *self
            .func_refs
            .get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let boxed_input = if Self::is_rc_type(ty) {
            let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
            if is_temp {
                self.remove_temp_rc_value(val);
                val
            } else {
                self.emit_retain(val, ty)
            }
        } else {
            val
        };
        let call = self.builder.ins().call(func, &[boxed_input]);
        let res = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(res, &BolideType::Dynamic);
        Ok(res)
    }

    fn prepare_value_for_storage(
        &mut self,
        val: Value,
        actual_ty: &BolideType,
        target_ty: &BolideType,
    ) -> Result<Value, String> {
        if matches!(target_ty, BolideType::Dynamic) && !matches!(actual_ty, BolideType::Dynamic) {
            return self.convert_to_dynamic(val, actual_ty);
        }

        Ok(val)
    }

    /// 根据函数返回类型确定 spawn/join 的类型后缀（_int/_float/_ptr）
    fn spawn_type_suffix(ret: &Option<BolideType>) -> &'static str {
        match ret {
            Some(BolideType::Float) => "_float",
            Some(BolideType::Str)
            | Some(BolideType::BigInt)
            | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic)
            | Some(BolideType::Ptr)
            | Some(BolideType::List(_))
            | Some(BolideType::Tuple(_))
            | Some(BolideType::Custom(_)) => "_ptr",
            _ => "_int",
        }
    }

    /// 编译 Spawn 表达式（与 JIT 一致：运行时判断 pool/thread 分支，按返回类型选后缀）
    fn compile_spawn(&mut self, name: &str, args: &[Expr]) -> Result<Value, String> {
        let prepared_args = self.prepare_call_args(name, args)?;
        let param_types: Vec<BolideType> = self
            .func_params
            .get(name)
            .map(|params| params.iter().map(|p| p.ty.clone()).collect())
            .unwrap_or_default();
        let return_type = self.func_return_types.get(name).cloned().flatten();
        let type_suffix = Self::spawn_type_suffix(&return_type);

        // 计算函数地址与 env 指针
        let (func_addr, env_ptr) = if prepared_args.is_empty() {
            let target_ref = *self
                .func_refs
                .get(name)
                .ok_or_else(|| format!("Undefined function: {}", name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, target_ref);
            let null_env = self.builder.ins().iconst(self.ptr_type, 0);
            (func_addr, null_env)
        } else {
            // 有参数：分配 env，存入参数，使用 trampoline
            let env_size = (prepared_args.len() * 8) as i64;
            let alloc_ref = *self
                .func_refs
                .get("@_bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(call)[0];

            for (i, arg) in prepared_args.iter().enumerate() {
                let raw_val = self.compile_prepared_arg(arg)?;
                let val =
                    if let (Some(target_ty), PreparedArg::Expr(expr)) = (param_types.get(i), arg) {
                        let actual_ty = self
                            .infer_expr_type(expr)
                            .map(|ty| self.normalize_bolide_type(&ty))
                            .unwrap_or(BolideType::Dynamic);
                        self.prepare_value_for_storage(raw_val, &actual_ty, target_ty)?
                    } else {
                        raw_val
                    };
                let offset = (i * 8) as i32;
                // 对 RC 类型参数 clone 一份交给子线程（跨线程生命周期安全 + 值语义）
                let arg_ty = param_types.get(i);
                let val_to_store = match arg_ty.and_then(Self::get_clone_func_name) {
                    Some(clone_func) => {
                        if let Some(&clone_ref) = self.func_refs.get(clone_func) {
                            let call = self.builder.ins().call(clone_ref, &[val]);
                            self.builder.inst_results(call)[0]
                        } else {
                            val
                        }
                    }
                    None => val,
                };
                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val_to_store, env_ptr, offset);
            }

            let trampoline_name = self.get_trampoline_name(name);
            let trampoline_ref = *self
                .func_refs
                .get(&trampoline_name)
                .ok_or_else(|| format!("Trampoline not found: {}", trampoline_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, trampoline_ref);
            (func_addr, env_ptr)
        };

        // 运行时判断是否处于线程池上下文
        let pool_is_active_ref = *self
            .func_refs
            .get("@_pool_is_active")
            .ok_or("pool_is_active not found")?;
        let is_active_call = self.builder.ins().call(pool_is_active_ref, &[]);
        let is_active = self.builder.inst_results(is_active_call)[0];

        let pool_block = self.builder.create_block();
        let thread_block = self.builder.create_block();
        let merge_block = self.builder.create_block();
        self.builder.append_block_param(merge_block, self.ptr_type);

        self.builder
            .ins()
            .brif(is_active, pool_block, &[], thread_block, &[]);

        let spawn_suffix = if prepared_args.is_empty() {
            type_suffix.to_string()
        } else {
            format!("{}_with_env", type_suffix)
        };

        // 线程池分支
        self.builder.switch_to_block(pool_block);
        self.builder.seal_block(pool_block);
        let pool_spawn_name = format!("@_pool_spawn{}", spawn_suffix);
        let pool_spawn_ref = *self
            .func_refs
            .get(&pool_spawn_name)
            .ok_or_else(|| format!("{} not found", pool_spawn_name))?;
        let pool_call = if prepared_args.is_empty() {
            self.builder.ins().call(pool_spawn_ref, &[func_addr])
        } else {
            self.builder
                .ins()
                .call(pool_spawn_ref, &[func_addr, env_ptr])
        };
        let pool_handle = self.builder.inst_results(pool_call)[0];
        self.builder.ins().jump(merge_block, &[pool_handle]);

        // 普通线程分支
        self.builder.switch_to_block(thread_block);
        self.builder.seal_block(thread_block);
        let thread_spawn_name = format!("@_thread_spawn{}", spawn_suffix);
        let thread_spawn_ref = *self
            .func_refs
            .get(&thread_spawn_name)
            .ok_or_else(|| format!("{} not found", thread_spawn_name))?;
        let thread_call = if prepared_args.is_empty() {
            self.builder.ins().call(thread_spawn_ref, &[func_addr])
        } else {
            self.builder
                .ins()
                .call(thread_spawn_ref, &[func_addr, env_ptr])
        };
        let thread_handle = self.builder.inst_results(thread_call)[0];
        self.builder.ins().jump(merge_block, &[thread_handle]);

        // 合并块
        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);
        Ok(self.builder.block_params(merge_block)[0])
    }

    fn tuple_type_tag(ty: &BolideType) -> u8 {
        match ty {
            BolideType::Int => 0,
            BolideType::Float => 1,
            BolideType::Bool => 2,
            BolideType::Str => 3,
            BolideType::BigInt => 4,
            BolideType::Decimal => 5,
            BolideType::List(_) => 6,
            BolideType::Dict(_, _) => 8,
            BolideType::Dynamic => 9,
            BolideType::Bytes => 10,
            _ => 7,
        }
    }

    fn spawn_call_parts<'c>(&self, expr: &'c Expr) -> Result<(&'c str, &'c [Expr]), String> {
        match expr {
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    Ok((name.as_str(), args.as_slice()))
                } else {
                    Err("spawn all/select only supports direct function calls".to_string())
                }
            }
            Expr::Spawn(name, args) => Ok((name.as_str(), args.as_slice())),
            _ => Err("spawn all/select expects tasks like foo(...)".to_string()),
        }
    }

    fn spawn_item_type(&self, expr: &Expr) -> Result<BolideType, String> {
        let (func_name, _) = self.spawn_call_parts(expr)?;
        Ok(self
            .func_return_types
            .get(func_name)
            .cloned()
            .flatten()
            .unwrap_or(BolideType::Int))
    }

    fn compile_pool_spawn(&mut self, name: &str, args: &[Expr]) -> Result<Value, String> {
        let prepared_args = self.prepare_call_args(name, args)?;
        let param_types: Vec<BolideType> = self
            .func_params
            .get(name)
            .map(|params| params.iter().map(|p| p.ty.clone()).collect())
            .unwrap_or_default();
        let return_type = self
            .func_return_types
            .get(name)
            .cloned()
            .flatten()
            .unwrap_or(BolideType::Int);
        let type_suffix = Self::spawn_type_suffix(&Some(return_type));

        let (func_addr, env_ptr) = if prepared_args.is_empty() {
            let target_ref = *self
                .func_refs
                .get(name)
                .ok_or_else(|| format!("Undefined function: {}", name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, target_ref);
            let null_env = self.builder.ins().iconst(self.ptr_type, 0);
            (func_addr, null_env)
        } else {
            let env_size = (prepared_args.len() * 8) as i64;
            let alloc_ref = *self
                .func_refs
                .get("@_bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(call)[0];

            for (i, arg) in prepared_args.iter().enumerate() {
                let raw_val = self.compile_prepared_arg(arg)?;
                let val =
                    if let (Some(target_ty), PreparedArg::Expr(expr)) = (param_types.get(i), arg) {
                        let actual_ty = self
                            .infer_expr_type(expr)
                            .map(|ty| self.normalize_bolide_type(&ty))
                            .unwrap_or(BolideType::Dynamic);
                        self.prepare_value_for_storage(raw_val, &actual_ty, target_ty)?
                    } else {
                        raw_val
                    };
                let offset = (i * 8) as i32;
                let arg_ty = param_types.get(i);
                let val_to_store = match arg_ty.and_then(Self::get_clone_func_name) {
                    Some(clone_func) => {
                        if let Some(&clone_ref) = self.func_refs.get(clone_func) {
                            let call = self.builder.ins().call(clone_ref, &[val]);
                            self.builder.inst_results(call)[0]
                        } else {
                            val
                        }
                    }
                    None => val,
                };
                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val_to_store, env_ptr, offset);
            }

            let trampoline_name = self.get_trampoline_name(name);
            let trampoline_ref = *self
                .func_refs
                .get(&trampoline_name)
                .ok_or_else(|| format!("Trampoline not found: {}", trampoline_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, trampoline_ref);
            (func_addr, env_ptr)
        };

        let spawn_suffix = if prepared_args.is_empty() {
            type_suffix.to_string()
        } else {
            format!("{}_with_env", type_suffix)
        };
        let pool_spawn_name = format!("@_pool_spawn{}", spawn_suffix);
        let pool_spawn_ref = *self
            .func_refs
            .get(&pool_spawn_name)
            .ok_or_else(|| format!("{} not found", pool_spawn_name))?;
        let call = if prepared_args.is_empty() {
            self.builder.ins().call(pool_spawn_ref, &[func_addr])
        } else {
            self.builder
                .ins()
                .call(pool_spawn_ref, &[func_addr, env_ptr])
        };
        Ok(self.builder.inst_results(call)[0])
    }

    fn compile_pool_join_handle(
        &mut self,
        handle: Value,
        ret_ty: &BolideType,
        keep_result: bool,
    ) -> Result<Value, String> {
        let join_name = format!(
            "@_pool_join{}",
            Self::spawn_type_suffix(&Some(ret_ty.clone()))
        );
        let join_ref = *self
            .func_refs
            .get(&join_name)
            .ok_or_else(|| format!("{} not found", join_name))?;
        let call = self.builder.ins().call(join_ref, &[handle]);
        let result = self.builder.inst_results(call)[0];
        let free_ref = *self
            .func_refs
            .get("@_pool_handle_free")
            .ok_or("pool_handle_free not found")?;
        self.builder.ins().call(free_ref, &[handle]);

        if keep_result {
            self.track_temp_rc_value(result, ret_ty);
        } else if Self::is_rc_type(ret_ty) {
            self.emit_release(result, ret_ty);
        }

        Ok(result)
    }

    fn compile_tuple_from_values(
        &mut self,
        values: &[Value],
        elem_types: &[BolideType],
    ) -> Result<Value, String> {
        if values.is_empty() {
            return Ok(self.builder.ins().iconst(types::I64, 0));
        }
        if values.len() == 1 {
            return Ok(values[0]);
        }

        let tuple_type = BolideType::Tuple(elem_types.to_vec());
        let tag_bytes: Vec<u8> = elem_types.iter().map(Self::tuple_type_tag).collect();
        let tags_slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            tag_bytes.len() as u32,
            1,
        ));
        let tags_ptr = self.builder.ins().stack_addr(self.ptr_type, tags_slot, 0);
        for (i, &tag) in tag_bytes.iter().enumerate() {
            let byte_val = self.builder.ins().iconst(types::I8, tag as i64);
            let addr = self.builder.ins().iadd_imm(tags_ptr, i as i64);
            self.builder.ins().store(MemFlags::new(), byte_val, addr, 0);
        }

        let tuple_new = *self
            .func_refs
            .get("@_tuple_new_typed")
            .ok_or("tuple_new_typed not found")?;
        let len = self.builder.ins().iconst(types::I64, values.len() as i64);
        let call = self.builder.ins().call(tuple_new, &[len, tags_ptr]);
        let tuple_ptr = self.builder.inst_results(call)[0];

        let tuple_set = *self
            .func_refs
            .get("@_tuple_set_typed")
            .ok_or("tuple_set_typed not found")?;
        for (i, value) in values.iter().enumerate() {
            let ty = &elem_types[i];
            let val_to_store = if Self::is_rc_type(ty) {
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == *value);
                if is_temp {
                    self.remove_temp_rc_value(*value);
                    *value
                } else if let Some(clone_func) = Self::get_clone_func_name(ty) {
                    if let Some(&clone_ref) = self.func_refs.get(clone_func) {
                        let call = self.builder.ins().call(clone_ref, &[*value]);
                        self.builder.inst_results(call)[0]
                    } else {
                        *value
                    }
                } else {
                    *value
                }
            } else if matches!(ty, BolideType::Float) {
                self.builder
                    .ins()
                    .bitcast(types::I64, MemFlags::new(), *value)
            } else {
                *value
            };
            let idx = self.builder.ins().iconst(types::I64, i as i64);
            let tag = self.builder.ins().iconst(types::I8, tag_bytes[i] as i64);
            self.builder
                .ins()
                .call(tuple_set, &[tuple_ptr, idx, val_to_store, tag]);
        }

        self.track_temp_rc_value(tuple_ptr, &tuple_type);
        Ok(tuple_ptr)
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
        let await_expr = Expr::Await(Box::new(inner.clone()));
        let expr_type = self.infer_expr_type(&await_expr).unwrap_or(BolideType::Int);

        let await_func_name = match &expr_type {
            BolideType::Float => "@_coroutine_await_float",
            BolideType::Str
            | BolideType::BigInt
            | BolideType::Decimal
            | BolideType::List(_)
            | BolideType::Tuple(_)
            | BolideType::Custom(_) => "@_coroutine_await_ptr",
            _ => "@_coroutine_await_int",
        };

        let func_ref = *self
            .func_refs
            .get(await_func_name)
            .ok_or_else(|| format!("{} not found", await_func_name))?;
        let call = self.builder.ins().call(func_ref, &[future]);
        let result = self.builder.inst_results(call)[0];

        // 释放 Future
        if let Some(&free_ref) = self.func_refs.get("@_coroutine_free") {
            self.builder.ins().call(free_ref, &[future]);
        }

        self.track_temp_rc_value(result, &expr_type);
        Ok(result)
    }

    /// 编译 Recv 表达式 (从通道接收)
    fn compile_recv_channel(&mut self, channel_name: &str) -> Result<Value, String> {
        // 获取通道变量（局部或全局）
        let ch = if let Some(&var) = self.variables.get(channel_name) {
            self.builder.use_var(var)
        } else if let Some(&gv) = self.global_refs.get(channel_name) {
            let addr = self.builder.ins().global_value(self.ptr_type, gv);
            self.builder
                .ins()
                .load(self.ptr_type, MemFlags::new(), addr, 0)
        } else {
            return Err(format!("Channel not found: {}", channel_name));
        };
        let func_ref = *self
            .func_refs
            .get("@_channel_recv")
            .ok_or("channel_recv not found")?;
        let call = self.builder.ins().call(func_ref, &[ch]);
        let value = self.builder.inst_results(call)[0];

        // 如果通道元素类型是 RC 类型，追踪接收的值
        let inner_ty = if let Some(ch_ty) = self.var_types.get(channel_name) {
            if let BolideType::Channel(inner) = ch_ty {
                if Self::is_rc_type(inner) {
                    Some(inner.as_ref().clone())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(inner) = inner_ty {
            self.track_temp_rc_value(value, &inner);
        }

        Ok(value)
    }

    /// 编译 SpawnAll 表达式
    fn compile_spawn_all(&mut self, exprs: &[Expr]) -> Result<Value, String> {
        let mut handles = Vec::new();
        let mut result_types = Vec::new();
        for expr in exprs {
            let (func_name, args) = self.spawn_call_parts(expr)?;
            let ret_ty = self.spawn_item_type(expr)?;
            let handle = self.compile_pool_spawn(func_name, args)?;
            handles.push(handle);
            result_types.push(ret_ty);
        }

        let mut results = Vec::new();
        for (handle, ret_ty) in handles.iter().zip(result_types.iter()) {
            let result = self.compile_pool_join_handle(*handle, ret_ty, true)?;
            results.push(result);
        }

        self.compile_tuple_from_values(&results, &result_types)
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
            Statement::Break => {
                let (_, break_block, scope_base) =
                    *self.loop_stack.last().ok_or("'break' outside of a loop")?;
                // 跳出前释放临时值与循环作用域内已声明的 RC 变量
                self.release_temp_rc_values();
                self.emit_scope_releases_from(scope_base);
                self.builder.ins().jump(break_block, &[]);
                true
            }
            Statement::Continue => {
                let (continue_block, _, scope_base) = *self
                    .loop_stack
                    .last()
                    .ok_or("'continue' outside of a loop")?;
                self.release_temp_rc_values();
                self.emit_scope_releases_from(scope_base);
                self.builder.ins().jump(continue_block, &[]);
                true
            }
            Statement::Throw(expr) => {
                // 计算异常值与类型标签，存入 thread-local，然后跳转到最近的 catch 落点。
                // 无 setjmp/longjmp：异常值经内存传递，控制流是普通分支，SSA 安全。
                let throw_ty = self.infer_expr_type(expr).unwrap_or(BolideType::Int);
                let tag = self.type_to_throw_tag(&throw_ty);
                let val = self.compile_expr(expr)?;
                // 抛出的 RC 临时值所有权转移给异常通道，避免语句末提前释放
                self.remove_temp_rc_value(val);
                let tag_val = self.builder.ins().iconst(types::I64, tag);
                let set_fn = *self
                    .func_refs
                    .get("@_exception_set")
                    .ok_or("exception_set not found")?;
                self.builder.ins().call(set_fn, &[val, tag_val]);

                if let Some(&catch_block) = self.catch_stack.last() {
                    self.builder.ins().jump(catch_block, &[]);
                } else {
                    let uncaught_fn = *self
                        .func_refs
                        .get("@_throw_uncaught")
                        .ok_or("throw_uncaught not found")?;
                    self.builder.ins().call(uncaught_fn, &[val]);
                    self.builder.ins().trap(TrapCode::unwrap_user(1));
                }
                true
            }
            Statement::Try(try_stmt) => self.compile_try(try_stmt)?,
            Statement::Match(match_stmt) => self.compile_match(match_stmt)?,
            Statement::Import(_)
            | Statement::ExternBlock(_)
            | Statement::FuncDef(_)
            | Statement::ClassDef(_)
            | Statement::EnumDef(_) => {
                // 这些语句在顶层处理，函数体内忽略（throw/try 稍后实现）
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
            Statement::SpawnSelect(spawn_select) => {
                self.compile_spawn_select(spawn_select)?;
                false
            }
        };

        if !is_terminator {
            // Release temporary values created by this statement if it didn't terminate
            self.release_temp_rc_values();
        }

        Ok(is_terminator)
    }

    /// 编译 try/catch/finally。返回 true 表示所有路径都发散。
    /// 与 JIT compile_try 语义一致（标签匹配分派 + finally 复制）。
    fn compile_try(&mut self, try_stmt: &bolide_parser::TryStmt) -> Result<bool, String> {
        let catch_clauses = try_stmt.catch_clauses.clone();
        let try_body = try_stmt.try_body.clone();
        let finally_body = try_stmt.finally.clone();
        let ptr_type = self.ptr_type;

        let catch_block = self.builder.create_block();
        let after_try = self.builder.create_block();

        // 1. Try body
        self.catch_stack.push(catch_block);
        let mut try_diverted = false;
        for s in &try_body {
            if try_diverted {
                break;
            }
            try_diverted = self.compile_stmt(s)?;
        }
        self.catch_stack.pop();
        if !try_diverted {
            self.emit_finally(&finally_body)?;
            self.builder.ins().jump(after_try, &[]);
        }

        // 2. Catch block
        self.builder.switch_to_block(catch_block);
        self.builder.seal_block(catch_block);

        let tag_fn = *self
            .func_refs
            .get("@_exception_tag")
            .ok_or("exception_tag not found")?;
        let tag_call = self.builder.ins().call(tag_fn, &[]);
        let cur_tag = self.builder.inst_results(tag_call)[0];

        let ex_get_fn = *self
            .func_refs
            .get("@_exception_get")
            .ok_or("exception_get not found")?;
        let ex_call = self.builder.ins().call(ex_get_fn, &[]);
        let ex_ptr = self.builder.inst_results(ex_call)[0];

        let mut all_catch_diverted = true;

        for clause in &catch_clauses {
            let match_tags = self.catch_match_tags(&clause.ty);
            let body_block = self.builder.create_block();
            let next_block = self.builder.create_block();

            if match_tags.is_empty() {
                self.builder.ins().jump(next_block, &[]);
            } else {
                let mut matched = self.builder.ins().iconst(types::I8, 0);
                for t in match_tags {
                    let tval = self.builder.ins().iconst(types::I64, t);
                    let eq = self.builder.ins().icmp(IntCC::Equal, cur_tag, tval);
                    matched = self.builder.ins().bor(matched, eq);
                }
                self.builder
                    .ins()
                    .brif(matched, body_block, &[], next_block, &[]);
            }

            self.builder.switch_to_block(body_block);
            self.builder.seal_block(body_block);
            let catch_var = self.declare_variable(&clause.var, ptr_type);
            self.builder.def_var(catch_var, ex_ptr);
            self.var_types.insert(clause.var.clone(), clause.ty.clone());

            let mut clause_diverted = false;
            for s in &clause.body {
                if clause_diverted {
                    break;
                }
                clause_diverted = self.compile_stmt(s)?;
            }
            if !clause_diverted {
                self.emit_finally(&finally_body)?;
                self.builder.ins().jump(after_try, &[]);
                all_catch_diverted = false;
            }

            self.builder.switch_to_block(next_block);
            self.builder.seal_block(next_block);
        }

        // 所有 catch 都不匹配：重抛（先执行 finally）
        self.emit_finally(&finally_body)?;
        if let Some(&outer_catch) = self.catch_stack.last() {
            let set_fn = *self
                .func_refs
                .get("@_exception_set")
                .ok_or("exception_set not found")?;
            self.builder.ins().call(set_fn, &[ex_ptr, cur_tag]);
            self.builder.ins().jump(outer_catch, &[]);
        } else {
            let uncaught_fn = *self
                .func_refs
                .get("@_throw_uncaught")
                .ok_or("throw_uncaught not found")?;
            self.builder.ins().call(uncaught_fn, &[ex_ptr]);
            self.builder.ins().trap(TrapCode::unwrap_user(1));
        }

        let both_diverged = try_diverted && all_catch_diverted;
        self.builder.switch_to_block(after_try);
        self.builder.seal_block(after_try);
        if both_diverged {
            self.builder.ins().trap(TrapCode::unwrap_user(1));
        }
        Ok(both_diverged)
    }

    fn compile_match(&mut self, match_stmt: &bolide_parser::MatchStmt) -> Result<bool, String> {
        let scrutinee_ty = self
            .infer_expr_type(&match_stmt.expr)
            .unwrap_or(BolideType::Dynamic);
        let scrutinee_val = self.compile_expr(&match_stmt.expr)?;
        match scrutinee_ty {
            BolideType::Adt(ref adt_name, ref type_args) => {
                self.compile_adt_match(scrutinee_val, adt_name, type_args, &match_stmt.arms)
            }
            other => Err(format!(
                "match currently supports enum/union values, got {:?}",
                other
            )),
        }
    }

    fn compile_adt_match(
        &mut self,
        scrutinee_val: Value,
        adt_name: &str,
        type_args: &[BolideType],
        arms: &[bolide_parser::MatchArm],
    ) -> Result<bool, String> {
        let adt_info = self
            .adts
            .get(adt_name)
            .ok_or_else(|| format!("Unknown enum/union '{}'", adt_name))?
            .clone();
        self.validate_adt_match_exhaustive(&adt_info, arms)?;

        let tag_val = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), scrutinee_val, 0);
        let after_block = self.builder.create_block();
        let type_map = Self::adt_type_map(&adt_info, type_args);
        let mut all_diverted = true;
        let mut saw_catch_all = false;

        for arm in arms {
            if saw_catch_all {
                return Err("match arm after wildcard/binding arm is unreachable".to_string());
            }

            match &arm.pattern {
                bolide_parser::Pattern::Wildcard => {
                    saw_catch_all = true;
                    let diverted = self.compile_match_arm_body(&arm.body)?;
                    if !diverted {
                        self.builder.ins().jump(after_block, &[]);
                        all_diverted = false;
                    }
                }
                bolide_parser::Pattern::Bind(name) => {
                    saw_catch_all = true;
                    self.bind_match_value(
                        name,
                        scrutinee_val,
                        &BolideType::Adt(adt_name.to_string(), type_args.to_vec()),
                    )?;
                    let diverted = self.compile_match_arm_body(&arm.body)?;
                    if !diverted {
                        self.builder.ins().jump(after_block, &[]);
                        all_diverted = false;
                    }
                }
                bolide_parser::Pattern::Variant {
                    enum_name,
                    variant,
                    fields,
                } => {
                    if let Some(pattern_adt) = enum_name {
                        if pattern_adt != adt_name {
                            return Err(format!(
                                "match arm uses '{}.{}' for value of '{}'",
                                pattern_adt, variant, adt_name
                            ));
                        }
                    }
                    let variant_info = adt_info
                        .variants
                        .iter()
                        .find(|v| v.name == *variant)
                        .ok_or_else(|| format!("Unknown variant '{}.{}'", adt_name, variant))?
                        .clone();
                    if fields.len() != variant_info.fields.len() {
                        return Err(format!(
                            "{}.{} pattern expects {} field(s), got {}",
                            adt_name,
                            variant,
                            variant_info.fields.len(),
                            fields.len()
                        ));
                    }

                    let body_block = self.builder.create_block();
                    let next_block = self.builder.create_block();
                    let expected = self.builder.ins().iconst(types::I64, variant_info.tag);
                    let matched = self.builder.ins().icmp(IntCC::Equal, tag_val, expected);
                    self.builder
                        .ins()
                        .brif(matched, body_block, &[], next_block, &[]);

                    self.builder.switch_to_block(body_block);
                    self.builder.seal_block(body_block);
                    self.bind_adt_variant_pattern_fields(
                        scrutinee_val,
                        &variant_info,
                        fields,
                        &type_map,
                    )?;
                    let diverted = self.compile_match_arm_body(&arm.body)?;
                    if !diverted {
                        self.builder.ins().jump(after_block, &[]);
                        all_diverted = false;
                    }

                    self.builder.switch_to_block(next_block);
                    self.builder.seal_block(next_block);
                }
                other => {
                    return Err(format!(
                        "Unsupported ADT match pattern {:?}; use Variant(...), _, or a binding",
                        other
                    ));
                }
            }
        }

        if !saw_catch_all {
            self.builder.ins().trap(TrapCode::unwrap_user(2));
        }

        self.builder.switch_to_block(after_block);
        self.builder.seal_block(after_block);
        if all_diverted {
            self.builder.ins().trap(TrapCode::unwrap_user(2));
        }
        Ok(all_diverted)
    }

    fn validate_adt_match_exhaustive(
        &self,
        adt_info: &AdtInfo,
        arms: &[bolide_parser::MatchArm],
    ) -> Result<(), String> {
        let mut covered = HashSet::new();
        for arm in arms {
            match &arm.pattern {
                bolide_parser::Pattern::Wildcard | bolide_parser::Pattern::Bind(_) => {
                    return Ok(());
                }
                bolide_parser::Pattern::Variant {
                    enum_name, variant, ..
                } => {
                    if enum_name.as_ref().map(|name| name.as_str()) == Some(adt_info.name.as_str())
                        || enum_name.is_none()
                    {
                        covered.insert(variant.clone());
                    }
                }
                _ => {}
            }
        }
        let missing: Vec<String> = adt_info
            .variants
            .iter()
            .filter(|variant| !covered.contains(&variant.name))
            .map(|variant| variant.name.clone())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "non-exhaustive match for '{}'; missing {}",
                adt_info.name,
                missing.join(", ")
            ))
        }
    }

    fn compile_match_arm_body(&mut self, body: &[Statement]) -> Result<bool, String> {
        let mut diverted = false;
        for stmt in body {
            if diverted {
                break;
            }
            diverted = self.compile_stmt(stmt)?;
        }
        Ok(diverted)
    }

    fn bind_adt_variant_pattern_fields(
        &mut self,
        scrutinee_val: Value,
        variant_info: &AdtVariantInfo,
        patterns: &[bolide_parser::Pattern],
        type_map: &HashMap<String, BolideType>,
    ) -> Result<(), String> {
        for (idx, pattern) in patterns.iter().enumerate() {
            let field = &variant_info.fields[idx];
            match pattern {
                bolide_parser::Pattern::Wildcard => {}
                bolide_parser::Pattern::Bind(name) => {
                    let field_ty = Self::substitute_type(&field.ty, type_map);
                    let cl_ty = self.bolide_type_to_cranelift(&field_ty);
                    let field_ptr = self
                        .builder
                        .ins()
                        .iadd_imm(scrutinee_val, field.offset as i64);
                    let field_val = self
                        .builder
                        .ins()
                        .load(cl_ty, MemFlags::new(), field_ptr, 0);
                    self.bind_match_value(name, field_val, &field_ty)?;
                }
                other => {
                    return Err(format!(
                        "nested match pattern {:?} is not supported in ADT fields yet",
                        other
                    ));
                }
            }
        }
        Ok(())
    }

    fn bind_match_value(
        &mut self,
        name: &str,
        value: Value,
        ty: &BolideType,
    ) -> Result<(), String> {
        let c_ty = self.bolide_type_to_cranelift(ty);
        let var = self.declare_variable(name, c_ty);
        let mut bind_val = value;
        if Self::is_rc_type(ty) {
            if let Some(func_name) = Self::get_clone_func_name(ty) {
                if let Some(&func_ref) = self.func_refs.get(func_name) {
                    let call = self.builder.ins().call(func_ref, &[value]);
                    bind_val = self.builder.inst_results(call)[0];
                }
            }
        }
        self.builder.def_var(var, bind_val);
        self.var_types.insert(name.to_string(), ty.clone());
        self.track_rc_variable(name, ty);
        Ok(())
    }

    /// 内联编译 finally body（finally 复制做法）。
    fn emit_finally(&mut self, finally_body: &Option<Vec<Statement>>) -> Result<(), String> {
        if let Some(body) = finally_body {
            for s in body {
                let diverted = self.compile_stmt(s)?;
                if diverted {
                    break;
                }
            }
        }
        Ok(())
    }

    /// 编译 Send 语句
    fn compile_send(&mut self, send_stmt: &bolide_parser::SendStmt) -> Result<(), String> {
        let ch = if let Some(&var) = self.variables.get(&send_stmt.channel) {
            self.builder.use_var(var)
        } else if let Some(&gv) = self.global_refs.get(&send_stmt.channel) {
            let addr = self.builder.ins().global_value(self.ptr_type, gv);
            self.builder
                .ins()
                .load(self.ptr_type, MemFlags::new(), addr, 0)
        } else {
            return Err(format!("Channel not found: {}", send_stmt.channel));
        };
        self.check_borrow_escape(&send_stmt.value, "channel send")?;
        let val = self.compile_expr(&send_stmt.value)?;

        // 如果通道元素类型是 RC 类型，先 retain 再发送
        let send_val = if let Some(ch_ty) = self.var_types.get(&send_stmt.channel) {
            if let BolideType::Channel(inner) = ch_ty {
                if Self::is_rc_type(inner) {
                    if let Some(clone_func) = Self::get_clone_func_name(inner) {
                        if let Some(&func_ref) = self.func_refs.get(clone_func) {
                            let call = self.builder.ins().call(func_ref, &[val]);
                            self.builder.inst_results(call)[0]
                        } else {
                            val
                        }
                    } else {
                        val
                    }
                } else {
                    val
                }
            } else {
                val
            }
        } else {
            val
        };

        let func_ref = *self
            .func_refs
            .get("@_channel_send")
            .ok_or("channel_send not found")?;
        self.builder.ins().call(func_ref, &[ch, send_val]);
        Ok(())
    }

    /// 编译 Pool 语句
    fn compile_pool(&mut self, pool_stmt: &bolide_parser::PoolStmt) -> Result<(), String> {
        let size = self.compile_expr(&pool_stmt.size)?;

        // 创建线程池
        let pool_create_ref = *self
            .func_refs
            .get("@_pool_create")
            .ok_or("pool_create not found")?;
        let call = self.builder.ins().call(pool_create_ref, &[size]);
        let pool_ptr = self.builder.inst_results(call)[0];

        // 进入线程池上下文
        let pool_enter_ref = *self
            .func_refs
            .get("@_pool_enter")
            .ok_or("pool_enter not found")?;
        self.builder.ins().call(pool_enter_ref, &[pool_ptr]);

        // 编译 pool 块内的语句
        for stmt in &pool_stmt.body {
            self.compile_stmt(stmt)?;
        }

        // 退出线程池上下文
        let pool_exit_ref = *self
            .func_refs
            .get("@_pool_exit")
            .ok_or("pool_exit not found")?;
        self.builder.ins().call(pool_exit_ref, &[]);

        // 销毁线程池
        let pool_destroy_ref = *self
            .func_refs
            .get("@_pool_destroy")
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
        let alloc_ref = *self
            .func_refs
            .get("@_bolide_alloc")
            .ok_or("bolide_alloc not found")?;
        let size_val = self.builder.ins().iconst(types::I64, array_size);
        let call = self.builder.ins().call(alloc_ref, &[size_val]);
        let array_ptr = self.builder.inst_results(call)[0];

        // 填充 channel 数组（支持全局变量）
        for (i, (_, channel_name, _)) in recv_branches.iter().enumerate() {
            let ch_ptr = if let Some(&var) = self.variables.get(*channel_name) {
                self.builder.use_var(var)
            } else if let Some(&gv) = self.global_refs.get(*channel_name) {
                let addr = self.builder.ins().global_value(self.ptr_type, gv);
                self.builder
                    .ins()
                    .load(self.ptr_type, MemFlags::new(), addr, 0)
            } else {
                return Err(format!("Undefined channel: {}", channel_name));
            };
            let offset = (i * 8) as i32;
            self.builder
                .ins()
                .store(MemFlags::new(), ch_ptr, array_ptr, offset);
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
        let select_ref = *self
            .func_refs
            .get("@_channel_select")
            .ok_or("channel_select not found")?;
        let count_val = self.builder.ins().iconst(types::I64, channel_count as i64);
        let call = self
            .builder
            .ins()
            .call(select_ref, &[array_ptr, count_val, timeout_val, value_ptr]);
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
            self.builder
                .ins()
                .brif(is_match, block, &[], next_block, &[]);
            self.builder.switch_to_block(next_block);
            self.builder.seal_block(next_block);
        }

        // 检查 timeout (-1)
        if let Some(block) = timeout_block {
            let timeout_val = self.builder.ins().iconst(types::I64, -1);
            let is_timeout = self
                .builder
                .ins()
                .icmp(IntCC::Equal, selected_idx, timeout_val);
            let next_block = self.builder.create_block();
            self.builder
                .ins()
                .brif(is_timeout, block, &[], next_block, &[]);
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

            let recv_val = self
                .builder
                .ins()
                .load(types::I64, MemFlags::new(), value_ptr, 0);
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
    fn compile_await_scope(
        &mut self,
        scope_stmt: &bolide_parser::AwaitScopeStmt,
    ) -> Result<(), String> {
        // 进入作用域
        let scope_enter_ref = *self
            .func_refs
            .get("@_scope_enter")
            .ok_or("scope_enter not found")?;
        self.builder.ins().call(scope_enter_ref, &[]);

        // 编译作用域内的语句
        for stmt in &scope_stmt.body {
            self.compile_stmt(stmt)?;
        }

        // 退出作用域
        let scope_exit_ref = *self
            .func_refs
            .get("@_scope_exit")
            .ok_or("scope_exit not found")?;
        self.builder.ins().call(scope_exit_ref, &[]);

        Ok(())
    }

    /// 编译 SpawnSelect 语句
    fn compile_spawn_select(
        &mut self,
        spawn_select: &bolide_parser::SpawnSelectStmt,
    ) -> Result<(), String> {
        use bolide_parser::SpawnSelectBranch;
        use cranelift_codegen::ir::StackSlotData;
        use cranelift_codegen::ir::StackSlotKind;

        if spawn_select.branches.is_empty() {
            return Ok(());
        }

        let branch_count = spawn_select.branches.len();

        // 1. 启动所有并行任务，收集 pool handles
        let mut handles: Vec<Value> = Vec::new();
        let mut result_types: Vec<BolideType> = Vec::new();
        for branch in &spawn_select.branches {
            let expr = match branch {
                SpawnSelectBranch::Bind { expr, .. } => expr,
                SpawnSelectBranch::Expr { expr, .. } => expr,
            };
            let (func_name, args) = self.spawn_call_parts(expr)?;
            let ret_ty = self.spawn_item_type(expr)?;
            let handle = self.compile_pool_spawn(func_name, args)?;
            handles.push(handle);
            result_types.push(ret_ty);
        }

        // 2. 在栈上分配数组存储 handles
        let array_size = (branch_count * 8) as u32;
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            array_size,
            0,
        ));
        let array_ptr = self.builder.ins().stack_addr(types::I64, slot, 0);

        // 3. 将 handles 存入数组
        for (i, handle) in handles.iter().enumerate() {
            let offset = (i * 8) as i32;
            self.builder
                .ins()
                .store(MemFlags::new(), *handle, array_ptr, offset);
        }

        // 4. 调用 pool_select_wait_first 获取第一个完成的索引
        let select_wait_first = *self
            .func_refs
            .get("@_pool_select_wait_first")
            .ok_or("pool_select_wait_first not found")?;
        let count = self.builder.ins().iconst(types::I64, branch_count as i64);
        let call = self
            .builder
            .ins()
            .call(select_wait_first, &[array_ptr, count]);
        let winner_idx = self.builder.inst_results(call)[0];

        // 5. 根据获胜索引执行对应分支
        self.compile_spawn_select_branches(spawn_select, &handles, &result_types, winner_idx)?;

        Ok(())
    }

    /// 编译 spawn select 分支选择逻辑
    fn compile_spawn_select_branches(
        &mut self,
        select_stmt: &bolide_parser::SpawnSelectStmt,
        handles: &[Value],
        result_types: &[BolideType],
        winner_idx: Value,
    ) -> Result<(), String> {
        use bolide_parser::SpawnSelectBranch;

        let merge_block = self.builder.create_block();

        for (i, branch) in select_stmt.branches.iter().enumerate() {
            let branch_block = self.builder.create_block();
            let next_block = self.builder.create_block();

            // 比较 winner_idx == i
            let idx_const = self.builder.ins().iconst(types::I64, i as i64);
            let cmp = self.builder.ins().icmp(IntCC::Equal, winner_idx, idx_const);
            self.builder
                .ins()
                .brif(cmp, branch_block, &[], next_block, &[]);

            // 分支块
            self.builder.switch_to_block(branch_block);
            self.builder.seal_block(branch_block);

            match branch {
                SpawnSelectBranch::Bind { var, body, .. } => {
                    let bound_ty = result_types[i].clone();
                    let result = self.compile_pool_join_handle(handles[i], &bound_ty, true)?;

                    let c_ty = self.bolide_type_to_cranelift(&bound_ty);
                    let var_decl = self.declare_variable(var, c_ty);
                    self.builder.def_var(var_decl, result);
                    self.var_types.insert(var.clone(), bound_ty.clone());
                    self.track_rc_variable(var, &bound_ty);

                    for stmt in body {
                        self.compile_stmt(stmt)?;
                    }
                }
                SpawnSelectBranch::Expr { body, .. } => {
                    self.compile_pool_join_handle(handles[i], &result_types[i], false)?;
                    for stmt in body {
                        self.compile_stmt(stmt)?;
                    }
                }
            }

            for (j, handle) in handles.iter().enumerate() {
                if j != i {
                    self.compile_pool_join_handle(*handle, &result_types[j], false)?;
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
        // 全局变量：仅在最顶层入口函数中才写入全局数据槽。
        // 其他函数中的局部变量即使与全局变量同名，也是独立声明，不应写回全局。
        let is_main = self.current_func == "__bolide_entry__" || self.current_func == "__main__";
        if is_main && self.global_refs.contains_key(&decl.name) {
            if let Some(ref value) = decl.value {
                let raw_val = self.compile_expr(value)?;
                let val = if let Some(target_ty) = self.global_var_types.get(&decl.name).cloned() {
                    let raw_ty = self
                        .infer_expr_type(value)
                        .map(|ty| self.normalize_bolide_type(&ty))
                        .unwrap_or(BolideType::Dynamic);
                    self.prepare_value_for_storage(raw_val, &raw_ty, &target_ty)?
                } else {
                    raw_val
                };
                // 从临时列表移除（全局变量接管所有权，不在语句结束时释放）
                self.remove_temp_rc_value(val);
                // 闭包对象：吸收所有权并记录
                if self.closure_temps.contains(&val) {
                    self.remove_temp_closure(val);
                    self.closure_vars.insert(decl.name.clone());
                } else if let Expr::Ident(src) = value {
                    if self.closure_vars.contains(src) {
                        self.emit_closure_retain(val);
                        self.closure_vars.insert(decl.name.clone());
                    }
                }
                let gv = self.global_refs[&decl.name];
                let addr = self.builder.ins().global_value(self.ptr_type, gv);
                self.builder.ins().store(MemFlags::new(), val, addr, 0);
            }
            return Ok(());
        }

        let declared_bolide_ty = decl.ty.as_ref().map(|t| self.normalize_bolide_type(t));
        let inferred_bolide_ty = if declared_bolide_ty.is_none() {
            decl.value
                .as_ref()
                .and_then(|value| self.infer_expr_type(value))
                .map(|ty| self.normalize_bolide_type(&ty))
        } else {
            None
        };
        let bolide_ty = declared_bolide_ty
            .clone()
            .or(inferred_bolide_ty.clone())
            .unwrap_or(BolideType::Int);
        let ty = self.bolide_type_to_cranelift(&bolide_ty);
        let var = self.declare_variable(&decl.name, ty);

        // Store the type in var_types
        if let Some(ref t) = declared_bolide_ty {
            self.var_types.insert(decl.name.clone(), t.clone());
        } else if let Some(ref value) = decl.value {
            // 检查是否是异步函数调用，异步函数调用返回 Future 而非内部类型
            let is_async_call = match value {
                Expr::Call(callee, _) => {
                    if let Expr::Ident(name) = callee.as_ref() {
                        self.async_funcs.contains(name.as_str())
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if is_async_call {
                self.var_types.insert(decl.name.clone(), BolideType::Future);
            } else if let Some(inferred_ty) = inferred_bolide_ty {
                self.var_types.insert(decl.name.clone(), inferred_ty);
            }
        }

        // 记录 spawn / async 调用的句柄变量 -> 函数名映射，供 join 推断返回类型
        if let Some(ref value) = decl.value {
            match value {
                Expr::Spawn(func_name, _) => {
                    self.spawn_func_map
                        .insert(decl.name.clone(), func_name.clone());
                }
                Expr::Call(func_expr, _) => {
                    if let Expr::Ident(func_name) = func_expr.as_ref() {
                        if self.async_funcs.contains(func_name) {
                            self.spawn_func_map
                                .insert(decl.name.clone(), func_name.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(ref value) = decl.value {
            // 空列表字面量需用类型标注确定元素类型
            let raw_val = if matches!(value, Expr::List(items) if items.is_empty())
                && matches!(bolide_ty, BolideType::List(_))
            {
                self.compile_list_with_hint(&[], Some(&bolide_ty))?
            } else {
                self.compile_expr(value)?
            };
            let raw_ty = self
                .infer_expr_type(value)
                .map(|ty| self.normalize_bolide_type(&ty))
                .unwrap_or(BolideType::Dynamic);
            let val = self.prepare_value_for_storage(raw_val, &raw_ty, &bolide_ty)?;

            let declared_ty = self.var_types.get(&decl.name).cloned();
            let is_weak_decl = declared_ty
                .as_ref()
                .map(|t| Self::is_weak_ref_type(t))
                .unwrap_or(false);
            let is_rc = declared_ty
                .as_ref()
                .map(|t| Self::is_rc_type(t))
                .unwrap_or(false);

            let mut store_val = val;
            if is_weak_decl {
                // weak/unowned: 不接管强引用所有权，只增加弱引用计数保住对象头；
                // 临时强引用仍在语句末释放
                if let Some(&weak_retain) = self.func_refs.get("@_object_weak_retain") {
                    self.builder.ins().call(weak_retain, &[val]);
                }
                self.weak_variables.insert(decl.name.clone());
            } else if is_rc {
                // RC 类型所有权处理（与 JIT 一致）：
                //   - 临时值（字面量/运算/函数返回）→ 接管所有权
                //   - 借用值（来自其它变量/容器）→ clone/retain 获得独立所有权
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                if is_temp {
                    self.remove_temp_rc_value(val);
                } else if let Some(ref t) = declared_ty {
                    store_val = self.emit_retain(val, t);
                }
            }

            self.builder.def_var(var, store_val);

            // 闭包所有权：变量接管闭包对象
            if self.closure_temps.contains(&val) {
                self.remove_temp_closure(val);
                self.closure_vars.insert(decl.name.clone());
            } else if let Expr::Ident(src) = value {
                if self.closure_vars.contains(src) {
                    self.emit_closure_retain(val);
                    self.closure_vars.insert(decl.name.clone());
                }
            }
        } else {
            let zero = self.builder.ins().iconst(types::I64, 0);
            self.builder.def_var(var, zero);
            if self
                .var_types
                .get(&decl.name)
                .map(|t| Self::is_weak_ref_type(t))
                .unwrap_or(false)
            {
                self.weak_variables.insert(decl.name.clone());
            }
        }

        // Register for cleanup
        if let Some(ty) = self.var_types.get(&decl.name).cloned() {
            self.track_rc_variable(&decl.name, &ty);
        }

        // 借用来源检查：在借用存活期间不允许对来源重新赋值
        self.check_borrow_source_assign(&decl.name)?;

        // 数据流追踪：如果值来自生命周期参数，记录变量的来源
        if self.uses_lifetime_mode() {
            if let Some(ref value) = decl.value {
                if let Some(source) = self.check_lifetime_source(value) {
                    self.var_lifetime_source.insert(decl.name.clone(), source);
                }
            }
        }

        // 记录变量作用域深度
        self.record_var_scope(&decl.name);

        // 调用者端借用检查：记录借用关系
        let is_from_lifetime_func = decl
            .value
            .as_ref()
            .map(|v| self.is_lifetime_func_call(v))
            .unwrap_or(false);
        if is_from_lifetime_func {
            if let Some(ref value) = decl.value {
                if let Some(source_var) = self.get_lifetime_call_source(value) {
                    self.record_borrow(&decl.name, &source_var);
                }
            }
        }

        Ok(())
    }

    /// 编译赋值语句
    fn compile_assign(&mut self, assign: &bolide_parser::Assign) -> Result<(), String> {
        match &assign.target {
            Expr::Ident(var_name) => {
                // 检查是否是全局变量（局部变量优先）
                let is_global = !self.variables.contains_key(var_name)
                    && self.global_refs.contains_key(var_name);

                if is_global {
                    let gv = self.global_refs[var_name];
                    let addr = self.builder.ins().global_value(self.ptr_type, gv);
                    let global_ty = self.global_var_types.get(var_name).cloned();

                    // 先编译新值
                    let raw_val = self.compile_expr(&assign.value)?;
                    let val = if let Some(ref ty) = global_ty {
                        let raw_ty = self
                            .infer_expr_type(&assign.value)
                            .map(|ty| self.normalize_bolide_type(&ty))
                            .unwrap_or(BolideType::Dynamic);
                        self.prepare_value_for_storage(raw_val, &raw_ty, ty)?
                    } else {
                        raw_val
                    };
                    let val_to_store = if let Some(ref ty) = global_ty {
                        if Self::is_rc_type(ty) {
                            let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                            if is_temp && !Self::is_weak_ref_type(ty) {
                                self.remove_temp_rc_value(val);
                                // 释放旧值
                                let old_val = self.builder.ins().load(
                                    self.ptr_type,
                                    MemFlags::new(),
                                    addr,
                                    0,
                                );
                                self.emit_release(old_val, ty);
                                val
                            } else {
                                let clone_fn = Self::get_clone_func_name(ty);
                                if let Some(fn_name) = clone_fn {
                                    if let Some(&fn_ref) = self.func_refs.get(fn_name) {
                                        let call = self.builder.ins().call(fn_ref, &[val]);
                                        let cloned = self.builder.inst_results(call)[0];
                                        // 释放旧值
                                        let old_val = self.builder.ins().load(
                                            self.ptr_type,
                                            MemFlags::new(),
                                            addr,
                                            0,
                                        );
                                        self.emit_release(old_val, ty);
                                        cloned
                                    } else {
                                        val
                                    }
                                } else {
                                    val
                                }
                            }
                        } else {
                            val
                        }
                    } else {
                        val
                    };
                    self.builder
                        .ins()
                        .store(MemFlags::new(), val_to_store, addr, 0);

                    // 调用者端借用检查：全局变量赋值同样记录/解除借用关系（对齐 JIT）
                    if self.is_lifetime_func_call(&assign.value) {
                        if let Some(source_var) = self.get_lifetime_call_source(&assign.value) {
                            self.record_borrow(var_name, &source_var);
                        }
                    } else {
                        self.borrowed_vars.remove(var_name);
                    }
                    return Ok(());
                }

                let var = *self
                    .variables
                    .get(var_name)
                    .ok_or_else(|| format!("Undefined variable: {}", var_name))?;

                // 借用来源检查：借用存活期间禁止对来源重新赋值
                self.check_borrow_source_assign(var_name)?;

                // 检查是否是 ref 参数，决定是否释放旧值（对齐 JIT）
                let is_ref_param = self.ref_params.iter().any(|(n, _, _)| n == var_name);
                let was_reassigned = self.ref_params_reassigned.contains(var_name);

                let var_ty = self.var_types.get(var_name).cloned();

                let should_release = if let Some(ref ty) = var_ty {
                    Self::is_rc_type(ty) && (!is_ref_param || was_reassigned)
                } else {
                    false
                };

                if should_release {
                    let old_val = self.builder.use_var(var);
                    self.emit_release(old_val, var_ty.as_ref().unwrap());
                }

                if is_ref_param && !was_reassigned {
                    self.ref_params_reassigned.insert(var_name.to_string());
                }

                let raw_val = self.compile_expr(&assign.value)?;
                let val = if let Some(ref ty) = var_ty {
                    let raw_ty = self
                        .infer_expr_type(&assign.value)
                        .map(|ty| self.normalize_bolide_type(&ty))
                        .unwrap_or(BolideType::Dynamic);
                    self.prepare_value_for_storage(raw_val, &raw_ty, ty)?
                } else {
                    raw_val
                };

                // RC 类型赋值：临时值接走所有权，非临时值 clone（对齐 JIT）
                if let Some(ref ty) = var_ty {
                    if Self::is_rc_type(ty) {
                        if Self::is_weak_ref_type(&ty) {
                            // weak/unowned 赋值：增加弱引用计数，不接管强引用
                            if let Some(&weak_retain) = self.func_refs.get("@_object_weak_retain") {
                                self.builder.ins().call(weak_retain, &[val]);
                            }
                            self.builder.def_var(var, val);
                        } else {
                            let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                            if is_temp {
                                // 临时值：接管所有权
                                self.remove_temp_rc_value(val);
                                self.builder.def_var(var, val);
                            } else {
                                // 非临时值（从变量借用）：clone 一份独立持有
                                let clone_fn = Self::get_clone_func_name(ty);
                                if let Some(fn_name) = clone_fn {
                                    if let Some(&fn_ref) = self.func_refs.get(fn_name) {
                                        let call = self.builder.ins().call(fn_ref, &[val]);
                                        let cloned = self.builder.inst_results(call)[0];
                                        self.builder.def_var(var, cloned);
                                    } else {
                                        self.builder.def_var(var, val);
                                    }
                                } else {
                                    self.builder.def_var(var, val);
                                }
                            }
                        }
                    } else {
                        self.builder.def_var(var, val);
                    }
                } else {
                    self.builder.def_var(var, val);
                }

                // 数据流追踪：如果赋值的值来自生命周期参数，记录/更新来源
                if self.uses_lifetime_mode() {
                    if let Some(source) = self.check_lifetime_source(&assign.value) {
                        self.var_lifetime_source.insert(var_name.clone(), source);
                    } else {
                        self.var_lifetime_source.remove(var_name);
                    }
                }

                // 调用者端借用检查：如果值来自生命周期函数调用，记录借用关系
                if self.is_lifetime_func_call(&assign.value) {
                    if let Some(source_var) = self.get_lifetime_call_source(&assign.value) {
                        self.record_borrow(var_name, &source_var);
                    }
                } else {
                    // 重新赋值为非借用值后，借用关系解除（对齐 JIT）
                    self.borrowed_vars.remove(var_name);
                }
            }
            Expr::Member(base, member) => {
                self.check_borrow_escape(&assign.value, "field assignment")?;
                self.compile_member_assign(base, member, &assign.value)?;
            }
            Expr::Index(base, index) => {
                self.check_borrow_escape(&assign.value, "index assignment")?;
                self.compile_index_assign(base, index, &assign.value)?;
            }
            _ => return Err("Unsupported assignment target".to_string()),
        }
        Ok(())
    }

    /// 编译成员赋值
    fn compile_member_assign(
        &mut self,
        base: &Expr,
        member: &str,
        value: &Expr,
    ) -> Result<(), String> {
        let base_val = self.compile_expr(base)?;
        let val = self.compile_expr(value)?;

        let base_type = self.infer_expr_type(base);

        // 处理 Weak/Unowned 类型，提取内部的 Custom 类型
        let class_name = match &base_type {
            Some(BolideType::Custom(name)) => Some(name.clone()),
            Some(BolideType::Weak(inner)) => {
                if let BolideType::Custom(name) = inner.as_ref() {
                    Some(name.clone())
                } else {
                    None
                }
            }
            Some(BolideType::Unowned(inner)) => {
                if let BolideType::Custom(name) = inner.as_ref() {
                    Some(name.clone())
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(class_name) = class_name {
            if let Some(class_info) = self.classes.get(&class_name).cloned() {
                for field in &class_info.fields {
                    if field.name == member {
                        let offset = field.offset as i32;

                        // Release old value if RC type
                        if Self::is_rc_type(&field.ty) {
                            let field_ptr = self.builder.ins().iadd_imm(base_val, offset as i64);
                            let old_val =
                                self.builder
                                    .ins()
                                    .load(types::I64, MemFlags::new(), field_ptr, 0);
                            self.emit_release(old_val, &field.ty);

                            // Take ownership of new value if it's a temp
                            self.remove_temp_rc_value(val);
                        }

                        self.builder
                            .ins()
                            .store(MemFlags::new(), val, base_val, offset);
                        return Ok(());
                    }
                }
                return Err(format!(
                    "Field '{}' not found in class '{}'",
                    member, class_name
                ));
            }
        }
        Err("Cannot assign to member of non-class type".to_string())
    }

    /// 编译索引赋值
    fn compile_index_assign(
        &mut self,
        base: &Expr,
        index: &Expr,
        value: &Expr,
    ) -> Result<(), String> {
        let base_type = self.infer_expr_type(base);
        let base_val = self.compile_expr(base)?;
        let index_val = self.compile_expr(index)?;
        let val = self.compile_expr(value)?;

        match base_type {
            Some(BolideType::List(ref elem_ty))
                if matches!(
                    elem_ty.as_ref(),
                    BolideType::Int | BolideType::Float | BolideType::Bool
                ) =>
            {
                return self.emit_list_set_inline(base_val, index_val, val, elem_ty.as_ref());
            }
            Some(BolideType::Dict(_, _)) => {
                let func_ref = *self
                    .func_refs
                    .get("@_dict_set")
                    .ok_or_else(|| "@_dict_set not found")?;
                self.builder
                    .ins()
                    .call(func_ref, &[base_val, index_val, val]);
            }
            Some(BolideType::Tuple(_)) => {
                // Tuple storage takes ownership of RC values; list/dict storage retains.
                self.remove_temp_rc_value(val);
                let func_ref = *self
                    .func_refs
                    .get("@_tuple_set")
                    .ok_or_else(|| "@_tuple_set not found")?;
                self.builder
                    .ins()
                    .call(func_ref, &[base_val, index_val, val]);
            }
            _ => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_set")
                    .ok_or_else(|| "@_list_set not found")?;
                self.builder
                    .ins()
                    .call(func_ref, &[base_val, index_val, val]);
            }
        };
        Ok(())
    }

    /// 编译返回语句
    fn compile_return(&mut self, expr: Option<&Expr>) -> Result<(), String> {
        if let Some(e) = expr {
            // 生命周期模式：验证返回值来源
            if self.uses_lifetime_mode() {
                self.validate_lifetime_return(e)?;
            }

            let raw_val = self.compile_expr(e)?;
            let val_ty = self.infer_expr_type(e);
            let return_ty = self
                .func_return_types
                .get(&self.current_func)
                .cloned()
                .flatten();
            let val = if let (Some(actual_ty), Some(return_ty)) = (&val_ty, &return_ty) {
                let actual_ty = self.normalize_bolide_type(actual_ty);
                self.prepare_value_for_storage(raw_val, &actual_ty, return_ty)?
            } else {
                raw_val
            };
            let val_ty = return_ty.or(val_ty);
            let returns_raw_value = val == raw_val;
            let mut final_val = val;

            // from 借用检查：非生命周期函数禁止返回借用值
            if !self.uses_lifetime_mode() {
                if let Expr::Ident(name) = e {
                    if let Some((src, _)) = self.borrowed_vars.get(name) {
                        return Err(format!(
                            "Lifetime error: cannot return '{}' which borrows from '{}'; \
                             declare the function with 'from' or copy the value",
                            name, src
                        ));
                    }
                }
            }

            // 返回值所有权处理（与 JIT 一致）
            let return_var = if let Expr::Ident(name) = e {
                self.variables.get(name).copied()
            } else {
                None
            };

            if let Some(ref ty) = val_ty {
                if Self::is_rc_type(ty) {
                    let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                    if is_temp {
                        // 临时 RC 值：移出临时列表，所有权移交调用方
                        self.remove_temp_rc_value(val);
                    } else if let (Some(_var), Expr::Ident(name)) = (return_var, e) {
                        // 返回 borrow/ref 参数时必须 clone（归调用方所有，不归本函数释放）
                        if self.caller_owned_params.contains(name.as_str()) {
                            final_val = self.emit_retain(val, ty);
                        }
                        // 否则返回本地变量：cleanup_except 跳过它，计数不变即移交
                    } else {
                        // 其它表达式（Index/Member 等借自容器）：cleanup 会释放容器，
                        // 故此处 retain/clone 一份保证调用方拿到有效对象
                        final_val = self.emit_retain(val, ty);
                    }
                }
            }

            // 返回闭包对象的所有权处理
            let is_closure_temp = self.closure_temps.contains(&val);
            let return_name = if let Expr::Ident(name) = e {
                Some(name.clone())
            } else {
                None
            };
            let is_closure_var = return_name
                .as_ref()
                .map(|n| self.closure_vars.contains(n))
                .unwrap_or(false);
            let is_closure_param = return_name
                .as_ref()
                .map(|n| self.closure_param_vars.contains(n))
                .unwrap_or(false);

            if is_closure_temp {
                self.remove_temp_closure(val);
            } else if is_closure_var || is_closure_param {
                // 局部闭包变量 / 函数类型参数：不释放，调用者共享
            } else if matches!(val_ty, Some(BolideType::FuncSig(_, _) | BolideType::Func)) {
                self.emit_closure_retain(val);
            }

            // 释放临时值（不包含被返回的）
            self.release_temp_rc_values();
            // 写回 ref 参数后释放 RC 变量（不包含被返回的局部变量）
            self.write_back_ref_params();
            let cleanup_except = if returns_raw_value { return_var } else { None };
            self.emit_rc_cleanup_except(cleanup_except);

            self.builder.ins().return_(&[final_val]);
        } else {
            // Release temporary values
            self.release_temp_rc_values();
            // 写回 ref 参数后释放局部 RC
            self.write_back_ref_params();

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

        self.builder
            .ins()
            .brif(cond_bool, then_block, &[], else_block, &[]);

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
            self.leave_scope(scope_idx)?;
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
            self.leave_scope(scope_idx_else)?;
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

        self.builder
            .ins()
            .brif(cond_bool, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);

        let scope_idx = self.enter_scope();
        // while: continue → 重新检查条件（header）；break → exit
        self.loop_stack.push((header_block, exit_block, scope_idx));
        let mut body_returned = false;
        for stmt in &while_stmt.body {
            if self.compile_stmt(stmt)? {
                body_returned = true;
                break;
            }
        }
        self.loop_stack.pop();

        if !body_returned {
            self.leave_scope(scope_idx)?;
            self.builder.ins().jump(header_block, &[]);
        } else {
            // 提前跳出路径已自行释放作用域变量，这里只清理编译期记录
            self.rc_variables.truncate(scope_idx);
        }

        // 现在所有 header_block 的前驱都已添加，可以 seal 了
        self.builder.seal_block(header_block);

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }

    /// 编译 for 语句
    /// 定义变量（声明 + 初始化 + 类型记录 + RC 跟踪），对齐 JIT define_variable
    fn define_variable(&mut self, name: &str, val: Value, ty: BolideType) -> Result<(), String> {
        let cl_ty = self.bolide_type_to_cranelift(&ty);
        let var = self.declare_variable(name, cl_ty);
        self.builder.def_var(var, val);
        self.var_types.insert(name.to_string(), ty.clone());
        if Self::is_rc_type(&ty) {
            self.track_rc_variable(name, &ty);
        }
        Ok(())
    }

    fn compile_for(&mut self, for_stmt: &bolide_parser::ForStmt) -> Result<(), String> {
        let vars = &for_stmt.vars;
        if vars.is_empty() {
            return Err("For loop must have at least one variable".to_string());
        }

        // 检查是否是 range() 调用
        if let Expr::Call(callee, args) = &for_stmt.iter {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "range" {
                    if vars.len() != 1 {
                        return Err("range() loop only supports single variable".to_string());
                    }
                    return self.compile_range_for(for_stmt, args);
                }
            }
        }

        // 检查是否是字典迭代
        if let Some(BolideType::Dict(_, _)) = self.infer_expr_type(&for_stmt.iter) {
            return self.compile_for_dict(for_stmt);
        }

        // 否则当作列表迭代
        self.compile_list_for(for_stmt)
    }

    /// 编译 range for 循环
    fn compile_range_for(
        &mut self,
        for_stmt: &bolide_parser::ForStmt,
        args: &[Expr],
    ) -> Result<(), String> {
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
        let var_name = for_stmt
            .vars
            .first()
            .ok_or("For loop requires at least one variable")?;
        let loop_var = self.declare_variable(var_name, types::I64);
        self.builder.def_var(loop_var, start);

        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let latch_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        self.builder.ins().jump(header_block, &[]);

        // 条件检查
        self.builder.switch_to_block(header_block);
        let idx = self.builder.use_var(loop_var);
        let cond = self.builder.ins().icmp(IntCC::SignedLessThan, idx, end);
        self.builder
            .ins()
            .brif(cond, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);

        let scope_idx = self.enter_scope();
        self.loop_stack.push((latch_block, exit_block, scope_idx));
        let mut body_returned = false;
        for stmt in &for_stmt.body {
            if self.compile_stmt(stmt)? {
                body_returned = true;
                break;
            }
        }
        self.loop_stack.pop();

        if !body_returned {
            self.leave_scope(scope_idx)?;
            self.builder.ins().jump(latch_block, &[]);
        } else {
            self.rc_variables.truncate(scope_idx);
        }

        // latch: 递增索引后回到 header（continue 跳转到此处以保证步进）
        self.builder.switch_to_block(latch_block);
        self.builder.seal_block(latch_block);
        let idx = self.builder.use_var(loop_var);
        let new_idx = self.builder.ins().iadd(idx, step);
        self.builder.def_var(loop_var, new_idx);
        self.builder.ins().jump(header_block, &[]);

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
        let len_ref = *self
            .func_refs
            .get("@_list_len")
            .ok_or("list_len not found")?;
        let call = self.builder.ins().call(len_ref, &[iter_val]);
        let len = self.builder.inst_results(call)[0];

        // 创建索引变量
        let idx_var = self.declare_variable("__for_idx", types::I64);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.def_var(idx_var, zero);

        // 创建循环变量
        let var_name = for_stmt
            .vars
            .first()
            .ok_or("For loop requires at least one variable")?;
        let loop_var = self.declare_variable(var_name, types::I64);
        self.builder.def_var(loop_var, zero);

        self.var_types.insert(var_name.clone(), elem_type.clone());

        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let latch_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        self.builder.ins().jump(header_block, &[]);

        // 条件检查
        self.builder.switch_to_block(header_block);
        let idx = self.builder.use_var(idx_var);
        let cond = self.builder.ins().icmp(IntCC::SignedLessThan, idx, len);
        self.builder
            .ins()
            .brif(cond, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);

        let scope_idx = self.enter_scope();
        if Self::is_rc_type(&elem_type) {
            self.track_rc_variable(var_name, &elem_type);
        }

        let get_ref = *self
            .func_refs
            .get("@_list_get")
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

        self.loop_stack.push((latch_block, exit_block, scope_idx));
        let mut body_returned = false;
        for stmt in &for_stmt.body {
            if self.compile_stmt(stmt)? {
                body_returned = true;
                break;
            }
        }
        self.loop_stack.pop();

        if !body_returned {
            self.leave_scope(scope_idx)?;
            self.builder.ins().jump(latch_block, &[]);
        } else {
            self.rc_variables.truncate(scope_idx);
        }

        // latch: 递增索引后回到 header（continue 跳转到此处）
        self.builder.switch_to_block(latch_block);
        self.builder.seal_block(latch_block);
        let idx = self.builder.use_var(idx_var);
        let one = self.builder.ins().iconst(types::I64, 1);
        let new_idx = self.builder.ins().iadd(idx, one);
        self.builder.def_var(idx_var, new_idx);
        self.builder.ins().jump(header_block, &[]);

        self.builder.seal_block(header_block);

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }

    /// 编译字典迭代：for k in d 或 for k, v in d
    fn compile_for_dict(&mut self, for_stmt: &bolide_parser::ForStmt) -> Result<(), String> {
        let vars = &for_stmt.vars;
        let dict_ptr = self.compile_expr(&for_stmt.iter)?;

        let (key_type, val_type) = match self.infer_expr_type(&for_stmt.iter) {
            Some(BolideType::Dict(k, v)) => (*k, *v),
            _ => (BolideType::Int, BolideType::Int),
        };

        // dict_iter(dict) → keys list
        let dict_iter_ref = *self
            .func_refs
            .get("@_dict_iter")
            .ok_or("dict_iter not found")?;
        let call = self.builder.ins().call(dict_iter_ref, &[dict_ptr]);
        let keys_list = self.builder.inst_results(call)[0];

        if vars.len() == 2 {
            // for k, v in dict: 遍历 keys，循环体内用 dict_get 取值
            let list_len_ref = *self
                .func_refs
                .get("@_list_len")
                .ok_or("list_len not found")?;
            let len_call = self.builder.ins().call(list_len_ref, &[keys_list]);
            let list_len = self.builder.inst_results(len_call)[0];

            let idx_name = format!("__for_idx_{}", vars[0]);
            let idx_var = self.declare_variable(&idx_name, types::I64);
            let zero = self.builder.ins().iconst(types::I64, 0);
            self.builder.def_var(idx_var, zero);

            let header_block = self.builder.create_block();
            let body_block = self.builder.create_block();
            let latch_block = self.builder.create_block();
            let exit_block = self.builder.create_block();

            self.builder.ins().jump(header_block, &[]);

            // Header: check idx < len
            self.builder.switch_to_block(header_block);
            let current_idx = self.builder.use_var(idx_var);
            let cond = self
                .builder
                .ins()
                .icmp(IntCC::SignedLessThan, current_idx, list_len);
            self.builder
                .ins()
                .brif(cond, body_block, &[], exit_block, &[]);

            // Body
            self.builder.switch_to_block(body_block);
            self.builder.seal_block(body_block);

            let list_get_ref = *self
                .func_refs
                .get("@_list_get")
                .ok_or("list_get not found")?;
            let get_key_call = self
                .builder
                .ins()
                .call(list_get_ref, &[keys_list, current_idx]);
            let key_val = self.builder.inst_results(get_key_call)[0];
            self.define_variable(&vars[0], key_val, key_type.clone())?;

            let dict_get_ref = *self
                .func_refs
                .get("@_dict_get")
                .ok_or("dict_get not found")?;
            let get_val_call = self.builder.ins().call(dict_get_ref, &[dict_ptr, key_val]);
            let val_val = self.builder.inst_results(get_val_call)[0];
            self.define_variable(&vars[1], val_val, val_type.clone())?;

            let scope_idx = self.enter_scope();
            self.loop_stack.push((latch_block, exit_block, scope_idx));
            let mut body_returned = false;
            for stmt in &for_stmt.body {
                if body_returned {
                    break;
                }
                body_returned = self.compile_stmt(stmt)?;
            }
            self.loop_stack.pop();

            if !body_returned {
                self.leave_scope(scope_idx)?;
                self.builder.ins().jump(latch_block, &[]);
            } else {
                self.rc_variables.truncate(scope_idx);
            }

            // Latch: idx += 1 → header
            self.builder.switch_to_block(latch_block);
            self.builder.seal_block(latch_block);
            let current = self.builder.use_var(idx_var);
            let next = self.builder.ins().iadd_imm(current, 1);
            self.builder.def_var(idx_var, next);
            self.builder.ins().jump(header_block, &[]);

            self.builder.seal_block(header_block);
            self.builder.switch_to_block(exit_block);
            self.builder.seal_block(exit_block);
        } else {
            // 单变量：遍历 keys
            self.compile_list_iteration_loop(vars, keys_list, key_type, &for_stmt.body)?;
        }

        // 释放 keys 列表
        let release_ref = *self
            .func_refs
            .get("@_list_release")
            .ok_or("list_release not found")?;
        self.builder.ins().call(release_ref, &[keys_list]);

        Ok(())
    }

    /// 编译列表迭代循环（用于 dict 的 keys 迭代：for k in d）
    fn compile_list_iteration_loop(
        &mut self,
        vars: &[String],
        list_ptr: Value,
        elem_type: BolideType,
        body: &[Statement],
    ) -> Result<(), String> {
        let list_len_ref = *self
            .func_refs
            .get("@_list_len")
            .ok_or("list_len not found")?;
        let len_call = self.builder.ins().call(list_len_ref, &[list_ptr]);
        let list_len = self.builder.inst_results(len_call)[0];

        let idx_var = self.declare_variable("__for_idx", types::I64);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.def_var(idx_var, zero);

        let cl_ty = self.bolide_type_to_cranelift(&elem_type);
        let var_name = &vars[0];
        let loop_var = self.declare_variable(var_name, cl_ty);
        self.builder.def_var(loop_var, zero);
        self.var_types.insert(var_name.clone(), elem_type.clone());

        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let latch_block = self.builder.create_block();
        let exit_block = self.builder.create_block();

        self.builder.ins().jump(header_block, &[]);

        self.builder.switch_to_block(header_block);
        let idx = self.builder.use_var(idx_var);
        let cond = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThan, idx, list_len);
        self.builder
            .ins()
            .brif(cond, body_block, &[], exit_block, &[]);

        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);

        let scope_idx = self.enter_scope();
        if Self::is_rc_type(&elem_type) {
            self.track_rc_variable(var_name, &elem_type);
        }

        let get_ref = *self
            .func_refs
            .get("@_list_get")
            .ok_or("list_get not found")?;
        let idx = self.builder.use_var(idx_var);
        let call = self.builder.ins().call(get_ref, &[list_ptr, idx]);
        let elem = self.builder.inst_results(call)[0];
        let elem = if Self::is_rc_type(&elem_type) {
            self.emit_retain(elem, &elem_type)
        } else {
            elem
        };
        self.builder.def_var(loop_var, elem);

        self.loop_stack.push((latch_block, exit_block, scope_idx));
        let mut body_returned = false;
        for stmt in body {
            if self.compile_stmt(stmt)? {
                body_returned = true;
                break;
            }
        }
        self.loop_stack.pop();

        if !body_returned {
            self.leave_scope(scope_idx)?;
            self.builder.ins().jump(latch_block, &[]);
        } else {
            self.rc_variables.truncate(scope_idx);
        }

        self.builder.switch_to_block(latch_block);
        self.builder.seal_block(latch_block);
        let idx = self.builder.use_var(idx_var);
        let one = self.builder.ins().iconst(types::I64, 1);
        let new_idx = self.builder.ins().iadd(idx, one);
        self.builder.def_var(idx_var, new_idx);
        self.builder.ins().jump(header_block, &[]);

        self.builder.seal_block(header_block);

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        Ok(())
    }
}
