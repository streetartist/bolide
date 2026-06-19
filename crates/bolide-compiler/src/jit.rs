//! JIT 编译器
//!
//! 使用 Cranelift 实现的即时编译器

use crate::ffi_spec::{
    is_jit_dynamic_lib_spec, resolve_dynamic_lib_spec, validate_extern_lib_spec, LINK_LIB_PREFIX,
};
use crate::inject_builtin_classes;
use bolide_parser::{
    Assign, BinOp, ClassDef, ClassField, Expr, ExternBlock, ForStmt, FuncDef, IfStmt, Param,
    ParamMode, Program, Statement, Type as BolideType, UnaryOp, VarDecl,
};
use cranelift::prelude::isa::{CallConv, TargetIsa};
use cranelift::prelude::*;
use cranelift_codegen::ir::{FuncRef, Function, StackSlotData, StackSlotKind, TrapCode};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

fn validate_jit_extern_lib_path(lib_path: &str) -> Result<(), String> {
    if lib_path == "bolide" {
        return Ok(());
    }
    validate_extern_lib_spec(lib_path)?;
    if is_jit_dynamic_lib_spec(lib_path) {
        return Ok(());
    }
    if lib_path.starts_with(LINK_LIB_PREFIX) || lib_path.starts_with("std:") {
        return Err(format!(
            "extern \"{}\" is a native link library. JIT mode cannot link native libraries; use `bolide compile`.",
            lib_path
        ));
    }
    Err(format!(
        "extern \"{}\" is not a JIT-loadable library. Use `dyn:name` for dynamic loading, `auto:name` for JIT dynamic/AOT native linking, or `lib:name` with `bolide compile`.",
        lib_path
    ))
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
    offset: usize,               // 字段在对象中的偏移（字节）
    default_value: Option<Expr>, // 字段默认值（来自 field: type = expr;）
}

/// 类信息
#[derive(Clone)]
struct ClassInfo {
    name: String,
    parent: Option<String>,
    fields: Vec<FieldInfo>,
    methods: Vec<String>, // 方法名列表
    size: usize,          // 对象数据大小（字节，不含头部）
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
struct BindingSnapshot {
    name: String,
    variable: Option<Variable>,
    var_type: Option<BolideType>,
    scope_depth: Option<usize>,
    borrowed: Option<(String, usize)>,
    weak: bool,
    moved: bool,
    closure_var: bool,
    closure_param_var: bool,
    spawn_func: Option<String>,
    task_func: Option<String>,
    force_thread_task: bool,
    lifetime_source: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FuncSigReturnSource {
    Raw,
    Closure,
    Param(usize),
    ParamSet(u64),
    Unknown,
}

fn funcsig_adapter_name(params: &[BolideType], ret: &Option<Box<BolideType>>) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}->{:?}", params, ret).hash(&mut hasher);
    format!("@_funcsig_raw_adapter_{:016x}", hasher.finish())
}

#[derive(Clone)]
enum PreparedArg {
    Expr {
        expr: Expr,
        target_index: usize,
    },
    PackedArgItem {
        target_index: usize,
        elem_ty: BolideType,
        item: PackedArgItem,
    },
    PackedKwargItem {
        target_index: usize,
        value_ty: BolideType,
        item: PackedKwargItem,
    },
}

impl PreparedArg {
    fn target_index(&self) -> usize {
        match self {
            PreparedArg::Expr { target_index, .. }
            | PreparedArg::PackedArgItem { target_index, .. }
            | PreparedArg::PackedKwargItem { target_index, .. } => *target_index,
        }
    }

    fn expr(&self) -> Option<&Expr> {
        match self {
            PreparedArg::Expr { expr, .. } => Some(expr),
            _ => None,
        }
    }
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
    /// ADT 名 -> ADT 信息映射
    adts: HashMap<String, AdtInfo>,
    /// 类名 -> 异常类型标签（>=100，按声明顺序分配，用于 catch 类型过滤）
    class_tags: HashMap<String, i64>,
    /// async 函数集合
    async_funcs: HashSet<String>,
    /// 全局 Future 变量 -> 对应 async 函数名（用于 await 的静态类型推断）
    global_spawn_funcs: HashMap<String, String>,
    /// extern 函数信息: 函数名 -> (库路径, 函数声明)
    extern_funcs: HashMap<String, (String, bolide_parser::ExternFunc)>,
    /// 模块名映射: 模块名 -> 文件路径
    modules: HashMap<String, String>,
    /// 使用生命周期模式的函数集合（返回借用而非拥有的值）
    lifetime_funcs: HashSet<String>,
    /// 函数名 -> 函数值返回来源。
    funcsig_return_sources: HashMap<String, FuncSigReturnSource>,
    /// 函数名 -> 需要按闭包对象 ABI 处理的函数类型参数下标。
    funcsig_closure_param_indices: HashMap<String, HashSet<usize>>,
    /// 全局变量名 -> 数据ID 映射
    global_data_ids: HashMap<String, cranelift_module::DataId>,
    /// 源文件所在目录（import 相对路径的解析基准）
    base_dir: Option<String>,
    /// 包管理器解析出的依赖映射
    dependency_manifest: Option<crate::deps::DependencyManifest>,
    /// 全局变量类型映射
    global_var_types: HashMap<String, BolideType>,
    /// 闭包计数器（生成唯一 lifted 函数名）
    closure_counter: usize,
    /// 待编译的 lifted 闭包函数（创建点入队，主函数后统一编译）
    pending_closures: Vec<ClosureJob>,
}

/// 一个待编译的 lifted 闭包函数
#[derive(Clone)]
struct ClosureJob {
    /// 模块中已声明的函数 ID
    func_id: FuncId,
    /// lifted 函数名
    name: String,
    /// 用户参数（不含前置 env 指针）
    params: Vec<Param>,
    /// 返回类型
    return_type: Option<BolideType>,
    /// 函数体
    body: Vec<Statement>,
    /// 捕获变量：(变量名, 类型)，env 中按此顺序排列（每个 8 字节槽）
    captures: Vec<(String, BolideType)>,
}

impl JitCompiler {
    pub fn new() -> Self {
        // 开启 Cranelift 优化（默认 opt_level=none 不做任何优化）
        let mut flag_builder = settings::builder();
        flag_builder
            .set("opt_level", "speed")
            .expect("invalid opt_level");
        let isa_builder = cranelift_native::builder().expect("host machine is not supported");
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .expect("Failed to create ISA");
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());

        // 注册运行时函数 - 基本类型打印 (统一在 print.rs)
        builder.symbol("@_print_int", bolide_runtime::bolide_print_int as *const u8);
        builder.symbol(
            "@_print_float",
            bolide_runtime::bolide_print_float as *const u8,
        );
        builder.symbol(
            "@_print_bool",
            bolide_runtime::bolide_print_bool as *const u8,
        );
        builder.symbol(
            "@_print_bigint",
            bolide_runtime::bolide_print_bigint as *const u8,
        );
        builder.symbol(
            "@_print_decimal",
            bolide_runtime::bolide_print_decimal as *const u8,
        );
        builder.symbol(
            "@_print_string",
            bolide_runtime::bolide_print_string as *const u8,
        );
        builder.symbol(
            "@_print_bytes",
            bolide_runtime::bolide_print_bytes as *const u8,
        );
        builder.symbol(
            "@_print_dynamic",
            bolide_runtime::bolide_print_dynamic as *const u8,
        );
        builder.symbol(
            "@_print_int_inline",
            bolide_runtime::bolide_print_int_inline as *const u8,
        );
        builder.symbol(
            "@_print_float_inline",
            bolide_runtime::bolide_print_float_inline as *const u8,
        );
        builder.symbol(
            "@_print_bool_inline",
            bolide_runtime::bolide_print_bool_inline as *const u8,
        );
        builder.symbol(
            "@_print_bigint_inline",
            bolide_runtime::bolide_print_bigint_inline as *const u8,
        );
        builder.symbol(
            "@_print_decimal_inline",
            bolide_runtime::bolide_print_decimal_inline as *const u8,
        );
        builder.symbol(
            "@_print_string_inline",
            bolide_runtime::bolide_print_string_inline as *const u8,
        );
        builder.symbol(
            "@_print_bytes_inline",
            bolide_runtime::bolide_print_bytes_inline as *const u8,
        );
        builder.symbol(
            "@_print_dynamic_inline",
            bolide_runtime::bolide_print_dynamic_inline as *const u8,
        );
        builder.symbol(
            "@_print_tuple_start",
            bolide_runtime::bolide_print_tuple_start as *const u8,
        );
        builder.symbol(
            "@_print_tuple_separator",
            bolide_runtime::bolide_print_tuple_separator as *const u8,
        );
        builder.symbol(
            "@_print_tuple_end_inline",
            bolide_runtime::bolide_print_tuple_end_inline as *const u8,
        );
        builder.symbol("@_println", bolide_runtime::bolide_println as *const u8);

        // 注册运行时函数 - 用户输入
        builder.symbol("@_input", bolide_runtime::bolide_input as *const u8);
        builder.symbol(
            "@_input_prompt",
            bolide_runtime::bolide_input_prompt as *const u8,
        );

        // 注册运行时函数 - BigInt
        builder.symbol(
            "@_bigint_from_i64",
            bolide_runtime::bolide_bigint_from_i64 as *const u8,
        );
        builder.symbol(
            "@_bigint_from_str",
            bolide_runtime::bolide_bigint_from_str as *const u8,
        );
        builder.symbol(
            "@_bigint_add",
            bolide_runtime::bolide_bigint_add as *const u8,
        );
        builder.symbol(
            "@_bigint_sub",
            bolide_runtime::bolide_bigint_sub as *const u8,
        );
        builder.symbol(
            "@_bigint_mul",
            bolide_runtime::bolide_bigint_mul as *const u8,
        );
        builder.symbol(
            "@_bigint_div",
            bolide_runtime::bolide_bigint_div as *const u8,
        );
        builder.symbol(
            "@_bigint_rem",
            bolide_runtime::bolide_bigint_rem as *const u8,
        );
        builder.symbol(
            "@_bigint_neg",
            bolide_runtime::bolide_bigint_neg as *const u8,
        );
        builder.symbol("@_bigint_eq", bolide_runtime::bolide_bigint_eq as *const u8);
        builder.symbol("@_bigint_lt", bolide_runtime::bolide_bigint_lt as *const u8);
        builder.symbol("@_bigint_le", bolide_runtime::bolide_bigint_le as *const u8);
        builder.symbol("@_bigint_gt", bolide_runtime::bolide_bigint_gt as *const u8);
        builder.symbol("@_bigint_ge", bolide_runtime::bolide_bigint_ge as *const u8);
        builder.symbol(
            "@_bigint_to_i64",
            bolide_runtime::bolide_bigint_to_i64 as *const u8,
        );
        builder.symbol(
            "@_bigint_clone",
            bolide_runtime::bolide_bigint_clone as *const u8,
        );
        builder.symbol(
            "@_bigint_debug_stats",
            bolide_runtime::bolide_bigint_debug_stats as *const u8,
        );

        // 注册运行时函数 - Decimal
        builder.symbol(
            "@_decimal_from_i64",
            bolide_runtime::bolide_decimal_from_i64 as *const u8,
        );
        builder.symbol(
            "@_decimal_from_f64",
            bolide_runtime::bolide_decimal_from_f64 as *const u8,
        );
        builder.symbol(
            "@_decimal_from_str",
            bolide_runtime::bolide_decimal_from_str as *const u8,
        );
        builder.symbol(
            "@_decimal_add",
            bolide_runtime::bolide_decimal_add as *const u8,
        );
        builder.symbol(
            "@_decimal_sub",
            bolide_runtime::bolide_decimal_sub as *const u8,
        );
        builder.symbol(
            "@_decimal_mul",
            bolide_runtime::bolide_decimal_mul as *const u8,
        );
        builder.symbol(
            "@_decimal_div",
            bolide_runtime::bolide_decimal_div as *const u8,
        );
        builder.symbol(
            "@_decimal_neg",
            bolide_runtime::bolide_decimal_neg as *const u8,
        );
        builder.symbol(
            "@_decimal_eq",
            bolide_runtime::bolide_decimal_eq as *const u8,
        );
        builder.symbol(
            "@_decimal_lt",
            bolide_runtime::bolide_decimal_lt as *const u8,
        );
        builder.symbol(
            "@_decimal_to_i64",
            bolide_runtime::bolide_decimal_to_i64 as *const u8,
        );
        builder.symbol(
            "@_decimal_to_f64",
            bolide_runtime::bolide_decimal_to_f64 as *const u8,
        );
        builder.symbol(
            "@_decimal_clone",
            bolide_runtime::bolide_decimal_clone as *const u8,
        );

        // 注册运行时函数 - Dynamic
        builder.symbol(
            "@_dynamic_from_int",
            bolide_runtime::bolide_dynamic_from_int as *const u8,
        );
        builder.symbol(
            "@_dynamic_from_float",
            bolide_runtime::bolide_dynamic_from_float as *const u8,
        );
        builder.symbol(
            "@_dynamic_from_bool",
            bolide_runtime::bolide_dynamic_from_bool as *const u8,
        );
        builder.symbol(
            "@_dynamic_from_string",
            bolide_runtime::bolide_dynamic_from_string as *const u8,
        );
        builder.symbol(
            "@_dynamic_from_list",
            bolide_runtime::bolide_dynamic_from_list as *const u8,
        );
        builder.symbol(
            "@_dynamic_from_bytes",
            bolide_runtime::bolide_dynamic_from_bytes as *const u8,
        );
        builder.symbol(
            "@_dynamic_from_dict",
            bolide_runtime::bolide_dynamic_from_dict as *const u8,
        );
        builder.symbol(
            "@_dynamic_from_bigint",
            bolide_runtime::bolide_dynamic_from_bigint as *const u8,
        );
        builder.symbol(
            "@_dynamic_from_decimal",
            bolide_runtime::bolide_dynamic_from_decimal as *const u8,
        );
        builder.symbol(
            "@_dynamic_to_int",
            bolide_runtime::bolide_dynamic_to_int as *const u8,
        );
        builder.symbol(
            "@_dynamic_to_float",
            bolide_runtime::bolide_dynamic_to_float as *const u8,
        );
        builder.symbol(
            "@_dynamic_to_string",
            bolide_runtime::bolide_dynamic_to_string as *const u8,
        );
        builder.symbol(
            "@_dynamic_add",
            bolide_runtime::bolide_dynamic_add as *const u8,
        );
        builder.symbol(
            "@_dynamic_sub",
            bolide_runtime::bolide_dynamic_sub as *const u8,
        );
        builder.symbol(
            "@_dynamic_mul",
            bolide_runtime::bolide_dynamic_mul as *const u8,
        );
        builder.symbol(
            "@_dynamic_div",
            bolide_runtime::bolide_dynamic_div as *const u8,
        );
        builder.symbol(
            "@_dynamic_neg",
            bolide_runtime::bolide_dynamic_neg as *const u8,
        );
        builder.symbol(
            "@_dynamic_eq",
            bolide_runtime::bolide_dynamic_eq as *const u8,
        );
        builder.symbol(
            "@_dynamic_lt",
            bolide_runtime::bolide_dynamic_lt as *const u8,
        );
        builder.symbol(
            "@_dynamic_clone",
            bolide_runtime::bolide_dynamic_clone as *const u8,
        );

        // 注册字符串函数
        builder.symbol(
            "@_bolide_string_new",
            bolide_runtime::bolide_string_new as *const u8,
        );
        builder.symbol(
            "@_string_from_slice",
            bolide_runtime::bolide_string_from_slice as *const u8,
        );
        builder.symbol(
            "@_string_literal",
            bolide_runtime::bolide_string_literal as *const u8,
        );
        builder.symbol(
            "@_string_as_cstr",
            bolide_runtime::bolide_string_as_cstr as *const u8,
        );
        builder.symbol(
            "@_string_concat",
            bolide_runtime::bolide_string_concat as *const u8,
        );
        builder.symbol(
            "@_string_concat_many",
            bolide_runtime::bolide_string_concat_many as *const u8,
        );
        builder.symbol("@_string_eq", bolide_runtime::bolide_string_eq as *const u8);

        // 注册类型转换函数
        builder.symbol(
            "@_string_from_int",
            bolide_runtime::bolide_string_from_int as *const u8,
        );
        builder.symbol(
            "@_string_from_float",
            bolide_runtime::bolide_string_from_float as *const u8,
        );
        builder.symbol(
            "@_string_from_bool",
            bolide_runtime::bolide_string_from_bool as *const u8,
        );
        builder.symbol(
            "@_string_from_bigint",
            bolide_runtime::bolide_string_from_bigint as *const u8,
        );
        builder.symbol(
            "@_string_from_decimal",
            bolide_runtime::bolide_string_from_decimal as *const u8,
        );
        builder.symbol(
            "@_string_to_int",
            bolide_runtime::bolide_string_to_int as *const u8,
        );
        builder.symbol(
            "@_string_to_float",
            bolide_runtime::bolide_string_to_float as *const u8,
        );

        // 注册内存分配函数
        builder.symbol("@_bolide_alloc", bolide_runtime::bolide_alloc as *const u8);
        builder.symbol("@_bolide_free", bolide_runtime::bolide_free as *const u8);

        // 注册对象运行时函数
        builder.symbol("@_object_alloc", bolide_runtime::object_alloc as *const u8);
        builder.symbol(
            "@_object_retain",
            bolide_runtime::object_retain as *const u8,
        );
        builder.symbol(
            "@_object_set_class_tag",
            bolide_runtime::object_set_class_tag as *const u8,
        );
        builder.symbol(
            "@_object_class_tag",
            bolide_runtime::object_class_tag as *const u8,
        );
        builder.symbol(
            "@_object_release",
            bolide_runtime::object_release as *const u8,
        );
        builder.symbol("@_object_clone", bolide_runtime::object_clone as *const u8);
        builder.symbol(
            "@_object_weak_retain",
            bolide_runtime::object_weak_retain as *const u8,
        );
        builder.symbol(
            "@_object_weak_release",
            bolide_runtime::object_weak_release as *const u8,
        );
        builder.symbol(
            "@_object_weak_clone",
            bolide_runtime::object_weak_clone as *const u8,
        );
        builder.symbol(
            "@_object_assert_alive",
            bolide_runtime::object_assert_alive as *const u8,
        );
        builder.symbol(
            "@_object_is_alive",
            bolide_runtime::object_is_alive as *const u8,
        );
        builder.symbol(
            "@_object_ref_count",
            bolide_runtime::object_ref_count as *const u8,
        );

        // 注册闭包运行时函数
        builder.symbol(
            "@_closure_new",
            bolide_runtime::bolide_closure_new as *const u8,
        );
        builder.symbol(
            "@_closure_fn_ptr",
            bolide_runtime::bolide_closure_fn_ptr as *const u8,
        );
        builder.symbol(
            "@_closure_env_ptr",
            bolide_runtime::bolide_closure_env_ptr as *const u8,
        );
        builder.symbol(
            "@_closure_retain",
            bolide_runtime::bolide_closure_retain as *const u8,
        );
        builder.symbol(
            "@_closure_release",
            bolide_runtime::bolide_closure_release as *const u8,
        );

        // 注册运行时函数 - 线程（无参版本）
        builder.symbol(
            "@_thread_spawn_int",
            bolide_runtime::bolide_thread_spawn_int as *const u8,
        );
        builder.symbol(
            "@_thread_spawn_float",
            bolide_runtime::bolide_thread_spawn_float as *const u8,
        );
        builder.symbol(
            "@_thread_spawn_ptr",
            bolide_runtime::bolide_thread_spawn_ptr as *const u8,
        );
        // 注册运行时函数 - 线程（带环境版本，用于带参数的 spawn）
        builder.symbol(
            "@_thread_spawn_int_with_env",
            bolide_runtime::bolide_thread_spawn_int_with_env as *const u8,
        );
        builder.symbol(
            "@_thread_spawn_float_with_env",
            bolide_runtime::bolide_thread_spawn_float_with_env as *const u8,
        );
        builder.symbol(
            "@_thread_spawn_ptr_with_env",
            bolide_runtime::bolide_thread_spawn_ptr_with_env as *const u8,
        );
        builder.symbol(
            "@_thread_join_int",
            bolide_runtime::bolide_thread_join_int as *const u8,
        );
        builder.symbol(
            "@_thread_join_float",
            bolide_runtime::bolide_thread_join_float as *const u8,
        );
        builder.symbol(
            "@_thread_join_ptr",
            bolide_runtime::bolide_thread_join_ptr as *const u8,
        );
        builder.symbol(
            "@_thread_handle_free",
            bolide_runtime::bolide_thread_handle_free as *const u8,
        );
        builder.symbol(
            "@_thread_cancel",
            bolide_runtime::bolide_thread_cancel as *const u8,
        );
        builder.symbol(
            "@_thread_is_cancelled",
            bolide_runtime::bolide_thread_is_cancelled as *const u8,
        );

        // 注册运行时函数 - 线程池（无参版本）
        builder.symbol(
            "@_pool_create",
            bolide_runtime::bolide_pool_create as *const u8,
        );
        builder.symbol(
            "@_pool_enter",
            bolide_runtime::bolide_pool_enter as *const u8,
        );
        builder.symbol("@_pool_exit", bolide_runtime::bolide_pool_exit as *const u8);
        builder.symbol(
            "@_pool_is_active",
            bolide_runtime::bolide_pool_is_active as *const u8,
        );
        builder.symbol(
            "@_pool_spawn_int",
            bolide_runtime::bolide_pool_spawn_int as *const u8,
        );
        builder.symbol(
            "@_pool_spawn_float",
            bolide_runtime::bolide_pool_spawn_float as *const u8,
        );
        builder.symbol(
            "@_pool_spawn_ptr",
            bolide_runtime::bolide_pool_spawn_ptr as *const u8,
        );
        // 注册运行时函数 - 线程池（带环境版本）
        builder.symbol(
            "@_pool_spawn_int_with_env",
            bolide_runtime::bolide_pool_spawn_int_with_env as *const u8,
        );
        builder.symbol(
            "@_pool_spawn_float_with_env",
            bolide_runtime::bolide_pool_spawn_float_with_env as *const u8,
        );
        builder.symbol(
            "@_pool_spawn_ptr_with_env",
            bolide_runtime::bolide_pool_spawn_ptr_with_env as *const u8,
        );
        builder.symbol(
            "@_pool_join_int",
            bolide_runtime::bolide_pool_join_int as *const u8,
        );
        builder.symbol(
            "@_pool_join_float",
            bolide_runtime::bolide_pool_join_float as *const u8,
        );
        builder.symbol(
            "@_pool_join_ptr",
            bolide_runtime::bolide_pool_join_ptr as *const u8,
        );
        builder.symbol(
            "@_pool_handle_free",
            bolide_runtime::bolide_pool_handle_free as *const u8,
        );
        builder.symbol(
            "@_pool_select_wait_first",
            bolide_runtime::bolide_pool_select_wait_first as *const u8,
        );
        builder.symbol(
            "@_pool_destroy",
            bolide_runtime::bolide_pool_destroy as *const u8,
        );

        // 注册运行时函数 - 通道
        builder.symbol(
            "@_channel_create",
            bolide_runtime::bolide_channel_create as *const u8,
        );
        builder.symbol(
            "@_channel_create_buffered",
            bolide_runtime::bolide_channel_create_buffered as *const u8,
        );
        builder.symbol(
            "@_channel_send",
            bolide_runtime::bolide_channel_send as *const u8,
        );
        builder.symbol(
            "@_channel_recv",
            bolide_runtime::bolide_channel_recv as *const u8,
        );
        builder.symbol(
            "@_channel_close",
            bolide_runtime::bolide_channel_close as *const u8,
        );
        builder.symbol(
            "@_channel_free",
            bolide_runtime::bolide_channel_free as *const u8,
        );
        builder.symbol(
            "@_channel_select",
            bolide_runtime::bolide_channel_select as *const u8,
        );

        // 注册运行时函数 - 协程
        builder.symbol(
            "@_coroutine_spawn_int",
            bolide_runtime::bolide_coroutine_spawn_int as *const u8,
        );
        builder.symbol(
            "@_coroutine_spawn_float",
            bolide_runtime::bolide_coroutine_spawn_float as *const u8,
        );
        builder.symbol(
            "@_coroutine_spawn_ptr",
            bolide_runtime::bolide_coroutine_spawn_ptr as *const u8,
        );
        builder.symbol(
            "@_coroutine_await_int",
            bolide_runtime::bolide_coroutine_await_int as *const u8,
        );
        builder.symbol(
            "@_coroutine_await_float",
            bolide_runtime::bolide_coroutine_await_float as *const u8,
        );
        builder.symbol(
            "@_coroutine_await_ptr",
            bolide_runtime::bolide_coroutine_await_ptr as *const u8,
        );
        builder.symbol(
            "@_coroutine_cancel",
            bolide_runtime::bolide_coroutine_cancel as *const u8,
        );
        builder.symbol(
            "@_coroutine_free",
            bolide_runtime::bolide_coroutine_free as *const u8,
        );
        builder.symbol(
            "@_coroutine_spawn_int_with_env",
            bolide_runtime::bolide_coroutine_spawn_int_with_env as *const u8,
        );
        builder.symbol(
            "@_coroutine_spawn_float_with_env",
            bolide_runtime::bolide_coroutine_spawn_float_with_env as *const u8,
        );
        builder.symbol(
            "@_coroutine_spawn_ptr_with_env",
            bolide_runtime::bolide_coroutine_spawn_ptr_with_env as *const u8,
        );
        builder.symbol(
            "@_scope_enter",
            bolide_runtime::bolide_scope_enter as *const u8,
        );
        builder.symbol(
            "@_scope_register",
            bolide_runtime::bolide_scope_register as *const u8,
        );
        builder.symbol(
            "@_scope_exit",
            bolide_runtime::bolide_scope_exit as *const u8,
        );

        // 注册运行时函数 - select
        builder.symbol(
            "@_select_wait_first",
            bolide_runtime::bolide_select_wait_first as *const u8,
        );

        // 注册运行时函数 - 元组
        builder.symbol("@_tuple_new", bolide_runtime::bolide_tuple_new as *const u8);
        builder.symbol(
            "@_tuple_new_typed",
            bolide_runtime::bolide_tuple_new_typed as *const u8,
        );
        builder.symbol(
            "@_tuple_free",
            bolide_runtime::bolide_tuple_free as *const u8,
        );
        builder.symbol("@_tuple_set", bolide_runtime::bolide_tuple_set as *const u8);
        builder.symbol(
            "@_tuple_set_typed",
            bolide_runtime::bolide_tuple_set_typed as *const u8,
        );
        builder.symbol("@_tuple_get", bolide_runtime::bolide_tuple_get as *const u8);
        builder.symbol("@_tuple_len", bolide_runtime::bolide_tuple_len as *const u8);
        builder.symbol(
            "@_tuple_slice_step",
            bolide_runtime::bolide_tuple_slice_step as *const u8,
        );
        builder.symbol(
            "@_tuple_retain",
            bolide_runtime::bolide_tuple_retain as *const u8,
        );
        builder.symbol(
            "@_tuple_clone",
            bolide_runtime::bolide_tuple_clone as *const u8,
        );
        builder.symbol(
            "@_tuple_release",
            bolide_runtime::bolide_tuple_release as *const u8,
        );
        builder.symbol(
            "@_tuple_get_type",
            bolide_runtime::bolide_tuple_get_type as *const u8,
        );
        builder.symbol(
            "@_tuple_debug_stats",
            bolide_runtime::bolide_tuple_debug_stats as *const u8,
        );
        builder.symbol(
            "@_print_tuple",
            bolide_runtime::bolide_print_tuple as *const u8,
        );

        builder.symbol(
            "@_ffi_load_library",
            bolide_runtime::bolide_ffi_load_library as *const u8,
        );
        builder.symbol(
            "@_ffi_get_symbol",
            bolide_runtime::bolide_ffi_get_symbol as *const u8,
        );
        builder.symbol(
            "@_test_callback",
            bolide_runtime::bolide_test_callback as *const u8,
        );
        builder.symbol("@_map_int", bolide_runtime::bolide_map_int as *const u8);
        builder.symbol(
            "bolide_fs_read_text",
            bolide_runtime::bolide_fs_read_text as *const u8,
        );
        builder.symbol(
            "bolide_fs_read_bytes",
            bolide_runtime::bolide_fs_read_bytes as *const u8,
        );
        builder.symbol(
            "bolide_fs_read_lines",
            bolide_runtime::bolide_fs_read_lines as *const u8,
        );
        builder.symbol(
            "bolide_fs_write_text",
            bolide_runtime::bolide_fs_write_text as *const u8,
        );
        builder.symbol(
            "bolide_fs_write_bytes",
            bolide_runtime::bolide_fs_write_bytes as *const u8,
        );
        builder.symbol(
            "bolide_fs_append_text",
            bolide_runtime::bolide_fs_append_text as *const u8,
        );
        builder.symbol(
            "bolide_fs_append_bytes",
            bolide_runtime::bolide_fs_append_bytes as *const u8,
        );
        builder.symbol(
            "bolide_fs_touch",
            bolide_runtime::bolide_fs_touch as *const u8,
        );
        builder.symbol(
            "bolide_fs_exists",
            bolide_runtime::bolide_fs_exists as *const u8,
        );
        builder.symbol(
            "bolide_fs_is_file",
            bolide_runtime::bolide_fs_is_file as *const u8,
        );
        builder.symbol(
            "bolide_fs_is_dir",
            bolide_runtime::bolide_fs_is_dir as *const u8,
        );
        builder.symbol(
            "bolide_fs_is_symlink",
            bolide_runtime::bolide_fs_is_symlink as *const u8,
        );
        builder.symbol(
            "bolide_fs_remove_file",
            bolide_runtime::bolide_fs_remove_file as *const u8,
        );
        builder.symbol(
            "bolide_fs_copy",
            bolide_runtime::bolide_fs_copy as *const u8,
        );
        builder.symbol(
            "bolide_fs_rename",
            bolide_runtime::bolide_fs_rename as *const u8,
        );
        builder.symbol(
            "bolide_fs_create_dir",
            bolide_runtime::bolide_fs_create_dir as *const u8,
        );
        builder.symbol(
            "bolide_fs_create_dir_all",
            bolide_runtime::bolide_fs_create_dir_all as *const u8,
        );
        builder.symbol(
            "bolide_fs_remove_dir",
            bolide_runtime::bolide_fs_remove_dir as *const u8,
        );
        builder.symbol(
            "bolide_fs_remove_dir_all",
            bolide_runtime::bolide_fs_remove_dir_all as *const u8,
        );
        builder.symbol(
            "bolide_fs_read_dir",
            bolide_runtime::bolide_fs_read_dir as *const u8,
        );
        builder.symbol(
            "bolide_fs_file_name",
            bolide_runtime::bolide_fs_file_name as *const u8,
        );
        builder.symbol(
            "bolide_fs_parent",
            bolide_runtime::bolide_fs_parent as *const u8,
        );
        builder.symbol(
            "bolide_fs_extension",
            bolide_runtime::bolide_fs_extension as *const u8,
        );
        builder.symbol(
            "bolide_fs_stem",
            bolide_runtime::bolide_fs_stem as *const u8,
        );
        builder.symbol(
            "bolide_fs_join",
            bolide_runtime::bolide_fs_join as *const u8,
        );
        builder.symbol(
            "bolide_fs_canonicalize",
            bolide_runtime::bolide_fs_canonicalize as *const u8,
        );
        builder.symbol(
            "bolide_fs_current_dir",
            bolide_runtime::bolide_fs_current_dir as *const u8,
        );
        builder.symbol(
            "bolide_fs_set_current_dir",
            bolide_runtime::bolide_fs_set_current_dir as *const u8,
        );
        builder.symbol("bolide_fs_len", bolide_runtime::bolide_fs_len as *const u8);
        builder.symbol(
            "bolide_fs_modified",
            bolide_runtime::bolide_fs_modified as *const u8,
        );
        builder.symbol(
            "bolide_fs_created",
            bolide_runtime::bolide_fs_created as *const u8,
        );
        builder.symbol(
            "bolide_fs_readonly",
            bolide_runtime::bolide_fs_readonly as *const u8,
        );
        builder.symbol(
            "bolide_fs_set_readonly",
            bolide_runtime::bolide_fs_set_readonly as *const u8,
        );
        builder.symbol(
            "bolide_web_app_new",
            bolide_runtime::bolide_web_app_new as *const u8,
        );
        builder.symbol(
            "bolide_web_app_free",
            bolide_runtime::bolide_web_app_free as *const u8,
        );
        builder.symbol(
            "bolide_web_app_set_workers",
            bolide_runtime::bolide_web_app_set_workers as *const u8,
        );
        builder.symbol(
            "bolide_web_app_set_max_body",
            bolide_runtime::bolide_web_app_set_max_body as *const u8,
        );
        builder.symbol(
            "bolide_web_route",
            bolide_runtime::bolide_web_route as *const u8,
        );
        builder.symbol(
            "bolide_web_route_handler",
            bolide_runtime::bolide_web_route_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_route_async_handler",
            bolide_runtime::bolide_web_route_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_static",
            bolide_runtime::bolide_web_static as *const u8,
        );
        builder.symbol(
            "bolide_web_get",
            bolide_runtime::bolide_web_get as *const u8,
        );
        builder.symbol(
            "bolide_web_get_handler",
            bolide_runtime::bolide_web_get_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_get_async_handler",
            bolide_runtime::bolide_web_get_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_post",
            bolide_runtime::bolide_web_post as *const u8,
        );
        builder.symbol(
            "bolide_web_post_handler",
            bolide_runtime::bolide_web_post_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_post_async_handler",
            bolide_runtime::bolide_web_post_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_put",
            bolide_runtime::bolide_web_put as *const u8,
        );
        builder.symbol(
            "bolide_web_put_handler",
            bolide_runtime::bolide_web_put_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_put_async_handler",
            bolide_runtime::bolide_web_put_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_patch",
            bolide_runtime::bolide_web_patch as *const u8,
        );
        builder.symbol(
            "bolide_web_patch_handler",
            bolide_runtime::bolide_web_patch_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_patch_async_handler",
            bolide_runtime::bolide_web_patch_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_delete",
            bolide_runtime::bolide_web_delete as *const u8,
        );
        builder.symbol(
            "bolide_web_delete_handler",
            bolide_runtime::bolide_web_delete_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_delete_async_handler",
            bolide_runtime::bolide_web_delete_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_head",
            bolide_runtime::bolide_web_head as *const u8,
        );
        builder.symbol(
            "bolide_web_head_handler",
            bolide_runtime::bolide_web_head_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_head_async_handler",
            bolide_runtime::bolide_web_head_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_options",
            bolide_runtime::bolide_web_options as *const u8,
        );
        builder.symbol(
            "bolide_web_options_handler",
            bolide_runtime::bolide_web_options_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_options_async_handler",
            bolide_runtime::bolide_web_options_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_trace",
            bolide_runtime::bolide_web_trace as *const u8,
        );
        builder.symbol(
            "bolide_web_trace_handler",
            bolide_runtime::bolide_web_trace_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_trace_async_handler",
            bolide_runtime::bolide_web_trace_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_connect",
            bolide_runtime::bolide_web_connect as *const u8,
        );
        builder.symbol(
            "bolide_web_connect_handler",
            bolide_runtime::bolide_web_connect_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_connect_async_handler",
            bolide_runtime::bolide_web_connect_async_handler as *const u8,
        );
        builder.symbol(
            "bolide_web_run",
            bolide_runtime::bolide_web_run as *const u8,
        );
        builder.symbol(
            "bolide_web_serve",
            bolide_runtime::bolide_web_serve as *const u8,
        );
        builder.symbol(
            "bolide_web_app_handle",
            bolide_runtime::bolide_web_app_handle as *const u8,
        );
        builder.symbol(
            "bolide_web_app_handle_with_headers",
            bolide_runtime::bolide_web_app_handle_with_headers as *const u8,
        );
        builder.symbol(
            "bolide_web_cookie_pair",
            bolide_runtime::bolide_web_cookie_pair as *const u8,
        );
        builder.symbol(
            "bolide_web_request_method",
            bolide_runtime::bolide_web_request_method as *const u8,
        );
        builder.symbol(
            "bolide_web_request_target",
            bolide_runtime::bolide_web_request_target as *const u8,
        );
        builder.symbol(
            "bolide_web_request_path",
            bolide_runtime::bolide_web_request_path as *const u8,
        );
        builder.symbol(
            "bolide_web_request_query",
            bolide_runtime::bolide_web_request_query as *const u8,
        );
        builder.symbol(
            "bolide_web_request_version",
            bolide_runtime::bolide_web_request_version as *const u8,
        );
        builder.symbol(
            "bolide_web_request_header",
            bolide_runtime::bolide_web_request_header as *const u8,
        );
        builder.symbol(
            "bolide_web_request_header_str",
            bolide_runtime::bolide_web_request_header_str as *const u8,
        );
        builder.symbol(
            "bolide_web_request_cookie",
            bolide_runtime::bolide_web_request_cookie as *const u8,
        );
        builder.symbol(
            "bolide_web_request_cookie_str",
            bolide_runtime::bolide_web_request_cookie_str as *const u8,
        );
        builder.symbol(
            "bolide_web_request_query_param",
            bolide_runtime::bolide_web_request_query_param as *const u8,
        );
        builder.symbol(
            "bolide_web_request_query_param_str",
            bolide_runtime::bolide_web_request_query_param_str as *const u8,
        );
        builder.symbol(
            "bolide_web_request_form_param",
            bolide_runtime::bolide_web_request_form_param as *const u8,
        );
        builder.symbol(
            "bolide_web_request_form_param_str",
            bolide_runtime::bolide_web_request_form_param_str as *const u8,
        );
        builder.symbol(
            "bolide_web_request_path_param",
            bolide_runtime::bolide_web_request_path_param as *const u8,
        );
        builder.symbol(
            "bolide_web_request_path_param_str",
            bolide_runtime::bolide_web_request_path_param_str as *const u8,
        );
        builder.symbol(
            "bolide_web_request_body_text",
            bolide_runtime::bolide_web_request_body_text as *const u8,
        );
        builder.symbol(
            "bolide_web_request_body_bytes",
            bolide_runtime::bolide_web_request_body_bytes as *const u8,
        );
        builder.symbol(
            "bolide_web_request_body_len",
            bolide_runtime::bolide_web_request_body_len as *const u8,
        );
        builder.symbol(
            "bolide_web_response_new",
            bolide_runtime::bolide_web_response_new as *const u8,
        );
        builder.symbol(
            "bolide_web_response_new_str",
            bolide_runtime::bolide_web_response_new_str as *const u8,
        );
        builder.symbol(
            "bolide_web_text",
            bolide_runtime::bolide_web_text as *const u8,
        );
        builder.symbol(
            "bolide_web_text_str",
            bolide_runtime::bolide_web_text_str as *const u8,
        );
        builder.symbol(
            "bolide_web_html",
            bolide_runtime::bolide_web_html as *const u8,
        );
        builder.symbol(
            "bolide_web_html_str",
            bolide_runtime::bolide_web_html_str as *const u8,
        );
        builder.symbol(
            "bolide_web_json",
            bolide_runtime::bolide_web_json as *const u8,
        );
        builder.symbol(
            "bolide_web_json_str",
            bolide_runtime::bolide_web_json_str as *const u8,
        );
        builder.symbol(
            "bolide_web_bytes",
            bolide_runtime::bolide_web_bytes as *const u8,
        );
        builder.symbol(
            "bolide_web_empty",
            bolide_runtime::bolide_web_empty as *const u8,
        );
        builder.symbol(
            "bolide_web_redirect",
            bolide_runtime::bolide_web_redirect as *const u8,
        );
        builder.symbol(
            "bolide_web_redirect_str",
            bolide_runtime::bolide_web_redirect_str as *const u8,
        );
        builder.symbol(
            "bolide_web_response_set_status",
            bolide_runtime::bolide_web_response_set_status as *const u8,
        );
        builder.symbol(
            "bolide_web_response_set_header",
            bolide_runtime::bolide_web_response_set_header as *const u8,
        );
        builder.symbol(
            "bolide_web_response_set_header_str",
            bolide_runtime::bolide_web_response_set_header_str as *const u8,
        );
        builder.symbol(
            "bolide_web_response_set_cookie",
            bolide_runtime::bolide_web_response_set_cookie as *const u8,
        );
        builder.symbol(
            "bolide_web_response_delete_cookie",
            bolide_runtime::bolide_web_response_delete_cookie as *const u8,
        );
        builder.symbol(
            "bolide_web_response_status",
            bolide_runtime::bolide_web_response_status as *const u8,
        );
        builder.symbol(
            "bolide_web_response_header",
            bolide_runtime::bolide_web_response_header as *const u8,
        );
        builder.symbol(
            "bolide_web_response_header_str",
            bolide_runtime::bolide_web_response_header_str as *const u8,
        );
        builder.symbol(
            "bolide_web_response_cookie_pair",
            bolide_runtime::bolide_web_response_cookie_pair as *const u8,
        );
        builder.symbol(
            "bolide_web_response_body_text",
            bolide_runtime::bolide_web_response_body_text as *const u8,
        );
        builder.symbol(
            "bolide_web_response_body_bytes",
            bolide_runtime::bolide_web_response_body_bytes as *const u8,
        );
        builder.symbol(
            "bolide_web_response_free",
            bolide_runtime::bolide_web_response_free as *const u8,
        );
        builder.symbol(
            "bolide_web_session",
            bolide_runtime::bolide_web_session as *const u8,
        );
        builder.symbol(
            "bolide_web_session_id",
            bolide_runtime::bolide_web_session_id as *const u8,
        );
        builder.symbol(
            "bolide_web_session_get",
            bolide_runtime::bolide_web_session_get as *const u8,
        );
        builder.symbol(
            "bolide_web_session_set",
            bolide_runtime::bolide_web_session_set as *const u8,
        );
        builder.symbol(
            "bolide_web_session_contains",
            bolide_runtime::bolide_web_session_contains as *const u8,
        );
        builder.symbol(
            "bolide_web_session_remove",
            bolide_runtime::bolide_web_session_remove as *const u8,
        );
        builder.symbol(
            "bolide_web_session_clear",
            bolide_runtime::bolide_web_session_clear as *const u8,
        );
        builder.symbol(
            "bolide_web_session_destroy",
            bolide_runtime::bolide_web_session_destroy as *const u8,
        );
        builder.symbol(
            "bolide_web_session_regenerate",
            bolide_runtime::bolide_web_session_regenerate as *const u8,
        );
        builder.symbol(
            "bolide_web_session_free",
            bolide_runtime::bolide_web_session_free as *const u8,
        );
        builder.symbol(
            "bolide_template_escape_html",
            bolide_runtime::bolide_template_escape_html as *const u8,
        );
        builder.symbol(
            "bolide_template_render",
            bolide_runtime::bolide_template_render as *const u8,
        );
        builder.symbol(
            "bolide_template_render_file",
            bolide_runtime::bolide_template_render_file as *const u8,
        );
        builder.symbol(
            "bolide_db_open",
            bolide_runtime::bolide_db_open as *const u8,
        );
        builder.symbol(
            "bolide_db_close",
            bolide_runtime::bolide_db_close as *const u8,
        );
        builder.symbol(
            "bolide_db_last_error",
            bolide_runtime::bolide_db_last_error as *const u8,
        );
        builder.symbol(
            "bolide_db_create_table",
            bolide_runtime::bolide_db_create_table as *const u8,
        );
        builder.symbol(
            "bolide_db_insert",
            bolide_runtime::bolide_db_insert as *const u8,
        );
        builder.symbol(
            "bolide_db_update",
            bolide_runtime::bolide_db_update as *const u8,
        );
        builder.symbol(
            "bolide_db_delete",
            bolide_runtime::bolide_db_delete as *const u8,
        );
        builder.symbol("bolide_db_get", bolide_runtime::bolide_db_get as *const u8);
        builder.symbol("bolide_db_all", bolide_runtime::bolide_db_all as *const u8);
        builder.symbol(
            "bolide_db_where_eq",
            bolide_runtime::bolide_db_where_eq as *const u8,
        );
        builder.symbol(
            "bolide_db_count",
            bolide_runtime::bolide_db_count as *const u8,
        );
        builder.symbol(
            "bolide_gui_backend",
            bolide_runtime::bolide_gui_backend as *const u8,
        );
        builder.symbol(
            "bolide_gui_run",
            bolide_runtime::bolide_gui_run as *const u8,
        );
        builder.symbol(
            "bolide_gui_label",
            bolide_runtime::bolide_gui_label as *const u8,
        );
        builder.symbol(
            "bolide_gui_heading",
            bolide_runtime::bolide_gui_heading as *const u8,
        );
        builder.symbol(
            "bolide_gui_small",
            bolide_runtime::bolide_gui_small as *const u8,
        );
        builder.symbol(
            "bolide_gui_strong",
            bolide_runtime::bolide_gui_strong as *const u8,
        );
        builder.symbol(
            "bolide_gui_separator",
            bolide_runtime::bolide_gui_separator as *const u8,
        );
        builder.symbol(
            "bolide_gui_space",
            bolide_runtime::bolide_gui_space as *const u8,
        );
        builder.symbol(
            "bolide_gui_button",
            bolide_runtime::bolide_gui_button as *const u8,
        );
        builder.symbol(
            "bolide_gui_selectable",
            bolide_runtime::bolide_gui_selectable as *const u8,
        );
        builder.symbol(
            "bolide_gui_link",
            bolide_runtime::bolide_gui_link as *const u8,
        );
        builder.symbol(
            "bolide_gui_text_input",
            bolide_runtime::bolide_gui_text_input as *const u8,
        );
        builder.symbol(
            "bolide_gui_password_input",
            bolide_runtime::bolide_gui_password_input as *const u8,
        );
        builder.symbol(
            "bolide_gui_multiline_input",
            bolide_runtime::bolide_gui_multiline_input as *const u8,
        );
        builder.symbol(
            "bolide_gui_checkbox",
            bolide_runtime::bolide_gui_checkbox as *const u8,
        );
        builder.symbol(
            "bolide_gui_slider",
            bolide_runtime::bolide_gui_slider as *const u8,
        );
        builder.symbol(
            "bolide_gui_progress",
            bolide_runtime::bolide_gui_progress as *const u8,
        );
        builder.symbol(
            "bolide_gui_pack",
            bolide_runtime::bolide_gui_pack as *const u8,
        );
        builder.symbol(
            "bolide_gui_row",
            bolide_runtime::bolide_gui_row as *const u8,
        );
        builder.symbol(
            "bolide_gui_column",
            bolide_runtime::bolide_gui_column as *const u8,
        );
        builder.symbol(
            "bolide_gui_group",
            bolide_runtime::bolide_gui_group as *const u8,
        );
        builder.symbol(
            "bolide_gui_grid",
            bolide_runtime::bolide_gui_grid as *const u8,
        );
        builder.symbol(
            "bolide_gui_end_row",
            bolide_runtime::bolide_gui_end_row as *const u8,
        );
        builder.symbol(
            "bolide_gui_frame",
            bolide_runtime::bolide_gui_frame as *const u8,
        );
        builder.symbol(
            "bolide_gui_scroll",
            bolide_runtime::bolide_gui_scroll as *const u8,
        );
        builder.symbol(
            "bolide_gui_indent",
            bolide_runtime::bolide_gui_indent as *const u8,
        );
        builder.symbol(
            "bolide_gui_centered",
            bolide_runtime::bolide_gui_centered as *const u8,
        );
        builder.symbol(
            "bolide_gui_align",
            bolide_runtime::bolide_gui_align as *const u8,
        );
        builder.symbol(
            "bolide_gui_pad",
            bolide_runtime::bolide_gui_pad as *const u8,
        );
        builder.symbol(
            "bolide_gui_width",
            bolide_runtime::bolide_gui_width as *const u8,
        );
        builder.symbol(
            "bolide_gui_height",
            bolide_runtime::bolide_gui_height as *const u8,
        );
        builder.symbol(
            "bolide_gui_size",
            bolide_runtime::bolide_gui_size as *const u8,
        );
        builder.symbol(
            "bolide_gui_fill_width",
            bolide_runtime::bolide_gui_fill_width as *const u8,
        );
        builder.symbol(
            "bolide_gui_fill_height",
            bolide_runtime::bolide_gui_fill_height as *const u8,
        );
        builder.symbol(
            "bolide_gui_fill",
            bolide_runtime::bolide_gui_fill as *const u8,
        );
        builder.symbol(
            "bolide_gui_place",
            bolide_runtime::bolide_gui_place as *const u8,
        );
        builder.symbol(
            "bolide_gui_collapsing",
            bolide_runtime::bolide_gui_collapsing as *const u8,
        );
        builder.symbol(
            "bolide_gui_available_width",
            bolide_runtime::bolide_gui_available_width as *const u8,
        );
        builder.symbol(
            "bolide_gui_available_height",
            bolide_runtime::bolide_gui_available_height as *const u8,
        );
        builder.symbol(
            "bolide_gui_request_repaint",
            bolide_runtime::bolide_gui_request_repaint as *const u8,
        );

        // 注册运行时函数 - RC 引用计数管理
        builder.symbol(
            "@_string_retain",
            bolide_runtime::bolide_string_retain as *const u8,
        );
        builder.symbol(
            "@_string_release",
            bolide_runtime::bolide_string_release as *const u8,
        );
        builder.symbol(
            "@_string_clone",
            bolide_runtime::bolide_string_clone as *const u8,
        );
        builder.symbol(
            "@_string_len",
            bolide_runtime::bolide_string_len as *const u8,
        );
        builder.symbol("@_bytes_new", bolide_runtime::bolide_bytes_new as *const u8);
        builder.symbol(
            "@_bytes_retain",
            bolide_runtime::bolide_bytes_retain as *const u8,
        );
        builder.symbol(
            "@_bytes_release",
            bolide_runtime::bolide_bytes_release as *const u8,
        );
        builder.symbol(
            "@_bytes_clone",
            bolide_runtime::bolide_bytes_clone as *const u8,
        );
        builder.symbol("@_bytes_len", bolide_runtime::bolide_bytes_len as *const u8);
        builder.symbol("@_bytes_get", bolide_runtime::bolide_bytes_get as *const u8);
        builder.symbol("@_bytes_set", bolide_runtime::bolide_bytes_set as *const u8);
        builder.symbol(
            "@_bytes_push",
            bolide_runtime::bolide_bytes_push as *const u8,
        );
        builder.symbol(
            "@_bytes_to_string_lossy",
            bolide_runtime::bolide_bytes_to_string_lossy as *const u8,
        );
        // 字符串切片 / 索引 / 完整方法集
        builder.symbol(
            "@_string_slice",
            bolide_runtime::bolide_string_slice as *const u8,
        );
        builder.symbol(
            "@_string_char_at",
            bolide_runtime::bolide_string_char_at as *const u8,
        );
        builder.symbol(
            "@_string_upper",
            bolide_runtime::bolide_string_upper as *const u8,
        );
        builder.symbol(
            "@_string_lower",
            bolide_runtime::bolide_string_lower as *const u8,
        );
        builder.symbol(
            "@_string_trim",
            bolide_runtime::bolide_string_trim as *const u8,
        );
        builder.symbol(
            "@_string_replace",
            bolide_runtime::bolide_string_replace as *const u8,
        );
        builder.symbol(
            "@_string_repeat",
            bolide_runtime::bolide_string_repeat as *const u8,
        );
        builder.symbol(
            "@_string_find",
            bolide_runtime::bolide_string_find as *const u8,
        );
        builder.symbol(
            "@_string_contains",
            bolide_runtime::bolide_string_contains as *const u8,
        );
        builder.symbol(
            "@_string_starts_with",
            bolide_runtime::bolide_string_starts_with as *const u8,
        );
        builder.symbol(
            "@_string_ends_with",
            bolide_runtime::bolide_string_ends_with as *const u8,
        );
        builder.symbol(
            "@_string_count",
            bolide_runtime::bolide_string_count as *const u8,
        );
        builder.symbol(
            "@_string_split",
            bolide_runtime::bolide_string_split as *const u8,
        );
        builder.symbol(
            "@_bigint_retain",
            bolide_runtime::bolide_bigint_retain as *const u8,
        );
        builder.symbol(
            "@_bigint_release",
            bolide_runtime::bolide_bigint_release as *const u8,
        );
        builder.symbol(
            "@_decimal_retain",
            bolide_runtime::bolide_decimal_retain as *const u8,
        );
        builder.symbol(
            "@_decimal_release",
            bolide_runtime::bolide_decimal_release as *const u8,
        );
        builder.symbol(
            "@_list_retain",
            bolide_runtime::bolide_list_retain as *const u8,
        );
        builder.symbol(
            "@_list_release",
            bolide_runtime::bolide_list_release as *const u8,
        );
        builder.symbol(
            "@_list_clone",
            bolide_runtime::bolide_list_clone as *const u8,
        );
        builder.symbol("@_list_new", bolide_runtime::bolide_list_new as *const u8);
        builder.symbol("@_list_push", bolide_runtime::bolide_list_push as *const u8);
        builder.symbol("@_list_pop", bolide_runtime::bolide_list_pop as *const u8);
        builder.symbol("@_list_len", bolide_runtime::bolide_list_len as *const u8);
        builder.symbol("@_list_get", bolide_runtime::bolide_list_get as *const u8);
        builder.symbol("@_list_set", bolide_runtime::bolide_list_set as *const u8);
        builder.symbol(
            "@_list_insert",
            bolide_runtime::bolide_list_insert as *const u8,
        );
        builder.symbol(
            "@_list_remove",
            bolide_runtime::bolide_list_remove as *const u8,
        );
        builder.symbol(
            "@_list_clear",
            bolide_runtime::bolide_list_clear as *const u8,
        );
        builder.symbol(
            "@_list_reverse",
            bolide_runtime::bolide_list_reverse as *const u8,
        );
        builder.symbol(
            "@_list_extend",
            bolide_runtime::bolide_list_extend as *const u8,
        );
        builder.symbol(
            "@_list_contains",
            bolide_runtime::bolide_list_contains as *const u8,
        );
        builder.symbol(
            "@_list_index_of",
            bolide_runtime::bolide_list_index_of as *const u8,
        );
        builder.symbol(
            "@_list_count",
            bolide_runtime::bolide_list_count as *const u8,
        );
        builder.symbol("@_list_sort", bolide_runtime::bolide_list_sort as *const u8);
        builder.symbol(
            "@_list_slice",
            bolide_runtime::bolide_list_slice as *const u8,
        );
        builder.symbol(
            "@_list_slice_step",
            bolide_runtime::bolide_list_slice_step as *const u8,
        );
        builder.symbol("@_list_map", bolide_runtime::bolide_list_map as *const u8);
        builder.symbol(
            "@_list_filter",
            bolide_runtime::bolide_list_filter as *const u8,
        );
        builder.symbol(
            "@_list_is_empty",
            bolide_runtime::bolide_list_is_empty as *const u8,
        );
        builder.symbol(
            "@_list_first",
            bolide_runtime::bolide_list_first as *const u8,
        );
        builder.symbol("@_list_last", bolide_runtime::bolide_list_last as *const u8);
        builder.symbol(
            "@_print_list",
            bolide_runtime::bolide_print_list as *const u8,
        );
        // Dict symbols
        builder.symbol("@_dict_new", bolide_runtime::bolide_dict_new as *const u8);
        builder.symbol(
            "@_dict_retain",
            bolide_runtime::bolide_dict_retain as *const u8,
        );
        builder.symbol(
            "@_dict_release",
            bolide_runtime::bolide_dict_release as *const u8,
        );
        builder.symbol(
            "@_dict_clone",
            bolide_runtime::bolide_dict_clone as *const u8,
        );
        builder.symbol(
            "@_dict_extend",
            bolide_runtime::bolide_dict_extend as *const u8,
        );
        builder.symbol("@_dict_set", bolide_runtime::bolide_dict_set as *const u8);
        builder.symbol("@_dict_get", bolide_runtime::bolide_dict_get as *const u8);
        builder.symbol(
            "@_dict_contains",
            bolide_runtime::bolide_dict_contains as *const u8,
        );
        builder.symbol(
            "@_dict_remove",
            bolide_runtime::bolide_dict_remove as *const u8,
        );
        builder.symbol("@_dict_len", bolide_runtime::bolide_dict_len as *const u8);
        builder.symbol(
            "@_dict_is_empty",
            bolide_runtime::bolide_dict_is_empty as *const u8,
        );
        builder.symbol(
            "@_dict_clear",
            bolide_runtime::bolide_dict_clear as *const u8,
        );
        builder.symbol("@_dict_keys", bolide_runtime::bolide_dict_keys as *const u8);
        builder.symbol(
            "@_dict_values",
            bolide_runtime::bolide_dict_values as *const u8,
        );
        builder.symbol("@_dict_iter", bolide_runtime::bolide_dict_iter as *const u8);
        builder.symbol(
            "@_print_dict",
            bolide_runtime::bolide_print_dict as *const u8,
        );
        builder.symbol(
            "@_dynamic_retain",
            bolide_runtime::bolide_dynamic_retain as *const u8,
        );
        builder.symbol(
            "@_dynamic_release",
            bolide_runtime::bolide_dynamic_release as *const u8,
        );
        builder.symbol(
            "@_print_dynamic",
            bolide_runtime::bolide_print_dynamic as *const u8,
        );

        // 异常处理
        builder.symbol(
            "@_exception_set",
            bolide_runtime::bolide_exception_set as *const u8,
        );
        builder.symbol(
            "@_exception_get",
            bolide_runtime::bolide_exception_get as *const u8,
        );
        builder.symbol(
            "@_exception_tag",
            bolide_runtime::bolide_exception_tag as *const u8,
        );
        builder.symbol(
            "@_exception_pending",
            bolide_runtime::bolide_exception_pending as *const u8,
        );
        builder.symbol(
            "@_throw_uncaught",
            bolide_runtime::bolide_throw_uncaught as *const u8,
        );

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
            adts: HashMap::new(),
            class_tags: HashMap::new(),
            async_funcs: HashSet::new(),
            global_spawn_funcs: HashMap::new(),
            extern_funcs: HashMap::new(),
            modules: HashMap::new(),
            lifetime_funcs: HashSet::new(),
            global_data_ids: HashMap::new(),
            global_var_types: HashMap::new(),
            base_dir: None,
            dependency_manifest: None,
            closure_counter: 0,
            pending_closures: Vec::new(),
            funcsig_return_sources: HashMap::new(),
            funcsig_closure_param_indices: HashMap::new(),
        }
    }

    /// 设置源文件所在目录（import 相对路径的解析基准）
    pub fn set_base_dir(&mut self, dir: &str) {
        self.base_dir = Some(dir.to_string());
    }

    /// 设置包管理器解析出的依赖映射
    pub fn set_dependency_manifest(&mut self, manifest: crate::deps::DependencyManifest) {
        self.dependency_manifest = Some(manifest);
    }

    /// 编译程序并返回入口函数指针
    pub fn compile(&mut self, program: &Program) -> Result<*const u8, String> {
        // 预处理 import 语句，加载并合并导入的模块
        let program = self.process_imports(program)?;
        // 注入内置类（Error 等），供 try/catch 使用
        let program = inject_builtin_classes(program);
        // 泛型函数单态化
        let program = crate::monomorphize(program)?;

        // 注册内置函数
        self.register_builtins()?;

        // 先处理所有 extern 块（必须在函数声明之前）
        for stmt in &program.statements {
            if let Statement::ExternBlock(eb) = stmt {
                self.register_extern_block(eb)?;
            }
        }

        // 收集所有 ADT 和类定义
        self.collect_adts(&program)?;
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
        self.funcsig_return_sources = self.collect_funcsig_return_sources(&program);
        self.funcsig_closure_param_indices = self.collect_funcsig_closure_param_indices(&program);
        self.funcsig_return_sources = self.collect_funcsig_return_sources(&program);
        self.funcsig_closure_param_indices = self.collect_funcsig_closure_param_indices(&program);
        self.declare_funcsig_raw_adapters()?;

        // 扫描并生成 trampolines（用于带参数的 spawn）
        let spawn_targets = self.collect_spawn_targets(&program);
        self.generate_trampolines(&spawn_targets)?;

        // 收集并声明全局变量（顶层 VarDecl）
        self.collect_global_variables(&program)?;

        // 编译类构造函数
        for class_name in self.classes.keys().cloned().collect::<Vec<_>>() {
            self.compile_class_constructor(&class_name)?;
        }

        // 编译类方法
        self.compile_class_methods(&program)?;
        self.compile_funcsig_raw_adapters()?;

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
            is_export: false,
            type_params: vec![],
            params: vec![],
            throws: vec![],
            return_type: Some(BolideType::Int),
            lifetime_deps: None,
            body: toplevel_stmts,
        };
        self.declare_function(&main_func)?;
        self.compile_function(&main_func)?;

        // 编译所有待处理的 lifted 闭包函数（可能产生嵌套闭包，循环直到清空）
        while let Some(job) = self.pending_closures.pop() {
            self.compile_closure_job(&job)?;
        }

        self.module
            .finalize_definitions()
            .map_err(|e| format!("Finalize error: {}", e))?;

        // 获取 __main__ 函数
        let func_id = self
            .functions
            .get("__main__")
            .ok_or("No __main__ function found")?;
        let main_ptr = self.module.get_finalized_function(*func_id);
        Ok(main_ptr)
    }

    /// 声明函数（第一遍）
    fn declare_function(&mut self, func: &FuncDef) -> Result<(), String> {
        let mut sig = self.module.make_signature();

        // 添加参数类型
        for param in &func.params {
            let param_ty = self.normalize_bolide_type(&param.ty);
            let ty = self.bolide_type_to_cranelift(&param_ty);
            sig.params.push(AbiParam::new(ty));
        }

        // 添加返回类型
        if let Some(ref ret_ty) = func.return_type {
            let ret_ty = self.normalize_bolide_type(ret_ty);
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(&ret_ty)));
        }

        let func_id = self
            .module
            .declare_function(&func.name, Linkage::Export, &sig)
            .map_err(|e| format!("Declare function error: {}", e))?;

        self.functions.insert(func.name.clone(), func_id);
        // 存储函数返回类型
        self.func_return_types.insert(
            func.name.clone(),
            func.return_type
                .as_ref()
                .map(|ty| self.normalize_bolide_type(ty)),
        );
        // 存储函数参数
        let mut params = func.params.clone();
        for param in &mut params {
            param.ty = self.normalize_bolide_type(&param.ty);
        }
        self.func_params.insert(func.name.clone(), params);
        // 记录生命周期函数
        if func.lifetime_deps.is_some() {
            self.lifetime_funcs.insert(func.name.clone());
        }
        Ok(())
    }

    /// 处理 import 语句，加载并合并导入的模块
    fn process_imports(&mut self, program: &Program) -> Result<Program, String> {
        let mut merged_statements = Vec::new();
        let mut imported_files: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // 先处理所有 import 语句
        let mut alias_pairs: Vec<(String, String)> = Vec::new();
        for stmt in &program.statements {
            if let Statement::Import(import) = stmt {
                // 包管理器：import http; 形式，将 path 解析为 file_path。
                // 入口文件通常名为 lib.bl，若用文件名作为模块名会导致不同包冲突，
                // 因此包导入统一使用包名作为模块命名空间。
                let mut import = import.clone();
                let mut pkg_module_name: Option<String> = None;
                if import.file_path.is_none() && !import.path.is_empty() {
                    let pkg_name = &import.path[0];
                    if let Some(ref manifest) = self.dependency_manifest {
                        if let Some(entry) = manifest.entry_file(pkg_name) {
                            if entry.exists() {
                                import.file_path = Some(entry.to_string_lossy().to_string());
                                pkg_module_name = Some(pkg_name.clone());
                            }
                        }
                    }
                }

                if let Some(ref file_path) = import.file_path {
                    // 模块名：包导入用包名，普通文件导入用文件名（保持原有行为）
                    let module_name = pkg_module_name
                        .clone()
                        .unwrap_or_else(|| Self::extract_module_name(file_path));

                    // import ... as 别名：记录别名，稍后把主程序里的
                    // alias.f / alias.Type 重写为 module.f / module.Type
                    // （在去重之前记录：同一文件可被多次 import 并使用不同别名）
                    if let Some(ref alias) = import.alias {
                        alias_pairs.push((alias.clone(), module_name.clone()));
                    }

                    // 避免重复导入
                    if imported_files.contains(file_path) {
                        continue;
                    }
                    imported_files.insert(file_path.clone());

                    self.modules.insert(module_name.clone(), file_path.clone());

                    // 加载并解析文件
                    let imported = self.load_module(file_path)?;

                    // 先收集模块中定义的类名
                    let mut class_names: HashSet<String> = HashSet::new();
                    for imp_stmt in &imported.statements {
                        if let Statement::ClassDef(class) = imp_stmt {
                            class_names.insert(class.name.clone());
                        }
                    }

                    // 合并导入的定义，添加模块前缀
                    for imp_stmt in imported.statements {
                        match imp_stmt {
                            Statement::FuncDef(mut func) => {
                                // 重命名函数: func -> @module_func
                                func.name = format!("@{}_{}", module_name, func.name);
                                // 重写函数内部的类型引用
                                Self::rewrite_func_class_refs(
                                    &mut func,
                                    &module_name,
                                    &class_names,
                                );
                                merged_statements.push(Statement::FuncDef(func));
                            }
                            Statement::ClassDef(mut class) => {
                                // 重命名类: Class -> @module_Class
                                let old_name = class.name.clone();
                                class.name = format!("@{}_{}", module_name, old_name);
                                // 重写方法内部的类型引用
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
                                // 保留 extern 声明（不添加前缀，C函数名必须保持不变）
                                merged_statements.push(Statement::ExternBlock(ext));
                            }
                            Statement::VarDecl(mut decl) => {
                                // 重命名模块级变量
                                decl.name = format!("@{}_{}", module_name, decl.name);
                                // 处理模块级变量声明
                                Self::rewrite_var_decl_class_refs(
                                    &mut decl,
                                    &module_name,
                                    &class_names,
                                );
                                merged_statements.push(Statement::VarDecl(decl));
                            }
                            _ => {} // 忽略其他顶层代码
                        }
                    }
                }
            }
        }

        // 添加原程序的所有语句（重写模块别名为真实模块名）
        for stmt in &program.statements {
            let mut stmt = stmt.clone();
            for (alias, module) in &alias_pairs {
                Self::rewrite_module_alias_in_stmt(&mut stmt, alias, module);
            }
            merged_statements.push(stmt);
        }

        Ok(Program {
            statements: merged_statements,
        })
    }

    /// 将语句中的模块别名引用重写为真实模块名（alias.f -> module.f）
    fn rewrite_module_alias_in_stmt(stmt: &mut Statement, alias: &str, module: &str) {
        match stmt {
            Statement::VarDecl(decl) => {
                if let Some(ref mut ty) = decl.ty {
                    Self::rewrite_module_alias_in_type(ty, alias, module);
                }
                if let Some(ref mut value) = decl.value {
                    Self::rewrite_module_alias_in_expr(value, alias, module);
                }
            }
            Statement::Assign(assign) => {
                Self::rewrite_module_alias_in_expr(&mut assign.target, alias, module);
                Self::rewrite_module_alias_in_expr(&mut assign.value, alias, module);
            }
            Statement::Expr(e) => Self::rewrite_module_alias_in_expr(e, alias, module),
            Statement::Return(Some(e)) => Self::rewrite_module_alias_in_expr(e, alias, module),
            Statement::FuncDef(func) => {
                if let Some(ref mut ret) = func.return_type {
                    Self::rewrite_module_alias_in_type(ret, alias, module);
                }
                for p in &mut func.params {
                    Self::rewrite_module_alias_in_type(&mut p.ty, alias, module);
                }
                for s in &mut func.body {
                    Self::rewrite_module_alias_in_stmt(s, alias, module);
                }
            }
            Statement::ClassDef(class) => {
                for f in &mut class.fields {
                    Self::rewrite_module_alias_in_type(&mut f.ty, alias, module);
                }
                for m in &mut class.methods {
                    if let Some(ref mut ret) = m.return_type {
                        Self::rewrite_module_alias_in_type(ret, alias, module);
                    }
                    for p in &mut m.params {
                        Self::rewrite_module_alias_in_type(&mut p.ty, alias, module);
                    }
                    for s in &mut m.body {
                        Self::rewrite_module_alias_in_stmt(s, alias, module);
                    }
                }
            }
            Statement::If(if_stmt) => {
                Self::rewrite_module_alias_in_expr(&mut if_stmt.condition, alias, module);
                for s in &mut if_stmt.then_body {
                    Self::rewrite_module_alias_in_stmt(s, alias, module);
                }
                for (cond, body) in &mut if_stmt.elif_branches {
                    Self::rewrite_module_alias_in_expr(cond, alias, module);
                    for s in body {
                        Self::rewrite_module_alias_in_stmt(s, alias, module);
                    }
                }
                if let Some(ref mut body) = if_stmt.else_body {
                    for s in body {
                        Self::rewrite_module_alias_in_stmt(s, alias, module);
                    }
                }
            }
            Statement::While(w) => {
                Self::rewrite_module_alias_in_expr(&mut w.condition, alias, module);
                for s in &mut w.body {
                    Self::rewrite_module_alias_in_stmt(s, alias, module);
                }
            }
            Statement::For(f) => {
                Self::rewrite_module_alias_in_expr(&mut f.iter, alias, module);
                for s in &mut f.body {
                    Self::rewrite_module_alias_in_stmt(s, alias, module);
                }
            }
            Statement::Pool(p) => {
                Self::rewrite_module_alias_in_expr(&mut p.size, alias, module);
                for s in &mut p.body {
                    Self::rewrite_module_alias_in_stmt(s, alias, module);
                }
            }
            Statement::AwaitScope(a) => {
                for s in &mut a.body {
                    Self::rewrite_module_alias_in_stmt(s, alias, module);
                }
            }
            _ => {}
        }
    }

    /// 表达式级别的别名重写：Member(Ident(alias), x) -> Member(Ident(module), x)
    fn rewrite_module_alias_in_expr(expr: &mut Expr, alias: &str, module: &str) {
        match expr {
            Expr::Member(base, _) => {
                if let Expr::Ident(name) = base.as_mut() {
                    if name == alias {
                        *name = module.to_string();
                        return;
                    }
                }
                Self::rewrite_module_alias_in_expr(base, alias, module);
            }
            Expr::Call(callee, args) => {
                Self::rewrite_module_alias_in_expr(callee, alias, module);
                for a in args {
                    Self::rewrite_module_alias_in_expr(a, alias, module);
                }
            }
            Expr::BinOp(l, _, r) => {
                Self::rewrite_module_alias_in_expr(l, alias, module);
                Self::rewrite_module_alias_in_expr(r, alias, module);
            }
            Expr::UnaryOp(_, e) => Self::rewrite_module_alias_in_expr(e, alias, module),
            Expr::Index(base, idx) => {
                Self::rewrite_module_alias_in_expr(base, alias, module);
                Self::rewrite_module_alias_in_expr(idx, alias, module);
            }
            Expr::List(items) | Expr::Tuple(items) | Expr::SpawnAll(items) => {
                for e in items {
                    Self::rewrite_module_alias_in_expr(e, alias, module);
                }
            }
            Expr::Dict(entries) => {
                for (k, v) in entries {
                    Self::rewrite_module_alias_in_expr(k, alias, module);
                    Self::rewrite_module_alias_in_expr(v, alias, module);
                }
            }
            Expr::Spawn(_, args) | Expr::SpawnThread(_, args) => {
                for a in args {
                    Self::rewrite_module_alias_in_expr(a, alias, module);
                }
            }
            Expr::Await(e) => Self::rewrite_module_alias_in_expr(e, alias, module),
            _ => {}
        }
    }

    /// 类型级别的别名重写：Custom("alias.X") -> Custom("module.X")
    fn rewrite_module_alias_in_type(ty: &mut BolideType, alias: &str, module: &str) {
        match ty {
            BolideType::Custom(name) => {
                let prefix = format!("{}.", alias);
                if let Some(rest) = name.strip_prefix(&prefix) {
                    *name = format!("{}.{}", module, rest);
                }
            }
            BolideType::Adt(name, args) => {
                let prefix = format!("{}.", alias);
                if let Some(rest) = name.strip_prefix(&prefix) {
                    *name = format!("{}.{}", module, rest);
                }
                for arg in args {
                    Self::rewrite_module_alias_in_type(arg, alias, module);
                }
            }
            BolideType::List(inner)
            | BolideType::Channel(inner)
            | BolideType::Weak(inner)
            | BolideType::Unowned(inner) => {
                Self::rewrite_module_alias_in_type(inner, alias, module);
            }
            BolideType::Dict(k, v) => {
                Self::rewrite_module_alias_in_type(k, alias, module);
                Self::rewrite_module_alias_in_type(v, alias, module);
            }
            BolideType::Tuple(types) => {
                for t in types {
                    Self::rewrite_module_alias_in_type(t, alias, module);
                }
            }
            BolideType::FuncSig(params, ret) => {
                for p in params {
                    Self::rewrite_module_alias_in_type(p, alias, module);
                }
                if let Some(r) = ret {
                    Self::rewrite_module_alias_in_type(r, alias, module);
                }
            }
            _ => {}
        }
    }

    /// 重写函数内部的类型引用，将模块内部类名转换为 @module_ClassName
    fn rewrite_func_class_refs(
        func: &mut FuncDef,
        module_name: &str,
        class_names: &HashSet<String>,
    ) {
        // 重写返回类型
        if let Some(ref mut ret_ty) = func.return_type {
            Self::rewrite_type_class_refs(ret_ty, module_name, class_names);
        }
        // 重写参数类型
        for param in &mut func.params {
            Self::rewrite_type_class_refs(&mut param.ty, module_name, class_names);
        }
        // 重写函数体内的语句
        for stmt in &mut func.body {
            Self::rewrite_stmt_class_refs(stmt, module_name, class_names);
        }
    }

    /// 重写类型中的类引用
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
            BolideType::List(inner) => {
                Self::rewrite_type_class_refs(inner, module_name, class_names)
            }
            BolideType::Dict(k, v) => {
                Self::rewrite_type_class_refs(k, module_name, class_names);
                Self::rewrite_type_class_refs(v, module_name, class_names);
            }
            BolideType::Channel(inner) => {
                Self::rewrite_type_class_refs(inner, module_name, class_names)
            }
            BolideType::Tuple(types) => {
                for t in types {
                    Self::rewrite_type_class_refs(t, module_name, class_names);
                }
            }
            BolideType::Weak(inner) | BolideType::Unowned(inner) => {
                Self::rewrite_type_class_refs(inner, module_name, class_names);
            }
            BolideType::FuncSig(params, ret) => {
                for p in params {
                    Self::rewrite_type_class_refs(p, module_name, class_names);
                }
                if let Some(r) = ret {
                    Self::rewrite_type_class_refs(r, module_name, class_names);
                }
            }
            _ => {}
        }
    }

    /// 重写变量声明中的类引用
    fn rewrite_var_decl_class_refs(
        decl: &mut VarDecl,
        module_name: &str,
        class_names: &HashSet<String>,
    ) {
        if let Some(ref mut ty) = decl.ty {
            Self::rewrite_type_class_refs(ty, module_name, class_names);
        }
        if let Some(ref mut val) = decl.value {
            Self::rewrite_expr_class_refs(val, module_name, class_names);
        }
    }

    /// 重写语句中的类引用
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
                Self::rewrite_expr_class_refs(&mut assign.value, module_name, class_names);
            }
            Statement::Expr(expr) => {
                Self::rewrite_expr_class_refs(expr, module_name, class_names);
            }
            Statement::Return(Some(expr)) => {
                Self::rewrite_expr_class_refs(expr, module_name, class_names);
            }
            Statement::If(if_stmt) => {
                Self::rewrite_expr_class_refs(&mut if_stmt.condition, module_name, class_names);
                for s in &mut if_stmt.then_body {
                    Self::rewrite_stmt_class_refs(s, module_name, class_names);
                }
                for (cond, body) in &mut if_stmt.elif_branches {
                    Self::rewrite_expr_class_refs(cond, module_name, class_names);
                    for s in body {
                        Self::rewrite_stmt_class_refs(s, module_name, class_names);
                    }
                }
                if let Some(else_body) = &mut if_stmt.else_body {
                    for s in else_body {
                        Self::rewrite_stmt_class_refs(s, module_name, class_names);
                    }
                }
            }
            Statement::While(while_stmt) => {
                Self::rewrite_expr_class_refs(&mut while_stmt.condition, module_name, class_names);
                for s in &mut while_stmt.body {
                    Self::rewrite_stmt_class_refs(s, module_name, class_names);
                }
            }
            Statement::For(for_stmt) => {
                Self::rewrite_expr_class_refs(&mut for_stmt.iter, module_name, class_names);
                for s in &mut for_stmt.body {
                    Self::rewrite_stmt_class_refs(s, module_name, class_names);
                }
            }
            _ => {}
        }
    }

    /// 重写表达式中的类引用（主要是构造函数调用）
    fn rewrite_expr_class_refs(expr: &mut Expr, module_name: &str, class_names: &HashSet<String>) {
        match expr {
            Expr::Call(callee, args) => {
                // 检查是否是类构造函数调用: ClassName(args)
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
            Expr::BinOp(left, _, right) => {
                Self::rewrite_expr_class_refs(left, module_name, class_names);
                Self::rewrite_expr_class_refs(right, module_name, class_names);
            }
            Expr::UnaryOp(_, operand) => {
                Self::rewrite_expr_class_refs(operand, module_name, class_names);
            }
            Expr::Index(base, idx) => {
                Self::rewrite_expr_class_refs(base, module_name, class_names);
                Self::rewrite_expr_class_refs(idx, module_name, class_names);
            }
            Expr::Member(base, _) => {
                Self::rewrite_expr_class_refs(base, module_name, class_names);
            }
            Expr::List(items) => {
                for item in items {
                    Self::rewrite_expr_class_refs(item, module_name, class_names);
                }
            }
            Expr::Dict(entries) => {
                for (k, v) in entries {
                    Self::rewrite_expr_class_refs(k, module_name, class_names);
                    Self::rewrite_expr_class_refs(v, module_name, class_names);
                }
            }
            Expr::Tuple(items) => {
                for item in items {
                    Self::rewrite_expr_class_refs(item, module_name, class_names);
                }
            }
            Expr::Await(inner) => {
                Self::rewrite_expr_class_refs(inner, module_name, class_names);
            }
            Expr::SpawnAll(exprs) => {
                for e in exprs {
                    Self::rewrite_expr_class_refs(e, module_name, class_names);
                }
            }
            Expr::Spawn(_, args) | Expr::SpawnThread(_, args) => {
                for arg in args {
                    Self::rewrite_expr_class_refs(arg, module_name, class_names);
                }
            }
            _ => {}
        }
    }

    /// 收集并声明全局变量
    fn collect_global_variables(&mut self, program: &Program) -> Result<(), String> {
        for stmt in &program.statements {
            if let Statement::VarDecl(decl) = stmt {
                // 推断类型
                let var_type = if let Some(ref ty) = decl.ty {
                    self.normalize_bolide_type(ty)
                } else if let Some(ref val) = decl.value {
                    self.normalize_bolide_type(&self.infer_expr_type_static(val))
                } else {
                    BolideType::Int
                };

                // 为全局变量创建数据段（8 字节用于存储值）
                let data_id = self
                    .module
                    .declare_data(&decl.name, Linkage::Local, true, false)
                    .map_err(|e| format!("Failed to declare global '{}': {}", decl.name, e))?;

                // 初始化数据段为 0
                self.data_desc.define_zeroinit(8);
                self.module
                    .define_data(data_id, &self.data_desc)
                    .map_err(|e| format!("Failed to define global '{}': {}", decl.name, e))?;
                self.data_desc.clear();

                // 记录全局变量
                self.global_data_ids.insert(decl.name.clone(), data_id);
                self.global_var_types.insert(decl.name.clone(), var_type);

                // 记录全局 Future 变量对应的 async 函数，供后续 await 类型推断
                if let Some(ref val) = decl.value {
                    match val {
                        Expr::Call(callee, _) => {
                            if let Expr::Ident(name) = callee.as_ref() {
                                if self.async_funcs.contains(name) {
                                    self.global_spawn_funcs
                                        .insert(decl.name.clone(), name.clone());
                                }
                            }
                        }
                        Expr::Spawn(fname, _) | Expr::SpawnThread(fname, _) => {
                            self.global_spawn_funcs
                                .insert(decl.name.clone(), fname.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// 静态推断表达式类型（用于全局变量收集阶段）
    fn infer_expr_type_static(&self, expr: &Expr) -> BolideType {
        match expr {
            Expr::Int(_) => BolideType::Int,
            Expr::Float(_) => BolideType::Float,
            Expr::Bool(_) => BolideType::Bool,
            Expr::String(_) => BolideType::Str,
            Expr::BigInt(_) => BolideType::BigInt,
            Expr::Decimal(_) => BolideType::Decimal,
            Expr::None => BolideType::Int,
            Expr::List(items) => {
                // 扫描元素推断统一类型（与函数内推断一致），混合类型为 Dynamic
                let item_type = if items.is_empty() {
                    BolideType::Int
                } else {
                    let mut inferred = self.infer_expr_type_static(&items[0]);
                    for item in items.iter().skip(1) {
                        if inferred != self.infer_expr_type_static(item) {
                            inferred = BolideType::Dynamic;
                            break;
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
                    let mut k_ty = self.infer_expr_type_static(&entries[0].0);
                    let mut v_ty = self.infer_expr_type_static(&entries[0].1);
                    for (k, v) in entries.iter().skip(1) {
                        if k_ty != self.infer_expr_type_static(k) {
                            k_ty = BolideType::Dynamic;
                        }
                        if v_ty != self.infer_expr_type_static(v) {
                            v_ty = BolideType::Dynamic;
                        }
                    }
                    (k_ty, v_ty)
                };
                BolideType::Dict(Box::new(k_type), Box::new(v_type))
            }
            Expr::Tuple(exprs) => {
                let types: Vec<BolideType> = exprs
                    .iter()
                    .map(|e| self.infer_expr_type_static(e))
                    .collect();
                BolideType::Tuple(types)
            }
            Expr::Member(base, member) => {
                // 处理模块成员访问，如 gui.COLOR_BLUE
                if let Expr::Ident(module_name) = base.as_ref() {
                    if self.modules.contains_key(module_name) {
                        // 模块常量访问，检查全局变量类型
                        let global_name = format!("@{}_{}", module_name, member);
                        if let Some(ty) = self.global_var_types.get(&global_name) {
                            return ty.clone();
                        }
                    }
                }
                BolideType::Int
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
                                return BolideType::Adt(adt_name.clone(), type_args);
                            }
                        }
                    }
                }
                // 检查是否是类构造函数或模块函数
                if let Expr::Ident(name) = callee.as_ref() {
                    if self.classes.contains_key(name) {
                        return BolideType::Custom(name.clone());
                    }
                    // async 函数直接调用返回 Future 句柄（await 后才是返回值）
                    if self.async_funcs.contains(name) {
                        return BolideType::Future;
                    }
                    // 普通函数调用：查函数返回类型
                    if let Some(Some(ret_ty)) = self.func_return_types.get(name) {
                        return ret_ty.clone();
                    }
                }
                if let Expr::Member(base, member) = callee.as_ref() {
                    if let Expr::Ident(module_name) = base.as_ref() {
                        if self.modules.contains_key(module_name) {
                            let func_name = format!("@{}_{}", module_name, member);
                            // 检查是否是类构造函数
                            if self.classes.contains_key(&func_name) {
                                return BolideType::Custom(func_name);
                            }
                            // 检查函数返回类型
                            if let Some(Some(ret_ty)) = self.func_return_types.get(&func_name) {
                                return ret_ty.clone();
                            }
                        }
                    }
                    let base_ty = self.infer_expr_type_static(base);
                    let class_name = match base_ty {
                        BolideType::Custom(name) => Some(name),
                        BolideType::Weak(inner) | BolideType::Unowned(inner) => {
                            if let BolideType::Custom(name) = *inner {
                                Some(name)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(class_name) = class_name {
                        if let Some(ret_ty) = self.lookup_method_return_type(&class_name, member) {
                            return ret_ty;
                        }
                    }
                    let base_ty = self.infer_expr_type_static(base);
                    match base_ty {
                        BolideType::Dict(k, v) => {
                            return match member.as_str() {
                                "keys" => BolideType::List(k),
                                "values" => BolideType::List(v),
                                "get" | "remove" => *v,
                                "clone" => BolideType::Dict(k, v),
                                _ => BolideType::Int,
                            };
                        }
                        BolideType::List(elem) => {
                            return match member.as_str() {
                                "pop" | "get" | "first" | "last" | "remove" => *elem,
                                "slice" | "copy" | "clone" | "filter" => BolideType::List(elem),
                                _ => BolideType::Int,
                            };
                        }
                        BolideType::Str => {
                            return match member.as_str() {
                                "upper" | "lower" | "trim" | "strip" | "replace" | "repeat"
                                | "substring" | "char_at" => BolideType::Str,
                                "split" => BolideType::List(Box::new(BolideType::Str)),
                                _ => BolideType::Int,
                            };
                        }
                        BolideType::Bytes => {
                            return match member.as_str() {
                                "copy" | "clone" => BolideType::Bytes,
                                _ => BolideType::Int,
                            };
                        }
                        _ => {}
                    }
                }
                BolideType::Int
            }
            // 裸函数名作为值：合成 FuncSig（一等函数支持）
            Expr::Ident(name) => {
                if let Some(ty) = self.global_var_types.get(name) {
                    return ty.clone();
                }
                if self.functions.contains_key(name) {
                    if let Some(params) = self.func_params.get(name) {
                        let param_types: Vec<BolideType> =
                            params.iter().map(|p| p.ty.clone()).collect();
                        let ret = self
                            .func_return_types
                            .get(name)
                            .cloned()
                            .flatten()
                            .map(Box::new);
                        return BolideType::FuncSig(param_types, ret);
                    }
                    return BolideType::Func;
                }
                BolideType::Int
            }
            // spawn 返回 Task 句柄
            Expr::Spawn(_, _) | Expr::SpawnThread(_, _) => BolideType::Future,
            // await 表达式返回协程的返回类型
            Expr::Await(inner) => self.infer_awaited_type_static(inner),
            Expr::SpawnAll(exprs) => {
                let elem_types = exprs
                    .iter()
                    .map(|e| self.spawn_item_type(e).unwrap_or(BolideType::Int))
                    .collect();
                BolideType::Tuple(elem_types)
            }
            Expr::Propagate(inner) | Expr::Raise(inner) => {
                match self.normalize_bolide_type(&self.infer_expr_type_static(inner)) {
                    BolideType::Adt(name, args)
                        if (name == "Result" || name == "Option") && !args.is_empty() =>
                    {
                        args[0].clone()
                    }
                    _ => BolideType::Int,
                }
            }
            Expr::TryExpr(body) => {
                let ok_ty = body
                    .last()
                    .and_then(|stmt| match stmt {
                        Statement::Expr(expr) => Some(self.infer_expr_type_static(expr)),
                        _ => None,
                    })
                    .unwrap_or(BolideType::Int);
                BolideType::Adt(
                    "Result".to_string(),
                    vec![ok_ty, BolideType::Custom("Error".to_string())],
                )
            }
            Expr::Index(base, idx) => match self.infer_expr_type_static(base) {
                BolideType::Tuple(elem_types) => {
                    if let Expr::Int(i) = idx.as_ref() {
                        elem_types
                            .get(*i as usize)
                            .cloned()
                            .unwrap_or(BolideType::Int)
                    } else {
                        elem_types.first().cloned().unwrap_or(BolideType::Int)
                    }
                }
                BolideType::List(elem_ty) => *elem_ty,
                BolideType::Dict(_, val_ty) => *val_ty,
                BolideType::Bytes => BolideType::Int,
                BolideType::Str => BolideType::Str,
                _ => BolideType::Int,
            },
            Expr::Closure {
                params,
                return_type,
                ..
            } => BolideType::FuncSig(
                params.iter().map(|p| p.ty.clone()).collect(),
                return_type.clone().map(Box::new),
            ),
            _ => BolideType::Int,
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
            let actual = self.infer_expr_type_static(arg);
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
            Expr::Spawn(name, args) | Expr::SpawnThread(name, args) => {
                Ok((name.as_str(), args.as_slice()))
            }
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

    /// 静态推断 await 一个 Future/Task 表达式后的类型（全局变量收集阶段使用，
    /// 此时函数签名已全部声明，但 spawn_func_map 尚未建立）
    fn infer_awaited_type_static(&self, expr: &Expr) -> BolideType {
        match expr {
            // await fetch_a()
            Expr::Call(callee, _) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    if let Some(Some(ret_ty)) = self.func_return_types.get(name) {
                        return ret_ty.clone();
                    }
                }
                BolideType::Int
            }
            // let f = fetch_a(); await f（全局 Future 变量）
            Expr::Ident(var_name) => {
                if let Some(func_name) = self.global_spawn_funcs.get(var_name) {
                    return self
                        .func_return_types
                        .get(func_name)
                        .cloned()
                        .flatten()
                        .unwrap_or(BolideType::Int);
                }
                BolideType::Int
            }
            // await spawn heavy(x)
            Expr::Spawn(func_name, _) | Expr::SpawnThread(func_name, _) => self
                .func_return_types
                .get(func_name)
                .cloned()
                .flatten()
                .unwrap_or(BolideType::Int),
            _ => BolideType::Int,
        }
    }

    /// 从文件路径提取模块名
    fn extract_module_name(file_path: &str) -> String {
        std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module")
            .to_string()
    }

    /// 加载模块文件（相对路径基于导入方源文件所在目录解析）
    fn load_module(&self, file_path: &str) -> Result<Program, String> {
        let resolved = self.resolve_import_path(file_path);
        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| format!("Failed to load module '{}': {}", resolved, e))?;

        bolide_parser::parse_source(&content)
            .map_err(|e| format!("Failed to parse module '{}': {}", resolved, e))
    }

    /// 解析 import 路径（确定性顺序，不依赖进程工作目录）：
    /// 1. 绝对路径按原样使用
    /// 2. 相对路径基于导入方源文件所在目录
    /// 3. 依赖 manifest（包管理器解析出的包）
    /// 4. BOLIDE_HOME 环境变量（开发期指向仓库根）
    /// 5. 可执行文件所在目录（发行版布局：std/ 与 bolide 可执行文件同级）
    fn resolve_import_path(&self, file_path: &str) -> String {
        let p = std::path::Path::new(file_path);
        if p.is_absolute() {
            return file_path.to_string();
        }
        // 导入方源文件目录
        if let Some(ref base) = self.base_dir {
            let joined = std::path::Path::new(base).join(p);
            if joined.exists() {
                return joined.to_string_lossy().to_string();
            }
        }
        // 包管理器依赖 manifest
        if let Some(ref manifest) = self.dependency_manifest {
            // import http; 形式在 process_imports 中被转为 file_path
            if let Some(entry) = manifest.entry_file(file_path) {
                if entry.exists() {
                    return entry.to_string_lossy().to_string();
                }
            }
            // import "http/utils.bl"; 形式
            if let Some(first_sep) = file_path.find('/') {
                let pkg_name = &file_path[..first_sep];
                let rest = &file_path[first_sep + 1..];
                // 优先相对入口文件所在目录（通常是 src/），再退回包根目录
                if let Some(entry) = manifest.entries.get(pkg_name) {
                    if let Some(src_dir) = entry.parent() {
                        let joined = src_dir.join(rest);
                        if joined.exists() {
                            return joined.to_string_lossy().to_string();
                        }
                    }
                }
                if let Some(pkg_root) = manifest.packages.get(pkg_name) {
                    let joined = pkg_root.join(rest);
                    if joined.exists() {
                        return joined.to_string_lossy().to_string();
                    }
                }
            }
        }
        // BOLIDE_HOME（显式指定的标准库根）
        if let Ok(home) = std::env::var("BOLIDE_HOME") {
            let joined = std::path::Path::new(&home).join(p);
            if joined.exists() {
                return joined.to_string_lossy().to_string();
            }
        }
        // 可执行文件同级目录（发行版的 std/ 摆放位置）
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let joined = exe_dir.join(p);
                if joined.exists() {
                    return joined.to_string_lossy().to_string();
                }
            }
        }
        // 未找到：返回原路径，由读取阶段报错（错误信息保留用户写的路径）
        file_path.to_string()
    }

    /// 规范化类型名称
    /// 将 "module.ClassName" 格式转换为 "@module_ClassName" 格式用于模块类查找
    fn normalize_type_name(&self, name: &str) -> String {
        if name.contains('.') {
            // 分割模块名和类型名: "gui.Window" -> ("gui", "Window")
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() == 2 {
                let module = parts[0];
                let type_name = parts[1];
                // 检查是否是已知模块
                if self.modules.contains_key(module) {
                    return format!("@{}_{}", module, type_name);
                }
            }
        }
        name.to_string()
    }

    /// 规范化 BolideType 中的类型名称
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

    /// 获取类信息，自动处理模块限定名
    fn get_class(&self, name: &str) -> Option<&ClassInfo> {
        // 首先尝试直接查找
        if let Some(info) = self.classes.get(name) {
            return Some(info);
        }
        // 尝试规范化后查找
        let normalized = self.normalize_type_name(name);
        if normalized != name {
            return self.classes.get(&normalized);
        }
        None
    }

    /// 获取类信息的可克隆版本
    fn get_class_cloned(&self, name: &str) -> Option<ClassInfo> {
        self.get_class(name).cloned()
    }
    /// 注册内置函数
    fn register_builtins(&mut self) -> Result<(), String> {
        let ptr = self.ptr_type;

        // print_int(int) -> void
        let mut print_int_sig = self.module.make_signature();
        print_int_sig.params.push(AbiParam::new(types::I64));
        let print_int_id = self
            .module
            .declare_function("@_print_int", Linkage::Import, &print_int_sig)
            .map_err(|e| format!("Declare print_int error: {}", e))?;
        self.functions
            .insert("@_print_int".to_string(), print_int_id);

        // print_float(float) -> void
        let mut print_float_sig = self.module.make_signature();
        print_float_sig.params.push(AbiParam::new(types::F64));
        let print_float_id = self
            .module
            .declare_function("@_print_float", Linkage::Import, &print_float_sig)
            .map_err(|e| format!("Declare print_float error: {}", e))?;
        self.functions
            .insert("@_print_float".to_string(), print_float_id);

        // print_bool(int) -> void
        let mut print_bool_sig = self.module.make_signature();
        print_bool_sig.params.push(AbiParam::new(types::I64));
        let print_bool_id = self
            .module
            .declare_function("@_print_bool", Linkage::Import, &print_bool_sig)
            .map_err(|e| format!("Declare print_bool error: {}", e))?;
        self.functions
            .insert("@_print_bool".to_string(), print_bool_id);

        // print_bigint(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_print_bigint", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_print_bigint".to_string(), id);

        // print_decimal(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_print_decimal", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_print_decimal".to_string(), id);

        // print_string(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_print_string", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_print_string".to_string(), id);

        // print_bytes(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_print_bytes", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_print_bytes".to_string(), id);

        let inline_prints = [
            ("@_print_int_inline", types::I64),
            ("@_print_float_inline", types::F64),
            ("@_print_bool_inline", types::I64),
            ("@_print_bigint_inline", ptr),
            ("@_print_decimal_inline", ptr),
            ("@_print_string_inline", ptr),
            ("@_print_bytes_inline", ptr),
            ("@_print_dynamic_inline", ptr),
        ];
        for (name, param_ty) in inline_prints {
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(param_ty));
            let id = self
                .module
                .declare_function(name, Linkage::Import, &sig)
                .map_err(|e| format!("Declare {} error: {}", name, e))?;
            self.functions.insert(name.to_string(), id);
        }

        for name in [
            "@_print_tuple_start",
            "@_print_tuple_separator",
            "@_print_tuple_end_inline",
            "@_println",
        ] {
            let sig = self.module.make_signature();
            let id = self
                .module
                .declare_function(name, Linkage::Import, &sig)
                .map_err(|e| format!("Declare {} error: {}", name, e))?;
            self.functions.insert(name.to_string(), id);
        }

        // ===== 用户输入函数 =====
        // input() -> ptr
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_input", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_input".to_string(), id);

        // input_prompt(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_input_prompt", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_input_prompt".to_string(), id);

        // ===== 类型转换函数 =====
        // string_from_int(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_from_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_from_int".to_string(), id);

        // string_from_float(f64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_from_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_from_float".to_string(), id);

        // string_from_bool(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_from_bool", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_from_bool".to_string(), id);

        // string_from_bigint(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_from_bigint", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_string_from_bigint".to_string(), id);

        // string_from_decimal(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_from_decimal", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_string_from_decimal".to_string(), id);

        // string_to_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_string_to_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_to_int".to_string(), id);

        // string_to_float(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self
            .module
            .declare_function("@_string_to_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_to_float".to_string(), id);

        // ===== RC Release 函数 =====
        // string_retain(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_retain", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_retain".to_string(), id);

        // string_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_release".to_string(), id);

        // bigint_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bigint_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_release".to_string(), id);

        // decimal_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_release".to_string(), id);

        // list_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_release".to_string(), id);

        // dynamic_release(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_release".to_string(), id);

        // exception_set(ptr, tag) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_exception_set", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_exception_set".to_string(), id);

        // exception_get() -> ptr
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_exception_get", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_exception_get".to_string(), id);

        // exception_tag() -> i64
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_exception_tag", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_exception_tag".to_string(), id);

        // exception_pending() -> i64
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_exception_pending", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_exception_pending".to_string(), id);

        // throw_uncaught(ptr) -> ! (noreturn)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_throw_uncaught", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_throw_uncaught".to_string(), id);

        // ===== RC Clone 函数 =====
        // string_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_clone".to_string(), id);

        // string_len(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_string_len", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_len".to_string(), id);

        let bytes_builtins = [
            ("@_bytes_new", vec![], Some(ptr)),
            ("@_bytes_retain", vec![ptr], Some(ptr)),
            ("@_bytes_release", vec![ptr], None),
            ("@_bytes_clone", vec![ptr], Some(ptr)),
            ("@_bytes_len", vec![ptr], Some(types::I64)),
            ("@_bytes_get", vec![ptr, types::I64], Some(types::I64)),
            (
                "@_bytes_set",
                vec![ptr, types::I64, types::I64],
                Some(types::I64),
            ),
            ("@_bytes_push", vec![ptr, types::I64], None),
            ("@_bytes_to_string_lossy", vec![ptr], Some(ptr)),
        ];
        for (name, params, ret) in bytes_builtins {
            let mut sig = self.module.make_signature();
            for param in params {
                sig.params.push(AbiParam::new(param));
            }
            if let Some(ret) = ret {
                sig.returns.push(AbiParam::new(ret));
            }
            let id = self
                .module
                .declare_function(name, Linkage::Import, &sig)
                .map_err(|e| format!("Declare {} error: {}", name, e))?;
            self.functions.insert(name.to_string(), id);
        }

        // bigint_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bigint_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_clone".to_string(), id);

        // decimal_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_clone".to_string(), id);

        // list_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_clone".to_string(), id);

        // list_new(elem_type: u8) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I8));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_new", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_new".to_string(), id);

        // list_push(list: ptr, value: i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_push", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_push".to_string(), id);

        // list_len(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_len", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_len".to_string(), id);

        // list_get(list: ptr, index: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_get", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_get".to_string(), id);

        // list_set(list: ptr, index: i64, value: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_set", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_set".to_string(), id);

        // list_pop(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_pop", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_pop".to_string(), id);

        // list_insert(list: ptr, index: i64, value: i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_insert", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_insert".to_string(), id);

        // list_remove(list: ptr, index: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_remove", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_remove".to_string(), id);

        // list_clear(list: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_clear", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_clear".to_string(), id);

        // list_reverse(list: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_reverse", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_reverse".to_string(), id);

        // list_extend(list: ptr, other: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_extend", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_extend".to_string(), id);

        // list_contains(list: ptr, value: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_contains", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_contains".to_string(), id);

        // list_index_of(list: ptr, value: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_index_of", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_index_of".to_string(), id);

        // list_count(list: ptr, value: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_count", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_count".to_string(), id);

        // list_sort(list: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_sort", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_sort".to_string(), id);

        // list_slice(list: ptr, start: i64, end: i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_slice", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_slice".to_string(), id);

        // list_map(list: ptr, callback: ptr, result_elem_type: i8) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I8));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_map", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_map".to_string(), id);

        // list_filter(list: ptr, callback: ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_list_filter", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_filter".to_string(), id);

        // list_is_empty(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_is_empty", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_is_empty".to_string(), id);

        // list_first(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_first", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_first".to_string(), id);

        // list_last(list: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_list_last", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_list_last".to_string(), id);

        // print_list(list: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_print_list", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_print_list".to_string(), id);

        // ===== Dict 函数 =====
        // dict_new(key_type: u8, value_type: u8) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I8));
        sig.params.push(AbiParam::new(types::I8));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_new", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_new".to_string(), id);

        // dict_retain(dict: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_retain", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_retain".to_string(), id);

        // dict_release(dict: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_release".to_string(), id);

        // dict_clone(dict: ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_clone".to_string(), id);

        // dict_extend(dict: ptr, other: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_extend", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_extend".to_string(), id);

        // dict_set(dict: ptr, key: i64, value: i64) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_dict_set", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_set".to_string(), id);

        // dict_get(dict: ptr, key: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_dict_get", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_get".to_string(), id);

        // dict_contains(dict: ptr, key: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_dict_contains", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_contains".to_string(), id);

        // dict_remove(dict: ptr, key: i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_dict_remove", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_remove".to_string(), id);

        // dict_len(dict: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_dict_len", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_len".to_string(), id);

        // dict_is_empty(dict: ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_dict_is_empty", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_is_empty".to_string(), id);

        // dict_clear(dict: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_clear", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_clear".to_string(), id);

        // dict_keys(dict: ptr) -> ptr (list)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_keys", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_keys".to_string(), id);

        // dict_values(dict: ptr) -> ptr (list)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_values", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_values".to_string(), id);

        // dict_iter(dict: ptr) -> ptr (list of keys for iteration)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dict_iter", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dict_iter".to_string(), id);

        // print_dict(dict: ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_print_dict", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_print_dict".to_string(), id);

        // dynamic_clone(ptr) -> ptr

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_clone".to_string(), id);

        // ===== BigInt 函数 =====
        // bigint_from_i64(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bigint_from_i64", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_from_i64".to_string(), id);

        // bigint_from_str(ptr, usize) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bigint_from_str", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_from_str".to_string(), id);

        // bigint_add(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bigint_add", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_add".to_string(), id);

        // bigint_sub(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bigint_sub", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_sub".to_string(), id);

        // bigint_mul(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bigint_mul", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_mul".to_string(), id);

        // bigint_div(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bigint_div", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_div".to_string(), id);

        // bigint_eq(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_bigint_eq", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_eq".to_string(), id);

        // bigint_lt(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_bigint_lt", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_lt".to_string(), id);

        // bigint_le(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_bigint_le", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_le".to_string(), id);

        // bigint_to_i64(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_bigint_to_i64", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bigint_to_i64".to_string(), id);

        // bigint_debug_stats() -> void
        let mut sig = self.module.make_signature();
        let id = self
            .module
            .declare_function("@_bigint_debug_stats", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_bigint_debug_stats".to_string(), id);

        // ===== Decimal 函数 =====
        // decimal_from_i64(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_from_i64", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_from_i64".to_string(), id);

        // decimal_from_f64(f64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_from_f64", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_from_f64".to_string(), id);

        // decimal_from_str(ptr, usize) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_from_str", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_from_str".to_string(), id);

        // decimal_add(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_add", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_add".to_string(), id);

        // decimal_sub(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_sub", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_sub".to_string(), id);

        // decimal_mul(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_mul", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_mul".to_string(), id);

        // decimal_div(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_decimal_div", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_div".to_string(), id);

        // decimal_eq(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_decimal_eq", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_eq".to_string(), id);

        // decimal_lt(ptr, ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_decimal_lt", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_lt".to_string(), id);

        // decimal_to_i64(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_decimal_to_i64", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_to_i64".to_string(), id);

        // decimal_to_f64(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self
            .module
            .declare_function("@_decimal_to_f64", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_decimal_to_f64".to_string(), id);

        // ===== Dynamic 函数 =====
        // dynamic_from_int(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_from_int".to_string(), id);

        // dynamic_from_float(f64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::F64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_dynamic_from_float".to_string(), id);

        // dynamic_from_bool(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_bool", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_from_bool".to_string(), id);

        // dynamic_from_string(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_string", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_dynamic_from_string".to_string(), id);

        // dynamic_from_list(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_list", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_from_list".to_string(), id);

        // dynamic_from_bytes(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_bytes", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_dynamic_from_bytes".to_string(), id);

        // dynamic_from_dict(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_dict", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_from_dict".to_string(), id);

        // dynamic_from_bigint(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_bigint", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_dynamic_from_bigint".to_string(), id);

        // dynamic_from_decimal(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_from_decimal", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_dynamic_from_decimal".to_string(), id);

        // dynamic_add(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_add", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_add".to_string(), id);

        // dynamic_sub(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_sub", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_sub".to_string(), id);

        // dynamic_mul(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_mul", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_mul".to_string(), id);

        // dynamic_div(ptr, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_div", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_div".to_string(), id);

        // print_dynamic(ptr) -> void
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_print_dynamic", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_print_dynamic".to_string(), id);

        // dynamic_to_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_dynamic_to_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_to_int".to_string(), id);

        // dynamic_to_float(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self
            .module
            .declare_function("@_dynamic_to_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_to_float".to_string(), id);

        // dynamic_to_string(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_dynamic_to_string", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_dynamic_to_string".to_string(), id);

        // ===== 字符串函数 =====
        // string_from_slice(ptr, i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr)); // 字符串数据指针
        sig.params.push(AbiParam::new(types::I64)); // 长度
        sig.returns.push(AbiParam::new(ptr)); // BolideString 指针
        let id = self
            .module
            .declare_function("@_string_from_slice", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_from_slice".to_string(), id);

        // string_literal(ptr, i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_literal", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_literal".to_string(), id);

        // bolide_string_new(ptr) -> ptr  (char* -> BolideString*)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bolide_string_new", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bolide_string_new".to_string(), id);

        // string_as_cstr(ptr) -> ptr  (BolideString* -> char*)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_as_cstr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_as_cstr".to_string(), id);

        // string_concat(ptr, ptr) -> ptr  (字符串拼接)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_concat", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_concat".to_string(), id);

        // string_concat_many(ptr, i64) -> ptr  (多段字符串拼接)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_string_concat_many", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_string_concat_many".to_string(), id);

        // string_eq(ptr, ptr) -> i64  (字符串比较)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_string_eq", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_string_eq".to_string(), id);

        // ===== 字符串方法 + 切片 + 索引（统一声明） =====
        // (name, params, ret) —— p=ptr, i=I64
        {
            let i = types::I64;
            let str_helpers: &[(&str, &[types::Type], Option<types::Type>)] = &[
                ("@_string_slice", &[ptr, i, i, i, i], Some(ptr)),
                ("@_string_char_at", &[ptr, i], Some(ptr)),
                ("@_string_upper", &[ptr], Some(ptr)),
                ("@_string_lower", &[ptr], Some(ptr)),
                ("@_string_trim", &[ptr], Some(ptr)),
                ("@_string_replace", &[ptr, ptr, ptr], Some(ptr)),
                ("@_string_repeat", &[ptr, i], Some(ptr)),
                ("@_string_find", &[ptr, ptr], Some(i)),
                ("@_string_contains", &[ptr, ptr], Some(i)),
                ("@_string_starts_with", &[ptr, ptr], Some(i)),
                ("@_string_ends_with", &[ptr, ptr], Some(i)),
                ("@_string_count", &[ptr, ptr], Some(i)),
                ("@_string_split", &[ptr, ptr], Some(ptr)),
                ("@_list_slice_step", &[ptr, i, i, i, i], Some(ptr)),
                ("@_tuple_slice_step", &[ptr, i, i, i, i], Some(ptr)),
            ];
            for (name, params, ret) in str_helpers {
                let mut sig = self.module.make_signature();
                for pty in *params {
                    sig.params.push(AbiParam::new(*pty));
                }
                if let Some(r) = ret {
                    sig.returns.push(AbiParam::new(*r));
                }
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &sig)
                    .map_err(|e| format!("Declare {} error: {}", name, e))?;
                self.functions.insert(name.to_string(), id);
            }
        }

        // ===== 内存分配函数 =====
        // bolide_alloc(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_bolide_alloc", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bolide_alloc".to_string(), id);

        // bolide_free(ptr, i64)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_bolide_free", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_bolide_free".to_string(), id);

        // ===== 线程函数（trampoline 方案） =====
        // thread_spawn_int(fn() -> i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr)); // 函数指针
        sig.returns.push(AbiParam::new(ptr)); // 线程句柄
        let id = self
            .module
            .declare_function("@_thread_spawn_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_thread_spawn_int".to_string(), id);

        // thread_spawn_float(fn() -> f64) -> ptr
        let id = self
            .module
            .declare_function("@_thread_spawn_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_thread_spawn_float".to_string(), id);

        // thread_spawn_ptr(fn() -> ptr) -> ptr
        let id = self
            .module
            .declare_function("@_thread_spawn_ptr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_thread_spawn_ptr".to_string(), id);

        // ===== 带环境的线程函数 =====
        // thread_spawn_int_with_env(fn(ptr) -> i64, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr)); // 函数指针
        sig.params.push(AbiParam::new(ptr)); // 环境指针
        sig.returns.push(AbiParam::new(ptr)); // 线程句柄
        let id = self
            .module
            .declare_function("@_thread_spawn_int_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_thread_spawn_int_with_env".to_string(), id);

        // thread_spawn_float_with_env
        let id = self
            .module
            .declare_function("@_thread_spawn_float_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_thread_spawn_float_with_env".to_string(), id);

        // thread_spawn_ptr_with_env
        let id = self
            .module
            .declare_function("@_thread_spawn_ptr_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_thread_spawn_ptr_with_env".to_string(), id);

        // thread_join_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_thread_join_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_thread_join_int".to_string(), id);

        // thread_join_float(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self
            .module
            .declare_function("@_thread_join_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_thread_join_float".to_string(), id);

        // thread_join_ptr(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_thread_join_ptr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_thread_join_ptr".to_string(), id);

        // thread_handle_free(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_thread_handle_free", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_thread_handle_free".to_string(), id);

        // thread_cancel(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_thread_cancel", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_thread_cancel".to_string(), id);

        // thread_is_cancelled(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_thread_is_cancelled", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_thread_is_cancelled".to_string(), id);

        // ===== 线程池函数 =====
        // pool_create(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_pool_create", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_create".to_string(), id);

        // pool_enter(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_pool_enter", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_enter".to_string(), id);

        // pool_exit()
        let mut sig = self.module.make_signature();
        let id = self
            .module
            .declare_function("@_pool_exit", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_exit".to_string(), id);

        // pool_is_active() -> i64
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_pool_is_active", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_is_active".to_string(), id);

        // pool_spawn_int(fn() -> i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr)); // 函数指针
        sig.returns.push(AbiParam::new(ptr)); // 任务句柄
        let id = self
            .module
            .declare_function("@_pool_spawn_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_spawn_int".to_string(), id);

        // pool_spawn_float
        let id = self
            .module
            .declare_function("@_pool_spawn_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_spawn_float".to_string(), id);

        // pool_spawn_ptr
        let id = self
            .module
            .declare_function("@_pool_spawn_ptr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_spawn_ptr".to_string(), id);

        // ===== 带环境的线程池函数 =====
        // pool_spawn_int_with_env(fn(ptr) -> i64, ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr)); // 函数指针
        sig.params.push(AbiParam::new(ptr)); // 环境指针
        sig.returns.push(AbiParam::new(ptr)); // 任务句柄
        let id = self
            .module
            .declare_function("@_pool_spawn_int_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_pool_spawn_int_with_env".to_string(), id);

        // pool_spawn_float_with_env
        let id = self
            .module
            .declare_function("@_pool_spawn_float_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_pool_spawn_float_with_env".to_string(), id);

        // pool_spawn_ptr_with_env
        let id = self
            .module
            .declare_function("@_pool_spawn_ptr_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_pool_spawn_ptr_with_env".to_string(), id);

        // pool_join_int(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_pool_join_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_join_int".to_string(), id);

        // pool_join_float(ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self
            .module
            .declare_function("@_pool_join_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_join_float".to_string(), id);

        // pool_join_ptr(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_pool_join_ptr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_join_ptr".to_string(), id);

        // pool_handle_free(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_pool_handle_free", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_handle_free".to_string(), id);

        // pool_select_wait_first(handles_ptr, count) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_pool_select_wait_first", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_pool_select_wait_first".to_string(), id);

        // pool_destroy(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_pool_destroy", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_pool_destroy".to_string(), id);

        // ===== 通道函数 =====
        // channel_create() -> ptr
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_channel_create", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_channel_create".to_string(), id);

        // channel_create_buffered(i64) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_channel_create_buffered", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_channel_create_buffered".to_string(), id);

        // channel_send(ptr, i64) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_channel_send", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_channel_send".to_string(), id);

        // channel_recv(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_channel_recv", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_channel_recv".to_string(), id);

        // channel_close(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_channel_close", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_channel_close".to_string(), id);

        // channel_free(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_channel_free", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_channel_free".to_string(), id);

        // channel_select(channels_ptr, count, timeout_ms, value_ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr)); // channels array pointer
        sig.params.push(AbiParam::new(types::I64)); // count
        sig.params.push(AbiParam::new(types::I64)); // timeout_ms
        sig.params.push(AbiParam::new(ptr)); // value output pointer
        sig.returns.push(AbiParam::new(types::I64)); // selected index
        let id = self
            .module
            .declare_function("@_channel_select", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_channel_select".to_string(), id);

        // ===== 协程函数 =====
        // coroutine_spawn_int(func_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_coroutine_spawn_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_spawn_int".to_string(), id);

        // coroutine_spawn_float(func_ptr) -> ptr
        let id = self
            .module
            .declare_function("@_coroutine_spawn_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_spawn_float".to_string(), id);

        // coroutine_spawn_ptr(func_ptr) -> ptr
        let id = self
            .module
            .declare_function("@_coroutine_spawn_ptr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_spawn_ptr".to_string(), id);

        // coroutine_spawn_*_with_env(func_ptr, env) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_coroutine_spawn_int_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_spawn_int_with_env".to_string(), id);
        let id = self
            .module
            .declare_function("@_coroutine_spawn_float_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_spawn_float_with_env".to_string(), id);
        let id = self
            .module
            .declare_function("@_coroutine_spawn_ptr_with_env", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_spawn_ptr_with_env".to_string(), id);

        // scope_enter(), scope_register(ptr), scope_exit()
        let mut sig = self.module.make_signature();
        let id = self
            .module
            .declare_function("@_scope_enter", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_scope_enter".to_string(), id);

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_scope_register", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_scope_register".to_string(), id);

        let mut sig = self.module.make_signature();
        let id = self
            .module
            .declare_function("@_scope_exit", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_scope_exit".to_string(), id);

        // select_wait_first(futures_ptr, count) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_select_wait_first", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_select_wait_first".to_string(), id);

        // coroutine_await_int(future_ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_coroutine_await_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_await_int".to_string(), id);

        // coroutine_await_float(future_ptr) -> f64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::F64));
        let id = self
            .module
            .declare_function("@_coroutine_await_float", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_await_float".to_string(), id);

        // coroutine_await_ptr(future_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_coroutine_await_ptr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_coroutine_await_ptr".to_string(), id);

        // coroutine_cancel(future_ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_coroutine_cancel", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_coroutine_cancel".to_string(), id);

        // coroutine_free(future_ptr)
        let id = self
            .module
            .declare_function("@_coroutine_free", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_coroutine_free".to_string(), id);

        // ===== Tuple 函数 =====
        // tuple_new(len) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_tuple_new", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_new".to_string(), id);

        // tuple_new_typed(len, type_tags_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_tuple_new_typed", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_new_typed".to_string(), id);

        // tuple_free(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_tuple_free", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_free".to_string(), id);

        // tuple_set(ptr, index, value) -- legacy，兼容
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_tuple_set", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_set".to_string(), id);

        // tuple_set_typed(ptr, index, value, type_tag)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I8));
        let id = self
            .module
            .declare_function("@_tuple_set_typed", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_set_typed".to_string(), id);

        // tuple_retain(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_tuple_retain", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_retain".to_string(), id);

        // tuple_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_tuple_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_clone".to_string(), id);

        // tuple_release(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_tuple_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_release".to_string(), id);

        // tuple_get_type(ptr, index) -> u8
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I8));
        let id = self
            .module
            .declare_function("@_tuple_get_type", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_get_type".to_string(), id);

        // tuple_get(ptr, index) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_tuple_get", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_get".to_string(), id);

        // tuple_len(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_tuple_len", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_len".to_string(), id);

        // print_tuple(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_print_tuple", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_print_tuple".to_string(), id);

        // tuple_debug_stats()
        let mut sig = self.module.make_signature();
        let id = self
            .module
            .declare_function("@_tuple_debug_stats", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_tuple_debug_stats".to_string(), id);

        // ffi_load_library(path_ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_ffi_load_library", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_ffi_load_library".to_string(), id);

        // ffi_get_symbol(lib_path_ptr, symbol_name_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_ffi_get_symbol", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_ffi_get_symbol".to_string(), id);

        // test_callback(callback, a, b) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr)); // callback
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_test_callback", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_test_callback".to_string(), id);

        // map_int(callback, value) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr)); // callback
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_map_int", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_map_int".to_string(), id);

        // ===== Object 函数 =====
        // object_alloc(size) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_object_alloc", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_object_alloc".to_string(), id);

        // object_release(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_object_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_object_release".to_string(), id);

        // object_set_class_tag(ptr, i64)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_object_set_class_tag", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_object_set_class_tag".to_string(), id);

        // object_class_tag(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_object_class_tag", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_object_class_tag".to_string(), id);

        // object_retain(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_object_retain", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_object_retain".to_string(), id);

        // object_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_object_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_object_clone".to_string(), id);

        // object_weak_retain(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_object_weak_retain", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_object_weak_retain".to_string(), id);

        // object_weak_release(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_object_weak_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_object_weak_release".to_string(), id);

        // object_weak_clone(ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_object_weak_clone", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_object_weak_clone".to_string(), id);

        // object_assert_alive(ptr)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_object_assert_alive", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions
            .insert("@_object_assert_alive".to_string(), id);

        // object_is_alive(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_object_is_alive", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_object_is_alive".to_string(), id);

        // object_ref_count(ptr) -> i64
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I64));
        let id = self
            .module
            .declare_function("@_object_ref_count", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_object_ref_count".to_string(), id);

        // ===== Closure 函数 =====
        // closure_new(fn_ptr, env_ptr, env_size, meta_ptr) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_closure_new", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_closure_new".to_string(), id);

        // closure_fn_ptr(closure) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_closure_fn_ptr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_closure_fn_ptr".to_string(), id);

        // closure_env_ptr(closure) -> ptr
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_closure_env_ptr", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_closure_env_ptr".to_string(), id);

        // closure_retain(closure)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_closure_retain", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_closure_retain".to_string(), id);

        // closure_release(closure)
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(ptr));
        let id = self
            .module
            .declare_function("@_closure_release", Linkage::Import, &sig)
            .map_err(|e| format!("{}", e))?;
        self.functions.insert("@_closure_release".to_string(), id);

        Ok(())
    }

    /// 编译函数（第二遍）
    fn compile_function(&mut self, func: &FuncDef) -> Result<(), String> {
        let func_id = *self
            .functions
            .get(&func.name)
            .ok_or_else(|| format!("Function {} not declared", func.name))?;

        // 预先计算参数类型
        let param_types: Vec<types::Type> = func
            .params
            .iter()
            .map(|p| self.bolide_type_to_cranelift(&self.normalize_bolide_type(&p.ty)))
            .collect();

        // 重建签名
        let mut sig = self.module.make_signature();
        for ty in &param_types {
            sig.params.push(AbiParam::new(*ty));
        }
        if let Some(ref ret_ty) = func.return_type {
            let ret_ty = self.normalize_bolide_type(ret_ty);
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(&ret_ty)));
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
        let adts = self.adts.clone();
        let class_tags = self.class_tags.clone();
        let async_funcs = self.async_funcs.clone();
        let extern_funcs = self.extern_funcs.clone();
        let modules = self.modules.clone();

        let lifetime_funcs = self.lifetime_funcs.clone();
        let funcsig_return_sources = self.funcsig_return_sources.clone();
        let funcsig_closure_param_indices = self.funcsig_closure_param_indices.clone();

        // 创建编译上下文
        let mut compile_ctx = CompileContext::new(
            &mut builder,
            &mut self.module,
            &self.global_data_ids,
            &self.global_var_types,
            func_refs,
            func_return_types,
            func_params,
            trampoline_refs,
            trampoline_param_types,
            trampoline_env_sizes,
            ptr_type,
            classes,
            adts,
            class_tags,
            async_funcs,
            extern_funcs,
            modules,
            func.lifetime_deps.clone(),
            func.name.clone(),
            lifetime_funcs,
            funcsig_return_sources,
            funcsig_closure_param_indices,
        );

        // 绑定参数到变量
        let params = compile_ctx.builder.block_params(entry_block).to_vec();

        for (i, param) in func.params.iter().enumerate() {
            let param_ty = compile_ctx.normalize_bolide_type(&param.ty);
            // 记录参数的 Bolide 类型
            compile_ctx
                .var_types
                .insert(param.name.clone(), param_ty.clone());

            // 函数类型参数只有在调用点实际传入闭包对象时才走闭包 ABI。
            if matches!(param_ty, BolideType::FuncSig(_, _) | BolideType::Func)
                && compile_ctx
                    .funcsig_closure_param_indices
                    .get(&func.name)
                    .map(|indices| indices.contains(&i))
                    .unwrap_or(false)
            {
                compile_ctx.closure_param_vars.insert(param.name.clone());
            }

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
                    if CompileContext::is_rc_type(&param_ty) {
                        compile_ctx
                            .rc_variables
                            .push((param.name.clone(), param_ty.clone()));
                    }
                }
                ParamMode::Ref => {
                    // Ref 参数：参数是指针地址，需要解引用
                    let ptr_addr = params[i];
                    let val =
                        compile_ctx
                            .builder
                            .ins()
                            .load(ptr_type, MemFlags::new(), ptr_addr, 0);
                    let var = compile_ctx.declare_variable(&param.name, param_types[i]);
                    compile_ctx.builder.def_var(var, val);
                    // 记录 Ref 参数，以便在函数返回前写回
                    compile_ctx
                        .ref_params
                        .push((param.name.clone(), var, ptr_addr));
                }
            }
        }

        // 绑定 super：在类方法体内，super 与 self 共享同一对象指针，
        // 但类型记为父类，使方法/字段解析从父类开始（静态派发 + 父类字段在前的布局兼容）。
        compile_ctx.bind_super_alias();

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
                // __main__ 返回前释放全局 RC 变量（避免退出泄漏）
                if func.name == "__main__" {
                    compile_ctx.emit_global_rc_cleanup();
                }
            }

            // 写回 Ref 参数
            compile_ctx.write_back_closure_captures();
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

        // 取出本函数创建的待编译闭包
        let pending = std::mem::take(&mut compile_ctx.pending_closures);

        builder.finalize();

        // 定义函数
        self.module
            .define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Define function error: {:?}", e))?;
        self.module.clear_context(&mut self.ctx);

        // 入队闭包 lifted 函数，待统一编译
        self.pending_closures.extend(pending);

        Ok(())
    }

    /// 编译一个 lifted 闭包函数：签名 (env_ptr, ...params) -> ret，
    /// 入口处从 env 恢复捕获变量为局部（借用，不参与 RC 清理）。
    fn compile_closure_job(&mut self, job: &ClosureJob) -> Result<(), String> {
        // 用户参数 cranelift 类型
        let param_types: Vec<types::Type> = job
            .params
            .iter()
            .map(|p| self.bolide_type_to_cranelift(&self.normalize_bolide_type(&p.ty)))
            .collect();

        // 构建签名: (env_ptr, ...params) -> ret
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
        self.ctx.func.name = cranelift_codegen::ir::UserFuncName::user(0, job.func_id.as_u32());

        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut builder_ctx);

        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        let mut func_refs = HashMap::new();
        for (name, id) in &self.functions {
            let func_ref = self.module.declare_func_in_func(*id, builder.func);
            func_refs.insert(name.clone(), func_ref);
        }

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
        let adts = self.adts.clone();
        let class_tags = self.class_tags.clone();
        let async_funcs = self.async_funcs.clone();
        let extern_funcs = self.extern_funcs.clone();
        let modules = self.modules.clone();
        let lifetime_funcs = self.lifetime_funcs.clone();
        let funcsig_return_sources = self.funcsig_return_sources.clone();
        let funcsig_closure_param_indices = self.funcsig_closure_param_indices.clone();

        let mut compile_ctx = CompileContext::new(
            &mut builder,
            &mut self.module,
            &self.global_data_ids,
            &self.global_var_types,
            func_refs,
            func_return_types,
            func_params,
            trampoline_refs,
            trampoline_param_types,
            trampoline_env_sizes,
            ptr_type,
            classes,
            adts,
            class_tags,
            async_funcs,
            extern_funcs,
            modules,
            None,
            job.name.clone(),
            lifetime_funcs,
            funcsig_return_sources,
            funcsig_closure_param_indices,
        );

        let block_params = compile_ctx.builder.block_params(entry_block).to_vec();
        let env_ptr = block_params[0];
        compile_ctx.closure_env_ptr = Some(env_ptr);
        compile_ctx.closure_captures = job.captures.clone();

        // 从 env 恢复捕获变量为局部（借用语义）
        for (i, (name, ty)) in job.captures.iter().enumerate() {
            let cty = compile_ctx.bolide_type_to_cranelift(ty);
            let offset = (i * 8) as i32;
            let raw =
                compile_ctx
                    .builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), env_ptr, offset);
            let val = if matches!(ty, BolideType::Float) {
                compile_ctx
                    .builder
                    .ins()
                    .bitcast(types::F64, MemFlags::new(), raw)
            } else {
                raw
            };
            let var = compile_ctx.declare_variable(name, cty);
            compile_ctx.builder.def_var(var, val);
            compile_ctx.var_types.insert(name.clone(), ty.clone());
            // 捕获为借用：不加入 rc_variables（闭包对象释放时统一处理）
        }

        // 绑定用户参数（block_params[1..]）
        for (i, param) in job.params.iter().enumerate() {
            let param_ty = compile_ctx.normalize_bolide_type(&param.ty);
            compile_ctx
                .var_types
                .insert(param.name.clone(), param_ty.clone());
            if matches!(param_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                compile_ctx.closure_param_vars.insert(param.name.clone());
            }
            let var = compile_ctx.declare_variable(&param.name, param_types[i]);
            compile_ctx.builder.def_var(var, block_params[i + 1]);
            // 闭包参数按借用处理（不接管所有权）
        }

        // 编译函数体
        let mut terminated = false;
        for stmt in &job.body {
            if terminated {
                break;
            }
            terminated = compile_ctx.compile_stmt(stmt)?;
        }

        if !terminated {
            compile_ctx.emit_rc_cleanup();
            compile_ctx.write_back_closure_captures();
            compile_ctx.write_back_ref_params();
            if let Some(ref ret_ty) = job.return_type {
                let zero = match ret_ty {
                    BolideType::Float => compile_ctx.builder.ins().f64const(0.0),
                    _ => compile_ctx.builder.ins().iconst(types::I64, 0),
                };
                compile_ctx.builder.ins().return_(&[zero]);
            } else {
                compile_ctx.builder.ins().return_(&[]);
            }
        }

        let pending = std::mem::take(&mut compile_ctx.pending_closures);

        builder.finalize();

        self.module
            .define_function(job.func_id, &mut self.ctx)
            .map_err(|e| format!("Define closure error: {:?}", e))?;
        self.module.clear_context(&mut self.ctx);

        self.pending_closures.extend(pending);
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
            Statement::For(for_stmt) => {
                self.collect_spawn_targets_in_expr(&for_stmt.iter, targets);
                for s in &for_stmt.body {
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
            Statement::Try(try_stmt) => {
                for s in &try_stmt.try_body {
                    self.collect_spawn_targets_in_stmt(s, targets);
                }
                for clause in &try_stmt.catch_clauses {
                    for s in &clause.body {
                        self.collect_spawn_targets_in_stmt(s, targets);
                    }
                }
                if let Some(ref finally_body) = try_stmt.finally {
                    for s in finally_body {
                        self.collect_spawn_targets_in_stmt(s, targets);
                    }
                }
            }
            Statement::Select(select_stmt) => {
                for branch in &select_stmt.branches {
                    if let bolide_parser::SelectBranch::Recv { body, .. } = branch {
                        for s in body {
                            self.collect_spawn_targets_in_stmt(s, targets);
                        }
                    }
                }
            }
            Statement::Throw(expr) => {
                self.collect_spawn_targets_in_expr(expr, targets);
            }
            Statement::AwaitScope(scope_stmt) => {
                for s in &scope_stmt.body {
                    self.collect_spawn_targets_in_stmt(s, targets);
                }
            }
            Statement::SpawnSelect(select_stmt) => {
                for branch in &select_stmt.branches {
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
                            targets.push(func_name.to_string());
                        }
                    }
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
            Expr::Spawn(func_name, args) | Expr::SpawnThread(func_name, args) => {
                if self
                    .func_params
                    .get(func_name)
                    .map(|p| !p.is_empty())
                    .unwrap_or(false)
                {
                    targets.push(func_name.clone());
                }
                for arg in args {
                    self.collect_spawn_targets_in_expr(arg, targets);
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
                            targets.push(func_name.to_string());
                        }
                    }
                    self.collect_spawn_targets_in_expr(expr, targets);
                }
            }
            Expr::NamedArg(_, value) | Expr::SpreadArg(value) | Expr::KwSpreadArg(value) => {
                self.collect_spawn_targets_in_expr(value, targets);
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
                    if self.async_funcs.contains(func_name)
                        && self
                            .func_params
                            .get(func_name)
                            .map(|p| !p.is_empty())
                            .unwrap_or(false)
                    {
                        targets.push(func_name.clone());
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
        let params = self
            .func_params
            .get(target_func_name)
            .ok_or_else(|| format!("Function {} not found", target_func_name))?
            .clone();
        let return_type = self
            .func_return_types
            .get(target_func_name)
            .ok_or_else(|| format!("Function {} return type not found", target_func_name))?
            .clone();

        // 计算 env 大小（每个参数 8 字节对齐）
        let env_size = (params.len() * 8) as i64;
        let param_types: Vec<BolideType> = params.iter().map(|p| p.ty.clone()).collect();

        // 生成 trampoline 名称
        let trampoline_name = format!(
            "__trampoline_{}_{}",
            target_func_name, self.trampoline_counter
        );
        self.trampoline_counter += 1;

        // 声明 trampoline 签名: (env: ptr) -> return_type
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(self.ptr_type));
        if let Some(ref ret_ty) = return_type {
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
        }

        let trampoline_id = self
            .module
            .declare_function(&trampoline_name, Linkage::Local, &sig)
            .map_err(|e| format!("Declare trampoline error: {}", e))?;

        // 预先计算参数的 Cranelift 类型（避免借用冲突）
        let cranelift_param_types: Vec<types::Type> = params
            .iter()
            .map(|p| self.bolide_type_to_cranelift(&p.ty))
            .collect();

        // 获取目标函数 ID
        let target_func_id = *self
            .functions
            .get(target_func_name)
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
        let target_func_ref = self
            .module
            .declare_func_in_func(target_func_id, builder.func);

        // 从 env 中加载参数
        let mut call_args = Vec::new();
        for (i, cranelift_type) in cranelift_param_types.iter().enumerate() {
            let offset = (i * 8) as i32;
            let val = builder
                .ins()
                .load(*cranelift_type, MemFlags::trusted(), env_ptr, offset);
            call_args.push(val);
        }

        // 调用目标函数
        let call = builder.ins().call(target_func_ref, &call_args);

        // 返回结果（先复制结果值以避免借用冲突）
        let result_val = {
            let results = builder.inst_results(call);
            if results.is_empty() {
                None
            } else {
                Some(results[0])
            }
        };

        // 释放 RC 类型参数（spawn 时 clone 的副本）
        for (i, param) in params.iter().enumerate() {
            let release_func = match &param.ty {
                BolideType::Str => Some("@_string_release"),
                BolideType::BigInt => Some("@_bigint_release"),
                BolideType::Decimal => Some("@_decimal_release"),
                BolideType::List(_) => Some("@_list_release"),
                BolideType::Dynamic => Some("@_dynamic_release"),
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
        self.module
            .define_function(trampoline_id, &mut self.ctx)
            .map_err(|e| format!("Define trampoline error: {}", e))?;
        self.module.clear_context(&mut self.ctx);

        // 存储 trampoline 信息
        self.trampolines.insert(
            target_func_name.to_string(),
            TrampolineInfo {
                func_id: trampoline_id,
                param_types,
                env_size,
            },
        );

        self.functions.insert(trampoline_name, trampoline_id);

        Ok(())
    }

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
            BolideType::Func => self.ptr_type,          // 函数指针
            BolideType::FuncSig(_, _) => self.ptr_type, // 带签名的函数指针
            BolideType::List(_) => self.ptr_type,
            BolideType::Dict(_, _) => self.ptr_type, // 字典作为指针
            BolideType::Tuple(_) => self.ptr_type,   // 元组作为指针
            BolideType::Generic(_) => self.ptr_type,
            BolideType::Adt(_, _) => self.ptr_type,

            BolideType::Custom(_) => self.ptr_type,
            BolideType::Weak(inner) => self.bolide_type_to_cranelift(inner),
            BolideType::Unowned(inner) => self.bolide_type_to_cranelift(inner),
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

    fn lookup_method_return_type(&self, class_name: &str, method_name: &str) -> Option<BolideType> {
        let mut current = self.normalize_type_name(class_name);
        loop {
            let full_name = format!("{}_{}", current, method_name);
            if let Some(ret) = self.func_return_types.get(&full_name) {
                return ret.clone();
            }
            if let Some(class_info) = self.classes.get(&current) {
                if let Some(ref parent) = class_info.parent {
                    current = parent.clone();
                    continue;
                }
            }
            return None;
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

        // 按程序声明顺序分配异常类型标签（>=100），保证 JIT/AOT 一致
        for stmt in &program.statements {
            if let Statement::ClassDef(class_def) = stmt {
                if !self.class_tags.contains_key(&class_def.name) {
                    let tag = 100 + self.class_tags.len() as i64;
                    self.class_tags.insert(class_def.name.clone(), tag);
                }
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

        let class_def = class_defs
            .get(name)
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
                default_value: field.default_value.clone(),
            });
            offset += 8;
        }

        let methods: Vec<String> = class_def.methods.iter().map(|m| m.name.clone()).collect();

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
        let class_info = self
            .classes
            .get(class_name)
            .ok_or_else(|| format!("Class not found: {}", class_name))?
            .clone();

        // 统计有默认值的字段数：有默认值的字段放到签名末尾，调用方可省略
        // 简单策略：所有字段都声明为参数（保持签名一致），缺参在构造体填默认值
        let mut sig = self.module.make_signature();

        for field in &class_info.fields {
            let ty = self.bolide_type_to_cranelift(&field.ty);
            sig.params.push(AbiParam::new(ty));
        }
        sig.returns.push(AbiParam::new(self.ptr_type));

        let func_name = class_name.to_string();
        let func_id = self
            .module
            .declare_function(&func_name, Linkage::Export, &sig)
            .map_err(|e| format!("Declare constructor error: {}", e))?;

        self.functions.insert(func_name.clone(), func_id);
        self.func_return_types.insert(
            func_name.clone(),
            Some(BolideType::Custom(class_name.to_string())),
        );

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
        self.func_params.insert(func_name, params);

        Ok(())
    }

    /// 编译类构造函数
    fn compile_class_constructor(&mut self, class_name: &str) -> Result<(), String> {
        let class_info = self
            .classes
            .get(class_name)
            .ok_or_else(|| format!("Class not found: {}", class_name))?
            .clone();

        let func_id = *self
            .functions
            .get(class_name)
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
        let object_alloc_id = *self
            .functions
            .get("@_object_alloc")
            .ok_or("object_alloc not found")?;
        let object_alloc_ref = self
            .module
            .declare_func_in_func(object_alloc_id, builder.func);

        // 调用 object_alloc(size) 分配内存
        let size_val = builder.ins().iconst(types::I64, class_info.size as i64);
        let call = builder.ins().call(object_alloc_ref, &[size_val]);
        let obj_ptr = builder.inst_results(call)[0];

        let class_tag = *self
            .class_tags
            .get(class_name)
            .ok_or_else(|| format!("Class tag not found: {}", class_name))?;
        let set_tag_id = *self
            .functions
            .get("@_object_set_class_tag")
            .ok_or("object_set_class_tag not found")?;
        let set_tag_ref = self.module.declare_func_in_func(set_tag_id, builder.func);
        let tag_val = builder.ins().iconst(types::I64, class_tag);
        builder.ins().call(set_tag_ref, &[obj_ptr, tag_val]);
        let closure_retain_ref = if class_info
            .fields
            .iter()
            .any(|field| matches!(field.ty, BolideType::FuncSig(_, _) | BolideType::Func))
        {
            let closure_retain_id = *self
                .functions
                .get("@_closure_retain")
                .ok_or("closure_retain not found")?;
            Some(
                self.module
                    .declare_func_in_func(closure_retain_id, builder.func),
            )
        } else {
            None
        };

        // 使用传入的参数初始化字段，缺参时填零值
        for (i, field) in class_info.fields.iter().enumerate() {
            let field_ptr = builder.ins().iadd_imm(obj_ptr, field.offset as i64);
            // 使用传入的参数值，如果没有则使用零值
            // TODO: 字段默认值（field.default_value）需在构造器内嵌表达式求值，
            //       但 current builder 是局部 func_ctx，需分离 compile_class_constructor
            //       的 builder 切换逻辑。
            let val = if i < params.len() {
                params[i]
            } else {
                builder.ins().iconst(types::I64, 0)
            };
            if let Some(retain_ref) = closure_retain_ref {
                if matches!(field.ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                    builder.ins().call(retain_ref, &[val]);
                }
            }
            builder.ins().store(MemFlags::new(), val, field_ptr, 0);
        }

        // 返回对象指针
        builder.ins().return_(&[obj_ptr]);
        builder.finalize();

        // 编译函数
        self.module
            .define_function(func_id, &mut self.ctx)
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
                        sig.returns
                            .push(AbiParam::new(self.bolide_type_to_cranelift(ret_ty)));
                    }

                    let func_id = self
                        .module
                        .declare_function(&method_name, Linkage::Export, &sig)
                        .map_err(|e| format!("Declare method error: {}", e))?;

                    self.functions.insert(method_name.clone(), func_id);
                    self.func_return_types
                        .insert(method_name.clone(), method.return_type.clone());

                    // 存储方法参数（包含隐式 self）
                    let mut params_with_self = vec![Param {
                        name: "self".to_string(),
                        ty: BolideType::Custom(class_def.name.clone()),
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

    fn collect_funcsig_return_sources(
        &self,
        program: &Program,
    ) -> HashMap<String, FuncSigReturnSource> {
        let mut funcs: Vec<(String, Vec<Param>, Vec<Statement>)> = Vec::new();
        for stmt in &program.statements {
            match stmt {
                Statement::FuncDef(func)
                    if matches!(
                        func.return_type,
                        Some(BolideType::FuncSig(_, _) | BolideType::Func)
                    ) =>
                {
                    funcs.push((func.name.clone(), func.params.clone(), func.body.clone()));
                }
                Statement::ClassDef(class_def) => {
                    for method in &class_def.methods {
                        if matches!(
                            method.return_type,
                            Some(BolideType::FuncSig(_, _) | BolideType::Func)
                        ) {
                            let mut params = vec![Param {
                                name: "self".to_string(),
                                ty: BolideType::Custom(class_def.name.clone()),
                                mode: ParamMode::Borrow,
                                default_value: None,
                                is_variadic: false,
                                is_kw_variadic: false,
                            }];
                            params.extend(method.params.clone());
                            funcs.push((
                                format!("{}_{}", class_def.name, method.name),
                                params,
                                method.body.clone(),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        let mut sources: HashMap<String, FuncSigReturnSource> = funcs
            .iter()
            .map(|(name, _, _)| (name.clone(), FuncSigReturnSource::Unknown))
            .collect();
        for _ in 0..funcs.len().saturating_add(1) {
            let mut changed = false;
            for (name, params, body) in &funcs {
                let source = self.analyze_funcsig_return_source(params, body, &sources);
                if sources.get(name).copied() != Some(source) {
                    sources.insert(name.clone(), source);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        sources
    }

    fn analyze_funcsig_return_source(
        &self,
        params: &[Param],
        body: &[Statement],
        summaries: &HashMap<String, FuncSigReturnSource>,
    ) -> FuncSigReturnSource {
        let param_sources: HashMap<String, FuncSigReturnSource> = params
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                if matches!(p.ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                    Some((p.name.clone(), FuncSigReturnSource::Param(i)))
                } else {
                    None
                }
            })
            .collect();
        let mut locals = HashMap::new();
        let mut returns = Vec::new();
        self.scan_funcsig_return_stmts(body, &param_sources, &mut locals, summaries, &mut returns);
        Self::merge_funcsig_sources(&returns)
    }

    fn scan_funcsig_return_stmts(
        &self,
        stmts: &[Statement],
        param_sources: &HashMap<String, FuncSigReturnSource>,
        locals: &mut HashMap<String, FuncSigReturnSource>,
        summaries: &HashMap<String, FuncSigReturnSource>,
        returns: &mut Vec<FuncSigReturnSource>,
    ) {
        for stmt in stmts {
            match stmt {
                Statement::Return(Some(expr)) => returns.push(self.funcsig_expr_source_static(
                    expr,
                    param_sources,
                    locals,
                    summaries,
                )),
                Statement::Return(None) => returns.push(FuncSigReturnSource::Unknown),
                Statement::VarDecl(decl) => {
                    if let Some(value) = &decl.value {
                        let source = self.funcsig_expr_source_static(
                            value,
                            param_sources,
                            locals,
                            summaries,
                        );
                        if source != FuncSigReturnSource::Unknown {
                            locals.insert(decl.name.clone(), source);
                        }
                    }
                }
                Statement::Assign(assign) => {
                    if let Expr::Ident(name) = &assign.target {
                        let source = self.funcsig_expr_source_static(
                            &assign.value,
                            param_sources,
                            locals,
                            summaries,
                        );
                        if source != FuncSigReturnSource::Unknown {
                            locals.insert(name.clone(), source);
                        }
                    }
                }
                Statement::If(if_stmt) => {
                    let mut then_locals = locals.clone();
                    self.scan_funcsig_return_stmts(
                        &if_stmt.then_body,
                        param_sources,
                        &mut then_locals,
                        summaries,
                        returns,
                    );
                    for (_, body) in &if_stmt.elif_branches {
                        let mut branch_locals = locals.clone();
                        self.scan_funcsig_return_stmts(
                            body,
                            param_sources,
                            &mut branch_locals,
                            summaries,
                            returns,
                        );
                    }
                    if let Some(body) = &if_stmt.else_body {
                        let mut else_locals = locals.clone();
                        self.scan_funcsig_return_stmts(
                            body,
                            param_sources,
                            &mut else_locals,
                            summaries,
                            returns,
                        );
                    }
                }
                Statement::While(while_stmt) => {
                    let mut inner_locals = locals.clone();
                    self.scan_funcsig_return_stmts(
                        &while_stmt.body,
                        param_sources,
                        &mut inner_locals,
                        summaries,
                        returns,
                    );
                }
                Statement::For(for_stmt) => {
                    let mut inner_locals = locals.clone();
                    self.scan_funcsig_return_stmts(
                        &for_stmt.body,
                        param_sources,
                        &mut inner_locals,
                        summaries,
                        returns,
                    );
                }
                Statement::Try(try_stmt) => {
                    let mut try_locals = locals.clone();
                    self.scan_funcsig_return_stmts(
                        &try_stmt.try_body,
                        param_sources,
                        &mut try_locals,
                        summaries,
                        returns,
                    );
                    for clause in &try_stmt.catch_clauses {
                        let mut catch_locals = locals.clone();
                        self.scan_funcsig_return_stmts(
                            &clause.body,
                            param_sources,
                            &mut catch_locals,
                            summaries,
                            returns,
                        );
                    }
                    if let Some(body) = &try_stmt.finally {
                        let mut finally_locals = locals.clone();
                        self.scan_funcsig_return_stmts(
                            body,
                            param_sources,
                            &mut finally_locals,
                            summaries,
                            returns,
                        );
                    }
                }
                Statement::Pool(pool_stmt) => {
                    let mut inner_locals = locals.clone();
                    self.scan_funcsig_return_stmts(
                        &pool_stmt.body,
                        param_sources,
                        &mut inner_locals,
                        summaries,
                        returns,
                    );
                }
                _ => {}
            }
        }
    }

    fn funcsig_expr_source_static(
        &self,
        expr: &Expr,
        param_sources: &HashMap<String, FuncSigReturnSource>,
        locals: &HashMap<String, FuncSigReturnSource>,
        summaries: &HashMap<String, FuncSigReturnSource>,
    ) -> FuncSigReturnSource {
        match expr {
            Expr::Ident(name) => locals
                .get(name)
                .copied()
                .or_else(|| param_sources.get(name).copied())
                .unwrap_or_else(|| {
                    if self.functions.contains_key(name) {
                        FuncSigReturnSource::Raw
                    } else {
                        FuncSigReturnSource::Unknown
                    }
                }),
            Expr::Closure { .. } => FuncSigReturnSource::Closure,
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    return self.substitute_funcsig_call_source(
                        name,
                        summaries
                            .get(name)
                            .copied()
                            .unwrap_or(FuncSigReturnSource::Unknown),
                        args,
                        param_sources,
                        locals,
                        summaries,
                    );
                }
                if let Expr::Member(base, member) = callee.as_ref() {
                    if let Expr::Ident(module_name) = base.as_ref() {
                        if self.modules.contains_key(module_name) {
                            let name = format!("@{}_{}", module_name, member);
                            return self.substitute_funcsig_call_source(
                                &name,
                                summaries
                                    .get(&name)
                                    .copied()
                                    .unwrap_or(FuncSigReturnSource::Unknown),
                                args,
                                param_sources,
                                locals,
                                summaries,
                            );
                        }
                    }
                }
                FuncSigReturnSource::Unknown
            }
            _ => FuncSigReturnSource::Unknown,
        }
    }

    fn substitute_funcsig_call_source(
        &self,
        func_name: &str,
        source: FuncSigReturnSource,
        args: &[Expr],
        param_sources: &HashMap<String, FuncSigReturnSource>,
        locals: &HashMap<String, FuncSigReturnSource>,
        summaries: &HashMap<String, FuncSigReturnSource>,
    ) -> FuncSigReturnSource {
        match source {
            FuncSigReturnSource::Param(i) => {
                if self
                    .funcsig_closure_param_indices
                    .get(func_name)
                    .map(|indices| indices.contains(&i))
                    .unwrap_or(false)
                {
                    FuncSigReturnSource::Closure
                } else {
                    args.get(i)
                        .map(|arg| {
                            self.funcsig_expr_source_static(arg, param_sources, locals, summaries)
                        })
                        .unwrap_or(FuncSigReturnSource::Unknown)
                }
            }
            FuncSigReturnSource::ParamSet(mask) => {
                let mut sources = Vec::new();
                for i in 0..64 {
                    if (mask & (1u64 << i)) == 0 {
                        continue;
                    }
                    sources.push(
                        if self
                            .funcsig_closure_param_indices
                            .get(func_name)
                            .map(|indices| indices.contains(&i))
                            .unwrap_or(false)
                        {
                            FuncSigReturnSource::Closure
                        } else {
                            args.get(i)
                                .map(|arg| {
                                    self.funcsig_expr_source_static(
                                        arg,
                                        param_sources,
                                        locals,
                                        summaries,
                                    )
                                })
                                .unwrap_or(FuncSigReturnSource::Unknown)
                        },
                    );
                }
                Self::merge_funcsig_sources(&sources)
            }
            other => other,
        }
    }

    fn merge_funcsig_sources(sources: &[FuncSigReturnSource]) -> FuncSigReturnSource {
        let mut result: Option<FuncSigReturnSource> = None;
        for source in sources.iter().copied() {
            result = Some(match (result, source) {
                (None, source) => source,
                (Some(current), source) if current == source => current,
                (Some(FuncSigReturnSource::Param(a)), FuncSigReturnSource::Param(b)) => {
                    let mut mask = 0u64;
                    if a < 64 {
                        mask |= 1u64 << a;
                    }
                    if b < 64 {
                        mask |= 1u64 << b;
                    }
                    if mask == 0 {
                        FuncSigReturnSource::Unknown
                    } else {
                        FuncSigReturnSource::ParamSet(mask)
                    }
                }
                (Some(FuncSigReturnSource::Param(a)), FuncSigReturnSource::ParamSet(mask))
                | (Some(FuncSigReturnSource::ParamSet(mask)), FuncSigReturnSource::Param(a)) => {
                    if a < 64 {
                        FuncSigReturnSource::ParamSet(mask | (1u64 << a))
                    } else {
                        FuncSigReturnSource::Unknown
                    }
                }
                (Some(FuncSigReturnSource::ParamSet(a)), FuncSigReturnSource::ParamSet(b)) => {
                    FuncSigReturnSource::ParamSet(a | b)
                }
                _ => FuncSigReturnSource::Unknown,
            });
            if result == Some(FuncSigReturnSource::Unknown) {
                return FuncSigReturnSource::Unknown;
            }
        }
        result.unwrap_or(FuncSigReturnSource::Unknown)
    }

    fn funcsig_source_param_indices(source: FuncSigReturnSource) -> Vec<usize> {
        match source {
            FuncSigReturnSource::Param(i) => vec![i],
            FuncSigReturnSource::ParamSet(mask) => {
                (0..64).filter(|i| (mask & (1u64 << i)) != 0).collect()
            }
            _ => Vec::new(),
        }
    }

    fn register_returned_funcsig_params_as_closure(
        &self,
        out: &mut HashMap<String, HashSet<usize>>,
    ) {
        for (func_name, source) in &self.funcsig_return_sources {
            let Some(params) = self.func_params.get(func_name) else {
                continue;
            };
            for index in Self::funcsig_source_param_indices(*source) {
                let Some(param) = params.get(index) else {
                    continue;
                };
                if matches!(param.ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                    out.entry(func_name.clone()).or_default().insert(index);
                }
            }
        }
    }

    fn collect_funcsig_closure_param_indices(
        &self,
        program: &Program,
    ) -> HashMap<String, HashSet<usize>> {
        let mut out: HashMap<String, HashSet<usize>> = HashMap::new();
        self.register_returned_funcsig_params_as_closure(&mut out);
        for _ in 0..32 {
            let before = out.clone();
            self.scan_closure_param_stmts(
                &program.statements,
                None,
                &[],
                &mut HashMap::new(),
                &mut out,
            );
            if out == before {
                break;
            }
        }
        out
    }

    fn funcsig_raw_adapter_sigs(
        &self,
    ) -> HashMap<String, (Vec<BolideType>, Option<Box<BolideType>>)> {
        let mut out = HashMap::new();
        for ty in self.global_var_types.values() {
            Self::collect_funcsig_adapter_type(ty, &mut out);
        }
        for class_info in self.classes.values() {
            for field in &class_info.fields {
                Self::collect_funcsig_adapter_type(&field.ty, &mut out);
            }
        }
        for params in self.func_params.values() {
            for param in params {
                Self::collect_funcsig_adapter_type(&param.ty, &mut out);
            }
        }
        for ret in self.func_return_types.values().flatten() {
            Self::collect_funcsig_adapter_type(ret, &mut out);
        }
        for (func_name, indices) in &self.funcsig_closure_param_indices {
            let Some(params) = self.func_params.get(func_name) else {
                continue;
            };
            for index in indices {
                let Some(param) = params.get(*index) else {
                    continue;
                };
                if let BolideType::FuncSig(sig_params, sig_ret) = &param.ty {
                    let name = funcsig_adapter_name(sig_params, sig_ret);
                    out.entry(name)
                        .or_insert_with(|| (sig_params.clone(), sig_ret.clone()));
                }
            }
        }
        out
    }

    fn collect_funcsig_adapter_type(
        ty: &BolideType,
        out: &mut HashMap<String, (Vec<BolideType>, Option<Box<BolideType>>)>,
    ) {
        match ty {
            BolideType::FuncSig(params, ret) => {
                let name = funcsig_adapter_name(params, ret);
                out.entry(name)
                    .or_insert_with(|| (params.clone(), ret.clone()));
                for param in params {
                    Self::collect_funcsig_adapter_type(param, out);
                }
                if let Some(ret_ty) = ret {
                    Self::collect_funcsig_adapter_type(ret_ty, out);
                }
            }
            BolideType::List(inner) | BolideType::Weak(inner) | BolideType::Unowned(inner) => {
                Self::collect_funcsig_adapter_type(inner, out);
            }
            BolideType::Dict(key, value) => {
                Self::collect_funcsig_adapter_type(key, out);
                Self::collect_funcsig_adapter_type(value, out);
            }
            BolideType::Tuple(items) => {
                for item in items {
                    Self::collect_funcsig_adapter_type(item, out);
                }
            }
            _ => {}
        }
    }

    fn declare_funcsig_raw_adapters(&mut self) -> Result<(), String> {
        for (name, (params, ret)) in self.funcsig_raw_adapter_sigs() {
            if self.functions.contains_key(&name) {
                continue;
            }
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(self.ptr_type));
            for param in &params {
                sig.params
                    .push(AbiParam::new(self.bolide_type_to_cranelift(param)));
            }
            let ret_ty = ret
                .as_ref()
                .map(|t| (**t).clone())
                .unwrap_or(BolideType::Int);
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(&ret_ty)));
            let func_id = self
                .module
                .declare_function(&name, Linkage::Local, &sig)
                .map_err(|e| format!("Declare funcsig adapter error: {}", e))?;
            self.functions.insert(name, func_id);
        }
        Ok(())
    }

    fn compile_funcsig_raw_adapters(&mut self) -> Result<(), String> {
        for (name, (params, ret)) in self.funcsig_raw_adapter_sigs() {
            let Some(func_id) = self.functions.get(&name).copied() else {
                continue;
            };
            let param_cl_types: Vec<types::Type> = params
                .iter()
                .map(|param| self.bolide_type_to_cranelift(param))
                .collect();
            let ret_ty = ret
                .as_ref()
                .map(|t| (**t).clone())
                .unwrap_or(BolideType::Int);
            let ret_cl_type = self.bolide_type_to_cranelift(&ret_ty);

            self.ctx.func = Function::new();
            self.ctx.func.signature = self.module.make_signature();
            self.ctx
                .func
                .signature
                .params
                .push(AbiParam::new(self.ptr_type));
            for param_ty in &param_cl_types {
                self.ctx
                    .func
                    .signature
                    .params
                    .push(AbiParam::new(*param_ty));
            }
            self.ctx
                .func
                .signature
                .returns
                .push(AbiParam::new(ret_cl_type));
            self.ctx.func.name = cranelift_codegen::ir::UserFuncName::user(0, func_id.as_u32());

            let mut builder_ctx = FunctionBuilderContext::new();
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut builder_ctx);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);

            let block_params = builder.block_params(entry).to_vec();
            let env_ptr = block_params[0];
            let fn_ptr = builder
                .ins()
                .load(self.ptr_type, MemFlags::trusted(), env_ptr, 0);

            #[cfg(target_os = "windows")]
            let mut raw_sig = Signature::new(CallConv::WindowsFastcall);
            #[cfg(not(target_os = "windows"))]
            let mut raw_sig = Signature::new(CallConv::SystemV);
            for param_ty in &param_cl_types {
                raw_sig.params.push(AbiParam::new(*param_ty));
            }
            raw_sig.returns.push(AbiParam::new(ret_cl_type));
            let sig_ref = builder.import_signature(raw_sig);
            let call = builder
                .ins()
                .call_indirect(sig_ref, fn_ptr, &block_params[1..]);
            let result = builder.inst_results(call)[0];
            builder.ins().return_(&[result]);
            builder.finalize();

            self.module
                .define_function(func_id, &mut self.ctx)
                .map_err(|e| format!("Define funcsig adapter error: {}", e))?;
            self.module.clear_context(&mut self.ctx);
        }
        Ok(())
    }

    fn scan_closure_param_stmts(
        &self,
        stmts: &[Statement],
        current_func: Option<&str>,
        current_params: &[Param],
        locals: &mut HashMap<String, FuncSigReturnSource>,
        out: &mut HashMap<String, HashSet<usize>>,
    ) {
        for stmt in stmts {
            match stmt {
                Statement::FuncDef(func) => {
                    self.scan_closure_param_stmts(
                        &func.body,
                        Some(&func.name),
                        &func.params,
                        &mut HashMap::new(),
                        out,
                    );
                }
                Statement::ClassDef(class_def) => {
                    for method in &class_def.methods {
                        let mut params = vec![Param {
                            name: "self".to_string(),
                            ty: BolideType::Custom(class_def.name.clone()),
                            mode: ParamMode::Borrow,
                            default_value: None,
                            is_variadic: false,
                            is_kw_variadic: false,
                        }];
                        params.extend(method.params.clone());
                        let method_name = format!("{}_{}", class_def.name, method.name);
                        self.scan_closure_param_stmts(
                            &method.body,
                            Some(&method_name),
                            &params,
                            &mut HashMap::new(),
                            out,
                        );
                    }
                }
                Statement::VarDecl(decl) => {
                    if let Some(value) = &decl.value {
                        self.scan_closure_param_expr(
                            value,
                            current_func,
                            current_params,
                            locals,
                            out,
                        );
                        let source = self.funcsig_expr_source_for_closure_scan(
                            value,
                            current_func,
                            current_params,
                            locals,
                            out,
                        );
                        if source != FuncSigReturnSource::Unknown {
                            locals.insert(decl.name.clone(), source);
                        }
                    }
                }
                Statement::Assign(assign) => {
                    self.scan_closure_param_expr(
                        &assign.value,
                        current_func,
                        current_params,
                        locals,
                        out,
                    );
                    if let Expr::Ident(name) = &assign.target {
                        let source = self.funcsig_expr_source_for_closure_scan(
                            &assign.value,
                            current_func,
                            current_params,
                            locals,
                            out,
                        );
                        if source != FuncSigReturnSource::Unknown {
                            locals.insert(name.clone(), source);
                        }
                    }
                }
                Statement::If(if_stmt) => {
                    self.scan_closure_param_expr(
                        &if_stmt.condition,
                        current_func,
                        current_params,
                        locals,
                        out,
                    );
                    let mut then_locals = locals.clone();
                    self.scan_closure_param_stmts(
                        &if_stmt.then_body,
                        current_func,
                        current_params,
                        &mut then_locals,
                        out,
                    );
                    for (cond, body) in &if_stmt.elif_branches {
                        self.scan_closure_param_expr(
                            cond,
                            current_func,
                            current_params,
                            locals,
                            out,
                        );
                        let mut branch_locals = locals.clone();
                        self.scan_closure_param_stmts(
                            body,
                            current_func,
                            current_params,
                            &mut branch_locals,
                            out,
                        );
                    }
                    if let Some(body) = &if_stmt.else_body {
                        let mut else_locals = locals.clone();
                        self.scan_closure_param_stmts(
                            body,
                            current_func,
                            current_params,
                            &mut else_locals,
                            out,
                        );
                    }
                }
                Statement::While(while_stmt) => {
                    self.scan_closure_param_expr(
                        &while_stmt.condition,
                        current_func,
                        current_params,
                        locals,
                        out,
                    );
                    let mut inner_locals = locals.clone();
                    self.scan_closure_param_stmts(
                        &while_stmt.body,
                        current_func,
                        current_params,
                        &mut inner_locals,
                        out,
                    );
                }
                Statement::For(for_stmt) => {
                    self.scan_closure_param_expr(
                        &for_stmt.iter,
                        current_func,
                        current_params,
                        locals,
                        out,
                    );
                    let mut inner_locals = locals.clone();
                    self.scan_closure_param_stmts(
                        &for_stmt.body,
                        current_func,
                        current_params,
                        &mut inner_locals,
                        out,
                    );
                }
                Statement::Try(try_stmt) => {
                    let mut try_locals = locals.clone();
                    self.scan_closure_param_stmts(
                        &try_stmt.try_body,
                        current_func,
                        current_params,
                        &mut try_locals,
                        out,
                    );
                    for clause in &try_stmt.catch_clauses {
                        let mut catch_locals = locals.clone();
                        self.scan_closure_param_stmts(
                            &clause.body,
                            current_func,
                            current_params,
                            &mut catch_locals,
                            out,
                        );
                    }
                    if let Some(body) = &try_stmt.finally {
                        let mut finally_locals = locals.clone();
                        self.scan_closure_param_stmts(
                            body,
                            current_func,
                            current_params,
                            &mut finally_locals,
                            out,
                        );
                    }
                }
                Statement::Pool(pool_stmt) => {
                    let mut inner_locals = locals.clone();
                    self.scan_closure_param_stmts(
                        &pool_stmt.body,
                        current_func,
                        current_params,
                        &mut inner_locals,
                        out,
                    );
                }
                Statement::Expr(expr) | Statement::Return(Some(expr)) | Statement::Throw(expr) => {
                    self.scan_closure_param_expr(expr, current_func, current_params, locals, out);
                }
                _ => {}
            }
        }
    }

    fn scan_closure_param_expr(
        &self,
        expr: &Expr,
        current_func: Option<&str>,
        current_params: &[Param],
        locals: &HashMap<String, FuncSigReturnSource>,
        out: &mut HashMap<String, HashSet<usize>>,
    ) {
        match expr {
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    self.record_closure_func_args(
                        name,
                        args,
                        current_func,
                        current_params,
                        locals,
                        out,
                        0,
                    );
                } else if let Expr::Member(base, member) = callee.as_ref() {
                    if let Expr::Ident(module_name) = base.as_ref() {
                        if self.modules.contains_key(module_name) {
                            let name = format!("@{}_{}", module_name, member);
                            self.record_closure_func_args(
                                &name,
                                args,
                                current_func,
                                current_params,
                                locals,
                                out,
                                0,
                            );
                        }
                    }
                    self.scan_closure_param_expr(base, current_func, current_params, locals, out);
                } else {
                    self.scan_closure_param_expr(callee, current_func, current_params, locals, out);
                }
                for arg in args {
                    self.scan_closure_param_expr(arg, current_func, current_params, locals, out);
                }
            }
            Expr::BinOp(left, _, right) => {
                self.scan_closure_param_expr(left, current_func, current_params, locals, out);
                self.scan_closure_param_expr(right, current_func, current_params, locals, out);
            }
            Expr::UnaryOp(_, inner) | Expr::Member(inner, _) | Expr::Await(inner) => {
                self.scan_closure_param_expr(inner, current_func, current_params, locals, out);
            }
            Expr::Index(base, index) => {
                self.scan_closure_param_expr(base, current_func, current_params, locals, out);
                self.scan_closure_param_expr(index, current_func, current_params, locals, out);
            }
            Expr::Slice(base, start, end, step) => {
                self.scan_closure_param_expr(base, current_func, current_params, locals, out);
                if let Some(start) = start {
                    self.scan_closure_param_expr(start, current_func, current_params, locals, out);
                }
                if let Some(end) = end {
                    self.scan_closure_param_expr(end, current_func, current_params, locals, out);
                }
                if let Some(step) = step {
                    self.scan_closure_param_expr(step, current_func, current_params, locals, out);
                }
            }
            Expr::NamedArg(_, inner) | Expr::SpreadArg(inner) | Expr::KwSpreadArg(inner) => {
                self.scan_closure_param_expr(inner, current_func, current_params, locals, out);
            }
            Expr::List(items) | Expr::Tuple(items) | Expr::SpawnAll(items) => {
                for item in items {
                    self.scan_closure_param_expr(item, current_func, current_params, locals, out);
                }
            }
            Expr::Dict(entries) => {
                for (k, v) in entries {
                    self.scan_closure_param_expr(k, current_func, current_params, locals, out);
                    self.scan_closure_param_expr(v, current_func, current_params, locals, out);
                }
            }
            Expr::Spawn(_, args) | Expr::SpawnThread(_, args) => {
                for arg in args {
                    self.scan_closure_param_expr(arg, current_func, current_params, locals, out);
                }
            }
            Expr::Closure { body, .. } => {
                self.scan_closure_param_stmts(
                    body,
                    current_func,
                    current_params,
                    &mut locals.clone(),
                    out,
                );
            }
            Expr::ListComprehension {
                expr, iter, filter, ..
            } => {
                self.scan_closure_param_expr(expr, current_func, current_params, locals, out);
                self.scan_closure_param_expr(iter, current_func, current_params, locals, out);
                if let Some(filter) = filter {
                    self.scan_closure_param_expr(filter, current_func, current_params, locals, out);
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_closure_func_args(
        &self,
        func_name: &str,
        args: &[Expr],
        current_func: Option<&str>,
        current_params: &[Param],
        locals: &HashMap<String, FuncSigReturnSource>,
        out: &mut HashMap<String, HashSet<usize>>,
        param_offset: usize,
    ) {
        let Some(params) = self.func_params.get(func_name) else {
            return;
        };
        for (arg_index, arg) in args.iter().enumerate() {
            let param_index = arg_index + param_offset;
            let Some(param) = params.get(param_index) else {
                continue;
            };
            if !matches!(param.ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                continue;
            }
            let source = self.funcsig_expr_source_for_closure_scan(
                arg,
                current_func,
                current_params,
                locals,
                out,
            );
            if matches!(source, FuncSigReturnSource::Closure) {
                out.entry(func_name.to_string())
                    .or_default()
                    .insert(param_index);
            }
        }
    }

    fn funcsig_expr_source_for_closure_scan(
        &self,
        expr: &Expr,
        current_func: Option<&str>,
        current_params: &[Param],
        locals: &HashMap<String, FuncSigReturnSource>,
        out: &HashMap<String, HashSet<usize>>,
    ) -> FuncSigReturnSource {
        match expr {
            Expr::Ident(name) => {
                if let Some(source) = locals.get(name).copied() {
                    return source;
                }
                if let Some((idx, _)) = current_params
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.name == *name)
                {
                    if let Some(func_name) = current_func {
                        if out
                            .get(func_name)
                            .map(|indices| indices.contains(&idx))
                            .unwrap_or(false)
                        {
                            return FuncSigReturnSource::Closure;
                        }
                    }
                    return FuncSigReturnSource::Raw;
                }
                if self.functions.contains_key(name) {
                    FuncSigReturnSource::Raw
                } else {
                    FuncSigReturnSource::Unknown
                }
            }
            Expr::Closure { .. } => FuncSigReturnSource::Closure,
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    return self.substitute_closure_scan_call_source(
                        name,
                        self.funcsig_return_sources
                            .get(name)
                            .copied()
                            .unwrap_or(FuncSigReturnSource::Unknown),
                        args,
                        current_func,
                        current_params,
                        locals,
                        out,
                    );
                }
                FuncSigReturnSource::Unknown
            }
            _ => FuncSigReturnSource::Unknown,
        }
    }

    fn substitute_closure_scan_call_source(
        &self,
        func_name: &str,
        source: FuncSigReturnSource,
        args: &[Expr],
        current_func: Option<&str>,
        current_params: &[Param],
        locals: &HashMap<String, FuncSigReturnSource>,
        out: &HashMap<String, HashSet<usize>>,
    ) -> FuncSigReturnSource {
        match source {
            FuncSigReturnSource::Param(i) => self.closure_scan_param_source_for_call(
                func_name,
                i,
                args,
                current_func,
                current_params,
                locals,
                out,
            ),
            FuncSigReturnSource::ParamSet(mask) => {
                let sources: Vec<_> =
                    Self::funcsig_source_param_indices(FuncSigReturnSource::ParamSet(mask))
                        .into_iter()
                        .map(|i| {
                            self.closure_scan_param_source_for_call(
                                func_name,
                                i,
                                args,
                                current_func,
                                current_params,
                                locals,
                                out,
                            )
                        })
                        .collect();
                Self::merge_funcsig_sources(&sources)
            }
            other => other,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn closure_scan_param_source_for_call(
        &self,
        func_name: &str,
        param_index: usize,
        args: &[Expr],
        current_func: Option<&str>,
        current_params: &[Param],
        locals: &HashMap<String, FuncSigReturnSource>,
        out: &HashMap<String, HashSet<usize>>,
    ) -> FuncSigReturnSource {
        if out
            .get(func_name)
            .map(|indices| indices.contains(&param_index))
            .unwrap_or(false)
        {
            return FuncSigReturnSource::Closure;
        }
        args.get(param_index)
            .map(|arg| {
                self.funcsig_expr_source_for_closure_scan(
                    arg,
                    current_func,
                    current_params,
                    locals,
                    out,
                )
            })
            .unwrap_or(FuncSigReturnSource::Unknown)
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
                    method_with_self.params.insert(
                        0,
                        Param {
                            name: "self".to_string(),
                            ty: BolideType::Custom(class_def.name.clone()),
                            mode: ParamMode::Borrow,
                            default_value: None,
                            is_variadic: false,
                            is_kw_variadic: false,
                        },
                    );

                    self.compile_function(&method_with_self)?;
                }
            }
        }
        Ok(())
    }

    /// 注册 extern 块中的函数声明（JitCompiler 级别）
    fn register_extern_block(&mut self, eb: &ExternBlock) -> Result<(), String> {
        let lib_path = &eb.lib_path;
        validate_jit_extern_lib_path(lib_path)?;

        for decl in &eb.declarations {
            if let bolide_parser::ExternDecl::Function(func) = decl {
                if lib_path == "bolide" {
                    let mut sig = self.module.make_signature();
                    for param in &func.params {
                        sig.params
                            .push(AbiParam::new(self.extern_ctype_to_cranelift(&param.ty)));
                    }
                    if let Some(ref ret_ty) = func.return_type {
                        sig.returns
                            .push(AbiParam::new(self.extern_ctype_to_cranelift(ret_ty)));
                    }
                    let id = self
                        .module
                        .declare_function(&func.name, Linkage::Import, &sig)
                        .map_err(|e| format!("{}", e))?;
                    self.functions.insert(func.name.clone(), id);
                }
                self.extern_funcs
                    .insert(func.name.clone(), (lib_path.clone(), func.clone()));
            }
        }
        Ok(())
    }

    fn extern_ctype_to_cranelift(&self, ctype: &bolide_parser::CType) -> types::Type {
        use bolide_parser::CType;
        match ctype {
            CType::Void => types::I64,
            CType::Char | CType::I8 | CType::UChar | CType::U8 => types::I8,
            CType::Short | CType::I16 | CType::UShort | CType::U16 => types::I16,
            CType::Int | CType::I32 | CType::UInt | CType::U32 => types::I32,
            CType::Long
            | CType::LongLong
            | CType::I64
            | CType::ULong
            | CType::ULongLong
            | CType::U64
            | CType::SizeT
            | CType::PtrDiffT => types::I64,
            CType::Float => types::F32,
            CType::Double => types::F64,
            CType::Bool => types::I8,
            CType::Ptr(_) | CType::Array(_, _) | CType::FuncPtr { .. } | CType::Struct(_) => {
                self.ptr_type
            }
        }
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
    module: &'a mut JITModule,
    global_data_ids: &'a HashMap<String, cranelift_module::DataId>,
    global_var_types: &'a HashMap<String, BolideType>,
    func_refs: HashMap<String, FuncRef>,
    variables: HashMap<String, Variable>,
    /// 变量的 Bolide 类型（用于类型推断）
    var_types: HashMap<String, BolideType>,
    /// 函数返回类型（用于 spawn/await 类型处理）
    func_return_types: HashMap<String, Option<BolideType>>,
    /// 函数参数信息（用于参数模式处理）
    func_params: HashMap<String, Vec<Param>>,
    /// spawn 变量对应的函数名（用于 await 时获取返回类型）
    spawn_func_map: HashMap<String, String>,
    /// 由 spawn 产生的热 task 变量；async 调用不会进入此表。
    task_func_map: HashMap<String, String>,
    /// 显式 `spawn thread` 产生的 task 变量。
    force_thread_tasks: HashSet<String>,
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
    /// ADT 信息
    adts: HashMap<String, AdtInfo>,
    /// 类名 -> 异常类型标签（>=100，用于 catch 类型过滤）
    class_tags: HashMap<String, i64>,
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
    /// 函数名 -> 函数值返回来源。
    funcsig_return_sources: HashMap<String, FuncSigReturnSource>,
    /// 函数名 -> 需要按闭包对象 ABI 处理的函数类型参数下标。
    funcsig_closure_param_indices: HashMap<String, HashSet<usize>>,
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
    /// 循环块栈：(continue 目标块, break 目标块, 入栈时 finally 深度)，用于编译 break/continue
    loop_stack: Vec<(Block, Block, usize)>,
    /// catch 落点栈：每个 try 块的 catch_block，用于编译 throw（同函数内直接跳转）
    catch_stack: Vec<Block>,
    /// 当前是否位于 catch 体内；用于决定 throw 是否需要先执行当前 try 的 finally。
    catch_body_depth: usize,
    /// 当前激活的 finally 语句栈（外层到内层）。
    finally_stack: Vec<Vec<Statement>>,
    /// 控制流退出时可见的 finally 深度上限；用于避免在 finally 中重复执行自身。
    finally_visibility_limit: Option<usize>,
    /// 本函数内创建的闭包，待外层函数编译完成后由顶层编译器统一编译
    pending_closures: Vec<ClosureJob>,
    /// 闭包局部计数（与 current_func_name 组合成唯一 lifted 名）
    closure_local_counter: usize,
    /// 当前语句中产生、尚未被变量吸收的闭包临时值（语句末 @_closure_release）
    closure_temps: Vec<Value>,
    /// 持有闭包对象的局部变量名（作用域结束时 @_closure_release）
    closure_vars: HashSet<String>,
    /// 函数类型参数名（闭包 ABI 调用，但不拥有所有权，作用域结束不释放）
    closure_param_vars: HashSet<String>,
    /// 每个词法作用域内被遮蔽的名称，用于离开作用域时恢复外层绑定。
    scope_bindings: Vec<Vec<BindingSnapshot>>,
    /// 当前 lifted closure 的 env 指针。
    closure_env_ptr: Option<Value>,
    /// 当前 lifted closure 捕获变量布局。
    closure_captures: Vec<(String, BolideType)>,
}

impl<'a, 'b> CompileContext<'a, 'b> {
    fn new(
        builder: &'a mut FunctionBuilder<'b>,
        module: &'a mut JITModule,
        global_data_ids: &'a HashMap<String, cranelift_module::DataId>,
        global_var_types: &'a HashMap<String, BolideType>,
        func_refs: HashMap<String, FuncRef>,
        func_return_types: HashMap<String, Option<BolideType>>,
        func_params: HashMap<String, Vec<Param>>,
        trampoline_refs: HashMap<String, FuncRef>,
        trampoline_param_types: HashMap<String, Vec<BolideType>>,
        trampoline_env_sizes: HashMap<String, i64>,
        ptr_type: types::Type,
        classes: HashMap<String, ClassInfo>,
        adts: HashMap<String, AdtInfo>,
        class_tags: HashMap<String, i64>,
        async_funcs: HashSet<String>,
        extern_funcs: HashMap<String, (String, bolide_parser::ExternFunc)>,
        modules: HashMap<String, String>,
        lifetime_deps: Option<Vec<String>>,
        current_func_name: String,
        lifetime_funcs: HashSet<String>,
        funcsig_return_sources: HashMap<String, FuncSigReturnSource>,
        funcsig_closure_param_indices: HashMap<String, HashSet<usize>>,
    ) -> Self {
        Self {
            builder,
            module,
            global_data_ids,
            global_var_types,
            func_refs,
            variables: HashMap::new(),
            var_types: HashMap::new(),
            func_return_types,
            func_params,
            spawn_func_map: HashMap::new(),
            task_func_map: HashMap::new(),
            force_thread_tasks: HashSet::new(),
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
            adts,
            class_tags,
            async_funcs,
            extern_funcs,
            modules,
            lifetime_deps,
            current_func_name,
            lifetime_funcs,
            funcsig_return_sources,
            funcsig_closure_param_indices,
            var_lifetime_source: HashMap::new(),
            scope_depth: 0,
            var_scope_depth: HashMap::new(),
            borrowed_vars: HashMap::new(),
            weak_variables: HashSet::new(),
            loop_stack: Vec::new(),
            catch_stack: Vec::new(),
            catch_body_depth: 0,
            finally_stack: Vec::new(),
            finally_visibility_limit: None,
            pending_closures: Vec::new(),
            closure_local_counter: 0,
            closure_temps: Vec::new(),
            closure_vars: HashSet::new(),
            closure_param_vars: HashSet::new(),
            scope_bindings: Vec::new(),
            closure_env_ptr: None,
            closure_captures: Vec::new(),
        }
    }

    /// 规范化类型名称
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

    /// 规范化 BolideType 中的类型名称
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
            Expr::Member(base, _) => self.check_lifetime_source(base),
            Expr::Index(base, _) => self.check_lifetime_source(base),
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
        self.scope_bindings.push(Vec::new());
    }

    /// 离开作用域，检查借用变量是否悬空
    fn leave_scope(&mut self) -> Result<(), String> {
        // 检查是否有借用变量依赖于当前作用域的变量
        let current_depth = self.scope_depth;

        // 找出当前作用域声明的变量
        let vars_in_scope: Vec<String> = self
            .var_scope_depth
            .iter()
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

        // 清理当前作用域的生命周期记录
        for var in &vars_in_scope {
            self.var_scope_depth.remove(var);
            self.borrowed_vars.remove(var);
        }

        if let Some(mut snapshots) = self.scope_bindings.pop() {
            while let Some(snapshot) = snapshots.pop() {
                let BindingSnapshot {
                    name,
                    variable,
                    var_type,
                    scope_depth,
                    borrowed,
                    weak,
                    moved,
                    closure_var,
                    closure_param_var,
                    spawn_func,
                    task_func,
                    force_thread_task,
                    lifetime_source,
                } = snapshot;

                match variable {
                    Some(var) => {
                        self.variables.insert(name.clone(), var);
                    }
                    None => {
                        self.variables.remove(&name);
                    }
                }
                match var_type {
                    Some(ty) => {
                        self.var_types.insert(name.clone(), ty);
                    }
                    None => {
                        self.var_types.remove(&name);
                    }
                }
                match scope_depth {
                    Some(depth) => {
                        self.var_scope_depth.insert(name.clone(), depth);
                    }
                    None => {
                        self.var_scope_depth.remove(&name);
                    }
                }
                match borrowed {
                    Some(info) => {
                        self.borrowed_vars.insert(name.clone(), info);
                    }
                    None => {
                        self.borrowed_vars.remove(&name);
                    }
                }
                if weak {
                    self.weak_variables.insert(name.clone());
                } else {
                    self.weak_variables.remove(&name);
                }
                if moved {
                    self.moved_variables.insert(name.clone());
                } else {
                    self.moved_variables.remove(&name);
                }
                if closure_var {
                    self.closure_vars.insert(name.clone());
                } else {
                    self.closure_vars.remove(&name);
                }
                if closure_param_var {
                    self.closure_param_vars.insert(name.clone());
                } else {
                    self.closure_param_vars.remove(&name);
                }
                match spawn_func {
                    Some(func) => {
                        self.spawn_func_map.insert(name.clone(), func);
                    }
                    None => {
                        self.spawn_func_map.remove(&name);
                    }
                }
                match task_func {
                    Some(func) => {
                        self.task_func_map.insert(name.clone(), func);
                    }
                    None => {
                        self.task_func_map.remove(&name);
                    }
                }
                if force_thread_task {
                    self.force_thread_tasks.insert(name.clone());
                } else {
                    self.force_thread_tasks.remove(&name);
                }
                match lifetime_source {
                    Some(source) => {
                        self.var_lifetime_source.insert(name, source);
                    }
                    None => {
                        self.var_lifetime_source.remove(&name);
                    }
                }
            }
        }

        self.scope_depth -= 1;
        Ok(())
    }

    fn snapshot_binding_for_scope(&mut self, name: &str) {
        let Some(scope) = self.scope_bindings.last_mut() else {
            return;
        };
        if scope.iter().any(|snapshot| snapshot.name == name) {
            return;
        }
        scope.push(BindingSnapshot {
            name: name.to_string(),
            variable: self.variables.get(name).copied(),
            var_type: self.var_types.get(name).cloned(),
            scope_depth: self.var_scope_depth.get(name).copied(),
            borrowed: self.borrowed_vars.get(name).cloned(),
            weak: self.weak_variables.contains(name),
            moved: self.moved_variables.contains(name),
            closure_var: self.closure_vars.contains(name),
            closure_param_var: self.closure_param_vars.contains(name),
            spawn_func: self.spawn_func_map.get(name).cloned(),
            task_func: self.task_func_map.get(name).cloned(),
            force_thread_task: self.force_thread_tasks.contains(name),
            lifetime_source: self.var_lifetime_source.get(name).cloned(),
        });
    }

    /// 推断 await 一个 Future/Task 表达式后得到的类型
    /// 支持三种形式：直接调用 async 函数、Future 变量、spawn 表达式
    fn infer_awaited_type(&self, expr: &Expr) -> BolideType {
        match expr {
            // await fetch_a() / spawn all { fetch_a(), ... }
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
            // let f = fetch_a(); await f
            Expr::Ident(var_name) => {
                // 只有当变量在当前作用域中是 Future 类型时，才用 spawn_func_map
                // 避免局部变量遮蔽全局同名变量（如全局 f: float）导致类型误判。
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
            // await spawn heavy(x)
            Expr::Spawn(func_name, _) | Expr::SpawnThread(func_name, _) => self
                .func_return_types
                .get(func_name)
                .cloned()
                .flatten()
                .unwrap_or(BolideType::Int),
            // 字面类型推断
            Expr::Int(_) => BolideType::Int,
            Expr::Float(_) => BolideType::Float,
            Expr::String(_) => BolideType::Str,
            Expr::Bool(_) => BolideType::Bool,
            Expr::BigInt(_) => BolideType::BigInt,
            Expr::Decimal(_) => BolideType::Decimal,
            _ => self.infer_expr_type(expr),
        }
    }

    /// 获取变量的 Bolide 类型（先查局部，再查全局）
    fn get_var_type(&self, name: &str) -> Result<BolideType, String> {
        if let Some(ty) = self.var_types.get(name) {
            return Ok(ty.clone());
        }
        if let Some(ty) = self.global_var_types.get(name) {
            return Ok(ty.clone());
        }
        Err(format!("Undefined variable: {}", name))
    }

    /// 读取变量的当前值（局部变量或全局变量）
    fn load_var_value(&mut self, name: &str) -> Result<Value, String> {
        if let Some(&var) = self.variables.get(name) {
            return Ok(self.builder.use_var(var));
        }
        if let Some(&data_id) = self.global_data_ids.get(name) {
            let gv = self.module.declare_data_in_func(data_id, self.builder.func);
            let addr = self.builder.ins().global_value(self.ptr_type, gv);
            let load_ty = self
                .global_var_types
                .get(name)
                .map(|t| self.bolide_type_to_cranelift(t))
                .unwrap_or(self.ptr_type);
            return Ok(self.builder.ins().load(load_ty, MemFlags::new(), addr, 0));
        }
        Err(format!("Undefined variable: {}", name))
    }

    /// 记录变量声明的作用域
    fn record_var_scope(&mut self, var_name: &str) {
        self.var_scope_depth
            .insert(var_name.to_string(), self.scope_depth);
    }

    /// 记录借用关系
    fn record_borrow(&mut self, borrower: &str, source: &str) {
        let source_depth = self.var_scope_depth.get(source).copied().unwrap_or(0);
        self.borrowed_vars
            .insert(borrower.to_string(), (source.to_string(), source_depth));
    }

    /// from 借用逃逸检查：借用值不拥有对象，
    /// 禁止存入容器/字段/通道或跨线程逃逸（编译期拒绝）
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

    /// from 借用来源检查：借用存活期间禁止对来源变量重新赋值
    /// （旧对象会被释放，借用方将悬空）
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

    /// 基本异常类型标签（与 AOT 一致）。自定义类的标签由 class_tags 提供（>=100）。
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

    fn class_extends(&self, class_name: &str, target: &str) -> bool {
        let mut cur = Some(class_name.to_string());
        while let Some(name) = cur {
            if name == target {
                return true;
            }
            cur = self
                .classes
                .get(&name)
                .and_then(|class| class.parent.clone());
        }
        false
    }

    fn is_error_type(&self, ty: &BolideType) -> bool {
        matches!(ty, BolideType::Custom(name) if self.class_extends(name, "Error"))
    }

    fn validate_error_type(&self, ty: &BolideType, context: &str) -> Result<(), String> {
        if self.is_error_type(ty) {
            Ok(())
        } else {
            Err(format!(
                "{} expects Error or an Error subclass, got {:?}",
                context, ty
            ))
        }
    }

    /// 计算 catch (e: T) 应匹配的标签集合。
    /// - 基本类型：单一标签。
    /// - 自定义类：T 自身 + 所有以 T 为祖先的子类（按继承链向上查找含 T）。
    fn catch_match_tags(&self, catch_ty: &BolideType) -> Vec<i64> {
        match catch_ty {
            BolideType::Custom(target) => {
                let mut tags = Vec::new();
                for (cls_name, &tag) in &self.class_tags {
                    // 沿继承链向上，看是否能到达 target
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
            // weak/unowned 类引用本身需要弱引用计数管理：
            // 创建时 weak+1 保住对象头，作用域结束时 weak-1，
            // 这样对象死亡后访问可被检测（trap）而不是 use-after-free
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
            // weak/unowned 释放的是弱引用计数（不触碰强引用）
            BolideType::Weak(inner) | BolideType::Unowned(inner)
                if matches!(inner.as_ref(), BolideType::Custom(_)) =>
            {
                Some("@_object_weak_release")
            }
            _ => None,
        }
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
            // weak/unowned 克隆只增加弱引用计数（不增加强引用）
            BolideType::Weak(inner) | BolideType::Unowned(inner)
                if matches!(inner.as_ref(), BolideType::Custom(_)) =>
            {
                Some("@_object_weak_clone")
            }
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
        let vars_to_release: Vec<_> = self
            .rc_variables
            .iter()
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
        for (_name, var, ty) in vars_to_release {
            let val = self.builder.use_var(var);
            self.emit_release(val, &ty);
        }

        // 释放持有闭包的局部变量（排除被返回的那个）
        let closure_names: Vec<String> = self
            .closure_vars
            .iter()
            .filter(|n| except_var != Some(n.as_str()))
            .cloned()
            .collect();
        for name in closure_names {
            if let Some(&var) = self.variables.get(&name) {
                let val = self.builder.use_var(var);
                self.emit_closure_release(val);
            }
        }
    }

    /// 释放所有全局 RC 变量（在 __main__ 返回前调用，避免退出时泄漏）
    fn emit_global_rc_cleanup(&mut self) {
        let ptr_type = self.ptr_type;
        let mut to_release: Vec<(Value, BolideType, cranelift_codegen::ir::GlobalValue)> =
            Vec::new();

        for (name, ty) in self.global_var_types.iter() {
            if Self::is_rc_type(ty) {
                if let Some(&data_id) = self.global_data_ids.get(name) {
                    let gv = self.module.declare_data_in_func(data_id, self.builder.func);
                    let addr = self.builder.ins().global_value(ptr_type, gv);
                    let val = self.builder.ins().load(ptr_type, MemFlags::new(), addr, 0);
                    to_release.push((val, ty.clone(), gv));
                }
            }
        }

        for (val, ty, gv) in to_release {
            // 跳过 null 指针（变量未初始化或已 move）
            let null_val = self.builder.ins().iconst(ptr_type, 0);
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

    /// 统一的 release 辅助函数，处理递归结构（如 Tuple/Class）
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
            let done_block = self.builder.create_block();

            self.builder
                .ins()
                .brif(is_null, done_block, &[], check_block, &[]);

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
            self.builder.ins().jump(done_block, &[]);

            self.builder.switch_to_block(done_block);
            self.builder.seal_block(done_block);
        } else if let BolideType::Custom(ref class_name) = ty {
            // 自定义类型（Class）
            let has_rc_fields = self
                .classes
                .get(class_name)
                .map(|ci| {
                    ci.fields.iter().any(|f| {
                        Self::is_rc_type(&f.ty)
                            || matches!(f.ty, BolideType::FuncSig(_, _) | BolideType::Func)
                    })
                })
                .unwrap_or(false);

            if has_rc_fields {
                // 有 RC 字段时需要先清理字段，但必须满足两个条件：
                // 1. 指针非 null（全局变量首次初始化时旧值为 null）
                // 2. 这是最后一个强引用（refcount == 1），否则共享对象的字段会被重复释放
                let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                let is_null = self.builder.ins().icmp(IntCC::Equal, val, null_val);

                let check_block = self.builder.create_block();
                let fields_block = self.builder.create_block();
                let release_block = self.builder.create_block();
                let done_block = self.builder.create_block();

                self.builder
                    .ins()
                    .brif(is_null, done_block, &[], check_block, &[]);

                // check_block: 仅当 strong_count == 1（即将销毁）时清理字段
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

                // fields_block: 释放对象内部的 RC 字段
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
                self.builder.ins().jump(done_block, &[]);

                self.builder.switch_to_block(done_block);
                self.builder.seal_block(done_block);
            } else {
                // 无 RC 字段：object_release 自身做 null 检查
                if let Some(&release_func) = self.func_refs.get("@_object_release") {
                    self.builder.ins().call(release_func, &[val]);
                }
            }
        } else {
            // 其他基本 RC 类型
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
                if matches!(field.ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                    let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field.offset as i64);
                    let field_val =
                        self.builder
                            .ins()
                            .load(types::I64, MemFlags::new(), field_ptr, 0);
                    self.emit_closure_release(field_val);
                } else if Self::is_rc_type(&field.ty) {
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

    /// 记录 RC 变量
    fn track_rc_variable(&mut self, name: &str, ty: &BolideType) {
        if Self::is_rc_type(ty) {
            self.rc_variables.push((name.to_string(), ty.clone()));
        }
    }

    /// 记录临时 RC 值（表达式中间结果）
    fn track_temp_rc_value(&mut self, val: Value, ty: &BolideType) {
        if Self::is_rc_type(ty) && !self.temp_rc_values.iter().any(|(v, _)| *v == val) {
            self.temp_rc_values.push((val, ty.clone()));
        }
    }

    /// 释放所有临时 RC 值
    fn release_temp_rc_values(&mut self) {
        let temps = std::mem::take(&mut self.temp_rc_values);
        for (val, ty) in temps {
            self.emit_release(val, &ty);
        }
        // 同时释放未被吸收的闭包临时值
        self.release_temp_closures();
    }

    /// 释放未被变量吸收的闭包临时值
    fn release_temp_closures(&mut self) {
        let temps = std::mem::take(&mut self.closure_temps);
        for val in temps {
            self.emit_closure_release(val);
        }
    }

    /// 对单个闭包对象生成 @_closure_release 调用
    fn emit_closure_release(&mut self, val: Value) {
        if let Some(&rref) = self.func_refs.get("@_closure_release") {
            self.builder.ins().call(rref, &[val]);
        }
    }

    /// 对单个闭包对象生成 @_closure_retain 调用
    fn emit_closure_retain(&mut self, val: Value) {
        if let Some(&rref) = self.func_refs.get("@_closure_retain") {
            self.builder.ins().call(rref, &[val]);
        }
    }

    /// 从闭包临时列表移除（被变量吸收或返回时）
    fn remove_temp_closure(&mut self, val: Value) {
        self.closure_temps.retain(|v| *v != val);
    }

    /// 从临时值列表中移除指定值（当值被存入变量时调用）
    fn remove_temp_rc_value(&mut self, val: Value) {
        self.temp_rc_values.retain(|(v, _)| *v != val);
    }

    fn current_finally_depth(&self) -> usize {
        self.finally_visibility_limit
            .unwrap_or(self.finally_stack.len())
            .min(self.finally_stack.len())
    }

    fn push_active_finally(&mut self, finally_body: &Option<Vec<Statement>>) -> Option<usize> {
        match finally_body {
            Some(body) if !body.is_empty() => {
                self.finally_stack.push(body.clone());
                Some(self.finally_stack.len() - 1)
            }
            _ => None,
        }
    }

    fn pop_active_finally(&mut self, finally_depth: Option<usize>) {
        if finally_depth.is_some() {
            self.finally_stack.pop();
        }
    }

    fn emit_active_finallys_from(&mut self, depth: usize) -> Result<(), String> {
        let active_len = self.current_finally_depth();
        let finalies: Vec<(usize, Vec<Statement>)> = (depth..active_len)
            .rev()
            .map(|idx| (idx, self.finally_stack[idx].clone()))
            .collect();
        for (idx, body) in finalies {
            let old_limit = self.finally_visibility_limit;
            self.finally_visibility_limit = Some(idx);
            self.emit_finally(&Some(body))?;
            self.finally_visibility_limit = old_limit;
        }
        Ok(())
    }

    fn emit_default_return_for_exception(&mut self) {
        let return_ty = self
            .func_return_types
            .get(&self.current_func_name)
            .cloned()
            .flatten();
        match return_ty {
            Some(BolideType::Float) => {
                let zero = self.builder.ins().f64const(0.0);
                self.builder.ins().return_(&[zero]);
            }
            Some(ty) => {
                let c_ty = self.bolide_type_to_cranelift(&ty);
                let zero = self.builder.ins().iconst(c_ty, 0);
                self.builder.ins().return_(&[zero]);
            }
            None => {
                self.builder.ins().return_(&[]);
            }
        }
    }

    fn emit_uncaught_exception(&mut self) -> Result<(), String> {
        let ex_get_fn = *self
            .func_refs
            .get("@_exception_get")
            .ok_or("exception_get not found")?;
        let ex_call = self.builder.ins().call(ex_get_fn, &[]);
        let ex_ptr = self.builder.inst_results(ex_call)[0];
        let uncaught_fn = *self
            .func_refs
            .get("@_throw_uncaught")
            .ok_or("throw_uncaught not found")?;
        self.builder.ins().call(uncaught_fn, &[ex_ptr]);
        self.builder.ins().trap(TrapCode::unwrap_user(1));
        Ok(())
    }

    fn emit_exception_set(&mut self, value: Value, tag: Value) -> Result<(), String> {
        let set_fn = *self
            .func_refs
            .get("@_exception_set")
            .ok_or("exception_set not found")?;
        self.builder.ins().call(set_fn, &[value, tag]);
        Ok(())
    }

    fn emit_pending_exception_finallys_from(&mut self, depth: usize) -> Result<(), String> {
        if depth >= self.current_finally_depth() {
            return Ok(());
        }

        let tag_fn = *self
            .func_refs
            .get("@_exception_tag")
            .ok_or("exception_tag not found")?;
        let tag_call = self.builder.ins().call(tag_fn, &[]);
        let tag = self.builder.inst_results(tag_call)[0];

        let ex_get_fn = *self
            .func_refs
            .get("@_exception_get")
            .ok_or("exception_get not found")?;
        let ex_call = self.builder.ins().call(ex_get_fn, &[]);
        let ex_ptr = self.builder.inst_results(ex_call)[0];

        self.emit_active_finallys_from(depth)?;
        self.emit_exception_set(ex_ptr, tag)?;
        Ok(())
    }

    fn emit_exception_transfer(
        &mut self,
        already_emitted_catch_finally: bool,
    ) -> Result<(), String> {
        if let Some(&catch_block) = self.catch_stack.last() {
            self.builder.ins().jump(catch_block, &[]);
        } else if self.current_func_name == "__main__" {
            self.emit_uncaught_exception()?;
        } else {
            if !already_emitted_catch_finally {
                self.emit_pending_exception_finallys_from(0)?;
            }
            self.emit_default_return_for_exception();
        }
        Ok(())
    }

    fn emit_exception_pending_check(&mut self) -> Result<(), String> {
        let pending_fn = *self
            .func_refs
            .get("@_exception_pending")
            .ok_or("exception_pending not found")?;
        let pending_call = self.builder.ins().call(pending_fn, &[]);
        let pending = self.builder.inst_results(pending_call)[0];
        let zero = self.builder.ins().iconst(types::I64, 0);
        let has_exception = self.builder.ins().icmp(IntCC::NotEqual, pending, zero);
        let exception_block = self.builder.create_block();
        let continue_block = self.builder.create_block();
        self.builder
            .ins()
            .brif(has_exception, exception_block, &[], continue_block, &[]);

        self.builder.switch_to_block(exception_block);
        self.builder.seal_block(exception_block);
        let emitted_catch_finally = if self.catch_body_depth > 0 {
            self.emit_pending_exception_finallys_from(
                self.current_finally_depth().saturating_sub(1),
            )?;
            true
        } else {
            false
        };
        self.emit_exception_transfer(emitted_catch_finally)?;

        self.builder.switch_to_block(continue_block);
        self.builder.seal_block(continue_block);
        Ok(())
    }

    /// 声明变量
    fn declare_variable(&mut self, name: &str, ty: types::Type) -> Variable {
        self.snapshot_binding_for_scope(name);
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
        self.record_var_scope(name);
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
            Statement::Break => {
                let (_, break_block, finally_depth) =
                    *self.loop_stack.last().ok_or("'break' outside of a loop")?;
                // 跳出前释放当前语句产生的临时 RC 值
                self.release_temp_rc_values();
                self.emit_active_finallys_from(finally_depth)?;
                self.builder.ins().jump(break_block, &[]);
                Ok(true)
            }
            Statement::Continue => {
                let (continue_block, _, finally_depth) = *self
                    .loop_stack
                    .last()
                    .ok_or("'continue' outside of a loop")?;
                self.release_temp_rc_values();
                self.emit_active_finallys_from(finally_depth)?;
                self.builder.ins().jump(continue_block, &[]);
                Ok(true)
            }
            Statement::Select(select_stmt) => {
                self.compile_select(select_stmt)?;
                Ok(false)
            }
            Statement::AwaitScope(scope_stmt) => {
                self.compile_await_scope(scope_stmt)?;
                Ok(false)
            }
            Statement::SpawnSelect(select_stmt) => {
                self.compile_spawn_select(select_stmt)?;
                Ok(false)
            }
            Statement::Throw(expr) => {
                // 计算异常值与类型标签，存入 thread-local，然后跳转到最近的 catch 落点。
                // 无 setjmp/longjmp：异常值经内存传递，控制流是普通分支，SSA 安全。
                let throw_ty = self.infer_expr_type(expr);
                self.validate_error_type(&throw_ty, "throw")?;
                let tag = self.type_to_throw_tag(&throw_ty);
                let val = self.compile_expr(expr)?;
                // 抛出的 RC 临时值所有权转移给异常通道，避免语句末提前释放
                self.remove_temp_rc_value(val);
                let tag_val = self.builder.ins().iconst(types::I64, tag);
                let emitted_catch_finally = if self.catch_body_depth > 0 {
                    self.emit_active_finallys_from(self.current_finally_depth().saturating_sub(1))?;
                    true
                } else {
                    false
                };
                self.emit_exception_set(val, tag_val)?;
                self.emit_exception_transfer(emitted_catch_finally)?;
                Ok(true)
            }
            Statement::Try(try_stmt) => Ok(self.compile_try(try_stmt)?),
            Statement::Match(match_stmt) => self.compile_match(match_stmt),
            Statement::FuncDef(_) => Ok(false),
            Statement::ClassDef(_) => Ok(false),
            Statement::EnumDef(_) => Ok(false),
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

    /// 编译 try/catch/finally。返回 true 表示所有路径都发散（try 与全部 catch 都终结）。
    ///
    /// 控制流：try body 期间将 catch_block 压入 catch_stack；内部 throw 设置
    /// (value, tag) 后跳到 catch_block。catch_block 读取 tag，按 catch 子句顺序做
    /// 标签匹配分派（含子类）；无匹配则重抛到外层。finally 在每条退出路径前内联编译
    /// （正常完成 / 各 catch 完成 / 重抛前），采用 finally 复制的标准做法。
    fn compile_try(&mut self, try_stmt: &bolide_parser::TryStmt) -> Result<bool, String> {
        let catch_clauses = try_stmt.catch_clauses.clone();
        let try_body = try_stmt.try_body.clone();
        let finally_body = try_stmt.finally.clone();
        let ptr_type = self.ptr_type;

        let catch_block = self.builder.create_block();
        let after_try = self.builder.create_block();

        // 1. Try body —— catch_block 压栈，内部 throw 跳到这里
        self.catch_stack.push(catch_block);
        let try_finally_depth = self.push_active_finally(&finally_body);
        let mut try_diverted = false;
        for s in &try_body {
            if try_diverted {
                break;
            }
            try_diverted = self.compile_stmt(s)?;
        }
        self.pop_active_finally(try_finally_depth);
        self.catch_stack.pop();
        if !try_diverted {
            self.emit_finally(&finally_body)?;
            self.builder.ins().jump(after_try, &[]);
        }

        // 2. Catch block：读取 tag + 异常值，按子句顺序分派
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

        // 全部 catch 子句是否都发散（用于判定 after_try 是否可达）
        let mut all_catch_diverted = true;

        for clause in &catch_clauses {
            self.validate_error_type(&clause.ty, "catch")?;
            let match_tags = self.catch_match_tags(&clause.ty);
            let body_block = self.builder.create_block();
            let next_block = self.builder.create_block();

            if match_tags.is_empty() {
                // 该 catch 类型无任何已知标签可匹配（如未声明的类）：永不命中
                self.builder.ins().jump(next_block, &[]);
            } else {
                // matched = OR(cur_tag == t)
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

            // body_block：绑定 typed 异常变量并执行 catch body
            self.builder.switch_to_block(body_block);
            self.builder.seal_block(body_block);
            self.catch_body_depth += 1;
            self.enter_scope();
            let catch_var = self.declare_variable(&clause.var, ptr_type);
            self.builder.def_var(catch_var, ex_ptr);
            self.var_types.insert(clause.var.clone(), clause.ty.clone());
            self.record_var_scope(&clause.var);
            let catch_finally_depth = self.push_active_finally(&finally_body);

            let mut clause_diverted = false;
            for s in &clause.body {
                if clause_diverted {
                    break;
                }
                clause_diverted = self.compile_stmt(s)?;
            }
            self.pop_active_finally(catch_finally_depth);
            self.catch_body_depth -= 1;
            self.leave_scope()?;
            if !clause_diverted {
                self.emit_finally(&finally_body)?;
                self.builder.ins().jump(after_try, &[]);
                all_catch_diverted = false;
            }

            // 继续检查下一子句
            self.builder.switch_to_block(next_block);
            self.builder.seal_block(next_block);
        }

        // 所有 catch 都不匹配：重抛（先执行 finally）
        self.emit_finally(&finally_body)?;
        let set_fn = *self
            .func_refs
            .get("@_exception_set")
            .ok_or("exception_set not found")?;
        self.builder.ins().call(set_fn, &[ex_ptr, cur_tag]);
        self.emit_exception_transfer(true)?;

        // 3. After try
        let both_diverged = try_diverted && all_catch_diverted;
        self.builder.switch_to_block(after_try);
        self.builder.seal_block(after_try);
        if both_diverged {
            self.builder.ins().trap(TrapCode::unwrap_user(1));
        }
        Ok(both_diverged)
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
            let actual = self.infer_expr_type(arg);
            Self::unify_generic_type(&field.ty, &actual, &mut bindings);
        }
        adt_info
            .type_params
            .iter()
            .map(|name| bindings.get(name).cloned().unwrap_or(BolideType::Dynamic))
            .collect()
    }

    fn compile_match(&mut self, match_stmt: &bolide_parser::MatchStmt) -> Result<bool, String> {
        let scrutinee_ty = self.infer_expr_type(&match_stmt.expr);
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
                    self.enter_scope();
                    let diverted = self.compile_match_arm_body(&arm.body)?;
                    self.leave_scope()?;
                    if !diverted {
                        self.builder.ins().jump(after_block, &[]);
                        all_diverted = false;
                    }
                }
                bolide_parser::Pattern::Bind(name) => {
                    saw_catch_all = true;
                    self.enter_scope();
                    self.bind_match_value(
                        name,
                        scrutinee_val,
                        &BolideType::Adt(adt_name.to_string(), type_args.to_vec()),
                    )?;
                    let diverted = self.compile_match_arm_body(&arm.body)?;
                    self.leave_scope()?;
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
                    self.enter_scope();
                    self.bind_adt_variant_pattern_fields(
                        scrutinee_val,
                        &variant_info,
                        fields,
                        &type_map,
                    )?;
                    let diverted = self.compile_match_arm_body(&arm.body)?;
                    self.leave_scope()?;
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
        if matches!(ty, BolideType::FuncSig(_, _) | BolideType::Func) {
            self.emit_closure_retain(value);
            self.closure_vars.insert(name.to_string());
        } else if Self::is_rc_type(ty) {
            if let Some(func_name) = Self::get_clone_func_name(ty) {
                if let Some(&func_ref) = self.func_refs.get(func_name) {
                    let call = self.builder.ins().call(func_ref, &[value]);
                    bind_val = self.builder.inst_results(call)[0];
                }
            }
        }
        self.builder.def_var(var, bind_val);
        self.var_types.insert(name.to_string(), ty.clone());
        self.record_var_scope(name);
        self.track_rc_variable(name, ty);
        Ok(())
    }

    /// 内联编译 finally body（在每条退出路径前调用，finally 复制做法）。
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
    fn compile_index_assign(
        &mut self,
        base: &Expr,
        index: &Expr,
        value: &Expr,
    ) -> Result<(), String> {
        // from 借用检查：借用值禁止存入容器
        self.check_borrow_escape(value, "index assignment")?;

        let base_type = self.infer_expr_type(base);
        let base_val = self.compile_expr(base)?;
        let index_val = self.compile_expr(index)?;
        let mut value_val = self.compile_expr(value)?;

        match base_type {
            BolideType::List(ref elem_ty) => {
                value_val =
                    self.prepare_funcsig_for_container_storage(value_val, value, elem_ty)?;
                // Int/Float/Bool 内联：单次 store，无运行时调用
                if matches!(
                    elem_ty.as_ref(),
                    BolideType::Int | BolideType::Float | BolideType::Bool
                ) {
                    return self.emit_list_set_inline(
                        base_val,
                        index_val,
                        value_val,
                        elem_ty.as_ref(),
                    );
                }
                let list_set = *self
                    .func_refs
                    .get("@_list_set")
                    .ok_or("list_set not found")?;
                self.builder
                    .ins()
                    .call(list_set, &[base_val, index_val, value_val]);
                Ok(())
            }
            BolideType::Dict(_, ref value_ty) => {
                value_val =
                    self.prepare_funcsig_for_container_storage(value_val, value, value_ty)?;
                let dict_set = *self
                    .func_refs
                    .get("@_dict_set")
                    .ok_or("dict_set not found")?;
                self.builder
                    .ins()
                    .call(dict_set, &[base_val, index_val, value_val]);
                Ok(())
            }

            BolideType::Tuple(_) => {
                let tuple_set = *self
                    .func_refs
                    .get("@_tuple_set")
                    .ok_or("tuple_set not found")?;
                self.builder
                    .ins()
                    .call(tuple_set, &[base_val, index_val, value_val]);
                Ok(())
            }
            _ => Err(format!(
                "Index assignment not supported for type: {:?}",
                base_type
            )),
        }
    }

    /// 编译变量赋值
    fn compile_var_assign(&mut self, var_name: &str, value: &Expr) -> Result<(), String> {
        // from 借用检查：借用存活期间禁止对来源变量重新赋值
        self.check_borrow_source_assign(var_name)?;

        // 首先检查是否是局部变量
        if let Some(&var) = self.variables.get(var_name) {
            // 局部变量赋值（原有逻辑）
            // 检查是否是 Ref 参数
            let is_ref_param = self.ref_params.iter().any(|(name, _, _)| name == var_name);
            // 检查 Ref 参数是否已经被重新赋值过
            let was_reassigned = self.ref_params_reassigned.contains(var_name);

            // 决定是否释放旧值
            let should_release = !is_ref_param || was_reassigned;

            let var_ty = self.var_types.get(var_name).cloned();
            if let Some(ref ty) = var_ty {
                if Self::is_rc_type(ty) && should_release {
                    let old_val = self.builder.use_var(var);
                    self.emit_release(old_val, ty);
                }
            }

            // 如果是 Ref 参数的首次赋值，标记为已重新赋值
            if is_ref_param && !was_reassigned {
                self.ref_params_reassigned.insert(var_name.to_string());
            }

            let raw_val = if matches!(value, Expr::List(items) if items.is_empty())
                && matches!(var_ty.as_ref(), Some(BolideType::List(_)))
            {
                self.compile_list_with_hint(&[], var_ty.as_ref())?
            } else {
                self.compile_expr(value)?
            };
            let val = if let Some(ref ty) = var_ty {
                let raw_ty = self.normalize_bolide_type(&self.infer_expr_type(value));
                self.prepare_value_for_storage(raw_val, &raw_ty, ty)?
            } else {
                raw_val
            };

            // 如果是 RC 类型，需要处理引用计数
            if let Some(ref ty) = var_ty {
                if Self::is_rc_type(ty) {
                    let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                    // weak/unowned 赋值不接管强引用所有权（见 compile_var_decl）
                    if is_temp && !Self::is_weak_ref_type(ty) {
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
            } else {
                // 重新赋值为非借用值后，借用关系解除
                self.borrowed_vars.remove(var_name);
            }

            return Ok(());
        }

        // 检查是否是全局变量
        if let Some(&data_id) = self.global_data_ids.get(var_name) {
            // 获取全局变量的类型
            let global_ty = self.global_var_types.get(var_name).cloned();

            // 获取全局变量的地址
            let gv = self.module.declare_data_in_func(data_id, self.builder.func);
            let addr = self.builder.ins().global_value(self.ptr_type, gv);

            // 先编译新值表达式(这样可以正确读取旧值, 例如 expr = expr + "1")
            let raw_val = if matches!(value, Expr::List(items) if items.is_empty())
                && matches!(global_ty.as_ref(), Some(BolideType::List(_)))
            {
                self.compile_list_with_hint(&[], global_ty.as_ref())?
            } else {
                self.compile_expr(value)?
            };
            let val = if let Some(ref ty) = global_ty {
                let raw_ty = self.normalize_bolide_type(&self.infer_expr_type(value));
                self.prepare_value_for_storage(raw_val, &raw_ty, ty)?
            } else {
                raw_val
            };

            // 如果是 RC 类型，需要处理引用计数
            if let Some(ref ty) = global_ty {
                if Self::is_rc_type(ty) {
                    let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                    // weak/unowned 全局变量不接管强引用所有权（走 clone 路径增加弱计数）
                    if is_temp && !Self::is_weak_ref_type(ty) {
                        // 值是临时的，移除临时标记，全局变量接管所有权
                        self.remove_temp_rc_value(val);
                        // 释放旧值（新值已经计算完成）
                        let old_val =
                            self.builder
                                .ins()
                                .load(self.ptr_type, MemFlags::new(), addr, 0);
                        self.emit_release(old_val, ty);
                        // 存储新值
                        self.builder.ins().store(MemFlags::new(), val, addr, 0);
                    } else {
                        // 值来自另一个变量，需要 clone
                        let clone_func_name = Self::get_clone_func_name(ty);
                        if let Some(func_name) = clone_func_name {
                            if let Some(&func_ref) = self.func_refs.get(func_name) {
                                let call = self.builder.ins().call(func_ref, &[val]);
                                let cloned_val = self.builder.inst_results(call)[0];
                                // 释放旧值
                                let old_val = self.builder.ins().load(
                                    self.ptr_type,
                                    MemFlags::new(),
                                    addr,
                                    0,
                                );
                                self.emit_release(old_val, ty);
                                // 存储新值
                                self.builder
                                    .ins()
                                    .store(MemFlags::new(), cloned_val, addr, 0);
                            } else {
                                self.builder.ins().store(MemFlags::new(), val, addr, 0);
                            }
                        } else {
                            self.builder.ins().store(MemFlags::new(), val, addr, 0);
                        }
                    }
                } else {
                    self.builder.ins().store(MemFlags::new(), val, addr, 0);
                }
            } else {
                self.builder.ins().store(MemFlags::new(), val, addr, 0);
            }

            // 全局变量：若值是闭包对象，吸收所有权并记录到 closure_vars
            if self.closure_temps.contains(&val) {
                self.remove_temp_closure(val);
                self.closure_vars.insert(var_name.to_string());
            } else if let Expr::Ident(src) = value {
                if self.closure_vars.contains(src) || self.closure_param_vars.contains(src) {
                    self.emit_closure_retain(val);
                    self.closure_vars.insert(var_name.to_string());
                }
            } else if matches!(
                global_ty,
                Some(BolideType::FuncSig(_, _) | BolideType::Func)
            ) && !self.expr_yields_raw_funcsig(value)
            {
                self.emit_closure_retain(val);
                self.closure_vars.insert(var_name.to_string());
            } else if matches!(
                global_ty,
                Some(BolideType::FuncSig(_, _) | BolideType::Func)
            ) {
                self.closure_vars.remove(var_name);
            }

            // 调用者端借用检查：全局变量赋值同样记录借用关系
            // （全局变量作用域深度为 0，借用内层作用域变量时 leave_scope 会报悬空错误）
            if self.is_lifetime_func_call(value) {
                if let Some(source_var) = self.get_lifetime_call_source(value) {
                    self.record_borrow(var_name, &source_var);
                }
            } else {
                self.borrowed_vars.remove(var_name);
            }

            return Ok(());
        }

        Err(format!("Undefined variable: {}", var_name))
    }
    fn compile_member_assign(
        &mut self,
        base: &Expr,
        member: &str,
        value: &Expr,
    ) -> Result<(), String> {
        // from 借用检查：借用值禁止存入对象字段
        self.check_borrow_escape(value, "field assignment")?;

        // 获取基础表达式的类型
        let class_name = self.get_expr_type(base)?;
        let class_name = match class_name {
            BolideType::Custom(name) => name,
            _ => return Err(format!("Member assign on non-class type: {:?}", class_name)),
        };

        // 获取类信息
        let class_info = self
            .classes
            .get(&class_name)
            .ok_or_else(|| format!("Class not found: {}", class_name))?
            .clone();

        // 查找字段
        let field = class_info
            .fields
            .iter()
            .find(|f| f.name == member)
            .ok_or_else(|| format!("Field '{}' not found in class '{}'", member, class_name))?;

        let field_offset = field.offset;
        let field_ty = field.ty.clone();

        // 编译基础表达式获取对象指针
        let obj_ptr = self.compile_expr(base)?;

        // 编译值表达式
        let mut val = self.compile_expr(value)?;
        val = self.prepare_funcsig_for_container_storage(val, value, &field_ty)?;

        // 计算字段地址
        let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field_offset as i64);

        // 如果字段是 RC 类型，先释放旧值再写入新值
        if Self::is_rc_type(&field_ty) {
            let old_val = self
                .builder
                .ins()
                .load(self.ptr_type, MemFlags::new(), field_ptr, 0);
            self.emit_release(old_val, &field_ty);
        } else if matches!(field_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
            let old_val = self
                .builder
                .ins()
                .load(self.ptr_type, MemFlags::new(), field_ptr, 0);
            self.emit_closure_release(old_val);
        }

        // 处理新值的引用计数
        if Self::is_rc_type(&field_ty) {
            let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
            // weak/unowned 字段不接管强引用所有权
            if is_temp && !Self::is_weak_ref_type(&field_ty) {
                // 值是临时的，移除临时标记，字段接管所有权
                self.remove_temp_rc_value(val);
            } else {
                // 值来自另一个变量，需要 clone
                if let Some(func_name) = Self::get_clone_func_name(&field_ty) {
                    if let Some(&func_ref) = self.func_refs.get(func_name) {
                        let call = self.builder.ins().call(func_ref, &[val]);
                        // val was replaced by cloned; use cloned result for store
                        let cloned = self.builder.inst_results(call)[0];
                        self.builder
                            .ins()
                            .store(MemFlags::new(), cloned, field_ptr, 0);
                        return Ok(());
                    }
                }
            }
        } else if matches!(field_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
            self.emit_closure_retain(val);
        }
        self.builder.ins().store(MemFlags::new(), val, field_ptr, 0);

        Ok(())
    }

    /// 编译变量声明
    fn compile_var_decl(&mut self, decl: &VarDecl) -> Result<(), String> {
        // from 借用检查：借用存活期间禁止重声明来源变量（旧对象会被释放）
        self.check_borrow_source_assign(&decl.name)?;
        self.snapshot_binding_for_scope(&decl.name);

        // 确定 Bolide 类型
        let raw_bolide_ty = if let Some(ref t) = decl.ty {
            t.clone()
        } else if let Some(ref value) = decl.value {
            // 从初始化表达式推断类型
            self.infer_expr_type(value)
        } else {
            BolideType::Int
        };
        let bolide_ty = self.normalize_bolide_type(&raw_bolide_ty);

        let mut pending_spawn_func: Option<String> = None;
        let mut pending_task_func: Option<String> = None;
        let mut pending_force_thread_task = false;
        if let Some(ref value) = decl.value {
            match value {
                Expr::Spawn(func_name, _) => {
                    pending_spawn_func = Some(func_name.clone());
                    pending_task_func = Some(func_name.clone());
                }
                Expr::SpawnThread(func_name, _) => {
                    pending_spawn_func = Some(func_name.clone());
                    pending_task_func = Some(func_name.clone());
                    pending_force_thread_task = true;
                }
                Expr::Call(func_expr, _) => {
                    // 检查是否是异步函数调用
                    if let Expr::Ident(func_name) = func_expr.as_ref() {
                        if self.async_funcs.contains(func_name) {
                            pending_spawn_func = Some(func_name.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        // 检查是否是全局变量
        // 只有顶层代码（__main__）中的 let 才操作全局变量；
        // 函数内的同名 let 声明新的局部变量（遮蔽全局）
        if self.current_func_name == "__main__" && self.global_data_ids.contains_key(&decl.name) {
            self.spawn_func_map.remove(&decl.name);
            self.task_func_map.remove(&decl.name);
            self.force_thread_tasks.remove(&decl.name);
            if let Some(func_name) = pending_spawn_func.clone() {
                self.spawn_func_map.insert(decl.name.clone(), func_name);
            }
            if let Some(func_name) = pending_task_func.clone() {
                self.task_func_map.insert(decl.name.clone(), func_name);
            }
            if pending_force_thread_task {
                self.force_thread_tasks.insert(decl.name.clone());
            }

            // 全局变量不需要创建局部变量，直接编译初始化赋值
            if let Some(ref val) = decl.value {
                self.compile_var_assign(&decl.name, val)?;
            }
            return Ok(());
        }

        // 转换为 Cranelift 类型
        let ty = self.bolide_type_to_cranelift(&bolide_ty);

        // 检查变量是否已存在（循环中已预初始化的变量）
        let existing_var = self.variables.get(&decl.name).copied().filter(|_| {
            self.var_scope_depth.get(&decl.name).copied().unwrap_or(0) == self.scope_depth
        });

        // 初始化表达式必须在新绑定生效前编译，这样 `let x = x.foo`
        // 会读取外层/已有的 x，而不是刚声明但尚未初始化的新 x。
        let init_value = if let Some(ref value) = decl.value {
            // 空列表字面量需用类型标注确定元素类型
            let raw_val = if matches!(value, Expr::List(items) if items.is_empty())
                && matches!(&bolide_ty, BolideType::List(_))
            {
                self.compile_list_with_hint(&[], Some(&bolide_ty))?
            } else {
                self.compile_expr(value)?
            };
            let raw_ty = self.normalize_bolide_type(&self.infer_expr_type(value));
            let val = self.prepare_value_for_storage(raw_val, &raw_ty, &bolide_ty)?;
            let is_from_lifetime_func = self.is_lifetime_func_call(value);
            Some((value, val, is_from_lifetime_func))
        } else {
            None
        };

        self.spawn_func_map.remove(&decl.name);
        self.task_func_map.remove(&decl.name);
        self.force_thread_tasks.remove(&decl.name);
        if let Some(func_name) = pending_spawn_func {
            self.spawn_func_map.insert(decl.name.clone(), func_name);
        }
        if let Some(func_name) = pending_task_func {
            self.task_func_map.insert(decl.name.clone(), func_name);
        }
        if pending_force_thread_task {
            self.force_thread_tasks.insert(decl.name.clone());
        }

        // 记录局部变量的 Bolide 类型（需要规范化类型名称）
        self.var_types.insert(decl.name.clone(), bolide_ty.clone());

        let var = if let Some(v) = existing_var {
            // 变量已存在（循环中预初始化过），release 旧值
            if Self::is_rc_type(&bolide_ty) {
                let old_val = self.builder.use_var(v);

                // 检查旧值是否为 null（第一次迭代时可能是 null）
                let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                let is_null = self.builder.ins().icmp(IntCC::Equal, old_val, null_val);

                let release_block = self.builder.create_block();
                let continue_block = self.builder.create_block();

                self.builder
                    .ins()
                    .brif(is_null, continue_block, &[], release_block, &[]);

                // release_block: 释放旧值
                self.builder.switch_to_block(release_block);
                self.builder.seal_block(release_block);

                self.emit_release(old_val, &bolide_ty);

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
        self.record_var_scope(&decl.name);

        if let Some((value, val, is_from_lifetime_func)) = init_value {
            // 如果是 RC 类型，需要处理引用计数
            if Self::is_rc_type(&bolide_ty) && !is_from_lifetime_func {
                // 检查值是否来自临时 RC 值（函数调用结果等）
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);

                // weak/unowned 声明不接管强引用所有权：
                // 临时强引用仍在语句末释放，这里只增加弱计数
                if is_temp && !Self::is_weak_ref_type(&bolide_ty) {
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

            // 闭包所有权：变量接管闭包对象
            if self.closure_temps.contains(&val) {
                // 来自闭包字面量/返回闭包的临时值：吸收所有权
                self.remove_temp_closure(val);
                self.closure_vars.insert(decl.name.clone());
            } else if let Expr::Ident(src) = value {
                // 别名另一个闭包变量：retain 一份，新变量独立持有
                if self.closure_vars.contains(src) || self.closure_param_vars.contains(src) {
                    self.emit_closure_retain(val);
                    self.closure_vars.insert(decl.name.clone());
                }
            } else if matches!(
                self.funcsig_expr_source(value),
                FuncSigReturnSource::Closure
            ) {
                self.emit_closure_retain(val);
                self.closure_vars.insert(decl.name.clone());
            } else if matches!(bolide_ty, BolideType::FuncSig(_, _) | BolideType::Func)
                && !self.expr_yields_raw_funcsig(value)
            {
                self.emit_closure_retain(val);
                self.closure_vars.insert(decl.name.clone());
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
        let is_from_lifetime_func = decl
            .value
            .as_ref()
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

        // 追踪 weak/unowned 变量（访问时需要检查对象是否存活）
        if matches!(bolide_ty, BolideType::Weak(_) | BolideType::Unowned(_)) {
            self.weak_variables.insert(decl.name.clone());
        }

        Ok(())
    }

    /// 编译 return 语句
    fn compile_return(&mut self, expr: Option<&Expr>) -> Result<(), String> {
        self.emit_active_finallys_from(0)?;
        if let Some(e) = expr {
            // 生命周期模式：验证返回值来源
            if self.uses_lifetime_mode() {
                self.validate_lifetime_return(e)?;
            }

            // 先编译返回表达式
            let raw_val = self.compile_expr(e)?;
            let val_ty = self.infer_expr_type(e);
            let return_ty = self
                .func_return_types
                .get(&self.current_func_name)
                .cloned()
                .flatten();
            let val = if let Some(ref return_ty) = return_ty {
                self.prepare_value_for_storage(raw_val, &val_ty, return_ty)?
            } else {
                raw_val
            };
            let val_ty = return_ty.unwrap_or(val_ty);
            let returns_raw_value = val == raw_val;

            // 最终使用的返回值（可能会因为 retain 而改变指针）
            let mut final_val = val;

            // 检查返回值是否是局部 RC 变量（如果是，不释放该变量）
            let return_var_name = if let Expr::Ident(name) = e {
                Some(name.clone())
            } else {
                None
            };

            // from 借用检查：非生命周期函数禁止返回借用值
            // （借用来源是局部变量，函数返回后即悬空）
            if !self.uses_lifetime_mode() {
                if let Some(ref name) = return_var_name {
                    if let Some((src, _)) = self.borrowed_vars.get(name) {
                        return Err(format!(
                            "Lifetime error: cannot return '{}' which borrows from '{}'; \
                             declare the function with 'from' or copy the value",
                            name, src
                        ));
                    }
                }
            }

            // 生命周期模式下跳过 ARC 操作
            if !self.uses_lifetime_mode() {
                // 如果是 RC 类型
                if Self::is_rc_type(&val_ty) {
                    let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                    if is_temp {
                        // 如果返回的是临时 RC 值，从临时列表中移除（调用者将接管所有权）
                        self.remove_temp_rc_value(val);
                    } else {
                        // 如果不是临时值
                        if let Some(ref name) = return_var_name {
                            // 借用（borrow/ref）参数归调用方所有，返回时必须 clone 一份
                            // 交给调用方独立持有（否则与调用方释放路径产生悬空指针）
                            let is_caller_owned_param = self
                                .func_params
                                .get(&self.current_func_name)
                                .map(|ps| {
                                    ps.iter().any(|p| {
                                        p.name == *name
                                            && matches!(p.mode, ParamMode::Borrow | ParamMode::Ref)
                                    })
                                })
                                .unwrap_or(false);
                            if is_caller_owned_param {
                                if let Some(new_val) = self.emit_retain(val, &val_ty) {
                                    final_val = new_val;
                                }
                            }
                            // 本地变量 (Ident)：cleanup_except 会跳过它，不需要 retain
                        } else {
                            // 如果是其他表达式 (如 Index, Member)，是从某个容器借用的
                            // cleanup 会释放容器，导致该值也被释放
                            // 所以这里必须 retain (clone) 一份，使 count +1
                            if let Some(new_val) = self.emit_retain(val, &val_ty) {
                                final_val = new_val;
                            }
                        }
                    }
                }

                // 返回闭包对象的所有权处理
                let is_closure_temp = self.closure_temps.contains(&val);
                let is_closure_var = return_var_name
                    .as_ref()
                    .map(|n| self.closure_vars.contains(n))
                    .unwrap_or(false);
                let is_closure_param = return_var_name
                    .as_ref()
                    .map(|n| self.closure_param_vars.contains(n))
                    .unwrap_or(false);

                let returns_raw_funcsig =
                    matches!(val_ty, BolideType::FuncSig(_, _) | BolideType::Func)
                        && self.current_func_returns_raw_funcsig();
                if returns_raw_funcsig {
                    // 裸函数值按普通函数指针返回，不走闭包对象引用计数。
                } else if is_closure_temp {
                    // 返回刚创建的闭包临时值：调用者接管所有权
                    self.remove_temp_closure(val);
                } else if is_closure_var || is_closure_param {
                    // 返回局部闭包变量 / 函数类型参数：不释放，调用者共享同一对象
                    // （调用者会 retain 或按需要释放）
                } else if matches!(val_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                    // 返回复杂表达式求值得到的闭包（如 getFn()()）：retain 一份给调用者
                    self.emit_closure_retain(val);
                }

                // 释放所有临时 RC 值（那些没有被返回的）
                self.release_temp_rc_values();

                // 释放所有 RC 变量，除了返回的那个
                let cleanup_except = if returns_raw_value {
                    return_var_name.as_deref()
                } else {
                    None
                };
                self.emit_rc_cleanup_except(cleanup_except);
                // __main__ 返回前释放全局 RC 变量
                if self.current_func_name == "__main__" {
                    self.emit_global_rc_cleanup();
                }
            }

            // 写回 Ref 参数
            self.write_back_closure_captures();
            self.write_back_ref_params();

            self.builder.ins().return_(&[final_val]);
        } else {
            // 生命周期模式下跳过 ARC 操作
            if !self.uses_lifetime_mode() {
                // 释放所有临时 RC 值
                self.release_temp_rc_values();

                // 无返回值，释放所有 RC 变量
                self.emit_rc_cleanup();
                // __main__ 返回前释放全局 RC 变量
                if self.current_func_name == "__main__" {
                    self.emit_global_rc_cleanup();
                }
            }

            // 写回 Ref 参数
            self.write_back_closure_captures();
            self.write_back_ref_params();

            self.builder.ins().return_(&[]);
        }
        Ok(())
    }

    /// 统一的 retain 辅助函数
    fn emit_retain(&mut self, val: Value, ty: &BolideType) -> Option<Value> {
        if let Some(clone_func) = Self::get_clone_func_name(ty) {
            if let Some(&func_ref) = self.func_refs.get(clone_func) {
                let call = self.builder.ins().call(func_ref, &[val]);
                Some(self.builder.inst_results(call)[0])
            } else {
                None
            }
        } else {
            None
        }
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

    fn prepare_funcsig_for_container_storage(
        &mut self,
        val: Value,
        expr: &Expr,
        target_ty: &BolideType,
    ) -> Result<Value, String> {
        if let BolideType::FuncSig(param_types, ret_type) = target_ty {
            if self.expr_yields_raw_funcsig(expr) {
                return self.wrap_raw_funcsig_as_closure(val, param_types, ret_type);
            }
        }
        Ok(val)
    }

    fn wrap_raw_funcsig_as_closure(
        &mut self,
        raw_func_ptr: Value,
        param_types: &[BolideType],
        ret_type: &Option<Box<BolideType>>,
    ) -> Result<Value, String> {
        let alloc_ref = *self
            .func_refs
            .get("@_bolide_alloc")
            .ok_or("bolide_alloc not found")?;
        let size_val = self.builder.ins().iconst(types::I64, 8);
        let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
        let env_ptr = self.builder.inst_results(alloc_call)[0];
        self.builder
            .ins()
            .store(MemFlags::trusted(), raw_func_ptr, env_ptr, 0);

        let adapter_name = funcsig_adapter_name(param_types, ret_type);
        let adapter_ref = *self
            .func_refs
            .get(&adapter_name)
            .ok_or_else(|| format!("funcsig adapter not found: {}", adapter_name))?;
        let fn_ptr = self.builder.ins().func_addr(self.ptr_type, adapter_ref);

        let new_ref = *self
            .func_refs
            .get("@_closure_new")
            .ok_or("closure_new not found")?;
        let meta_ptr = self.builder.ins().iconst(self.ptr_type, 0);
        let call = self
            .builder
            .ins()
            .call(new_ref, &[fn_ptr, env_ptr, size_val, meta_ptr]);
        let closure_val = self.builder.inst_results(call)[0];
        self.closure_temps.push(closure_val);
        Ok(closure_val)
    }

    fn current_func_returns_raw_funcsig(&self) -> bool {
        match self
            .funcsig_return_sources
            .get(&self.current_func_name)
            .copied()
        {
            Some(FuncSigReturnSource::Raw) => true,
            Some(FuncSigReturnSource::Param(index)) => self
                .func_params
                .get(&self.current_func_name)
                .and_then(|params| params.get(index))
                .map(|param| !self.closure_param_vars.contains(&param.name))
                .unwrap_or(false),
            Some(FuncSigReturnSource::ParamSet(mask)) => self
                .func_params
                .get(&self.current_func_name)
                .map(|params| {
                    JitCompiler::funcsig_source_param_indices(FuncSigReturnSource::ParamSet(mask))
                        .into_iter()
                        .all(|index| {
                            params
                                .get(index)
                                .map(|param| !self.closure_param_vars.contains(&param.name))
                                .unwrap_or(false)
                        })
                })
                .unwrap_or(false),
            _ => false,
        }
    }

    fn expr_yields_raw_funcsig(&self, expr: &Expr) -> bool {
        matches!(self.funcsig_expr_source(expr), FuncSigReturnSource::Raw)
    }

    fn direct_call_returns_raw_funcsig(&self, func_name: &str, args: &[Expr]) -> bool {
        matches!(
            self.substitute_runtime_call_source(
                func_name,
                self.funcsig_return_sources
                    .get(func_name)
                    .copied()
                    .unwrap_or(FuncSigReturnSource::Unknown),
                None,
                args,
                0,
            ),
            FuncSigReturnSource::Raw
        )
    }

    fn funcsig_return_source_uses_param(&self, func_name: &str) -> bool {
        self.funcsig_return_sources
            .get(func_name)
            .copied()
            .map(|source| !JitCompiler::funcsig_source_param_indices(source).is_empty())
            .unwrap_or(false)
    }

    fn method_call_returns_raw_funcsig(&self, func_name: &str, base: &Expr, args: &[Expr]) -> bool {
        let source = self
            .funcsig_return_sources
            .get(func_name)
            .copied()
            .unwrap_or(FuncSigReturnSource::Unknown);
        let resolved = self.substitute_runtime_call_source(func_name, source, Some(base), args, 1);
        matches!(resolved, FuncSigReturnSource::Raw)
    }

    fn substitute_runtime_call_source(
        &self,
        func_name: &str,
        source: FuncSigReturnSource,
        base: Option<&Expr>,
        args: &[Expr],
        param_offset: usize,
    ) -> FuncSigReturnSource {
        match source {
            FuncSigReturnSource::Param(i) => {
                self.funcsig_param_source_for_call(func_name, i, base, args, param_offset)
            }
            FuncSigReturnSource::ParamSet(mask) => {
                let sources: Vec<_> =
                    JitCompiler::funcsig_source_param_indices(FuncSigReturnSource::ParamSet(mask))
                        .into_iter()
                        .map(|i| {
                            self.funcsig_param_source_for_call(
                                func_name,
                                i,
                                base,
                                args,
                                param_offset,
                            )
                        })
                        .collect();
                JitCompiler::merge_funcsig_sources(&sources)
            }
            other => other,
        }
    }

    fn funcsig_param_source_for_call(
        &self,
        func_name: &str,
        param_index: usize,
        base: Option<&Expr>,
        args: &[Expr],
        param_offset: usize,
    ) -> FuncSigReturnSource {
        if self
            .funcsig_closure_param_indices
            .get(func_name)
            .map(|indices| indices.contains(&param_index))
            .unwrap_or(false)
        {
            return FuncSigReturnSource::Closure;
        }
        if param_index < param_offset {
            return base
                .map(|expr| self.funcsig_expr_source(expr))
                .unwrap_or(FuncSigReturnSource::Unknown);
        }
        args.get(param_index - param_offset)
            .map(|arg| self.funcsig_expr_source(arg))
            .unwrap_or(FuncSigReturnSource::Unknown)
    }

    fn funcsig_expr_source(&self, expr: &Expr) -> FuncSigReturnSource {
        match expr {
            Expr::Ident(name) => {
                if self.func_refs.contains_key(name) {
                    return FuncSigReturnSource::Raw;
                }
                if self.closure_vars.contains(name) || self.closure_param_vars.contains(name) {
                    return FuncSigReturnSource::Closure;
                }
                if matches!(
                    self.var_types
                        .get(name)
                        .or_else(|| self.global_var_types.get(name)),
                    Some(BolideType::FuncSig(_, _) | BolideType::Func)
                ) {
                    return FuncSigReturnSource::Raw;
                }
                FuncSigReturnSource::Unknown
            }
            Expr::Closure { .. } => FuncSigReturnSource::Closure,
            Expr::Index(_, _) | Expr::Member(_, _) => {
                if matches!(self.infer_expr_type(expr), BolideType::FuncSig(_, _)) {
                    FuncSigReturnSource::Closure
                } else {
                    FuncSigReturnSource::Unknown
                }
            }
            Expr::Call(callee, args) => match callee.as_ref() {
                Expr::Ident(name) => self.substitute_runtime_call_source(
                    name,
                    self.funcsig_return_sources
                        .get(name)
                        .copied()
                        .unwrap_or(FuncSigReturnSource::Unknown),
                    None,
                    args,
                    0,
                ),
                Expr::Member(base, member) => {
                    if let Expr::Ident(module_name) = base.as_ref() {
                        if self.modules.contains_key(module_name) {
                            let func_name = format!("@{}_{}", module_name, member);
                            return self.substitute_runtime_call_source(
                                &func_name,
                                self.funcsig_return_sources
                                    .get(&func_name)
                                    .copied()
                                    .unwrap_or(FuncSigReturnSource::Unknown),
                                None,
                                args,
                                0,
                            );
                        }
                    }
                    if let Ok(BolideType::Custom(class_name)) = self.get_expr_type(base) {
                        if let Ok(func_name) = self.find_method(&class_name, member) {
                            if self.method_call_returns_raw_funcsig(&func_name, base, args) {
                                return FuncSigReturnSource::Raw;
                            }
                            if matches!(
                                self.funcsig_return_sources.get(&func_name),
                                Some(FuncSigReturnSource::Closure)
                            ) {
                                return FuncSigReturnSource::Closure;
                            }
                        }
                    }
                    FuncSigReturnSource::Unknown
                }
                _ => FuncSigReturnSource::Unknown,
            },
            _ => FuncSigReturnSource::Unknown,
        }
    }

    /// 写回所有 Ref 参数的值
    fn write_back_ref_params(&mut self) {
        for (_, var, ptr_addr) in &self.ref_params.clone() {
            let current_val = self.builder.use_var(*var);
            self.builder
                .ins()
                .store(MemFlags::new(), current_val, *ptr_addr, 0);
        }
    }

    fn write_back_closure_captures(&mut self) {
        let Some(env_ptr) = self.closure_env_ptr else {
            return;
        };
        for (i, (name, ty)) in self.closure_captures.clone().iter().enumerate() {
            if let Some(&var) = self.variables.get(name) {
                let mut val = self.builder.use_var(var);
                if matches!(ty, BolideType::Float) {
                    val = self.builder.ins().bitcast(types::I64, MemFlags::new(), val);
                }
                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val, env_ptr, (i * 8) as i32);
            }
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

        self.builder
            .ins()
            .brif(cond, then_block, &[], else_block, &[]);

        self.builder.switch_to_block(then_block);
        self.builder.seal_block(then_block);
        self.enter_scope(); // 进入 then 作用域
        let mut then_terminated = false;
        for stmt in &if_stmt.then_body {
            if then_terminated {
                break;
            }
            then_terminated = self.compile_stmt(stmt)?;
        }
        self.leave_scope()?; // 离开 then 作用域
        if !then_terminated {
            self.builder.ins().jump(merge_block, &[]);
        }

        self.builder.switch_to_block(else_block);
        self.builder.seal_block(else_block);

        let else_terminated = if !if_stmt.elif_branches.is_empty() {
            self.compile_elif_chain(&if_stmt.elif_branches, &if_stmt.else_body, merge_block)?
        } else if let Some(ref else_body) = if_stmt.else_body {
            self.enter_scope(); // 进入 else 作用域
            let mut terminated = false;
            for stmt in else_body {
                if terminated {
                    break;
                }
                terminated = self.compile_stmt(stmt)?;
            }
            self.leave_scope()?; // 离开 else 作用域
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
                self.enter_scope();
                let mut terminated = false;
                for stmt in body {
                    if terminated {
                        break;
                    }
                    terminated = self.compile_stmt(stmt)?;
                }
                self.leave_scope()?;
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

        self.builder
            .ins()
            .brif(cond, then_block, &[], else_block, &[]);

        self.builder.switch_to_block(then_block);
        self.builder.seal_block(then_block);
        self.enter_scope();
        let mut then_terminated = false;
        for stmt in then_body {
            if then_terminated {
                break;
            }
            then_terminated = self.compile_stmt(stmt)?;
        }
        self.leave_scope()?;
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
        self.builder
            .ins()
            .brif(cond, body_block, &[], exit_block, &[]);

        // 第二遍：正常编译循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);
        self.enter_scope(); // 进入循环体作用域
                            // while: continue → 重新检查条件（header）；break → exit
        self.loop_stack
            .push((header_block, exit_block, self.current_finally_depth()));
        let mut terminated = false;
        for stmt in &while_stmt.body {
            if terminated {
                break;
            }
            terminated = self.compile_stmt(stmt)?;
        }
        self.loop_stack.pop();
        self.leave_scope()?; // 离开循环体作用域
        if !terminated {
            self.builder.ins().jump(header_block, &[]);
        }

        self.builder.seal_block(header_block);

        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);

        // 释放循环体内预声明的 RC 变量（最后一次迭代的值）
        for (var_name, var_ty) in &loop_rc_vars {
            if let Some(&var) = self.variables.get(var_name) {
                let val = self.builder.use_var(var);
                // 跳过 null（变量未初始化的情况）
                let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                let is_null = self.builder.ins().icmp(IntCC::Equal, val, null_val);
                let release_block = self.builder.create_block();
                let skip_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(is_null, skip_block, &[], release_block, &[]);
                self.builder.switch_to_block(release_block);
                self.builder.seal_block(release_block);
                self.emit_release(val, var_ty);
                self.builder.ins().jump(skip_block, &[]);
                self.builder.switch_to_block(skip_block);
                self.builder.seal_block(skip_block);
            }
            // 从 rc_variables 中移除（已在此处处理）
            self.rc_variables.retain(|(n, _)| n != var_name);
        }

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
    fn compile_for_range(
        &mut self,
        var_name: &str,
        args: &[Expr],
        body: &[Statement],
    ) -> Result<(), String> {
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
                let is_neg = if let Expr::Int(n) = &args[2] {
                    *n < 0
                } else {
                    false
                };
                (start, end, step, is_neg)
            }
            _ => return Err("range() expects 1, 2, or 3 arguments".to_string()),
        };

        self.enter_scope();

        // 创建循环变量
        let loop_var = self.declare_variable(var_name, types::I64);
        self.builder.def_var(loop_var, start_val);
        self.var_types.insert(var_name.to_string(), BolideType::Int);
        self.record_var_scope(var_name);

        // 创建基本块（latch 块承载递增逻辑，continue 跳到 latch 以保证步进）
        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let latch_block = self.builder.create_block();
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
            self.builder
                .ins()
                .icmp(IntCC::SignedGreaterThan, current_val, end_val)
        } else {
            // 正步长: i < end
            self.builder
                .ins()
                .icmp(IntCC::SignedLessThan, current_val, end_val)
        };
        self.builder
            .ins()
            .brif(cond, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);
        self.enter_scope();
        self.loop_stack
            .push((latch_block, exit_block, self.current_finally_depth()));
        let mut terminated = false;
        for stmt in body {
            if terminated {
                break;
            }
            terminated = self.compile_stmt(stmt)?;
        }
        self.loop_stack.pop();
        self.leave_scope()?;

        if !terminated {
            self.builder.ins().jump(latch_block, &[]);
        }

        // latch: 递增/递减循环变量后回到 header
        self.builder.switch_to_block(latch_block);
        self.builder.seal_block(latch_block);
        let current = self.builder.use_var(loop_var);
        let next = self.builder.ins().iadd(current, step_val);
        self.builder.def_var(loop_var, next);
        self.builder.ins().jump(header_block, &[]);

        self.builder.seal_block(header_block);
        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);
        self.leave_scope()?;

        Ok(())
    }

    /// 编译 for item in list { ... }
    /// 编译列表迭代逻辑 (通用)
    fn compile_list_iteration_loop(
        &mut self,
        vars: &[String],
        list_ptr: Value,
        elem_type: BolideType,
        body: &[Statement],
    ) -> Result<(), String> {
        // 获取列表长度: list_len(list_ptr)
        let list_len_ref = *self
            .func_refs
            .get("@_list_len")
            .ok_or("list_len not found")?;
        let len_call = self.builder.ins().call(list_len_ref, &[list_ptr]);
        let list_length = self.builder.inst_results(len_call)[0];

        self.enter_scope();

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
            self.var_types
                .insert(vars[0].to_string(), elem_type.clone());
            self.record_var_scope(&vars[0]);
            Some(v)
        } else {
            None // Destructuring handled inside body
        };

        // 创建基本块（latch 块承载索引递增，continue 跳到 latch）
        let header_block = self.builder.create_block();
        let body_block = self.builder.create_block();
        let latch_block = self.builder.create_block();
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
        let cond = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThan, current_idx, list_length);
        self.builder
            .ins()
            .brif(cond, body_block, &[], exit_block, &[]);

        // 循环体
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);

        // 获取当前元素: list_get(list_ptr, idx)
        let list_get_ref = *self
            .func_refs
            .get("@_list_get")
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
                BolideType::List(inner_type) => {
                    // List unpacking
                    let list_get_ref = *self
                        .func_refs
                        .get("@_list_get")
                        .ok_or("list_get not found")?;
                    for (i, var_name) in vars.iter().enumerate() {
                        let idx_const = self.builder.ins().iconst(types::I64, i as i64);
                        let call = self
                            .builder
                            .ins()
                            .call(list_get_ref, &[elem_val, idx_const]);
                        let val = self.builder.inst_results(call)[0];
                        self.define_variable(var_name, val, *inner_type.clone())?;
                    }
                }
                BolideType::Tuple(inner_types) => {
                    // Tuple unpacking
                    let tuple_get_ref = *self
                        .func_refs
                        .get("@_tuple_get")
                        .ok_or("tuple_get not found")?;
                    // Ensure vars count matches tuple size? or min?
                    for (i, var_name) in vars.iter().enumerate() {
                        let idx_const = self.builder.ins().iconst(types::I64, i as i64);
                        let call = self
                            .builder
                            .ins()
                            .call(tuple_get_ref, &[elem_val, idx_const]);
                        let val = self.builder.inst_results(call)[0];

                        let ty = if i < inner_types.len() {
                            inner_types[i].clone()
                        } else {
                            BolideType::Int
                        }; // Fallback
                        self.define_variable(var_name, val, ty)?;
                    }
                }
                _ => return Err(format!("Cannot unpack type {:?} in for loop", elem_type)),
            }
        }

        self.enter_scope();
        self.loop_stack
            .push((latch_block, exit_block, self.current_finally_depth()));
        let mut terminated = false;
        for stmt in body {
            if terminated {
                break;
            }
            terminated = self.compile_stmt(stmt)?;
        }
        self.loop_stack.pop();
        self.leave_scope()?;

        if !terminated {
            self.builder.ins().jump(latch_block, &[]);
        }

        // latch: 递增索引后回到 header
        self.builder.switch_to_block(latch_block);
        self.builder.seal_block(latch_block);
        let current = self.builder.use_var(idx_var);
        let next = self.builder.ins().iadd_imm(current, 1);
        self.builder.def_var(idx_var, next);
        self.builder.ins().jump(header_block, &[]);

        self.builder.seal_block(header_block);
        self.builder.switch_to_block(exit_block);
        self.builder.seal_block(exit_block);
        self.leave_scope()?;

        Ok(())
    }

    /// 编译 for item in list { ... }
    fn compile_for_list(
        &mut self,
        vars: &[String],
        iter_expr: &Expr,
        body: &[Statement],
    ) -> Result<(), String> {
        let list_ptr = self.compile_expr(iter_expr)?;
        let elem_type = match self.infer_expr_type(iter_expr) {
            BolideType::List(inner) => *inner,
            _ => BolideType::Int,
        };
        let iter_ty = BolideType::List(Box::new(elem_type.clone()));
        let owns_temp_iter = self.temp_rc_values.iter().any(|(v, _)| *v == list_ptr);
        if owns_temp_iter {
            self.remove_temp_rc_value(list_ptr);
        }
        self.compile_list_iteration_loop(vars, list_ptr, elem_type, body)?;
        if owns_temp_iter {
            self.emit_release(list_ptr, &iter_ty);
        }
        Ok(())
    }

    /// 编译 for key in dict { ... }
    fn compile_for_dict(
        &mut self,
        vars: &[String],
        iter_expr: &Expr,
        body: &[Statement],
    ) -> Result<(), String> {
        let dict_ptr = self.compile_expr(iter_expr)?;
        self.enter_scope();

        let dict_iter = *self
            .func_refs
            .get("@_dict_iter")
            .ok_or("dict_iter not found")?;
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
            let list_len_ref = *self
                .func_refs
                .get("@_list_len")
                .ok_or("list_len not found")?;
            let len_call = self.builder.ins().call(list_len_ref, &[keys_list_ptr]);
            let list_length = self.builder.inst_results(len_call)[0];

            let idx_var = self.declare_variable(&format!("__for_idx_{}", vars[0]), types::I64);
            let zero = self.builder.ins().iconst(types::I64, 0);
            self.builder.def_var(idx_var, zero);

            let header_block = self.builder.create_block();
            let body_block = self.builder.create_block();
            let latch_block = self.builder.create_block();
            let exit_block = self.builder.create_block();

            self.builder.ins().jump(header_block, &[]);

            // Header
            self.builder.switch_to_block(header_block);
            let current_idx = self.builder.use_var(idx_var);
            let cond = self
                .builder
                .ins()
                .icmp(IntCC::SignedLessThan, current_idx, list_length);
            self.builder
                .ins()
                .brif(cond, body_block, &[], exit_block, &[]);

            // Body
            self.builder.switch_to_block(body_block);
            self.builder.seal_block(body_block);

            // Get Key
            let list_get_ref = *self
                .func_refs
                .get("@_list_get")
                .ok_or("list_get not found")?;
            let get_key_call = self
                .builder
                .ins()
                .call(list_get_ref, &[keys_list_ptr, current_idx]);
            let key_val = self.builder.inst_results(get_key_call)[0];

            self.define_variable(&vars[0], key_val, key_type.clone())?;

            // Get Value: val = dict_get(dict_ptr, key)
            let dict_get_ref = *self
                .func_refs
                .get("@_dict_get")
                .ok_or("dict_get not found")?;
            let get_val_call = self.builder.ins().call(dict_get_ref, &[dict_ptr, key_val]);
            let val_val = self.builder.inst_results(get_val_call)[0];

            self.define_variable(&vars[1], val_val, val_type.clone())?;

            // Compile body
            self.enter_scope();
            self.loop_stack
                .push((latch_block, exit_block, self.current_finally_depth()));
            let mut terminated = false;
            for stmt in body {
                if terminated {
                    break;
                }
                terminated = self.compile_stmt(stmt)?;
            }
            self.loop_stack.pop();
            self.leave_scope()?;

            if !terminated {
                self.builder.ins().jump(latch_block, &[]);
            }

            // latch: 递增索引后回到 header
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
            // 单变量迭代 (Keys)
            self.compile_list_iteration_loop(vars, keys_list_ptr, key_type, body)?;
        }

        // Release keys list
        let release_fn = *self
            .func_refs
            .get("@_list_release")
            .ok_or("list_release not found")?;
        self.builder.ins().call(release_fn, &[keys_list_ptr]);
        self.leave_scope()?;

        Ok(())
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
            Expr::String(s) => {
                // JIT 与生成代码同进程：编译期直接完成 interning，
                // 运行期只做一次 retain（替代原先每次求值的哈希查找）
                let interned =
                    bolide_runtime::bolide_string_literal(s.as_ptr() as *const i8, s.len());

                let retain_ref = *self
                    .func_refs
                    .get("@_string_retain")
                    .ok_or("string_retain not found")?;

                let ptr_val = self.builder.ins().iconst(self.ptr_type, interned as i64);
                self.builder.ins().call(retain_ref, &[ptr_val]);
                Ok(ptr_val)
            }
            Expr::BigInt(s) => self.compile_bigint_literal(s),
            Expr::Decimal(s) => self.compile_decimal_literal(s),
            Expr::Ident(name) => self.compile_ident(name),
            Expr::BinOp(left, op, right) => self.compile_binop(left, op, right),
            Expr::UnaryOp(op, operand) => self.compile_unary(op, operand),
            Expr::Call(callee, args) => self.compile_call(callee, args),
            Expr::NamedArg(..) | Expr::SpreadArg(_) | Expr::KwSpreadArg(_) => {
                Err("argument modifiers are only valid inside call argument lists".to_string())
            }
            Expr::Index(base, index) => self.compile_index(base, index),
            Expr::Slice(base, start, end, step) => self.compile_slice(base, start, end, step),
            Expr::Member(base, member) => self.compile_member_access(base, member),
            Expr::List(items) => self.compile_list(items),
            Expr::ListComprehension {
                expr,
                vars,
                iter,
                filter,
            } => self.compile_list_comprehension(expr, vars, iter, filter.as_deref()),
            Expr::Spawn(func_name, args) => self.compile_spawn(func_name, args, false),
            Expr::SpawnThread(func_name, args) => self.compile_spawn(func_name, args, true),
            Expr::None => Ok(self.builder.ins().iconst(types::I64, 0)),
            Expr::Await(inner_expr) => self.compile_await(inner_expr),
            Expr::SpawnAll(exprs) => self.compile_spawn_all(exprs),
            Expr::Propagate(inner) => self.compile_propagate(inner),
            Expr::Raise(inner) => self.compile_raise(inner),
            Expr::TryExpr(body) => self.compile_try_expr(body),
            Expr::Tuple(exprs) => self.compile_tuple(exprs),
            Expr::Dict(entries) => self.compile_dict(entries),
            Expr::Closure {
                params,
                return_type,
                body,
            } => self.compile_closure(params, return_type.as_ref(), body),
        }
    }

    fn compile_adt_variant_from_values(
        &mut self,
        adt_name: &str,
        variant_name: &str,
        values: &[(Value, BolideType, bool)],
        type_args: Vec<BolideType>,
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
        if values.len() != variant.fields.len() {
            return Err(format!(
                "{}.{} expects {} value(s), got {}",
                adt_name,
                variant_name,
                variant.fields.len(),
                values.len()
            ));
        }

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

        let type_map = Self::adt_type_map(&adt_info, &type_args);
        for (field, (raw_val, actual_ty, owned)) in variant.fields.iter().zip(values.iter()) {
            let field_ty = Self::substitute_type(&field.ty, &type_map);
            let mut val = self.prepare_value_for_storage(*raw_val, actual_ty, &field_ty)?;
            let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field.offset as i64);
            if matches!(field_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                if !*owned {
                    self.emit_closure_retain(val);
                }
            } else if Self::is_rc_type(&field_ty) {
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                if *owned || (is_temp && !Self::is_weak_ref_type(&field_ty)) {
                    self.remove_temp_rc_value(val);
                } else if let Some(func_name) = Self::get_clone_func_name(&field_ty) {
                    if let Some(&func_ref) = self.func_refs.get(func_name) {
                        let clone_call = self.builder.ins().call(func_ref, &[val]);
                        val = self.builder.inst_results(clone_call)[0];
                    }
                }
            }
            self.builder.ins().store(MemFlags::new(), val, field_ptr, 0);
        }

        let result_ty = BolideType::Adt(adt_name.to_string(), type_args);
        self.track_temp_rc_value(obj_ptr, &result_ty);
        Ok(obj_ptr)
    }

    fn adt_success_field_value(
        &mut self,
        obj_ptr: Value,
        field_ty: &BolideType,
    ) -> Result<Value, String> {
        let field_ptr = self.builder.ins().iadd_imm(obj_ptr, 8);
        let cl_ty = self.bolide_type_to_cranelift(field_ty);
        let field_val = self
            .builder
            .ins()
            .load(cl_ty, MemFlags::new(), field_ptr, 0);
        if Self::is_rc_type(field_ty) {
            if let Some(retained) = self.emit_retain(field_val, field_ty) {
                self.track_temp_rc_value(retained, field_ty);
                return Ok(retained);
            }
        }
        Ok(field_val)
    }

    fn emit_return_value_now(
        &mut self,
        val: Value,
        val_ty: &BolideType,
        return_var_name: Option<&str>,
    ) -> Result<(), String> {
        self.emit_active_finallys_from(0)?;
        let mut final_val = val;
        if !self.uses_lifetime_mode() {
            if Self::is_rc_type(val_ty) {
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                if is_temp {
                    self.remove_temp_rc_value(val);
                } else if return_var_name.is_none() {
                    if let Some(retained) = self.emit_retain(val, val_ty) {
                        final_val = retained;
                    }
                }
            }
            self.release_temp_rc_values();
            let cleanup_except = if final_val == val {
                return_var_name
            } else {
                None
            };
            self.emit_rc_cleanup_except(cleanup_except);
            if self.current_func_name == "__main__" {
                self.emit_global_rc_cleanup();
            }
        }
        self.write_back_closure_captures();
        self.write_back_ref_params();
        self.builder.ins().return_(&[final_val]);
        Ok(())
    }

    fn compile_propagate(&mut self, inner: &Expr) -> Result<Value, String> {
        let inner_ty = self.normalize_bolide_type(&self.infer_expr_type(inner));
        let (adt_name, args) = match inner_ty.clone() {
            BolideType::Adt(name, args) if name == "Result" || name == "Option" => (name, args),
            other => {
                return Err(format!(
                    "? expects Result<T, E> or Option<T>, got {:?}",
                    other
                ))
            }
        };
        if args.is_empty() || (adt_name == "Result" && args.len() < 2) {
            return Err(format!("{} used with incomplete type arguments", adt_name));
        }

        let return_ty = self
            .func_return_types
            .get(&self.current_func_name)
            .cloned()
            .flatten()
            .ok_or("'?' requires the current function to return Result or Option")?;
        match (&adt_name, &return_ty) {
            (name, BolideType::Adt(ret_name, _)) if name == ret_name => {}
            _ => {
                return Err(format!(
                    "? in function returning {:?} cannot propagate {:?}",
                    return_ty, inner_ty
                ))
            }
        }

        let obj_ptr = self.compile_expr(inner)?;
        let tag_val = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), obj_ptr, 0);
        let success_tag = self.builder.ins().iconst(types::I64, 0);
        let is_success = self.builder.ins().icmp(IntCC::Equal, tag_val, success_tag);
        let success_load_block = self.builder.create_block();
        let success_block = self.builder.create_block();
        let fail_block = self.builder.create_block();
        let ok_ty = args[0].clone();
        let ok_cl_ty = self.bolide_type_to_cranelift(&ok_ty);
        self.builder.append_block_param(success_block, ok_cl_ty);
        self.builder
            .ins()
            .brif(is_success, success_load_block, &[], fail_block, &[]);

        self.builder.switch_to_block(fail_block);
        self.builder.seal_block(fail_block);
        let return_var_name = if let Expr::Ident(name) = inner {
            Some(name.as_str())
        } else {
            None
        };
        self.emit_return_value_now(obj_ptr, &inner_ty, return_var_name)?;

        self.builder.switch_to_block(success_load_block);
        self.builder.seal_block(success_load_block);
        let ok_val = self.adt_success_field_value(obj_ptr, &ok_ty)?;
        self.builder.ins().jump(success_block, &[ok_val]);

        self.builder.switch_to_block(success_block);
        self.builder.seal_block(success_block);
        Ok(self.builder.block_params(success_block)[0])
    }

    fn compile_raise(&mut self, inner: &Expr) -> Result<Value, String> {
        let inner_ty = self.normalize_bolide_type(&self.infer_expr_type(inner));
        let args = match inner_ty.clone() {
            BolideType::Adt(name, args) if name == "Result" && args.len() >= 2 => args,
            other => return Err(format!("! expects Result<T, Error>, got {:?}", other)),
        };
        let ok_ty = args[0].clone();
        let err_ty = args[1].clone();
        self.validate_error_type(&err_ty, "!")?;

        let obj_ptr = self.compile_expr(inner)?;
        let tag_val = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), obj_ptr, 0);
        let success_tag = self.builder.ins().iconst(types::I64, 0);
        let is_success = self.builder.ins().icmp(IntCC::Equal, tag_val, success_tag);
        let success_load_block = self.builder.create_block();
        let success_block = self.builder.create_block();
        let throw_block = self.builder.create_block();
        let ok_cl_ty = self.bolide_type_to_cranelift(&ok_ty);
        self.builder.append_block_param(success_block, ok_cl_ty);
        self.builder
            .ins()
            .brif(is_success, success_load_block, &[], throw_block, &[]);

        self.builder.switch_to_block(throw_block);
        self.builder.seal_block(throw_block);
        let err_ptr = self.adt_success_field_value(obj_ptr, &err_ty)?;
        self.remove_temp_rc_value(err_ptr);
        let tag = self.type_to_throw_tag(&err_ty);
        let tag_val = self.builder.ins().iconst(types::I64, tag);
        let emitted_catch_finally = if self.catch_body_depth > 0 {
            self.emit_active_finallys_from(self.current_finally_depth().saturating_sub(1))?;
            true
        } else {
            false
        };
        self.emit_exception_set(err_ptr, tag_val)?;
        self.emit_exception_transfer(emitted_catch_finally)?;

        self.builder.switch_to_block(success_load_block);
        self.builder.seal_block(success_load_block);
        let ok_val = self.adt_success_field_value(obj_ptr, &ok_ty)?;
        self.builder.ins().jump(success_block, &[ok_val]);

        self.builder.switch_to_block(success_block);
        self.builder.seal_block(success_block);
        Ok(self.builder.block_params(success_block)[0])
    }

    fn try_expr_ok_type(&self, body: &[Statement]) -> BolideType {
        body.last()
            .and_then(|stmt| match stmt {
                Statement::Expr(expr) => Some(self.infer_expr_type(expr)),
                _ => None,
            })
            .unwrap_or(BolideType::Int)
    }

    fn compile_try_expr(&mut self, body: &[Statement]) -> Result<Value, String> {
        let ok_ty = self.try_expr_ok_type(body);
        let err_ty = BolideType::Custom("Error".to_string());
        let result_ty = BolideType::Adt("Result".to_string(), vec![ok_ty.clone(), err_ty.clone()]);
        let catch_block = self.builder.create_block();
        let after_block = self.builder.create_block();
        self.builder
            .append_block_param(after_block, self.bolide_type_to_cranelift(&result_ty));

        self.catch_stack.push(catch_block);
        let mut diverted = false;
        let last_expr_index = body
            .last()
            .and_then(|stmt| matches!(stmt, Statement::Expr(_)).then_some(body.len() - 1));
        for (idx, stmt) in body.iter().enumerate() {
            if diverted {
                break;
            }
            if Some(idx) == last_expr_index {
                if let Statement::Expr(expr) = stmt {
                    let ok_val = self.compile_expr(expr)?;
                    let ok_res = self.compile_adt_variant_from_values(
                        "Result",
                        "Ok",
                        &[(ok_val, ok_ty.clone(), false)],
                        vec![ok_ty.clone(), err_ty.clone()],
                    )?;
                    self.remove_temp_rc_value(ok_res);
                    self.builder.ins().jump(after_block, &[ok_res]);
                    diverted = true;
                }
            } else {
                diverted = self.compile_stmt(stmt)?;
            }
        }
        self.catch_stack.pop();
        if !diverted {
            let zero = self.builder.ins().iconst(types::I64, 0);
            let ok_res = self.compile_adt_variant_from_values(
                "Result",
                "Ok",
                &[(zero, BolideType::Int, true)],
                vec![ok_ty.clone(), err_ty.clone()],
            )?;
            self.remove_temp_rc_value(ok_res);
            self.builder.ins().jump(after_block, &[ok_res]);
        }

        self.builder.switch_to_block(catch_block);
        self.builder.seal_block(catch_block);
        let ex_get_fn = *self
            .func_refs
            .get("@_exception_get")
            .ok_or("exception_get not found")?;
        let ex_call = self.builder.ins().call(ex_get_fn, &[]);
        let ex_ptr = self.builder.inst_results(ex_call)[0];
        let err_res = self.compile_adt_variant_from_values(
            "Result",
            "Err",
            &[(ex_ptr, err_ty.clone(), true)],
            vec![ok_ty, err_ty],
        )?;
        self.remove_temp_rc_value(err_res);
        self.builder.ins().jump(after_block, &[err_res]);

        self.builder.switch_to_block(after_block);
        self.builder.seal_block(after_block);
        let result = self.builder.block_params(after_block)[0];
        self.track_temp_rc_value(result, &result_ty);
        Ok(result)
    }

    /// RC 捕获释放 tag（与 runtime closure.rs release_capture 对齐）。非 RC 返回 0。
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

    /// 获取类型对应的 retain 函数名（闭包捕获时增加引用计数）
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

    /// 编译闭包表达式：lifting + 捕获 env + 生成闭包对象。
    /// 返回闭包对象指针（运行时 RC 管理，TypeTag::Closure）。
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
            // 只有真正的局部 SSA 变量才捕获；全局/函数名由 lifted 函数直接访问
            if self.variables.contains_key(name) {
                let ty = self.var_types.get(name).cloned().unwrap_or(BolideType::Int);
                captures.push((name.clone(), ty));
            }
        }

        // 2. 生成 lifted 函数名 + 声明签名 (env_ptr, ...params) -> ret
        let lifted_name = format!(
            "__closure_{}_{}",
            self.current_func_name, self.closure_local_counter
        );
        self.closure_local_counter += 1;

        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(self.ptr_type)); // env_ptr
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

        // 3. 取 lifted 函数地址
        let func_ref = self.module.declare_func_in_func(func_id, self.builder.func);
        let fn_ptr = self.builder.ins().func_addr(self.ptr_type, func_ref);

        // 4. 构造 env + meta
        let env_size = (captures.len() * 8) as i64;
        let (env_ptr, meta_ptr) = if captures.is_empty() {
            let null = self.builder.ins().iconst(self.ptr_type, 0);
            (null, self.builder.ins().iconst(self.ptr_type, 0))
        } else {
            // 分配 env
            let alloc_ref = *self
                .func_refs
                .get("@_bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(alloc_call)[0];

            // 逐个捕获：求值 + retain(RC) + 存入 env
            let mut tags: Vec<i64> = vec![captures.len() as i64];
            for (i, (name, ty)) in captures.clone().iter().enumerate() {
                let mut val = self.compile_ident(name)?;
                // RC 类型：retain 一份给闭包持有
                if let Some(retain) = Self::get_retain_func_name(ty) {
                    if let Some(&rref) = self.func_refs.get(retain) {
                        self.builder.ins().call(rref, &[val]);
                    }
                }
                // float 以位模式存入 i64 槽
                if matches!(ty, BolideType::Float) {
                    val = self.builder.ins().bitcast(types::I64, MemFlags::new(), val);
                }
                let offset = (i * 8) as i32;
                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val, env_ptr, offset);
                tags.push(Self::capture_release_tag(ty));
            }

            // meta 表泄漏为静态内存（程序生命周期内有效）
            let meta_boxed: Box<[i64]> = tags.into_boxed_slice();
            let meta_addr = Box::leak(meta_boxed).as_ptr() as i64;
            let meta_ptr = self.builder.ins().iconst(self.ptr_type, meta_addr);
            (env_ptr, meta_ptr)
        };

        // 5. 调用 closure_new(fn_ptr, env_ptr, env_size, meta_ptr)
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

        // 6. 入队 lifted 函数体编译
        self.pending_closures.push(ClosureJob {
            func_id,
            name: lifted_name,
            params: params.to_vec(),
            return_type: return_type.cloned(),
            body: body.to_vec(),
            captures,
        });

        // 标记为未吸收闭包临时值（语句末若未被变量吸收则 release）
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

        // 推断迭代器元素类型
        let iter_elem_ty = self.infer_iter_elem_type(iter);

        // 临时绑定循环变量以推断推导式元素类型
        let old_ty = self.var_types.get(loop_var_name).cloned();
        self.var_types
            .insert(loop_var_name.clone(), iter_elem_ty.clone());
        let elem_ty = self.infer_expr_type(expr);
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

        // 用合成变量保存结果列表，供循环体中 push 使用
        let result_name = format!("@_lc_result_{}", self.var_counter);
        let result_var = self.declare_variable(&result_name, self.ptr_type);
        self.builder.def_var(result_var, list_ptr);
        self.var_types
            .insert(result_name.clone(), BolideType::List(Box::new(elem_ty)));

        // 构造循环体：if filter { result.push(expr); }
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

        // 复用 for 循环编译
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
            BolideType::List(inner) => *inner,
            _ => BolideType::Int,
        }
    }

    /// 编译 BigInt 字面量
    fn compile_bigint_literal(&mut self, s: &str) -> Result<Value, String> {
        // 尝试作为 i64 解析，如果成功则用 bigint_from_i64
        let result = if let Ok(n) = s.parse::<i64>() {
            let func_ref = *self
                .func_refs
                .get("@_bigint_from_i64")
                .ok_or("bigint_from_i64 not found")?;
            let val = self.builder.ins().iconst(types::I64, n);
            let call = self.builder.ins().call(func_ref, &[val]);
            let results = self.builder.inst_results(call);
            results[0]
        } else {
            // 用字符串方式创建 BigInt（超出 i64 范围的大数）
            let func_ref = *self
                .func_refs
                .get("@_bigint_from_str")
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
            let func_ref = *self
                .func_refs
                .get("@_decimal_from_f64")
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
            return Err(format!(
                "Variable '{}' has been moved and cannot be used",
                name
            ));
        }

        // 先查找变量
        if let Some(&var) = self.variables.get(name) {
            let val = self.builder.use_var(var);

            // weak/unowned 变量访问时检查对象是否存活（运行时检查）
            // 对象已被释放或指针为 nil 时确定性 abort，而不是 use-after-free
            if self.weak_variables.contains(name) {
                if let Some(var_ty) = self.var_types.get(name) {
                    let inner_ty = match var_ty {
                        BolideType::Weak(inner) | BolideType::Unowned(inner) => {
                            inner.as_ref().clone()
                        }
                        other => other.clone(),
                    };
                    // 只对类实例进行存活检查
                    if matches!(inner_ty, BolideType::Custom(_)) {
                        if let Some(&assert_ref) = self.func_refs.get("@_object_assert_alive") {
                            self.builder.ins().call(assert_ref, &[val]);
                        }
                        return Ok(val);
                    }
                }
            }

            return Ok(val);
        }

        // 检查是否是全局变量
        if let Some(&data_id) = self.global_data_ids.get(name) {
            // 获取全局变量的地址
            let gv = self.module.declare_data_in_func(data_id, self.builder.func);
            let addr = self.builder.ins().global_value(self.ptr_type, gv);
            // 按全局变量的实际类型加载（float 全局必须加载为 F64）
            let load_ty = self
                .global_var_types
                .get(name)
                .map(|t| self.bolide_type_to_cranelift(t))
                .unwrap_or(self.ptr_type);
            let val = self.builder.ins().load(load_ty, MemFlags::new(), addr, 0);

            // weak/unowned 全局变量访问时检查对象是否存活
            if let Some(global_ty) = self.global_var_types.get(name) {
                if Self::is_weak_ref_type(global_ty) {
                    if let Some(&assert_ref) = self.func_refs.get("@_object_assert_alive") {
                        self.builder.ins().call(assert_ref, &[val]);
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
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.compile_short_circuit_binop(left, op, right);
        }

        // 推断操作数类型
        let left_ty = self.infer_expr_type(left);
        let right_ty = self.infer_expr_type(right);

        // 类类型运算符重载
        if let BolideType::Custom(ref class_name) = left_ty {
            if let Some(result) = self.try_operator_overload(left, op, right, class_name)? {
                return Ok(result);
            }
        }

        if matches!(op, BinOp::Add)
            && matches!(left_ty, BolideType::Str)
            && matches!(right_ty, BolideType::Str)
        {
            let mut parts = Vec::new();
            self.collect_string_concat_operands(left, &mut parts);
            self.collect_string_concat_operands(right, &mut parts);
            if parts.len() > 2 {
                return self.compile_string_concat_many(&parts);
            }
        }

        let lhs = self.compile_expr(left)?;
        let rhs = self.compile_expr(right)?;

        if matches!(left_ty, BolideType::Dynamic) || matches!(right_ty, BolideType::Dynamic) {
            return self.compile_dynamic_binop(lhs, &left_ty, op, rhs, &right_ty);
        }

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
                let func_ref = *self
                    .func_refs
                    .get("@_string_concat")
                    .ok_or("string_concat not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                return Ok(result);
            } else if matches!(op, BinOp::Eq) {
                let func_ref = *self
                    .func_refs
                    .get("@_string_eq")
                    .ok_or("string_eq not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                return Ok(self.builder.inst_results(call)[0]);
            } else if matches!(op, BinOp::Ne) {
                let func_ref = *self
                    .func_refs
                    .get("@_string_eq")
                    .ok_or("string_eq not found")?;
                let call = self.builder.ins().call(func_ref, &[lhs, rhs]);
                let eq_result = self.builder.inst_results(call)[0];
                let one = self.builder.ins().iconst(types::I64, 1);
                return Ok(self.builder.ins().isub(one, eq_result));
            } else {
                return Err(format!("Unsupported string operation: {:?}", op));
            }
        }

        if matches!(left_ty, BolideType::Str) || matches!(right_ty, BolideType::Str) {
            return Err(format!(
                "Cannot apply {:?} to {:?} and {:?}",
                op, left_ty, right_ty
            ));
        }

        // Float 运算
        let is_float =
            matches!(left_ty, BolideType::Float) || matches!(right_ty, BolideType::Float);
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
                    let cmp = self
                        .builder
                        .ins()
                        .fcmp(FloatCC::GreaterThanOrEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::And | BinOp::Or => {
                    return Err("Logical operations not supported for float".to_string());
                }
                BinOp::Shl | BinOp::Shr | BinOp::BitAnd | BinOp::BitOr | BinOp::Xor => {
                    return Err("Bit operations not supported for float".to_string());
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
                    let cmp = self
                        .builder
                        .ins()
                        .icmp(IntCC::SignedLessThanOrEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Gt => {
                    let cmp = self.builder.ins().icmp(IntCC::SignedGreaterThan, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }
                BinOp::Ge => {
                    let cmp = self
                        .builder
                        .ins()
                        .icmp(IntCC::SignedGreaterThanOrEqual, lhs, rhs);
                    self.builder.ins().uextend(types::I64, cmp)
                }

                BinOp::And => self.builder.ins().band(lhs, rhs),
                BinOp::Or => self.builder.ins().bor(lhs, rhs),
                BinOp::Shl => self.builder.ins().ishl(lhs, rhs),
                BinOp::Shr => self.builder.ins().sshr(lhs, rhs),
                BinOp::BitAnd => self.builder.ins().band(lhs, rhs),
                BinOp::BitOr => self.builder.ins().bor(lhs, rhs),
                BinOp::Xor => self.builder.ins().bxor(lhs, rhs),
            }
        };

        Ok(result)
    }

    fn compile_dynamic_binop(
        &mut self,
        lhs: Value,
        left_ty: &BolideType,
        op: &BinOp,
        rhs: Value,
        right_ty: &BolideType,
    ) -> Result<Value, String> {
        let lhs_dyn = if matches!(left_ty, BolideType::Dynamic) {
            lhs
        } else {
            self.convert_to_dynamic(lhs, left_ty)?
        };
        let rhs_dyn = if matches!(right_ty, BolideType::Dynamic) {
            rhs
        } else {
            self.convert_to_dynamic(rhs, right_ty)?
        };

        let func_name = match op {
            BinOp::Add => "@_dynamic_add",
            BinOp::Sub => "@_dynamic_sub",
            BinOp::Mul => "@_dynamic_mul",
            BinOp::Div => "@_dynamic_div",
            _ => return Err(format!("Unsupported dynamic operation: {:?}", op)),
        };
        let func_ref = *self
            .func_refs
            .get(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let call = self.builder.ins().call(func_ref, &[lhs_dyn, rhs_dyn]);
        let result = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(result, &BolideType::Dynamic);
        Ok(result)
    }

    fn boolish_to_i64(&mut self, value: Value) -> Value {
        let ty = self.builder.func.dfg.value_type(value);
        if ty == types::I64 {
            let zero = self.builder.ins().iconst(types::I64, 0);
            let cmp = self.builder.ins().icmp(IntCC::NotEqual, value, zero);
            self.builder.ins().uextend(types::I64, cmp)
        } else if ty == types::I8 {
            let zero = self.builder.ins().iconst(types::I8, 0);
            let cmp = self.builder.ins().icmp(IntCC::NotEqual, value, zero);
            self.builder.ins().uextend(types::I64, cmp)
        } else {
            value
        }
    }

    fn compile_short_circuit_binop(
        &mut self,
        left: &Expr,
        op: &BinOp,
        right: &Expr,
    ) -> Result<Value, String> {
        let lhs = self.compile_expr(left)?;
        let result_var = self.declare_variable(
            &format!("@_logical_result_{}", self.var_counter),
            types::I64,
        );

        let rhs_block = self.builder.create_block();
        let skip_block = self.builder.create_block();
        let merge_block = self.builder.create_block();

        match op {
            BinOp::And => {
                self.builder
                    .ins()
                    .brif(lhs, rhs_block, &[], skip_block, &[]);
                self.builder.switch_to_block(skip_block);
                self.builder.seal_block(skip_block);
                let false_val = self.builder.ins().iconst(types::I64, 0);
                self.builder.def_var(result_var, false_val);
                self.builder.ins().jump(merge_block, &[]);
            }
            BinOp::Or => {
                self.builder
                    .ins()
                    .brif(lhs, skip_block, &[], rhs_block, &[]);
                self.builder.switch_to_block(skip_block);
                self.builder.seal_block(skip_block);
                let true_val = self.builder.ins().iconst(types::I64, 1);
                self.builder.def_var(result_var, true_val);
                self.builder.ins().jump(merge_block, &[]);
            }
            _ => unreachable!(),
        }

        self.builder.switch_to_block(rhs_block);
        self.builder.seal_block(rhs_block);
        let rhs = self.compile_expr(right)?;
        let rhs_bool = self.boolish_to_i64(rhs);
        self.builder.def_var(result_var, rhs_bool);
        self.builder.ins().jump(merge_block, &[]);

        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);
        Ok(self.builder.use_var(result_var))
    }

    fn collect_string_concat_operands<'expr>(&self, expr: &'expr Expr, out: &mut Vec<&'expr Expr>) {
        if let Expr::BinOp(left, BinOp::Add, right) = expr {
            let left_is_str = matches!(self.infer_expr_type(left), BolideType::Str);
            let right_is_str = matches!(self.infer_expr_type(right), BolideType::Str);
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

    /// 编译 BigInt 二元操作
    fn compile_bigint_binop(
        &mut self,
        lhs: Value,
        op: &BinOp,
        rhs: Value,
    ) -> Result<Value, String> {
        // 算术运算返回新的 BigInt，需要跟踪为临时值
        let is_arithmetic = matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        );

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

        // 算术运算的结果是新分配的 BigInt，需要跟踪
        if is_arithmetic {
            self.track_temp_rc_value(result, &BolideType::BigInt);
        }

        Ok(result)
    }

    /// 编译 Decimal 二元操作
    fn compile_decimal_binop(
        &mut self,
        lhs: Value,
        op: &BinOp,
        rhs: Value,
    ) -> Result<Value, String> {
        // 算术运算返回新的 Decimal，需要跟踪为临时值
        let is_arithmetic = matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        );

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
        func_sig: Option<(Vec<BolideType>, Option<Box<BolideType>>)>,
    ) -> Result<Value, String> {
        // 获取函数指针（支持局部变量 / 全局变量 / 裸函数名）
        let func_ptr = self.compile_ident(var_name)?;
        self.compile_indirect_call_ptr(func_ptr, args, func_sig)
    }

    /// 通过闭包变量名调用闭包（解析签名后走闭包 ABI）
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
        // 取 fn_ptr 与 env_ptr
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

        // 编译参数
        let mut arg_values = vec![env_ptr];
        for arg in args {
            arg_values.push(self.compile_expr(arg)?);
        }

        // 构造签名: (env_ptr, ...params) -> ret
        #[cfg(target_os = "windows")]
        let mut sig = Signature::new(CallConv::WindowsFastcall);
        #[cfg(not(target_os = "windows"))]
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(self.ptr_type)); // env
        if param_types.is_empty() {
            for arg in args {
                let ty = self.infer_expr_type(arg);
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
        self.emit_exception_pending_check()?;
        let result = self.builder.inst_results(call)[0];

        // 返回值若是 RC 类型，登记为临时
        if Self::is_rc_type(&ret_b) {
            self.track_temp_rc_value(result, &ret_b);
        }
        // 返回值若是闭包对象，供上层变量吸收所有权
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
                sig.params
                    .push(AbiParam::new(self.bolide_type_to_cranelift(ty)));
            }
        } else {
            // 无签名时从参数推断
            for arg in args {
                let ty = self.infer_expr_type(arg);
                sig.params
                    .push(AbiParam::new(self.bolide_type_to_cranelift(&ty)));
            }
        }

        // 使用签名中的返回类型
        if let Some((_, Some(ret_type))) = &func_sig {
            sig.returns
                .push(AbiParam::new(self.bolide_type_to_cranelift(ret_type)));
        } else {
            // 无返回类型时默认 i64
            sig.returns.push(AbiParam::new(types::I64));
        }

        let sig_ref = self.builder.import_signature(sig);
        let call = self
            .builder
            .ins()
            .call_indirect(sig_ref, func_ptr, &arg_values);
        self.emit_exception_pending_check()?;
        let result = self.builder.inst_results(call)[0];

        // 如果返回类型是 RC 类型，track 为临时值
        if let Some((_, Some(ret_type))) = &func_sig {
            if Self::is_rc_type(ret_type) {
                self.track_temp_rc_value(result, ret_type);
            }
        }

        Ok(result)
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
        let mut explicit_slots = vec![false; params.len()];
        let mut prepared = Vec::with_capacity(params.len());
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
                        explicit_slots[i] = true;
                        slots[i] = Some((**value).clone());
                        prepared.push(PreparedArg::Expr {
                            expr: (**value).clone(),
                            target_index: i,
                        });
                    } else if let Some(target_index) = kwargs_index {
                        prepared.push(PreparedArg::PackedKwargItem {
                            target_index,
                            value_ty: match &params[target_index].ty {
                                BolideType::Dict(_, value) => value.as_ref().clone(),
                                _ => BolideType::Dynamic,
                            },
                            item: PackedKwargItem::Entry(name.clone(), (**value).clone()),
                        });
                    } else {
                        return Err(format!(
                            "{} got unexpected keyword argument '{}'",
                            call_name, name
                        ));
                    }
                }
                Expr::SpreadArg(value) => {
                    named_or_spread_seen = true;
                    if let Some(target_index) = args_index {
                        prepared.push(PreparedArg::PackedArgItem {
                            target_index,
                            elem_ty: match &params[target_index].ty {
                                BolideType::List(inner) => inner.as_ref().clone(),
                                _ => BolideType::Dynamic,
                            },
                            item: PackedArgItem::Spread((**value).clone()),
                        });
                    } else {
                        return Err(format!("{} does not accept *args", call_name));
                    }
                }
                Expr::KwSpreadArg(value) => {
                    named_or_spread_seen = true;
                    if let Some(target_index) = kwargs_index {
                        prepared.push(PreparedArg::PackedKwargItem {
                            target_index,
                            value_ty: match &params[target_index].ty {
                                BolideType::Dict(_, value) => value.as_ref().clone(),
                                _ => BolideType::Dynamic,
                            },
                            item: PackedKwargItem::Spread((**value).clone()),
                        });
                    } else {
                        return Err(format!("{} does not accept **kwargs", call_name));
                    }
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
                        explicit_slots[next_pos] = true;
                        slots[next_pos] = Some(expr.clone());
                        prepared.push(PreparedArg::Expr {
                            expr: expr.clone(),
                            target_index: next_pos,
                        });
                        next_pos += 1;
                    } else if let Some(target_index) = args_index {
                        prepared.push(PreparedArg::PackedArgItem {
                            target_index,
                            elem_ty: match &params[target_index].ty {
                                BolideType::List(inner) => inner.as_ref().clone(),
                                _ => BolideType::Dynamic,
                            },
                            item: PackedArgItem::Expr(expr.clone()),
                        });
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
                    prepared.push(PreparedArg::Expr {
                        expr: default_value.clone(),
                        target_index: i,
                    });
                } else {
                    return Err(format!(
                        "{} missing required argument '{}'",
                        call_name, param.name
                    ));
                }
            }
        }

        for (i, param) in params.iter().enumerate() {
            if !param.is_variadic
                && !param.is_kw_variadic
                && !explicit_slots[i]
                && slots[i].is_some()
            {
                continue;
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
                expr => prepared.push(PreparedArg::Expr {
                    expr: expr.clone(),
                    target_index: prepared.len(),
                }),
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
            PreparedArg::Expr { expr, .. } => self.compile_expr(expr),
            _ => {
                Err("internal error: packed argument item compiled as standalone value".to_string())
            }
        }
    }

    fn compile_prepared_args_for_params(
        &mut self,
        call_name: &str,
        prepared_args: &[PreparedArg],
        params: &[Param],
        param_offset: usize,
    ) -> Result<Vec<Value>, String> {
        let mut values: Vec<Option<Value>> = vec![None; params.len()];
        for (i, param) in params.iter().enumerate() {
            if param.is_variadic {
                values[i] = Some(self.new_packed_args(&param.ty)?);
            } else if param.is_kw_variadic {
                values[i] = Some(self.new_packed_kwargs(&param.ty)?);
            }
        }
        for arg in prepared_args.iter() {
            let target_index = arg.target_index();
            match arg {
                PreparedArg::Expr { expr, .. } => {
                    let raw_val = self.compile_expr(expr)?;
                    let val = if let Some(param) = params.get(target_index) {
                        let actual_ty = self.normalize_bolide_type(&self.infer_expr_type(expr));
                        let mut val =
                            self.prepare_value_for_storage(raw_val, &actual_ty, &param.ty)?;
                        let param_index = target_index + param_offset;
                        let callee_expects_closure = self
                            .funcsig_closure_param_indices
                            .get(call_name)
                            .map(|indices| indices.contains(&param_index))
                            .unwrap_or(false);
                        if callee_expects_closure
                            && matches!(self.funcsig_expr_source(expr), FuncSigReturnSource::Raw)
                        {
                            if let BolideType::FuncSig(param_types, ret_type) = &param.ty {
                                val =
                                    self.wrap_raw_funcsig_as_closure(val, param_types, ret_type)?;
                            }
                        }
                        val
                    } else {
                        raw_val
                    };
                    values[target_index] = Some(val);
                }
                PreparedArg::PackedArgItem { elem_ty, item, .. } => {
                    let list_ptr = values[target_index]
                        .ok_or_else(|| "internal error: missing variadic container".to_string())?;
                    self.append_packed_arg_item(list_ptr, elem_ty, item)?;
                }
                PreparedArg::PackedKwargItem { value_ty, item, .. } => {
                    let dict_ptr = values[target_index]
                        .ok_or_else(|| "internal error: missing kwargs container".to_string())?;
                    self.append_packed_kwarg_item(dict_ptr, value_ty, item)?;
                }
            }
        }
        values
            .into_iter()
            .map(|value| {
                value.ok_or_else(|| "internal error: missing prepared argument".to_string())
            })
            .collect()
    }

    fn new_packed_args(&mut self, param_ty: &BolideType) -> Result<Value, String> {
        let list_new = *self
            .func_refs
            .get("@_list_new")
            .ok_or("list_new not found")?;
        let elem_ty = match param_ty {
            BolideType::List(inner) => inner.as_ref().clone(),
            _ => BolideType::Dynamic,
        };
        let elem_tag = Self::bolide_type_to_element_tag(&elem_ty);
        let elem_tag_val = self.builder.ins().iconst(types::I8, elem_tag as i64);
        let call = self.builder.ins().call(list_new, &[elem_tag_val]);
        let list_ptr = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(list_ptr, &BolideType::List(Box::new(elem_ty)));
        Ok(list_ptr)
    }

    fn append_packed_arg_item(
        &mut self,
        list_ptr: Value,
        elem_ty: &BolideType,
        item: &PackedArgItem,
    ) -> Result<(), String> {
        let list_push = *self
            .func_refs
            .get("@_list_push")
            .ok_or("list_push not found")?;
        let list_extend = *self
            .func_refs
            .get("@_list_extend")
            .ok_or("list_extend not found")?;
        let elem_tag = Self::bolide_type_to_element_tag(elem_ty);
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
        Ok(())
    }

    fn new_packed_kwargs(&mut self, param_ty: &BolideType) -> Result<Value, String> {
        let dict_new = *self
            .func_refs
            .get("@_dict_new")
            .ok_or("dict_new not found")?;
        let value_ty = match param_ty {
            BolideType::Dict(_, value) => value.as_ref().clone(),
            _ => BolideType::Dynamic,
        };
        let key_tag = self.builder.ins().iconst(types::I8, 3);
        let value_tag = self.builder.ins().iconst(
            types::I8,
            Self::bolide_type_to_element_tag(&value_ty) as i64,
        );
        let call = self.builder.ins().call(dict_new, &[key_tag, value_tag]);
        let dict_ptr = self.builder.inst_results(call)[0];
        let dict_ty = BolideType::Dict(Box::new(BolideType::Str), Box::new(value_ty));
        self.track_temp_rc_value(dict_ptr, &dict_ty);
        Ok(dict_ptr)
    }

    fn append_packed_kwarg_item(
        &mut self,
        dict_ptr: Value,
        value_ty: &BolideType,
        item: &PackedKwargItem,
    ) -> Result<(), String> {
        let dict_set = *self
            .func_refs
            .get("@_dict_set")
            .ok_or("dict_set not found")?;
        let dict_extend = *self
            .func_refs
            .get("@_dict_extend")
            .ok_or("dict_extend not found")?;
        match item {
            PackedKwargItem::Entry(name, expr) => {
                self.check_borrow_escape(expr, "**kwargs")?;
                let key = self.compile_expr(&Expr::String(name.clone()))?;
                let mut val = self.compile_expr(expr)?;
                if matches!(value_ty, BolideType::Dynamic) {
                    let actual_ty = self.infer_expr_type(expr);
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
        Ok(())
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
                let actual_ty = self.infer_expr_type(arg);
                if actual_ty != BolideType::Dynamic {
                    val = self.convert_to_dynamic(val, &actual_ty)?;
                }
            }
            if matches!(field_ty, BolideType::FuncSig(_, _) | BolideType::Func) {
                val = self.prepare_funcsig_for_container_storage(val, arg, &field_ty)?;
                if self.closure_temps.contains(&val) {
                    self.remove_temp_closure(val);
                } else {
                    self.emit_closure_retain(val);
                }
            } else if Self::is_rc_type(&field_ty) {
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

    /// 编译函数调用
    fn compile_call(&mut self, callee: &Expr, args: &[Expr]) -> Result<Value, String> {
        // Intercept 'print' for Dynamic type
        if let Expr::Ident(name) = callee {
            if name == "print" && args.len() == 1 {
                if self.infer_expr_type(&args[0]) == BolideType::Dynamic {
                    let func = *self
                        .func_refs
                        .get("@_print_dynamic")
                        .ok_or("print_dynamic not found")?;
                    let val = self.compile_expr(&args[0])?;
                    self.builder.ins().call(func, &[val]);
                    return Ok(self.builder.ins().iconst(types::I64, 0));
                }
            }
        }

        if let Expr::Member(base, variant_name) = callee {
            if let Expr::Ident(adt_name) = base.as_ref() {
                if self.adts.contains_key(adt_name) {
                    return self.compile_adt_variant(adt_name, variant_name, args);
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
            // 闭包变量 / 函数类型参数：走闭包 ABI
            if self.closure_vars.contains(name) || self.closure_param_vars.contains(name) {
                return self.compile_closure_call_ident(name, args);
            }
            // 检查是否是 func 类型的变量（先局部后全局）
            let var_type = self
                .var_types
                .get(name)
                .or_else(|| self.global_var_types.get(name))
                .cloned();
            if let Some(var_type) = var_type {
                match &var_type {
                    BolideType::Func => return self.compile_indirect_call(name, args, None),
                    BolideType::FuncSig(param_types, ret_type) => {
                        return self.compile_indirect_call(
                            name,
                            args,
                            Some((param_types.clone(), ret_type.clone())),
                        );
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
            // 任意 callee 表达式（fns[0](x) / getFn()(x) / make_adder(5)(10) 等）
            other => {
                // 闭包字面量直接调用：(fn(x){...})(3)
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
                if let BolideType::FuncSig(params, ret) = self.infer_expr_type(other) {
                    if self.expr_yields_raw_funcsig(other) {
                        let func_ptr = self.compile_expr(other)?;
                        return self.compile_indirect_call_ptr(func_ptr, args, Some((params, ret)));
                    }
                    let closure_val = self.compile_expr(other)?;
                    // 临时闭包在调用期间保持存活
                    let was_temp = self.closure_temps.contains(&closure_val);
                    if was_temp {
                        self.remove_temp_closure(closure_val);
                    }
                    let result = self.compile_closure_call_ptr(closure_val, args, &params, &ret)?;
                    // 调用结束后再释放（如果未被返回/吸收会在语句末释放）
                    if was_temp {
                        self.closure_temps.push(closure_val);
                    }
                    return Ok(result);
                }
                let func_ptr = self.compile_expr(other)?;
                let func_sig = match self.infer_expr_type(other) {
                    BolideType::FuncSig(p, r) => Some((p, r)),
                    _ => None,
                };
                return self.compile_indirect_call_ptr(func_ptr, args, func_sig);
            }
        };

        // 处理类型转换函数和特殊函数
        match func_name.as_str() {
            "int" => return self.compile_type_conversion_to_int(args),
            "float" => return self.compile_type_conversion_to_float(args),
            "str" => return self.compile_type_conversion_to_str(args),
            "bytes" => return self.compile_bytes_new(args),
            "bigint" => return self.compile_type_conversion_to_bigint(args),
            "decimal" => return self.compile_type_conversion_to_decimal(args),

            // 通用 print 函数 - 根据参数类型自动选择
            "print" => {
                if args.len() != 1 {
                    return Err("print expects 1 argument".to_string());
                }
                return self.compile_print(&args[0]);
            }
            // channel 函数 - 创建通道
            "channel" => {
                return self.compile_channel_create(args);
            }
            // bigint_debug_stats - 调试用
            "bigint_debug_stats" => {
                let func_ref = *self
                    .func_refs
                    .get("@_bigint_debug_stats")
                    .ok_or("bigint_debug_stats not found")?;
                self.builder.ins().call(func_ref, &[]);
                return Ok(self.builder.ins().iconst(types::I64, 0));
            }
            // tuple_debug_stats - 调试用
            "tuple_debug_stats" => {
                let func_ref = *self
                    .func_refs
                    .get("@_tuple_debug_stats")
                    .ok_or("tuple_debug_stats not found")?;
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

        let func_ref = *self
            .func_refs
            .get(&func_name)
            .ok_or_else(|| format!("Undefined function: {}", func_name))?;

        let prepared_args = self.prepare_call_args(&func_name, args)?;

        // 获取函数参数信息
        let param_modes: Vec<ParamMode> = self
            .func_params
            .get(&func_name)
            .map(|params| params.iter().map(|p| p.mode).collect())
            .unwrap_or_else(|| vec![ParamMode::Borrow; prepared_args.len()]);

        let params = self
            .func_params
            .get(&func_name)
            .cloned()
            .unwrap_or_default();
        let mut arg_values: Vec<Option<Value>> = vec![None; params.len()];
        for (i, param) in params.iter().enumerate() {
            if param.is_variadic {
                arg_values[i] = Some(self.new_packed_args(&param.ty)?);
            } else if param.is_kw_variadic {
                arg_values[i] = Some(self.new_packed_kwargs(&param.ty)?);
            }
        }
        let mut ref_writebacks: Vec<(usize, String, Option<Value>)> = Vec::new();

        for arg in prepared_args.iter() {
            let target_index = arg.target_index();
            let mode = param_modes
                .get(target_index)
                .copied()
                .unwrap_or(ParamMode::Borrow);

            match mode {
                ParamMode::Borrow => match arg {
                    PreparedArg::Expr { expr, .. } => {
                        let val = self.compile_expr(expr)?;
                        let target_ty = params.get(target_index).map(|param| param.ty.clone());
                        let val = if let Some(target_ty) = target_ty {
                            let actual_ty = self.normalize_bolide_type(&self.infer_expr_type(expr));
                            let mut val =
                                self.prepare_value_for_storage(val, &actual_ty, &target_ty)?;
                            let callee_expects_closure = self
                                .funcsig_closure_param_indices
                                .get(&func_name)
                                .map(|indices| indices.contains(&target_index))
                                .unwrap_or(false);
                            if callee_expects_closure
                                && matches!(
                                    self.funcsig_expr_source(expr),
                                    FuncSigReturnSource::Raw
                                )
                            {
                                if let BolideType::FuncSig(param_types, ret_type) = target_ty {
                                    val = self.wrap_raw_funcsig_as_closure(
                                        val,
                                        &param_types,
                                        &ret_type,
                                    )?;
                                }
                            }
                            val
                        } else {
                            val
                        };
                        arg_values[target_index] = Some(val);
                    }
                    PreparedArg::PackedArgItem { elem_ty, item, .. } => {
                        let list_ptr = arg_values[target_index].ok_or_else(|| {
                            "internal error: missing variadic container".to_string()
                        })?;
                        self.append_packed_arg_item(list_ptr, elem_ty, item)?;
                    }
                    PreparedArg::PackedKwargItem { value_ty, item, .. } => {
                        let dict_ptr = arg_values[target_index].ok_or_else(|| {
                            "internal error: missing kwargs container".to_string()
                        })?;
                        self.append_packed_kwarg_item(dict_ptr, value_ty, item)?;
                    }
                },
                ParamMode::Owned => {
                    // 传值，然后标记变量为已移动
                    let expr = arg.expr().ok_or_else(|| {
                        "owned parameter cannot receive packed arguments".to_string()
                    })?;
                    let raw_val = self.compile_expr(expr)?;
                    let target_ty = params.get(target_index).map(|param| param.ty.clone());
                    let val = if let Some(target_ty) = target_ty {
                        let actual_ty = self.normalize_bolide_type(&self.infer_expr_type(expr));
                        self.prepare_value_for_storage(raw_val, &actual_ty, &target_ty)?
                    } else {
                        raw_val
                    };
                    arg_values[target_index] = Some(val);

                    // 如果参数是变量，标记为已移动并置空
                    if let Expr::Ident(var_name) = expr {
                        self.moved_variables.insert(var_name.clone());
                        // 置空变量（设为 null）
                        if let Some(&var) = self.variables.get(var_name) {
                            let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                            self.builder.def_var(var, null_val);
                        } else if let Some(&data_id) = self.global_data_ids.get(var_name) {
                            let gv = self.module.declare_data_in_func(data_id, self.builder.func);
                            let addr = self.builder.ins().global_value(self.ptr_type, gv);
                            let null_val = self.builder.ins().iconst(self.ptr_type, 0);
                            self.builder.ins().store(MemFlags::new(), null_val, addr, 0);
                        }
                        // 从 rc_variables 中移除（不再需要在作用域结束时释放）
                        self.rc_variables.retain(|(n, _)| n != var_name);
                    } else {
                        // 临时值作为 Owned 参数，所有权转移，从临时列表移除
                        self.remove_temp_rc_value(val);
                    }
                }
                ParamMode::Ref => {
                    let expr = arg.expr().ok_or_else(|| {
                        "ref parameter cannot receive packed arguments".to_string()
                    })?;
                    // 传递变量的栈地址
                    if let Expr::Ident(var_name) = expr {
                        if let Some(&var) = self.variables.get(var_name) {
                            let current_val = self.builder.use_var(var);

                            // 创建栈槽存储变量值
                            let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                                StackSlotKind::ExplicitSlot,
                                8, // 指针大小
                                0,
                            ));
                            let slot_addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);

                            // 将当前值存入栈槽
                            self.builder
                                .ins()
                                .store(MemFlags::new(), current_val, slot_addr, 0);

                            arg_values[target_index] = Some(slot_addr);
                            ref_writebacks.push((target_index, var_name.clone(), None));

                            // 注意：函数返回后需要从栈槽读回新值
                            // 这需要在 call 之后处理
                        } else if let Some(&data_id) = self.global_data_ids.get(var_name) {
                            // 全局变量：直接传递其数据段地址，被调函数原地读写
                            let gv = self.module.declare_data_in_func(data_id, self.builder.func);
                            let addr = self.builder.ins().global_value(self.ptr_type, gv);
                            // 记录旧值，调用后由调用方释放（与局部变量语义一致）
                            let old_val =
                                self.builder
                                    .ins()
                                    .load(self.ptr_type, MemFlags::new(), addr, 0);
                            arg_values[target_index] = Some(addr);
                            ref_writebacks.push((target_index, var_name.clone(), Some(old_val)));
                        } else {
                            return Err(format!("Undefined variable for ref: {}", var_name));
                        }
                    } else {
                        return Err("ref parameter must be a variable".to_string());
                    }
                }
            }
        }

        let arg_values: Vec<Value> = arg_values
            .into_iter()
            .map(|value| {
                value.ok_or_else(|| "internal error: missing prepared argument".to_string())
            })
            .collect::<Result<_, _>>()?;
        let call = self.builder.ins().call(func_ref, &arg_values);
        self.emit_exception_pending_check()?;

        // 检查是否是生命周期函数
        let is_lifetime_func = self.lifetime_funcs.contains(&func_name);

        // 处理 Ref 参数：从栈槽读回新值
        // 对于生命周期函数，跳过释放旧值（因为返回值可能就是参数本身）
        for (i, var_name, old_global_value) in ref_writebacks {
            if let Some(old_val) = old_global_value {
                // 全局变量：被调函数已原地写入新值，这里释放调用前的旧值
                if !is_lifetime_func {
                    if let Some(var_ty) = self.global_var_types.get(&var_name).cloned() {
                        if Self::is_rc_type(&var_ty) {
                            if let Some(func_name) = Self::get_release_func_name(&var_ty) {
                                if let Some(&func_ref) = self.func_refs.get(func_name) {
                                    self.builder.ins().call(func_ref, &[old_val]);
                                }
                            }
                        }
                    }
                }
                continue;
            }

            // arg_values[i] 是栈槽地址，从中读取新值
            let slot_addr = arg_values[i];
            let new_val = self
                .builder
                .ins()
                .load(self.ptr_type, MemFlags::new(), slot_addr, 0);

            if let Some(&var) = self.variables.get(&var_name) {
                // 释放旧值（调用者原本拥有的对象）
                // 但对于生命周期函数，跳过释放（返回值可能就是参数本身）
                if !is_lifetime_func {
                    if let Some(var_ty) = self.var_types.get(&var_name).cloned() {
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
                    // 函数返回闭包对象：在调用点标记为闭包临时值，供变量吸收
                    if matches!(ret_ty, BolideType::FuncSig(_, _) | BolideType::Func)
                        && !self.direct_call_returns_raw_funcsig(&func_name, args)
                        && !self.funcsig_return_source_uses_param(&func_name)
                    {
                        self.closure_temps.push(result);
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
            BolideType::Int => Ok(val), // 恒等转换
            BolideType::Float => {
                // float -> int: 截断
                Ok(self.builder.ins().fcvt_to_sint(types::I64, val))
            }
            BolideType::Str => {
                // str -> int: 调用 string_to_int
                let func_ref = *self
                    .func_refs
                    .get("@_string_to_int")
                    .ok_or("string_to_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::BigInt => {
                // bigint -> int: 调用 bigint_to_i64
                let func_ref = *self
                    .func_refs
                    .get("@_bigint_to_i64")
                    .ok_or("bigint_to_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Decimal => {
                // decimal -> int: 调用 decimal_to_i64
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_to_i64")
                    .ok_or("decimal_to_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Dynamic => {
                let func_ref = *self
                    .func_refs
                    .get("@_dynamic_to_int")
                    .ok_or("dynamic_to_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Err(format!("Cannot convert {:?} to int", arg_type)),
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
            BolideType::Float => Ok(val), // 恒等转换
            BolideType::Int => {
                // int -> float
                Ok(self.builder.ins().fcvt_from_sint(types::F64, val))
            }
            BolideType::Str => {
                // str -> float: 调用 string_to_float
                let func_ref = *self
                    .func_refs
                    .get("@_string_to_float")
                    .ok_or("string_to_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Decimal => {
                // decimal -> float: 调用 decimal_to_f64
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_to_f64")
                    .ok_or("decimal_to_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Dynamic => {
                let func_ref = *self
                    .func_refs
                    .get("@_dynamic_to_float")
                    .ok_or("dynamic_to_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Err(format!("Cannot convert {:?} to float", arg_type)),
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
            BolideType::Str => return Ok(val), // 恒等转换
            BolideType::Int => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_int")
                    .ok_or("string_from_int not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::Float => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_float")
                    .ok_or("string_from_float not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::Bool => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_bool")
                    .ok_or("string_from_bool not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::BigInt => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_bigint")
                    .ok_or("string_from_bigint not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::Decimal => {
                let func_ref = *self
                    .func_refs
                    .get("@_string_from_decimal")
                    .ok_or("string_from_decimal not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            BolideType::Dynamic => {
                let func_ref = *self
                    .func_refs
                    .get("@_dynamic_to_string")
                    .ok_or("dynamic_to_string not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                self.builder.inst_results(call)[0]
            }
            _ => return Err(format!("Cannot convert {:?} to str", arg_type)),
        };

        // 返回的字符串需要 RC 跟踪
        self.track_temp_rc_value(result, &BolideType::Str);
        Ok(result)
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

    /// 类型转换: bigint(x) - 支持 int
    fn compile_type_conversion_to_bigint(&mut self, args: &[Expr]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("bigint() expects 1 argument".to_string());
        }
        let arg_type = self.infer_expr_type(&args[0]);
        let val = self.compile_expr(&args[0])?;

        match arg_type {
            BolideType::BigInt => Ok(val), // 恒等转换
            BolideType::Int => {
                let func_ref = *self
                    .func_refs
                    .get("@_bigint_from_i64")
                    .ok_or("bigint_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::BigInt);
                Ok(result)
            }
            _ => Err(format!("Cannot convert {:?} to bigint", arg_type)),
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
            BolideType::Decimal => Ok(val), // 恒等转换
            BolideType::Int => {
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_from_i64")
                    .ok_or("decimal_from_i64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Decimal);
                Ok(result)
            }
            BolideType::Float => {
                let func_ref = *self
                    .func_refs
                    .get("@_decimal_from_f64")
                    .ok_or("decimal_from_f64 not found")?;
                let call = self.builder.ins().call(func_ref, &[val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Decimal);
                Ok(result)
            }
            _ => Err(format!("Cannot convert {:?} to decimal", arg_type)),
        }
    }

    /// 编译通用 print 函数 - 根据表达式类型自动选择打印函数
    fn compile_print(&mut self, expr: &Expr) -> Result<Value, String> {
        let expr_type = self.infer_expr_type(expr);
        let val = self.compile_expr(expr)?;

        if let BolideType::Tuple(elem_types) = &expr_type {
            self.compile_print_tuple_inline(val, elem_types)?;
            let println_ref = *self.func_refs.get("@_println").ok_or("println not found")?;
            self.builder.ins().call(println_ref, &[]);
            return Ok(self.builder.ins().iconst(types::I64, 0));
        }

        // 容器索引 / 元组元素取出时值为 i64，Float 需 bitcast 回 f64
        let val = if matches!(expr_type, BolideType::Float) {
            self.builder.ins().bitcast(types::F64, MemFlags::new(), val)
        } else {
            val
        };

        let func_name = match expr_type {
            BolideType::Int => "@_print_int",
            BolideType::Float => "@_print_float",
            BolideType::Bool => "@_print_bool",
            BolideType::BigInt => "@_print_bigint",
            BolideType::Decimal => "@_print_decimal",
            BolideType::Str => "@_print_string",
            BolideType::Bytes => "@_print_bytes",
            BolideType::Dynamic => "@_print_dynamic",
            BolideType::Tuple(_) => "@_print_tuple",
            BolideType::List(_) => "@_print_list",
            BolideType::Dict(_, _) => "@_print_dict",

            _ => "@_print_int", // 默认用 int 打印
        };

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

    /// 编译 input 函数 - 读取用户输入
    fn compile_input(&mut self, args: &[Expr]) -> Result<Value, String> {
        let result = if args.is_empty() {
            // 无参数版本: input()
            let func_ref = *self.func_refs.get("@_input").ok_or("input not found")?;
            let call = self.builder.ins().call(func_ref, &[]);
            self.builder.inst_results(call)[0]
        } else if args.len() == 1 {
            // 带提示版本: input("prompt")
            let prompt = self.compile_expr(&args[0])?;
            let func_ref = *self
                .func_refs
                .get("@_input_prompt")
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
            Expr::Spawn(_, _) | Expr::SpawnThread(_, _) => BolideType::Future,
            Expr::Closure {
                params,
                return_type,
                ..
            } => BolideType::FuncSig(
                params.iter().map(|p| p.ty.clone()).collect(),
                return_type.clone().map(Box::new),
            ),
            Expr::Ident(name) => {
                // 查找局部变量类型
                if let Some(ty) = self.var_types.get(name) {
                    return ty.clone();
                }
                // 查找全局变量类型
                if let Some(ty) = self.global_var_types.get(name) {
                    return ty.clone();
                }
                // 裸函数名作为值：合成 FuncSig（一等函数支持）
                if self.func_refs.contains_key(name) {
                    if let Some(params) = self.func_params.get(name) {
                        let param_types: Vec<BolideType> =
                            params.iter().map(|p| p.ty.clone()).collect();
                        let ret = self
                            .func_return_types
                            .get(name)
                            .cloned()
                            .flatten()
                            .map(Box::new);
                        return BolideType::FuncSig(param_types, ret);
                    }
                    return BolideType::Func;
                }
                BolideType::Int
            }
            Expr::BinOp(left, op, right) => {
                let left_ty = self.infer_expr_type(left);
                let right_ty = self.infer_expr_type(right);
                // 类型提升规则
                match (&left_ty, &right_ty) {
                    (BolideType::Str, BolideType::Str) => match op {
                        BinOp::Add => BolideType::Str,
                        BinOp::Eq | BinOp::Ne => BolideType::Bool,
                        _ => BolideType::Int,
                    },
                    (BolideType::Dynamic, _) | (_, BolideType::Dynamic) => match op {
                        BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                        | BinOp::And
                        | BinOp::Or => BolideType::Bool,
                        _ => BolideType::Dynamic,
                    },
                    (BolideType::Float, _) | (_, BolideType::Float) => BolideType::Float,
                    (BolideType::BigInt, _) | (_, BolideType::BigInt) => BolideType::BigInt,
                    (BolideType::Decimal, _) | (_, BolideType::Decimal) => BolideType::Decimal,
                    _ => match op {
                        BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                        | BinOp::And
                        | BinOp::Or => BolideType::Bool,
                        _ => BolideType::Int,
                    },
                }
            }
            Expr::UnaryOp(op, operand) => match op {
                UnaryOp::Not => BolideType::Bool,
                UnaryOp::Neg => self.infer_expr_type(operand),
            },
            Expr::Call(callee, args) => {
                if let Expr::Member(base, variant_name) = callee.as_ref() {
                    if let Expr::Ident(adt_name) = base.as_ref() {
                        if let Some(adt_info) = self.adts.get(adt_name) {
                            if let Some(variant) =
                                adt_info.variants.iter().find(|v| v.name == *variant_name)
                            {
                                let type_args = self.infer_adt_type_args(adt_info, variant, args);
                                return BolideType::Adt(adt_name.clone(), type_args);
                            }
                        }
                    }
                }
                // 根据函数名推断返回类型
                if let Expr::Ident(name) = callee.as_ref() {
                    match name.as_str() {
                        "bigint" => BolideType::BigInt,
                        "decimal" => BolideType::Decimal,
                        "int" => BolideType::Int,
                        "float" => BolideType::Float,
                        "str" => BolideType::Str, // str 函数返回字符串
                        "bytes" => BolideType::Bytes,
                        "channel" => BolideType::Channel(Box::new(BolideType::Int)), // 默认 int，实际类型从声明获取
                        "input" => BolideType::Str, // input 函数返回字符串
                        _ => {
                            // 查找用户定义函数的返回类型
                            if let Some(Some(ret_ty)) = self.func_return_types.get(name.as_str()) {
                                ret_ty.clone()
                            } else if let Some(BolideType::FuncSig(_, Some(ret))) =
                                self.var_types.get(name.as_str())
                            {
                                // 函数指针变量（间接调用）：取签名中的返回类型
                                (**ret).clone()
                            } else if let Some(BolideType::FuncSig(_, Some(ret))) =
                                self.global_var_types.get(name.as_str())
                            {
                                // 全局函数指针变量（闭包）
                                (**ret).clone()
                            } else if let Some((_, extern_func)) =
                                self.extern_funcs.get(name.as_str())
                            {
                                extern_func
                                    .return_type
                                    .as_ref()
                                    .map(Self::extern_return_type_to_bolide)
                                    .unwrap_or(BolideType::Int)
                            } else {
                                BolideType::Int
                            }
                        }
                    }
                } else if let Expr::Member(base, method) = callee.as_ref() {
                    if let Expr::Ident(module_name) = base.as_ref() {
                        if self.modules.contains_key(module_name) {
                            let func_name = format!("@{}_{}", module_name, method);
                            if let Some(Some(ret_ty)) = self.func_return_types.get(&func_name) {
                                return ret_ty.clone();
                            }
                        }
                    }

                    let base_ty = self.infer_expr_type(base);
                    match base_ty {
                        BolideType::Dict(k, v) => match method.as_str() {
                            "keys" => BolideType::List(k),
                            "values" => BolideType::List(v),
                            "get" | "remove" => *v,
                            "clone" => BolideType::Dict(k, v),
                            "len" | "is_empty" | "contains" => BolideType::Int,
                            _ => BolideType::Int,
                        },
                        BolideType::List(elem) => match method.as_str() {
                            "pop" | "get" | "first" | "last" => *elem,
                            "slice" | "copy" | "clone" | "filter" => BolideType::List(elem),
                            // map: 元素类型 = 回调返回类型（取不到则沿用源元素类型）
                            "map" => {
                                let ret = args
                                    .first()
                                    .and_then(|a| self.func_ptr_return_type(a))
                                    .unwrap_or(*elem);
                                BolideType::List(Box::new(ret))
                            }
                            "len" | "index_of" | "count" | "is_empty" => BolideType::Int,
                            _ => BolideType::Int,
                        },
                        BolideType::Str => match method.as_str() {
                            // 返回新串的方法
                            "upper" | "lower" | "trim" | "strip" | "replace" | "repeat"
                            | "substring" | "char_at" => BolideType::Str,
                            // 返回 list<str>
                            "split" => BolideType::List(Box::new(BolideType::Str)),
                            // 其余（len/find/contains/starts_with/ends_with/count...）返回 int
                            _ => BolideType::Int,
                        },
                        BolideType::Bytes => match method.as_str() {
                            "copy" | "clone" => BolideType::Bytes,
                            "to_string_lossy" => BolideType::Str,
                            _ => BolideType::Int,
                        },
                        // 用户类方法：沿继承链查方法返回类型
                        BolideType::Custom(class_name) => self
                            .lookup_method_return_type(&class_name, method)
                            .unwrap_or(BolideType::Int),
                        _ => BolideType::Int,
                    }
                } else {
                    BolideType::Int
                }
            }
            Expr::Member(base, member) => {
                // 获取基础表达式的类型，然后查找字段类型
                let base_ty = self.infer_expr_type(base);
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
                            return field.ty.clone();
                        }
                    }
                }
                BolideType::Int
            }
            Expr::Tuple(exprs) => {
                let elem_types: Vec<BolideType> =
                    exprs.iter().map(|e| self.infer_expr_type(e)).collect();
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
                    BolideType::Bytes => BolideType::Int,
                    // 字符串索引按码点，返回单码点新串
                    BolideType::Str => BolideType::Str,
                    _ => BolideType::Int,
                }
            }
            Expr::Slice(base, _, _, _) => {
                // 切片保持容器类型：Str->Str, List(e)->List(e), Tuple->Tuple
                let base_ty = self.infer_expr_type(base);
                match base_ty {
                    BolideType::Str => BolideType::Str,
                    BolideType::List(_) | BolideType::Tuple(_) => base_ty,
                    _ => base_ty,
                }
            }
            Expr::Await(inner_expr) => {
                // await 表达式返回协程的返回类型
                self.infer_awaited_type(inner_expr)
            }
            Expr::SpawnAll(exprs) => {
                let elem_types = exprs
                    .iter()
                    .map(|e| self.spawn_item_type(e).unwrap_or(BolideType::Int))
                    .collect();
                BolideType::Tuple(elem_types)
            }
            Expr::Propagate(inner) | Expr::Raise(inner) => {
                match self.normalize_bolide_type(&self.infer_expr_type(inner)) {
                    BolideType::Adt(name, args)
                        if (name == "Result" || name == "Option") && !args.is_empty() =>
                    {
                        args[0].clone()
                    }
                    _ => BolideType::Int,
                }
            }
            Expr::TryExpr(body) => {
                let ok_ty = body
                    .last()
                    .and_then(|stmt| match stmt {
                        Statement::Expr(expr) => Some(self.infer_expr_type(expr)),
                        _ => None,
                    })
                    .unwrap_or(BolideType::Int);
                BolideType::Adt(
                    "Result".to_string(),
                    vec![ok_ty, BolideType::Custom("Error".to_string())],
                )
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
            Expr::ListComprehension { .. } => BolideType::List(Box::new(BolideType::Dynamic)),
            Expr::Dict(entries) => {
                let (k_type, v_type) = if entries.is_empty() {
                    (BolideType::Int, BolideType::Int)
                } else {
                    let mut k_ty = self.infer_expr_type(&entries[0].0);
                    let mut v_ty = self.infer_expr_type(&entries[0].1);
                    for (k, v) in entries.iter().skip(1) {
                        let next_k = self.infer_expr_type(k);
                        if k_ty != next_k {
                            k_ty = BolideType::Dynamic;
                        }
                        let next_v = self.infer_expr_type(v);
                        if v_ty != next_v {
                            v_ty = BolideType::Dynamic;
                        }
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
            BolideType::Bytes => self.ptr_type,
            BolideType::BigInt => self.ptr_type,
            BolideType::Decimal => self.ptr_type,
            BolideType::Dynamic => self.ptr_type,
            BolideType::Ptr => self.ptr_type,
            BolideType::Channel(_) => self.ptr_type,
            BolideType::Future => self.ptr_type,
            BolideType::Func => self.ptr_type,          // 函数指针
            BolideType::FuncSig(_, _) => self.ptr_type, // 带签名的函数指针
            BolideType::List(_) => self.ptr_type,
            BolideType::Dict(_, _) => self.ptr_type,
            BolideType::Tuple(_) => self.ptr_type, // 元组作为指针
            BolideType::Generic(_) => self.ptr_type,
            BolideType::Adt(_, _) => self.ptr_type,

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
        let pool_create_ref = *self
            .func_refs
            .get("@_pool_create")
            .ok_or("pool_create not found")?;
        let call = self.builder.ins().call(pool_create_ref, &[size]);
        let pool_ptr = self.builder.inst_results(call)[0];

        // 进入线程池上下文: pool_enter(pool)
        let pool_enter_ref = *self
            .func_refs
            .get("@_pool_enter")
            .ok_or("pool_enter not found")?;
        self.builder.ins().call(pool_enter_ref, &[pool_ptr]);

        // 编译 pool 块内的语句
        for stmt in &pool_stmt.body {
            self.compile_stmt(stmt)?;
        }

        // 退出线程池上下文: pool_exit()
        let pool_exit_ref = *self
            .func_refs
            .get("@_pool_exit")
            .ok_or("pool_exit not found")?;
        self.builder.ins().call(pool_exit_ref, &[]);

        // 销毁线程池: pool_destroy(pool)
        let pool_destroy_ref = *self
            .func_refs
            .get("@_pool_destroy")
            .ok_or("pool_destroy not found")?;
        self.builder.ins().call(pool_destroy_ref, &[pool_ptr]);

        Ok(())
    }

    /// 编译 send 语句: ch <- value
    /// 编译通道方法调用: ch.send(v) / ch.recv()
    /// base 求值得到 channel 指针，inner 为通道元素类型。
    fn compile_channel_method_call(
        &mut self,
        base: &Expr,
        method_name: &str,
        args: &[Expr],
        inner: BolideType,
    ) -> Result<Value, String> {
        match method_name {
            "send" => {
                if args.len() != 1 {
                    return Err(format!(
                        "channel.send expects 1 argument, got {}",
                        args.len()
                    ));
                }
                // from 借用检查：借用值禁止通过通道逃逸
                self.check_borrow_escape(&args[0], "channel send")?;

                let channel_ptr = self.compile_expr(base)?;
                let value = self.compile_expr(&args[0])?;

                // 元素是 RC 类型时先 retain 再发送，确保接收方拿到活指针
                // （不受发送方释放影响）
                let send_value = if Self::is_rc_type(&inner) {
                    if let Some(clone_func) = Self::get_clone_func_name(&inner) {
                        if let Some(&func_ref) = self.func_refs.get(clone_func) {
                            let call = self.builder.ins().call(func_ref, &[value]);
                            self.builder.inst_results(call)[0]
                        } else {
                            value
                        }
                    } else {
                        value
                    }
                } else {
                    value
                };

                // 调用 channel_send(channel, value)
                let channel_send_ref = *self
                    .func_refs
                    .get("@_channel_send")
                    .ok_or("channel_send not found")?;
                self.builder
                    .ins()
                    .call(channel_send_ref, &[channel_ptr, send_value]);

                // send 在表达式语句中使用，返回占位值
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "recv" => {
                if !args.is_empty() {
                    return Err(format!(
                        "channel.recv expects 0 arguments, got {}",
                        args.len()
                    ));
                }
                let channel_ptr = self.compile_expr(base)?;

                // 调用 channel_recv(channel) -> i64
                let channel_recv_ref = *self
                    .func_refs
                    .get("@_channel_recv")
                    .ok_or("channel_recv not found")?;
                let call = self.builder.ins().call(channel_recv_ref, &[channel_ptr]);
                let value = self.builder.inst_results(call)[0];

                // 元素是 RC 类型时，接收方接管发送方 retain 的引用
                if Self::is_rc_type(&inner) {
                    self.track_temp_rc_value(value, &inner);
                }
                Ok(value)
            }
            _ => Err(format!("Unknown Channel method: {}", method_name)),
        }
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
        let stack_slot =
            self.builder
                .create_sized_stack_slot(cranelift::prelude::StackSlotData::new(
                    cranelift::prelude::StackSlotKind::ExplicitSlot,
                    array_size as u32,
                    0,
                ));
        let array_ptr = self.builder.ins().stack_addr(self.ptr_type, stack_slot, 0);

        // 填充 channel 指针数组
        for (i, (_, channel_name, _)) in recv_branches.iter().enumerate() {
            let channel_ptr = self
                .load_var_value(channel_name)
                .map_err(|_| format!("Undefined channel: {}", channel_name))?;
            let offset = (i * 8) as i32;
            self.builder
                .ins()
                .store(MemFlags::new(), channel_ptr, array_ptr, offset);
        }

        // 分配接收值的栈空间
        let value_slot =
            self.builder
                .create_sized_stack_slot(cranelift::prelude::StackSlotData::new(
                    cranelift::prelude::StackSlotKind::ExplicitSlot,
                    8,
                    0,
                ));
        let value_ptr = self.builder.ins().stack_addr(self.ptr_type, value_slot, 0);

        // 确定 timeout 值
        let timeout_val = if default_branch.is_some() {
            self.builder.ins().iconst(types::I64, -2) // has default
        } else if let Some((duration_expr, _)) = &timeout_branch {
            self.compile_expr(duration_expr)?
        } else {
            self.builder.ins().iconst(types::I64, -1) // no timeout
        };

        // 调用 bolide_channel_select
        let select_ref = *self
            .func_refs
            .get("@_channel_select")
            .ok_or("channel_select not found")?;
        let count_val = self.builder.ins().iconst(types::I64, channel_count as i64);
        let call = self
            .builder
            .ins()
            .call(select_ref, &[array_ptr, count_val, timeout_val, value_ptr]);
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
            let received_val = self
                .builder
                .ins()
                .load(types::I64, MemFlags::new(), value_ptr, 0);

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
        if let Some(block) = default_block {
            let default_val = self.builder.ins().iconst(types::I64, -2);
            let is_default = self
                .builder
                .ins()
                .icmp(IntCC::Equal, selected_idx, default_val);
            self.builder
                .ins()
                .brif(is_default, block, &[], exit_block, &[]);
        } else {
            self.builder.ins().jump(exit_block, &[]);
        }

        Ok(())
    }

    /// 编译 spawn 表达式
    fn compile_spawn(
        &mut self,
        func_name: &str,
        args: &[Expr],
        force_thread: bool,
    ) -> Result<Value, String> {
        let prepared_args = self.prepare_call_args(func_name, args)?;

        // 获取目标函数的返回类型，确定 spawn 函数后缀
        let return_type = self
            .func_return_types
            .get(func_name)
            .cloned()
            .unwrap_or(None);
        let type_suffix = match &return_type {
            Some(BolideType::Float) => "_float",
            Some(BolideType::Str)
            | Some(BolideType::BigInt)
            | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic)
            | Some(BolideType::Ptr)
            | Some(BolideType::List(_))
            | Some(BolideType::Tuple(_))
            | Some(BolideType::Custom(_)) => "_ptr",
            _ => "_int", // Int, Bool, None 都用 int
        };

        // 根据是否有参数选择不同的路径
        let (func_addr, env_ptr) = if prepared_args.is_empty() {
            // 无参数：直接使用目标函数
            let target_func_ref = *self
                .func_refs
                .get(func_name)
                .ok_or_else(|| format!("Undefined function: {}", func_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, target_func_ref);
            let null_ptr = self.builder.ins().iconst(self.ptr_type, 0);
            (func_addr, null_ptr)
        } else {
            // 有参数：使用 trampoline
            let trampoline_ref = *self
                .trampoline_refs
                .get(func_name)
                .ok_or_else(|| format!("No trampoline for function: {}", func_name))?;
            let param_types = self
                .trampoline_param_types
                .get(func_name)
                .ok_or_else(|| format!("No param types for trampoline: {}", func_name))?
                .clone();
            let env_size = *self
                .trampoline_env_sizes
                .get(func_name)
                .ok_or_else(|| format!("No env size for trampoline: {}", func_name))?;

            // 分配 env 内存
            let alloc_ref = *self
                .func_refs
                .get("@_bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(alloc_call)[0];

            let params = self.func_params.get(func_name).cloned().unwrap_or_default();
            let arg_values =
                self.compile_prepared_args_for_params(func_name, &prepared_args, &params, 0)?;

            // 将参数存储到 env
            // 对于 RC 类型，需要 clone 后传给子线程（跨线程安全）
            for (target_index, val) in arg_values.into_iter().enumerate() {
                let offset = (target_index * 8) as i32;
                let bolide_type = &param_types[target_index];

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

                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val_to_store, env_ptr, offset);
            }

            // 获取 trampoline 函数地址
            let func_addr = self.builder.ins().func_addr(self.ptr_type, trampoline_ref);
            (func_addr, env_ptr)
        };

        // 根据是否有参数选择 spawn 函数
        let spawn_suffix = if prepared_args.is_empty() {
            type_suffix.to_string()
        } else {
            format!("{}_with_env", type_suffix)
        };

        if force_thread {
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
            return Ok(self.builder.inst_results(thread_call)[0]);
        }

        // 检查是否在线程池上下文中
        let pool_is_active_ref = *self
            .func_refs
            .get("@_pool_is_active")
            .ok_or("pool_is_active not found")?;
        let is_active_call = self.builder.ins().call(pool_is_active_ref, &[]);
        let is_active = self.builder.inst_results(is_active_call)[0];

        // 创建分支块
        let pool_block = self.builder.create_block();
        let thread_block = self.builder.create_block();
        let merge_block = self.builder.create_block();
        self.builder.append_block_param(merge_block, self.ptr_type);

        self.builder
            .ins()
            .brif(is_active, pool_block, &[], thread_block, &[]);

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
        let result_handle = self.builder.block_params(merge_block)[0];

        Ok(result_handle)
    }

    /// 编译 async 函数调用 - 启动协程并返回 Future
    fn compile_async_call(&mut self, func_name: &str, args: &[Expr]) -> Result<Value, String> {
        let prepared_args = self.prepare_call_args(func_name, args)?;

        // 获取返回类型确定 spawn 函数后缀
        let return_type = self
            .func_return_types
            .get(func_name)
            .cloned()
            .unwrap_or(None);
        let type_suffix = match &return_type {
            Some(BolideType::Float) => "_float",
            Some(BolideType::Str)
            | Some(BolideType::BigInt)
            | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic)
            | Some(BolideType::Ptr)
            | Some(BolideType::List(_))
            | Some(BolideType::Custom(_)) => "_ptr",
            _ => "_int",
        };

        // 获取函数地址和环境指针
        let (func_addr, env_ptr) = if prepared_args.is_empty() {
            let target_func_ref = *self
                .func_refs
                .get(func_name)
                .ok_or_else(|| format!("Undefined async function: {}", func_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, target_func_ref);
            let null_ptr = self.builder.ins().iconst(self.ptr_type, 0);
            (func_addr, null_ptr)
        } else {
            // 有参数：使用 trampoline
            let trampoline_ref = *self
                .trampoline_refs
                .get(func_name)
                .ok_or_else(|| format!("No trampoline for async function: {}", func_name))?;
            let param_types = self
                .trampoline_param_types
                .get(func_name)
                .ok_or_else(|| format!("No param types for trampoline: {}", func_name))?
                .clone();
            let env_size = *self
                .trampoline_env_sizes
                .get(func_name)
                .ok_or_else(|| format!("No env size for trampoline: {}", func_name))?;

            // 分配 env 内存
            let alloc_ref = *self
                .func_refs
                .get("@_bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(alloc_call)[0];

            let params = self.func_params.get(func_name).cloned().unwrap_or_default();
            let arg_values =
                self.compile_prepared_args_for_params(func_name, &prepared_args, &params, 0)?;

            // 存储参数到 env
            for (target_index, val) in arg_values.into_iter().enumerate() {
                let offset = (target_index * 8) as i32;
                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val, env_ptr, offset);
            }

            let func_addr = self.builder.ins().func_addr(self.ptr_type, trampoline_ref);
            (func_addr, env_ptr)
        };

        // 调用 coroutine_spawn_* 启动协程
        let (spawn_func_name, call) = if prepared_args.is_empty() {
            let spawn_func_name = format!("@_coroutine_spawn{}", type_suffix);
            let spawn_ref = *self
                .func_refs
                .get(&spawn_func_name)
                .ok_or_else(|| format!("{} not found", spawn_func_name))?;
            let call = self.builder.ins().call(spawn_ref, &[func_addr]);
            (spawn_func_name, call)
        } else {
            let spawn_func_name = format!("@_coroutine_spawn{}_with_env", type_suffix);
            let spawn_ref = *self
                .func_refs
                .get(&spawn_func_name)
                .ok_or_else(|| format!("{} not found", spawn_func_name))?;
            let call = self.builder.ins().call(spawn_ref, &[func_addr, env_ptr]);
            (spawn_func_name, call)
        };
        let _ = spawn_func_name; // 避免警告
        let future_ptr = self.builder.inst_results(call)[0];

        // 注册 Future 到当前 scope（如果在 scope 内）
        let scope_register = *self
            .func_refs
            .get("@_scope_register")
            .ok_or("scope_register not found")?;
        self.builder.ins().call(scope_register, &[future_ptr]);

        Ok(future_ptr)
    }

    /// 编译 await 表达式
    fn compile_await(&mut self, inner_expr: &Expr) -> Result<Value, String> {
        if matches!(inner_expr, Expr::Spawn(_, _) | Expr::SpawnThread(_, _))
            || matches!(inner_expr, Expr::Ident(name) if self.task_func_map.contains_key(name))
        {
            return self.compile_task_await(inner_expr);
        }

        // 编译内部表达式，应该返回 Future 指针
        let future_ptr = self.compile_expr(inner_expr)?;

        // 获取协程的返回类型（不是 Future 类型，而是 await 后的结果类型）
        let await_expr = Expr::Await(Box::new(inner_expr.clone()));
        let expr_type = self.infer_expr_type(&await_expr);

        let await_func_name = match &expr_type {
            BolideType::Float => "@_coroutine_await_float",
            BolideType::Str
            | BolideType::BigInt
            | BolideType::Decimal
            | BolideType::List(_)
            | BolideType::Custom(_) => "@_coroutine_await_ptr",
            _ => "@_coroutine_await_int",
        };

        let await_ref = *self
            .func_refs
            .get(await_func_name)
            .ok_or_else(|| format!("{} not found", await_func_name))?;

        let call = self.builder.ins().call(await_ref, &[future_ptr]);
        let result = self.builder.inst_results(call)[0];

        // 释放 Future
        let free_ref = *self
            .func_refs
            .get("@_coroutine_free")
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

        // from 借用检查：借用值禁止存入元组
        for expr in exprs {
            self.check_borrow_escape(expr, "tuple literal")?;
        }

        // 收集元素类型，构建 Tuple 类型
        let mut elem_types = Vec::new();
        for expr in exprs {
            elem_types.push(self.infer_expr_type(expr));
        }
        let tuple_type = BolideType::Tuple(elem_types.clone());

        // 收集类型标签数组（u8 数组地址）传给 tuple_new_typed
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
                BolideType::FuncSig(_, _) => 11,
                _ => 7, // Ptr / Custom / Tuple / Future
            })
            .collect();

        // 分配栈上缓冲区存类型标签数组
        let tags_slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            tag_bytes.len() as u32,
            1,
        ));
        let tags_ptr = self.builder.ins().stack_addr(self.ptr_type, tags_slot, 0);
        // 逐个写入标签字节
        for (i, &b) in tag_bytes.iter().enumerate() {
            let byte_val = self.builder.ins().iconst(types::I8, b as i64);
            let addr = self.builder.ins().iadd_imm(tags_ptr, i as i64);
            self.builder.ins().store(MemFlags::new(), byte_val, addr, 0);
        }

        // 调用 tuple_new_typed 创建元组（类型感知）
        let tuple_new = *self
            .func_refs
            .get("@_tuple_new_typed")
            .ok_or("tuple_new_typed not found")?;
        let len = self.builder.ins().iconst(types::I64, exprs.len() as i64);
        let call = self.builder.ins().call(tuple_new, &[len, tags_ptr]);
        let tuple_ptr = self.builder.inst_results(call)[0];

        // 编译并设置每个元素（用 tuple_set_typed）
        let tuple_set = *self
            .func_refs
            .get("@_tuple_set_typed")
            .ok_or("tuple_set_typed not found")?;
        for (i, expr) in exprs.iter().enumerate() {
            let mut val = self.compile_expr(expr)?;
            let ty = self.infer_expr_type(expr);
            val = self.prepare_funcsig_for_container_storage(val, expr, &ty)?;

            let val_to_store = if matches!(ty, BolideType::FuncSig(_, _)) {
                if self.closure_temps.contains(&val) {
                    self.remove_temp_closure(val);
                } else {
                    self.emit_closure_retain(val);
                }
                val
            } else if Self::is_rc_type(&ty) {
                let is_temp = self.temp_rc_values.iter().any(|(v, _)| *v == val);
                if is_temp {
                    self.remove_temp_rc_value(val);
                    val
                } else {
                    if let Some(clone_func) = Self::get_clone_func_name(&ty) {
                        if let Some(&clone_ref) = self.func_refs.get(clone_func) {
                            let call = self.builder.ins().call(clone_ref, &[val]);
                            self.builder.inst_results(call)[0]
                        } else {
                            val
                        }
                    } else {
                        val
                    }
                }
            } else if matches!(ty, BolideType::Float) {
                // Float: bitcast f64 -> i64，存入 i64 槽
                self.builder.ins().bitcast(types::I64, MemFlags::new(), val)
            } else {
                val
            };

            let idx = self.builder.ins().iconst(types::I64, i as i64);
            let tag = self.builder.ins().iconst(types::I8, tag_bytes[i] as i64);
            self.builder
                .ins()
                .call(tuple_set, &[tuple_ptr, idx, val_to_store, tag]);
        }

        // 标记 Tuple 本身为临时 RC 值
        self.track_temp_rc_value(tuple_ptr, &tuple_type);

        Ok(tuple_ptr)
    }

    /// 编译列表字面量 [a, b, c]
    fn compile_list(&mut self, items: &[Expr]) -> Result<Value, String> {
        self.compile_list_with_hint(items, None)
    }

    /// hint 提供类型标注指定的元素类型（用于空列表或未初始化列表）
    fn compile_list_with_hint(
        &mut self,
        items: &[Expr],
        hint: Option<&BolideType>,
    ) -> Result<Value, String> {
        // from 借用检查：借用值禁止存入列表
        for item in items {
            self.check_borrow_escape(item, "list literal")?;
        }

        // 确定元素类型：优先用标注，否则从元素推断
        let elem_ty = if let Some(BolideType::List(inner)) = hint {
            inner.as_ref().clone()
        } else if items.is_empty() {
            BolideType::Int
        } else {
            self.infer_expr_type(&items[0])
        };
        let elem_type = Self::bolide_type_to_element_tag(&elem_ty);

        // 调用 list_new(elem_type) 创建列表
        let list_new = *self
            .func_refs
            .get("@_list_new")
            .ok_or("list_new not found")?;
        let elem_type_val = self.builder.ins().iconst(types::I8, elem_type as i64);
        let call = self.builder.ins().call(list_new, &[elem_type_val]);
        let list_ptr = self.builder.inst_results(call)[0];

        // 编译并添加每个元素
        let list_push = *self
            .func_refs
            .get("@_list_push")
            .ok_or("list_push not found")?;
        // Float 元素：list_push 形参是 i64，需将 f64 按位重解释为 i64 存入
        let elem_is_float = elem_type == 1;
        for expr in items {
            let mut val = self.compile_expr(expr)?;
            val = self.prepare_funcsig_for_container_storage(val, expr, &elem_ty)?;
            if elem_is_float && self.builder.func.dfg.value_type(val) == types::F64 {
                val = self.builder.ins().bitcast(types::I64, MemFlags::new(), val);
            }
            self.builder.ins().call(list_push, &[list_ptr, val]);
        }

        self.track_temp_rc_value(list_ptr, &BolideType::List(Box::new(elem_ty)));
        Ok(list_ptr)
    }

    /// 将值转换为 Dynamic 类型 (Boxing)
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
            BolideType::Dynamic => return Ok(val), // Already dynamic
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
                self.emit_retain(val, ty).unwrap_or(val)
            }
        } else {
            val
        };
        let call = self.builder.ins().call(func, &[boxed_input]);
        let res = self.builder.inst_results(call)[0];
        self.track_temp_rc_value(res, &BolideType::Dynamic);
        Ok(res)
    }

    /// 编译字典字面量 {k: v, ...}
    fn compile_dict(&mut self, entries: &[(Expr, Expr)]) -> Result<Value, String> {
        // from 借用检查：借用值禁止存入字典
        for (k, v) in entries {
            self.check_borrow_escape(k, "dict literal")?;
            self.check_borrow_escape(v, "dict literal")?;
        }

        // 确定键和值类型 (需扫描所有元素以处理 Dynamic)
        let (key_type_tag, val_type_tag, key_final_ty, val_final_ty) = if entries.is_empty() {
            (0u8, 0u8, BolideType::Int, BolideType::Int) // default int: int
        } else {
            // 第一次扫描：推断统一类型 (Dynamic or specific)
            let mut k_final_ty = self.infer_expr_type(&entries[0].0);
            let mut v_final_ty = self.infer_expr_type(&entries[0].1);

            for (k, v) in entries.iter().skip(1) {
                let next_k = self.infer_expr_type(k);
                if k_final_ty != next_k {
                    k_final_ty = BolideType::Dynamic;
                }
                let next_v = self.infer_expr_type(v);
                if v_final_ty != next_v {
                    v_final_ty = BolideType::Dynamic;
                }
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
                    BolideType::Bytes => 10,
                    BolideType::FuncSig(_, _) => 11,
                    _ => 7, // Ptr / Custom / Tuple / Future
                }
            };
            (
                map_tag(&k_final_ty),
                map_tag(&v_final_ty),
                k_final_ty,
                v_final_ty,
            )
        };

        // 创建字典
        let dict_new = *self
            .func_refs
            .get("@_dict_new")
            .ok_or("dict_new not found")?;
        let k_type_val = self.builder.ins().iconst(types::I8, key_type_tag as i64);
        let v_type_val = self.builder.ins().iconst(types::I8, val_type_tag as i64);
        let call = self.builder.ins().call(dict_new, &[k_type_val, v_type_val]);
        let dict_ptr = self.builder.inst_results(call)[0];

        // 设置元素
        let dict_set = *self
            .func_refs
            .get("@_dict_set")
            .ok_or("dict_set not found")?;

        for (key, val) in entries {
            let mut k_val = self.compile_expr(key)?;
            let mut v_val = self.compile_expr(val)?;
            k_val = self.prepare_funcsig_for_container_storage(k_val, key, &key_final_ty)?;
            v_val = self.prepare_funcsig_for_container_storage(v_val, val, &val_final_ty)?;

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

        self.track_temp_rc_value(
            dict_ptr,
            &BolideType::Dict(Box::new(key_final_ty), Box::new(val_final_ty)),
        );
        Ok(dict_ptr)
    }

    /// 编译索引访问 (元组或列表)
    fn compile_index(&mut self, base: &Expr, index: &Expr) -> Result<Value, String> {
        let base_type = self.infer_expr_type(base);
        let index_type = self.infer_expr_type(index);
        let base_val = self.compile_expr(base)?;
        let index_val = self.compile_expr(index)?;

        // 根据类型选择不同的索引函数
        match base_type {
            BolideType::List(ref elem_ty) => {
                // Int/Float/Bool 内联：单次 load，无运行时调用
                if matches!(
                    elem_ty.as_ref(),
                    BolideType::Int | BolideType::Float | BolideType::Bool
                ) {
                    return self.emit_list_get_inline(base_val, index_val, elem_ty.as_ref());
                }
                let list_get = *self
                    .func_refs
                    .get("@_list_get")
                    .ok_or("list_get not found")?;
                let call = self.builder.ins().call(list_get, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Dict(key_ty, _) => {
                if !Self::dict_key_type_accepts(key_ty.as_ref(), &index_type) {
                    return Err(format!(
                        "Dict key type mismatch: expected {:?}, got {:?}",
                        key_ty, index_type
                    ));
                }
                let dict_get = *self
                    .func_refs
                    .get("@_dict_get")
                    .ok_or("dict_get not found")?;
                let call = self.builder.ins().call(dict_get, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Bytes => {
                let bytes_get = *self
                    .func_refs
                    .get("@_bytes_get")
                    .ok_or("bytes_get not found")?;
                let call = self.builder.ins().call(bytes_get, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
            }
            BolideType::Str => {
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
                // 默认使用元组索引
                let tuple_get = *self
                    .func_refs
                    .get("@_tuple_get")
                    .ok_or("tuple_get not found")?;
                let call = self.builder.ins().call(tuple_get, &[base_val, index_val]);
                Ok(self.builder.inst_results(call)[0])
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

        // flags: bit0=has_start, bit1=has_end
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
            BolideType::Str => ("@_string_slice", BolideType::Str),
            BolideType::List(_) => ("@_list_slice_step", base_type.clone()),
            BolideType::Tuple(_) => ("@_tuple_slice_step", base_type.clone()),
            _ => return Err(format!("Cannot slice non-sequence type: {:?}", base_type)),
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

    fn dict_key_type_accepts(expected: &BolideType, actual: &BolideType) -> bool {
        matches!(expected, BolideType::Dynamic) || expected == actual
    }

    // ==================== List 内联索引（Int/Float/Bool） ====================
    /// BolideList 内存布局常量
    ///   offset 0..16: RcHeader
    ///   offset 16:   data (*mut i64)
    ///   offset 24:   len (usize / i64)
    const LIST_DATA_OFFSET: i64 = 16;
    const LIST_LEN_OFFSET: i64 = 24;

    /// 内联 list[index] 读取（仅 Int/Float/Bool）
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
        // load len
        let len_ptr = self.builder.ins().iadd_imm(list_ptr, Self::LIST_LEN_OFFSET);
        let len = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), len_ptr, 0);
        let in_bounds = self.builder.ins().icmp(IntCC::UnsignedLessThan, index, len);
        // load data ptr (now *mut u8)
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

    /// 内联 list[index] = value 写入（仅 Int/Float/Bool）
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
        // load len
        let len_ptr = self.builder.ins().iadd_imm(list_ptr, Self::LIST_LEN_OFFSET);
        let _len = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), len_ptr, 0);
        // load data ptr
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

    fn spawn_result_suffix(ret_ty: &BolideType) -> &'static str {
        match ret_ty {
            BolideType::Float => "_float",
            BolideType::Str
            | BolideType::BigInt
            | BolideType::Decimal
            | BolideType::Dynamic
            | BolideType::Ptr
            | BolideType::List(_)
            | BolideType::Tuple(_)
            | BolideType::Dict(_, _)
            | BolideType::Custom(_) => "_ptr",
            _ => "_int",
        }
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
            BolideType::FuncSig(_, _) => 11,
            _ => 7,
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
            Expr::Spawn(name, args) | Expr::SpawnThread(name, args) => {
                Ok((name.as_str(), args.as_slice()))
            }
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

    fn compile_pool_spawn(&mut self, func_name: &str, args: &[Expr]) -> Result<Value, String> {
        let prepared_args = self.prepare_call_args(func_name, args)?;
        let return_type = self
            .func_return_types
            .get(func_name)
            .cloned()
            .unwrap_or(None)
            .unwrap_or(BolideType::Int);
        let type_suffix = Self::spawn_result_suffix(&return_type);

        let (func_addr, env_ptr) = if prepared_args.is_empty() {
            let target_func_ref = *self
                .func_refs
                .get(func_name)
                .ok_or_else(|| format!("Undefined function: {}", func_name))?;
            let func_addr = self.builder.ins().func_addr(self.ptr_type, target_func_ref);
            let null_ptr = self.builder.ins().iconst(self.ptr_type, 0);
            (func_addr, null_ptr)
        } else {
            let trampoline_ref = *self
                .trampoline_refs
                .get(func_name)
                .ok_or_else(|| format!("No trampoline for function: {}", func_name))?;
            let param_types = self
                .trampoline_param_types
                .get(func_name)
                .ok_or_else(|| format!("No param types for trampoline: {}", func_name))?
                .clone();
            let env_size = *self
                .trampoline_env_sizes
                .get(func_name)
                .ok_or_else(|| format!("No env size for trampoline: {}", func_name))?;
            let alloc_ref = *self
                .func_refs
                .get("@_bolide_alloc")
                .ok_or("bolide_alloc not found")?;
            let size_val = self.builder.ins().iconst(types::I64, env_size);
            let alloc_call = self.builder.ins().call(alloc_ref, &[size_val]);
            let env_ptr = self.builder.inst_results(alloc_call)[0];

            let params = self.func_params.get(func_name).cloned().unwrap_or_default();
            let arg_values =
                self.compile_prepared_args_for_params(func_name, &prepared_args, &params, 0)?;

            for (target_index, val) in arg_values.into_iter().enumerate() {
                let offset = (target_index * 8) as i32;
                let bolide_type = &param_types[target_index];
                let val_to_store = if Self::is_rc_type(bolide_type) {
                    if let Some(clone_func) = Self::get_clone_func_name(bolide_type) {
                        if let Some(clone_ref) = self.func_refs.get(clone_func) {
                            let call = self.builder.ins().call(*clone_ref, &[val]);
                            self.builder.inst_results(call)[0]
                        } else {
                            val
                        }
                    } else {
                        val
                    }
                } else {
                    val
                };
                self.builder
                    .ins()
                    .store(MemFlags::trusted(), val_to_store, env_ptr, offset);
            }

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
        let join_name = format!("@_pool_join{}", Self::spawn_result_suffix(ret_ty));
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
            if Self::is_rc_type(ret_ty) {
                self.track_temp_rc_value(result, ret_ty);
            }
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

    /// 编译 spawn all 表达式
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

    /// 编译 await scope 语句
    fn compile_await_scope(
        &mut self,
        scope_stmt: &bolide_parser::AwaitScopeStmt,
    ) -> Result<(), String> {
        // 进入 scope
        let scope_enter = *self
            .func_refs
            .get("@_scope_enter")
            .ok_or("scope_enter not found")?;
        self.builder.ins().call(scope_enter, &[]);

        // 执行 scope 内的语句
        for stmt in &scope_stmt.body {
            self.compile_stmt(stmt)?;
        }

        // 退出 scope（等待所有未完成的 Future）
        let scope_exit = *self
            .func_refs
            .get("@_scope_exit")
            .ok_or("scope_exit not found")?;
        self.builder.ins().call(scope_exit, &[]);

        Ok(())
    }

    /// 编译 spawn select 语句
    fn compile_spawn_select(
        &mut self,
        select_stmt: &bolide_parser::SpawnSelectStmt,
    ) -> Result<(), String> {
        use bolide_parser::SpawnSelectBranch;

        if select_stmt.branches.is_empty() {
            return Ok(());
        }

        let branch_count = select_stmt.branches.len();

        // 1. 启动所有并行任务，收集 pool handles
        let mut handles: Vec<Value> = Vec::new();
        let mut result_types: Vec<BolideType> = Vec::new();
        for branch in &select_stmt.branches {
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

        // 2. 在栈上分配数组存储 handles (使用 I64 作为指针类型)
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
        self.compile_select_branches(select_stmt, &handles, &result_types, winner_idx)?;

        Ok(())
    }

    /// 编译 select 分支选择逻辑
    fn compile_select_branches(
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

                    // 绑定变量是分支内的局部变量（遮蔽同名全局），并登记类型
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
    /// 编译 await 的热 Task 路径，等待线程/任务完成
    fn compile_task_await(&mut self, handle_expr: &Expr) -> Result<Value, String> {
        let handle = self.compile_expr(handle_expr)?;

        // 从 handle 表达式获取变量名，然后查找对应的 spawn 函数返回类型
        let return_type = match handle_expr {
            Expr::Ident(var_name) => self
                .spawn_func_map
                .get(var_name)
                .and_then(|func_name| self.func_return_types.get(func_name).cloned().flatten()),
            Expr::Spawn(func_name, _) | Expr::SpawnThread(func_name, _) => {
                self.func_return_types.get(func_name).cloned().flatten()
            }
            _ => None,
        };

        let force_thread_join = matches!(handle_expr, Expr::SpawnThread(_, _))
            || matches!(handle_expr, Expr::Ident(name) if self.force_thread_tasks.contains(name));

        // 根据返回类型确定等待函数后缀（只有 _int, _float, _ptr 三种）
        let type_suffix = match &return_type {
            Some(BolideType::Float) => "_float",
            Some(BolideType::Str)
            | Some(BolideType::BigInt)
            | Some(BolideType::Decimal)
            | Some(BolideType::Dynamic)
            | Some(BolideType::Ptr)
            | Some(BolideType::List(_))
            | Some(BolideType::Tuple(_))
            | Some(BolideType::Custom(_)) => "_ptr",
            _ => "_int", // Int, Bool, Channel, Future, None 都用 int
        };

        // 确定 merge_block 的参数类型（与 type_suffix 保持一致）
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

        if force_thread_join {
            let thread_join_name = format!("@_thread_join{}", type_suffix);
            let thread_join_ref = *self
                .func_refs
                .get(&thread_join_name)
                .ok_or(format!("{} not found", thread_join_name))?;
            let thread_call = self.builder.ins().call(thread_join_ref, &[handle]);
            let result = self.builder.inst_results(thread_call)[0];
            let thread_free_ref = *self
                .func_refs
                .get("@_thread_handle_free")
                .ok_or("thread_handle_free not found")?;
            self.builder.ins().call(thread_free_ref, &[handle]);
            if let Some(ref ret_ty) = return_type {
                if Self::is_rc_type(ret_ty) {
                    self.track_temp_rc_value(result, ret_ty);
                }
            }
            return Ok(result);
        }

        // 先检查是否在线程池上下文
        let pool_is_active_ref = *self
            .func_refs
            .get("@_pool_is_active")
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
        self.builder
            .ins()
            .brif(is_active, pool_block, &[], thread_block, &[]);

        // 线程池分支: 使用 pool_join
        self.builder.switch_to_block(pool_block);
        self.builder.seal_block(pool_block);
        let pool_join_name = format!("@_pool_join{}", type_suffix);
        let pool_join_ref = *self
            .func_refs
            .get(&pool_join_name)
            .ok_or(format!("{} not found", pool_join_name))?;
        let pool_call = self.builder.ins().call(pool_join_ref, &[handle]);
        let pool_result = self.builder.inst_results(pool_call)[0];
        let pool_free_ref = *self
            .func_refs
            .get("@_pool_handle_free")
            .ok_or("pool_handle_free not found")?;
        self.builder.ins().call(pool_free_ref, &[handle]);
        self.builder.ins().jump(merge_block, &[pool_result]);

        // 普通线程分支: 使用 thread_join
        self.builder.switch_to_block(thread_block);
        self.builder.seal_block(thread_block);
        let thread_join_name = format!("@_thread_join{}", type_suffix);
        let thread_join_ref = *self
            .func_refs
            .get(&thread_join_name)
            .ok_or(format!("{} not found", thread_join_name))?;
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
            let channel_create_ref = *self
                .func_refs
                .get("@_channel_create")
                .ok_or("channel_create not found")?;
            let call = self.builder.ins().call(channel_create_ref, &[]);
            let channel_ptr = self.builder.inst_results(call)[0];
            Ok(channel_ptr)
        } else if args.len() == 1 {
            // 带缓冲通道: channel_create_buffered(capacity)
            let capacity = self.compile_expr(&args[0])?;
            let channel_create_buffered_ref = *self
                .func_refs
                .get("@_channel_create_buffered")
                .ok_or("channel_create_buffered not found")?;
            let call = self
                .builder
                .ins()
                .call(channel_create_buffered_ref, &[capacity]);
            let channel_ptr = self.builder.inst_results(call)[0];
            Ok(channel_ptr)
        } else {
            Err("channel() expects 0 or 1 argument".to_string())
        }
    }

    /// 编译成员访问 (obj.field)
    fn compile_member_access(&mut self, base: &Expr, member: &str) -> Result<Value, String> {
        // 特殊处理模块成员访问
        if let Expr::Ident(name) = base {
            // 检查是否是模块名
            if self.modules.contains_key(name) {
                let global_name = format!("@{}_{}", name, member);
                if let Some(&data_id) = self.global_data_ids.get(&global_name) {
                    // 获取全局变量的地址
                    let gv = self.module.declare_data_in_func(data_id, self.builder.func);
                    let addr = self.builder.ins().global_value(self.ptr_type, gv);
                    // 从地址加载值
                    let val = self
                        .builder
                        .ins()
                        .load(self.ptr_type, MemFlags::new(), addr, 0);
                    return Ok(val);
                } else {
                    return Err(format!(
                        "DEBUG: Global not found: {}, in module: {}, keys: {:?}, modules: {:?}",
                        global_name,
                        name,
                        self.global_data_ids.keys().take(10).collect::<Vec<_>>(),
                        self.modules.keys().collect::<Vec<_>>()
                    ));
                }
            }
        }

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
                    return Err(format!(
                        "Member access on non-class unowned type: {:?}",
                        inner
                    ));
                }
            }
            _ => return Err(format!("Member access on non-class type: {:?}", base_type)),
        };

        let class_info = self
            .classes
            .get(&class_name)
            .ok_or_else(|| format!("Class not found: {}", class_name))?
            .clone();

        let field = class_info
            .fields
            .iter()
            .find(|f| f.name == member)
            .ok_or_else(|| format!("Field '{}' not found in class '{}'", member, class_name))?;

        let field_offset = field.offset;
        let obj_ptr = self.compile_expr(base)?;
        let field_ptr = self.builder.ins().iadd_imm(obj_ptr, field_offset as i64);
        let value = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), field_ptr, 0);

        Ok(value)
    }

    /// 获取表达式的类型
    fn get_expr_type(&self, expr: &Expr) -> Result<BolideType, String> {
        match expr {
            Expr::Ident(name) => {
                if let Some(ty) = self.var_types.get(name) {
                    return Ok(ty.clone());
                }
                if let Some(ty) = self.global_var_types.get(name) {
                    return Ok(ty.clone());
                }
                Err(format!("Unknown variable type: {}", name))
            }
            Expr::Call(callee, args) => {
                if let Expr::Ident(func_name) = callee.as_ref() {
                    // 内置类型转换函数
                    match func_name.as_str() {
                        "bigint" => return Ok(BolideType::BigInt),
                        "decimal" => return Ok(BolideType::Decimal),
                        "int" => return Ok(BolideType::Int),
                        "float" => return Ok(BolideType::Float),
                        "str" => return Ok(BolideType::Str),
                        "bytes" => return Ok(BolideType::Bytes),
                        "input" => return Ok(BolideType::Str),
                        "channel" => return Ok(BolideType::Channel(Box::new(BolideType::Int))),
                        _ => {}
                    }
                    if self.classes.contains_key(func_name) {
                        return Ok(BolideType::Custom(func_name.clone()));
                    }
                    self.func_return_types
                        .get(func_name)
                        .cloned()
                        .flatten()
                        .or_else(|| {
                            self.extern_funcs
                                .get(func_name)
                                .and_then(|(_, extern_func)| {
                                    extern_func
                                        .return_type
                                        .as_ref()
                                        .map(Self::extern_return_type_to_bolide)
                                })
                        })
                        .ok_or_else(|| format!("Unknown function return type: {}", func_name))
                } else if let Expr::Member(base, member) = callee.as_ref() {
                    if let Expr::Ident(module_name) = base.as_ref() {
                        if self.modules.contains_key(module_name) {
                            let func_name = format!("@{}_{}", module_name, member);
                            return self
                                .func_return_types
                                .get(&func_name)
                                .cloned()
                                .flatten()
                                .ok_or_else(|| {
                                    format!("Unknown function return type: {}", func_name)
                                });
                        }
                    }
                    let base_type = self.get_expr_type(base)?;
                    match base_type {
                        BolideType::Dict(k, v) => match member.as_str() {
                            "keys" => Ok(BolideType::List(k)),
                            "values" => Ok(BolideType::List(v)),
                            "get" | "remove" => Ok(*v),
                            "clone" => Ok(BolideType::Dict(k, v)),
                            "len" | "is_empty" | "contains" => Ok(BolideType::Int),
                            _ => Err(format!("Unknown Dict method: {}", member)),
                        },
                        BolideType::List(elem) => match member.as_str() {
                            "pop" | "get" | "first" | "last" => Ok(*elem),
                            "slice" | "copy" | "clone" | "filter" => Ok(BolideType::List(elem)),
                            "map" => {
                                let ret = args
                                    .first()
                                    .and_then(|arg| self.func_ptr_return_type(arg))
                                    .unwrap_or(*elem);
                                Ok(BolideType::List(Box::new(ret)))
                            }
                            "len" | "index_of" | "count" | "is_empty" => Ok(BolideType::Int),
                            _ => Err(format!("Unknown List method: {}", member)),
                        },
                        BolideType::Str => match member.as_str() {
                            "upper" | "lower" | "trim" | "strip" | "replace" | "repeat"
                            | "substring" | "substr" | "char_at" => Ok(BolideType::Str),
                            "split" => Ok(BolideType::List(Box::new(BolideType::Str))),
                            "len" | "length" | "size" | "find" | "index_of" | "contains"
                            | "includes" | "starts_with" | "ends_with" | "count" => {
                                Ok(BolideType::Int)
                            }
                            _ => Err(format!("Unknown Str method: {}", member)),
                        },
                        BolideType::Bytes => match member.as_str() {
                            "copy" | "clone" => Ok(BolideType::Bytes),
                            "to_string_lossy" => Ok(BolideType::Str),
                            "len" | "length" | "size" | "get" | "set" | "push" => {
                                Ok(BolideType::Int)
                            }
                            _ => Err(format!("Unknown Bytes method: {}", member)),
                        },
                        BolideType::Future => match member.as_str() {
                            "close" | "cancel" | "is_cancelled" => Ok(BolideType::Int),
                            _ => Err(format!("Unknown Future method: {}", member)),
                        },
                        BolideType::Channel(inner) => match member.as_str() {
                            "recv" => Ok(*inner),
                            "send" => Ok(BolideType::Int),
                            _ => Err(format!("Unknown Channel method: {}", member)),
                        },
                        BolideType::Custom(class_name) => self
                            .lookup_method_return_type(&class_name, member)
                            .ok_or_else(|| {
                                format!("Unknown method return type: {}.{}", class_name, member)
                            }),
                        BolideType::Weak(inner) | BolideType::Unowned(inner) => {
                            if let BolideType::Custom(class_name) = inner.as_ref() {
                                self.lookup_method_return_type(class_name, member)
                                    .ok_or_else(|| {
                                        format!(
                                            "Unknown method return type: {}.{}",
                                            class_name, member
                                        )
                                    })
                            } else {
                                Err(format!(
                                    "Method call on non-class reference type: {:?}",
                                    inner
                                ))
                            }
                        }
                        other => Err(format!("Method call on non-class type: {:?}", other)),
                    }
                } else {
                    Err("Cannot determine type of indirect call".to_string())
                }
            }
            Expr::Member(base, member) => {
                // 特殊处理模块成员访问
                if let Expr::Ident(name) = base.as_ref() {
                    // 检查是否是模块名
                    if self.modules.contains_key(name) {
                        let global_name = format!("@{}_{}", name, member);
                        if let Some(ty) = self.global_var_types.get(&global_name) {
                            return Ok(ty.clone());
                        }
                    }
                }

                let base_type = self.get_expr_type(base)?;
                // 处理 Weak/Unowned 类型，提取内部的 Custom 类型
                let class_name = match &base_type {
                    BolideType::Custom(name) => name.clone(),
                    BolideType::Weak(inner) => {
                        if let BolideType::Custom(name) = inner.as_ref() {
                            name.clone()
                        } else {
                            return Err(format!(
                                "Member access on non-class weak type: {:?}",
                                inner
                            ));
                        }
                    }
                    BolideType::Unowned(inner) => {
                        if let BolideType::Custom(name) = inner.as_ref() {
                            name.clone()
                        } else {
                            return Err(format!(
                                "Member access on non-class unowned type: {:?}",
                                inner
                            ));
                        }
                    }
                    _ => return Err(format!("Member access on non-class type: {:?}", base_type)),
                };
                let class_info = self
                    .classes
                    .get(&class_name)
                    .ok_or_else(|| format!("Class not found: {}", class_name))?;
                let field = class_info
                    .fields
                    .iter()
                    .find(|f| f.name == *member)
                    .ok_or_else(|| {
                        format!("Field '{}' not found in class '{}'", member, class_name)
                    })?;
                Ok(field.ty.clone())
            }
            _ => Err("Cannot determine expression type".to_string()),
        }
    }

    /// 编译模块函数调用 (module.func())
    fn compile_module_call(&mut self, func_name: &str, args: &[Expr]) -> Result<Value, String> {
        let func_ref = *self
            .func_refs
            .get(func_name)
            .ok_or_else(|| format!("Undefined function: {}", func_name))?;

        let prepared_args = self.prepare_call_args(func_name, args)?;
        let params = self.func_params.get(func_name).cloned().unwrap_or_default();
        let arg_values =
            self.compile_prepared_args_for_params(func_name, &prepared_args, &params, 0)?;

        // 调用函数
        let call = self.builder.ins().call(func_ref, &arg_values);
        self.emit_exception_pending_check()?;
        let results = self.builder.inst_results(call);

        if results.is_empty() {
            Ok(self.builder.ins().iconst(types::I64, 0))
        } else {
            Ok(results[0])
        }
    }

    fn emit_class_method_call(
        &mut self,
        full_method_name: &str,
        self_val: Value,
        arg_values: &[Value],
    ) -> Result<Value, String> {
        let func_ref = *self
            .func_refs
            .get(full_method_name)
            .ok_or_else(|| format!("Method '{}' not found", full_method_name))?;
        let mut call_args = Vec::with_capacity(arg_values.len() + 1);
        call_args.push(self_val);
        call_args.extend_from_slice(arg_values);
        let call = self.builder.ins().call(func_ref, &call_args);
        self.emit_exception_pending_check()?;
        let results = self.builder.inst_results(call);
        let result = if results.is_empty() {
            self.builder.ins().iconst(types::I64, 0)
        } else {
            results[0]
        };
        Ok(result)
    }

    /// 编译方法调用 (obj.method(args))
    fn compile_method_call(
        &mut self,
        base: &Expr,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        // 获取对象类型
        let class_name = self.get_expr_type(base)?;

        // 检查是否是 Future 类型的方法调用
        if matches!(class_name, BolideType::Future) {
            let handle = self.compile_expr(base)?;
            match method_name {
                "close" | "cancel" => {
                    // 调用 thread_cancel
                    let cancel_ref = *self
                        .func_refs
                        .get("@_thread_cancel")
                        .ok_or("thread_cancel not found")?;
                    self.builder.ins().call(cancel_ref, &[handle]);
                    return Ok(self.builder.ins().iconst(types::I64, 0));
                }
                "is_cancelled" => {
                    // 调用 thread_is_cancelled
                    let is_cancelled_ref = *self
                        .func_refs
                        .get("@_thread_is_cancelled")
                        .ok_or("thread_is_cancelled not found")?;
                    let call = self.builder.ins().call(is_cancelled_ref, &[handle]);
                    return Ok(self.builder.inst_results(call)[0]);
                }
                _ => return Err(format!("Unknown Future method: {}", method_name)),
            }
        }

        // 检查是否是 Str 类型的方法调用
        if matches!(class_name, BolideType::Str) {
            let str_ptr = self.compile_expr(base)?;
            return self.compile_str_method_call(str_ptr, method_name, args);
        }

        // 检查是否是 Bytes 类型的方法调用
        if matches!(class_name, BolideType::Bytes) {
            let bytes_ptr = self.compile_expr(base)?;
            return self.compile_bytes_method_call(bytes_ptr, method_name, args);
        }

        // 检查是否是 List 类型的方法调用
        if let BolideType::List(ref elem) = class_name {
            let elem_ty = elem.as_ref().clone();
            let list_ptr = self.compile_expr(base)?;
            return self.compile_list_method_call(list_ptr, method_name, args, elem_ty);
        }

        // 检查是否是 Dict 类型的方法调用
        if matches!(class_name, BolideType::Dict(_, _)) {
            let dict_ptr = self.compile_expr(base)?;
            return self.compile_dict_method_call(dict_ptr, method_name, args);
        }

        // 检查是否是 Channel 类型的方法调用 (send / recv)
        if let BolideType::Channel(ref inner) = class_name {
            let inner_ty = inner.as_ref().clone();
            return self.compile_channel_method_call(base, method_name, args, inner_ty);
        }

        let is_super_call = matches!(base, Expr::Ident(name) if name == "super");
        let class_name = match class_name {
            BolideType::Custom(name) => name,
            _ => return Err(format!("Method call on non-class type: {:?}", class_name)),
        };

        // 查找方法（支持继承链）
        let self_val = self.compile_expr(base)?;
        let full_method_name = self.find_method(&class_name, method_name)?;

        // 编译其他参数
        let user_params = self
            .func_params
            .get(&full_method_name)
            .map(|params| params.get(1..).unwrap_or(&[]).to_vec());
        let prepared_args = if let Some(user_params) = user_params.as_ref() {
            self.normalize_args_for_params(&full_method_name, user_params, args)?
        } else {
            self.prepare_plain_args(&full_method_name, args)?
        };
        let empty_params = Vec::new();
        let user_params = user_params.as_ref().unwrap_or(&empty_params);
        let user_arg_values = self.compile_prepared_args_for_params(
            &full_method_name,
            &prepared_args,
            &user_params,
            1,
        )?;
        let ret_ty_opt = self
            .func_return_types
            .get(&full_method_name)
            .cloned()
            .flatten();

        if is_super_call {
            let result =
                self.emit_class_method_call(&full_method_name, self_val, &user_arg_values)?;
            if let Some(ref ret_ty) = ret_ty_opt {
                if Self::is_rc_type(ret_ty) {
                    self.track_temp_rc_value(result, ret_ty);
                }
                if matches!(ret_ty, BolideType::FuncSig(_, _) | BolideType::Func)
                    && !self.method_call_returns_raw_funcsig(&full_method_name, base, args)
                    && !self.funcsig_return_source_uses_param(&full_method_name)
                {
                    self.closure_temps.push(result);
                }
            }
            return Ok(result);
        }

        let class_tag_ref = *self
            .func_refs
            .get("@_object_class_tag")
            .ok_or("object_class_tag not found")?;
        let class_tag_call = self.builder.ins().call(class_tag_ref, &[self_val]);
        let class_tag_val = self.builder.inst_results(class_tag_call)[0];

        let mut dispatch_classes: Vec<(i64, String)> = self
            .class_tags
            .iter()
            .map(|(name, tag)| (*tag, name.clone()))
            .collect();
        dispatch_classes.sort_by_key(|(tag, _)| *tag);

        if dispatch_classes.is_empty() {
            let result =
                self.emit_class_method_call(&full_method_name, self_val, &user_arg_values)?;
            if let Some(ref ret_ty) = ret_ty_opt {
                if Self::is_rc_type(ret_ty) {
                    self.track_temp_rc_value(result, ret_ty);
                }
                if matches!(ret_ty, BolideType::FuncSig(_, _) | BolideType::Func)
                    && !self.method_call_returns_raw_funcsig(&full_method_name, base, args)
                    && !self.funcsig_return_source_uses_param(&full_method_name)
                {
                    self.closure_temps.push(result);
                }
            }
            return Ok(result);
        }

        let result_type = ret_ty_opt
            .as_ref()
            .map(|ty| self.bolide_type_to_cranelift(ty))
            .unwrap_or(types::I64);
        let result_block = self.builder.create_block();
        self.builder.append_block_param(result_block, result_type);

        let compare_blocks: Vec<Block> = (0..dispatch_classes.len())
            .map(|_| self.builder.create_block())
            .collect();
        let fallback_block = self.builder.create_block();

        self.builder.ins().jump(compare_blocks[0], &[]);

        for (index, (tag, class_name)) in dispatch_classes.into_iter().enumerate() {
            self.builder.switch_to_block(compare_blocks[index]);
            let match_block = self.builder.create_block();
            let next_block = compare_blocks
                .get(index + 1)
                .copied()
                .unwrap_or(fallback_block);
            let tag_val = self.builder.ins().iconst(types::I64, tag);
            let cond = self
                .builder
                .ins()
                .icmp(IntCC::Equal, class_tag_val, tag_val);
            self.builder
                .ins()
                .brif(cond, match_block, &[], next_block, &[]);

            self.builder.seal_block(compare_blocks[index]);

            self.builder.switch_to_block(match_block);
            let dispatch_name = self
                .find_method(&class_name, method_name)
                .unwrap_or_else(|_| full_method_name.clone());
            let result = self.emit_class_method_call(&dispatch_name, self_val, &user_arg_values)?;
            self.builder.ins().jump(result_block, &[result]);
            self.builder.seal_block(match_block);
        }

        self.builder.switch_to_block(fallback_block);
        let fallback =
            self.emit_class_method_call(&full_method_name, self_val, &user_arg_values)?;
        self.builder.ins().jump(result_block, &[fallback]);
        self.builder.seal_block(fallback_block);

        self.builder.switch_to_block(result_block);
        self.builder.seal_block(result_block);
        let result = self.builder.block_params(result_block)[0];
        if let Some(ref ret_ty) = ret_ty_opt {
            if Self::is_rc_type(ret_ty) {
                self.track_temp_rc_value(result, ret_ty);
            }
            if matches!(ret_ty, BolideType::FuncSig(_, _) | BolideType::Func)
                && !self.method_call_returns_raw_funcsig(&full_method_name, base, args)
                && !self.funcsig_return_source_uses_param(&full_method_name)
            {
                self.closure_temps.push(result);
            }
        }
        Ok(result)
    }

    /// 编译字符串方法调用
    fn compile_str_method_call(
        &mut self,
        str_ptr: Value,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        // 调用 (sym, 额外字符串参数个数, 返回是否为 RC 串)
        let call_str = |s: &mut Self,
                        sym: &str,
                        n_str_args: usize,
                        ret_is_str: bool|
         -> Result<Value, String> {
            let func_ref = *s
                .func_refs
                .get(sym)
                .ok_or_else(|| format!("{} not found", sym))?;
            let mut vals = vec![str_ptr];
            for arg in args.iter().take(n_str_args) {
                vals.push(s.compile_expr(arg)?);
            }
            let call = s.builder.ins().call(func_ref, &vals);
            let results = s.builder.inst_results(call);
            let val = if results.is_empty() {
                s.builder.ins().iconst(types::I64, 0)
            } else {
                results[0]
            };
            if ret_is_str {
                s.track_temp_rc_value(val, &BolideType::Str);
            }
            Ok(val)
        };

        match method_name {
            "len" | "length" | "size" => call_str(self, "@_string_len", 0, false),
            "upper" => call_str(self, "@_string_upper", 0, true),
            "lower" => call_str(self, "@_string_lower", 0, true),
            "trim" | "strip" => call_str(self, "@_string_trim", 0, true),
            "replace" => call_str(self, "@_string_replace", 2, true),
            "find" | "index_of" => call_str(self, "@_string_find", 1, false),
            "contains" | "includes" => call_str(self, "@_string_contains", 1, false),
            "starts_with" => call_str(self, "@_string_starts_with", 1, false),
            "ends_with" => call_str(self, "@_string_ends_with", 1, false),
            "count" => call_str(self, "@_string_count", 1, false),
            "split" => {
                // 返回 list<str>，按 RC 列表跟踪
                let func_ref = *self
                    .func_refs
                    .get("@_string_split")
                    .ok_or("string_split not found")?;
                let mut vals = vec![str_ptr];
                if let Some(arg) = args.first() {
                    vals.push(self.compile_expr(arg)?);
                } else {
                    // 无分隔符时传 null（按单码点拆）
                    vals.push(self.builder.ins().iconst(self.ptr_type, 0));
                }
                let call = self.builder.ins().call(func_ref, &vals);
                let val = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(val, &BolideType::List(Box::new(BolideType::Str)));
                Ok(val)
            }
            "repeat" => {
                // repeat(n)：n 为整数参数
                let func_ref = *self
                    .func_refs
                    .get("@_string_repeat")
                    .ok_or("string_repeat not found")?;
                let n = if let Some(arg) = args.first() {
                    self.compile_expr(arg)?
                } else {
                    self.builder.ins().iconst(types::I64, 0)
                };
                let call = self.builder.ins().call(func_ref, &[str_ptr, n]);
                let val = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(val, &BolideType::Str);
                Ok(val)
            }
            "substring" | "substr" => {
                // substring(a, b)：复用 @_string_slice，step=1, flags=both
                let func_ref = *self
                    .func_refs
                    .get("@_string_slice")
                    .ok_or("string_slice not found")?;
                let a = if let Some(arg) = args.first() {
                    self.compile_expr(arg)?
                } else {
                    self.builder.ins().iconst(types::I64, 0)
                };
                let b = if let Some(arg) = args.get(1) {
                    self.compile_expr(arg)?
                } else {
                    self.builder.ins().iconst(types::I64, 0)
                };
                let step = self.builder.ins().iconst(types::I64, 1);
                let flags = self.builder.ins().iconst(types::I64, 3); // has_start|has_end
                let call = self
                    .builder
                    .ins()
                    .call(func_ref, &[str_ptr, a, b, step, flags]);
                let val = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(val, &BolideType::Str);
                Ok(val)
            }
            "char_at" => call_str(self, "@_string_char_at", 1, true),
            _ => Err(format!("Unknown Str method: {}", method_name)),
        }
    }

    fn compile_bytes_method_call(
        &mut self,
        bytes_ptr: Value,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        match method_name {
            "len" | "length" | "size" => {
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_len")
                    .ok_or("bytes_len not found")?;
                let call = self.builder.ins().call(func_ref, &[bytes_ptr]);
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
                let call = self.builder.ins().call(func_ref, &[bytes_ptr, index]);
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
                    .call(func_ref, &[bytes_ptr, index, value]);
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
                self.builder.ins().call(func_ref, &[bytes_ptr, value]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "copy" | "clone" => {
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_clone")
                    .ok_or("bytes_clone not found")?;
                let call = self.builder.ins().call(func_ref, &[bytes_ptr]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Bytes);
                Ok(result)
            }
            "to_string_lossy" => {
                let func_ref = *self
                    .func_refs
                    .get("@_bytes_to_string_lossy")
                    .ok_or("bytes_to_string_lossy not found")?;
                let call = self.builder.ins().call(func_ref, &[bytes_ptr]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::Str);
                Ok(result)
            }
            _ => Err(format!("Unknown Bytes method: {}", method_name)),
        }
    }

    /// BolideType -> 元素类型标签（与运行时 ElementType 对齐）
    fn bolide_type_to_element_tag(ty: &BolideType) -> u8 {
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
            BolideType::FuncSig(_, _) => 11,
            _ => 7, // Ptr / Custom / Tuple / Future
        }
    }

    /// 推断作为 map/filter 回调的函数表达式的返回类型。
    fn func_ptr_return_type(&self, expr: &Expr) -> Option<BolideType> {
        if let Expr::Ident(name) = expr {
            match name.as_str() {
                "str" | "string" => return Some(BolideType::Str),
                "int" => return Some(BolideType::Int),
                "float" => return Some(BolideType::Float),
                "bigint" => return Some(BolideType::BigInt),
                "decimal" => return Some(BolideType::Decimal),
                _ => {}
            }
            if let Some(Some(ret_ty)) = self.func_return_types.get(name) {
                return Some(ret_ty.clone());
            }
            if let Some(BolideType::FuncSig(_, Some(ret))) = self.var_types.get(name) {
                return Some(ret.as_ref().clone());
            }
        }
        None
    }

    fn compile_list_method_call(
        &mut self,
        list_ptr: Value,
        method_name: &str,
        args: &[Expr],
        list_elem_ty: BolideType,
    ) -> Result<Value, String> {
        // from 借用检查：借用值禁止通过存储型方法进入容器
        if matches!(method_name, "push" | "append" | "insert") {
            for arg in args {
                self.check_borrow_escape(arg, "list method")?;
            }
        }
        match method_name {
            // push(value) -> void
            "push" | "append" => {
                if args.len() != 1 {
                    return Err(format!("{} expects 1 argument", method_name));
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_push")
                    .ok_or("list_push not found")?;
                self.builder.ins().call(func_ref, &[list_ptr, value]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // pop() -> value
            "pop" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_pop")
                    .ok_or("list_pop not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // len() -> int
            "len" | "length" | "size" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_len")
                    .ok_or("list_len not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // get(index) -> value
            "get" => {
                if args.len() != 1 {
                    return Err("get expects 1 argument".to_string());
                }
                let index = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_get")
                    .ok_or("list_get not found")?;
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
                let func_ref = *self
                    .func_refs
                    .get("@_list_set")
                    .ok_or("list_set not found")?;
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
                let func_ref = *self
                    .func_refs
                    .get("@_list_insert")
                    .ok_or("list_insert not found")?;
                self.builder.ins().call(func_ref, &[list_ptr, index, value]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // remove(index) -> value
            "remove" => {
                if args.len() != 1 {
                    return Err("remove expects 1 argument".to_string());
                }
                let index = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_remove")
                    .ok_or("list_remove not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, index]);
                Ok(self.builder.inst_results(call)[0])
            }
            // clear() -> void
            "clear" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_clear")
                    .ok_or("list_clear not found")?;
                self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // reverse() -> void
            "reverse" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_reverse")
                    .ok_or("list_reverse not found")?;
                self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // extend(other_list) -> void
            "extend" => {
                if args.len() != 1 {
                    return Err("extend expects 1 argument".to_string());
                }
                let other = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_extend")
                    .ok_or("list_extend not found")?;
                self.builder.ins().call(func_ref, &[list_ptr, other]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            // contains(value) -> bool
            "contains" | "includes" => {
                if args.len() != 1 {
                    return Err(format!("{} expects 1 argument", method_name));
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_contains")
                    .ok_or("list_contains not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, value]);
                Ok(self.builder.inst_results(call)[0])
            }
            // index_of(value) -> int (-1 if not found)
            "index_of" | "index" | "find" => {
                if args.len() != 1 {
                    return Err(format!("{} expects 1 argument", method_name));
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_index_of")
                    .ok_or("list_index_of not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, value]);
                Ok(self.builder.inst_results(call)[0])
            }
            // count(value) -> int
            "count" => {
                if args.len() != 1 {
                    return Err("count expects 1 argument".to_string());
                }
                let value = self.compile_expr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_count")
                    .ok_or("list_count not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, value]);
                Ok(self.builder.inst_results(call)[0])
            }
            // sort() -> void
            "sort" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_sort")
                    .ok_or("list_sort not found")?;
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
                let func_ref = *self
                    .func_refs
                    .get("@_list_slice")
                    .ok_or("list_slice not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, start, end]);
                Ok(self.builder.inst_results(call)[0])
            }
            // is_empty() -> bool
            "is_empty" | "empty" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_is_empty")
                    .ok_or("list_is_empty not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // first() -> value
            "first" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_first")
                    .ok_or("list_first not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // last() -> value
            "last" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_last")
                    .ok_or("list_last not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // copy() -> list (shallow copy, same as clone)
            "copy" | "clone" => {
                let func_ref = *self
                    .func_refs
                    .get("@_list_clone")
                    .ok_or("list_clone not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            // map(callback_fn) -> new list（结果元素类型 = 回调返回类型）
            "map" => {
                if args.len() != 1 {
                    return Err("map expects 1 argument (function)".to_string());
                }
                let ret_ty = self
                    .func_ptr_return_type(&args[0])
                    .unwrap_or(list_elem_ty.clone());
                let result_tag = Self::bolide_type_to_element_tag(&ret_ty);
                let func_ptr = self.compile_expr_as_list_map_func_ptr(&args[0], &list_elem_ty)?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_map")
                    .ok_or("list_map not found")?;
                let tag_val = self.builder.ins().iconst(types::I8, result_tag as i64);
                let call = self
                    .builder
                    .ins()
                    .call(func_ref, &[list_ptr, func_ptr, tag_val]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::List(Box::new(ret_ty)));
                Ok(result)
            }
            // filter(callback_fn) -> new list（结果元素类型 = 源列表元素类型）
            "filter" => {
                if args.len() != 1 {
                    return Err("filter expects 1 argument (function)".to_string());
                }
                let func_ptr = self.compile_expr_as_func_ptr(&args[0])?;
                let func_ref = *self
                    .func_refs
                    .get("@_list_filter")
                    .ok_or("list_filter not found")?;
                let call = self.builder.ins().call(func_ref, &[list_ptr, func_ptr]);
                let result = self.builder.inst_results(call)[0];
                self.track_temp_rc_value(result, &BolideType::List(Box::new(list_elem_ty.clone())));
                Ok(result)
            }
            _ => Err(format!("Unknown list method: {}", method_name)),
        }
    }

    /// 将函数引用 Expr (Ident or func variable) 编译为函数指针 Value。
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

    /// 将 list.map 的回调编译为函数指针。转换内置函数没有用户级函数声明，
    /// 但其 runtime 符号可直接作为对应源元素类型的回调。
    fn compile_expr_as_list_map_func_ptr(
        &mut self,
        expr: &Expr,
        src_elem_ty: &BolideType,
    ) -> Result<Value, String> {
        if let Expr::Ident(name) = expr {
            if let Some(runtime_name) = Self::builtin_map_callback_name(name, src_elem_ty) {
                let func_ref = *self
                    .func_refs
                    .get(runtime_name)
                    .ok_or_else(|| format!("{} not found", runtime_name))?;
                return Ok(self.builder.ins().func_addr(self.ptr_type, func_ref));
            }
        }
        self.compile_expr_as_func_ptr(expr)
    }

    fn builtin_map_callback_name(name: &str, src_elem_ty: &BolideType) -> Option<&'static str> {
        match (name, src_elem_ty) {
            ("str", BolideType::Int) | ("string", BolideType::Int) => Some("@_string_from_int"),
            ("str", BolideType::Float) | ("string", BolideType::Float) => {
                Some("@_string_from_float")
            }
            ("str", BolideType::Bool) | ("string", BolideType::Bool) => Some("@_string_from_bool"),
            ("str", BolideType::BigInt) | ("string", BolideType::BigInt) => {
                Some("@_string_from_bigint")
            }
            ("str", BolideType::Decimal) | ("string", BolideType::Decimal) => {
                Some("@_string_from_decimal")
            }
            ("str", BolideType::Dynamic) | ("string", BolideType::Dynamic) => {
                Some("@_dynamic_to_string")
            }
            ("int", BolideType::Str) => Some("@_string_to_int"),
            ("float", BolideType::Str) => Some("@_string_to_float"),
            ("bigint", BolideType::Int) => Some("@_bigint_from_i64"),
            ("bigint", BolideType::Str) => Some("@_bigint_from_str"),
            ("decimal", BolideType::Int) => Some("@_decimal_from_i64"),
            ("decimal", BolideType::Float) => Some("@_decimal_from_f64"),
            ("decimal", BolideType::Str) => Some("@_decimal_from_str"),
            _ => None,
        }
    }

    /// 从 List 类型的 base expr 推断元素类型。
    fn infer_expr_type_from_list(&self, base: &Expr) -> BolideType {
        let base_ty = self.infer_expr_type(base);
        if let BolideType::List(elem) = base_ty {
            *elem
        } else {
            BolideType::Int
        }
    }

    /// 编译字典方法调用
    fn compile_dict_method_call(
        &mut self,
        dict_ptr: Value,
        method_name: &str,
        args: &[Expr],
    ) -> Result<Value, String> {
        match method_name {
            "set" => {
                let set_fn = *self.func_refs.get("@_dict_set").ok_or("dict_set failed")?;
                let k = self.compile_expr(&args[0])?;
                let v = self.compile_expr(&args[1])?;
                self.builder.ins().call(set_fn, &[dict_ptr, k, v]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "get" => {
                let get_fn = *self.func_refs.get("@_dict_get").ok_or("dict_get failed")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(get_fn, &[dict_ptr, k]);
                Ok(self.builder.inst_results(call)[0])
            }
            "contains" => {
                let contains_fn = *self
                    .func_refs
                    .get("@_dict_contains")
                    .ok_or("dict_contains failed")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(contains_fn, &[dict_ptr, k]);
                Ok(self.builder.inst_results(call)[0])
            }
            "remove" => {
                let remove_fn = *self
                    .func_refs
                    .get("@_dict_remove")
                    .ok_or("dict_remove failed")?;
                let k = self.compile_expr(&args[0])?;
                let call = self.builder.ins().call(remove_fn, &[dict_ptr, k]);
                Ok(self.builder.inst_results(call)[0])
            }
            "len" => {
                let len_fn = *self.func_refs.get("@_dict_len").ok_or("dict_len failed")?;
                let call = self.builder.ins().call(len_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            "is_empty" => {
                let is_empty_fn = *self
                    .func_refs
                    .get("@_dict_is_empty")
                    .ok_or("dict_is_empty failed")?;
                let call = self.builder.ins().call(is_empty_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            "clear" => {
                let clear_fn = *self
                    .func_refs
                    .get("@_dict_clear")
                    .ok_or("dict_clear failed")?;
                self.builder.ins().call(clear_fn, &[dict_ptr]);
                Ok(self.builder.ins().iconst(types::I64, 0))
            }
            "keys" => {
                let keys_fn = *self
                    .func_refs
                    .get("@_dict_keys")
                    .ok_or("dict_keys failed")?;
                let call = self.builder.ins().call(keys_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            "values" => {
                let values_fn = *self
                    .func_refs
                    .get("@_dict_values")
                    .ok_or("dict_values failed")?;
                let call = self.builder.ins().call(values_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            "clone" => {
                let clone_fn = *self
                    .func_refs
                    .get("@_dict_clone")
                    .ok_or("dict_clone failed")?;
                let call = self.builder.ins().call(clone_fn, &[dict_ptr]);
                Ok(self.builder.inst_results(call)[0])
            }
            _ => Err(format!("Unknown dictionary method: {}", method_name)),
        }
    }

    /// 在类方法体内绑定 `super`：与 `self` 复用同一对象指针变量，
    /// 但其 Bolide 类型记为当前类的父类。由于派发是静态的、且子类字段
    /// 布局在父类字段之后（父类指针布局兼容），用同一指针即可正确解析
    /// 父类方法与字段。无父类（或非类方法）时不绑定。
    fn bind_super_alias(&mut self) {
        let Some(self_var) = self.variables.get("self").copied() else {
            return;
        };
        let class_name = match self.var_types.get("self") {
            Some(BolideType::Custom(name)) => name.clone(),
            _ => return,
        };
        let parent = match self.classes.get(&class_name).and_then(|c| c.parent.clone()) {
            Some(p) => p,
            None => return,
        };
        // super 复用 self 的 Variable（同一 SSA 值），仅类型不同
        self.variables.insert("super".to_string(), self_var);
        self.var_types
            .insert("super".to_string(), BolideType::Custom(parent));
    }

    /// 在继承链中查找方法

    fn find_method(&self, class_name: &str, method_name: &str) -> Result<String, String> {
        let mut current = self.normalize_type_name(class_name);
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
            return Err(format!(
                "Method '{}' not found in class '{}' or its parents",
                method_name, class_name
            ));
        }
    }

    /// 沿继承链查找方法返回类型（用于 print 等处的方法调用类型推断）。
    fn lookup_method_return_type(&self, class_name: &str, method_name: &str) -> Option<BolideType> {
        let mut current = self.normalize_type_name(class_name);
        loop {
            let full_name = format!("{}_{}", current, method_name);
            if let Some(ret) = self.func_return_types.get(&full_name) {
                return ret.clone();
            }
            if let Some(class_info) = self.classes.get(&current) {
                if let Some(ref parent) = class_info.parent {
                    current = parent.clone();
                    continue;
                }
            }
            return None;
        }
    }

    /// 尝试运算符重载
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
        validate_jit_extern_lib_path(lib_path)?;

        // 遍历所有声明
        for decl in &eb.declarations {
            match decl {
                bolide_parser::ExternDecl::Function(func) => {
                    // 记录 extern 函数信息
                    self.extern_funcs
                        .insert(func.name.clone(), (lib_path.clone(), func.clone()));
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

    fn create_string_constant(&mut self, s: &str) -> Result<Value, String> {
        let mut bytes: Vec<u8> = s.bytes().collect();
        bytes.push(0);

        let slot = self
            .builder
            .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                bytes.len() as u32,
                0,
            ));
        let ptr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);

        for (i, byte) in bytes.iter().enumerate() {
            let val = self.builder.ins().iconst(types::I8, *byte as i64);
            self.builder
                .ins()
                .store(cranelift_codegen::ir::MemFlags::new(), val, ptr, i as i32);
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
        if lib_path == "bolide" {
            return self.compile_linked_extern_call(extern_func, args);
        }

        if is_jit_dynamic_lib_spec(lib_path) {
            return self.compile_dynamic_extern_call(lib_path, extern_func, args);
        }

        Err(format!(
            "extern \"{}\" is a native link library. JIT mode cannot link native libraries; use `bolide compile`.",
            lib_path
        ))
    }

    fn compile_dynamic_extern_call(
        &mut self,
        lib_path: &str,
        extern_func: &bolide_parser::ExternFunc,
        args: &[Expr],
    ) -> Result<Value, String> {
        let resolved_lib = resolve_dynamic_lib_spec(lib_path)?;
        let lib_path_ptr = self.create_string_constant(&resolved_lib)?;
        let func_name_ptr = self.create_string_constant(&extern_func.name)?;

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

    fn compile_linked_extern_call(
        &mut self,
        extern_func: &bolide_parser::ExternFunc,
        args: &[Expr],
    ) -> Result<Value, String> {
        let func_ref = *self
            .func_refs
            .get(&extern_func.name)
            .ok_or_else(|| format!("Extern function not declared: {}", extern_func.name))?;

        let arg_values = self.compile_extern_args(extern_func, args)?;
        let call = self.builder.ins().call(func_ref, &arg_values);
        let results = self.builder.inst_results(call).to_vec();
        self.convert_extern_result(extern_func, &results)
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
            if let Some(param) = extern_func.params.get(i) {
                if matches!(param.ty, bolide_parser::CType::FuncPtr { .. }) {
                    if let Expr::Ident(func_name) = arg {
                        if let Some(&func_ref) = self.func_refs.get(func_name) {
                            let func_addr = self.builder.ins().func_addr(self.ptr_type, func_ref);
                            arg_values.push(func_addr);
                            continue;
                        }
                    }
                }
            }

            let val = self.compile_expr(arg)?;

            if let Some(param) = extern_func.params.get(i) {
                if let bolide_parser::CType::Ptr(inner) = &param.ty {
                    if matches!(inner.as_ref(), bolide_parser::CType::Char) {
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
        if let Some(ref ret_ty) = extern_func.return_type {
            if let Some(managed_ty) = Self::managed_extern_return_type(ret_ty) {
                self.track_temp_rc_value(result, &managed_ty);
                return Ok(result);
            }

            if let bolide_parser::CType::Ptr(inner) = ret_ty {
                if matches!(inner.as_ref(), bolide_parser::CType::Char) {
                    let string_new_ref = *self
                        .func_refs
                        .get("@_bolide_string_new")
                        .ok_or("bolide_string_new not found")?;
                    let call = self.builder.ins().call(string_new_ref, &[result]);
                    let bolide_string = self.builder.inst_results(call)[0];
                    self.track_temp_rc_value(bolide_string, &BolideType::Str);
                    return Ok(bolide_string);
                }
            }
        }

        let result_ty = self.builder.func.dfg.value_type(result);
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

    fn managed_extern_return_type(ctype: &bolide_parser::CType) -> Option<BolideType> {
        use bolide_parser::CType;
        match ctype {
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

    fn extern_return_type_to_bolide(ctype: &bolide_parser::CType) -> BolideType {
        use bolide_parser::CType;
        if let Some(managed_ty) = Self::managed_extern_return_type(ctype) {
            return managed_ty;
        }
        match ctype {
            CType::Float | CType::Double => BolideType::Float,
            CType::Ptr(inner) => match inner.as_ref() {
                CType::Char => BolideType::Str,
                CType::Struct(name) if name == "dynamic" => BolideType::Dynamic,
                _ => BolideType::Ptr,
            },
            _ => BolideType::Int,
        }
    }

    /// C 类型转换为 Cranelift 类型
    fn ctype_to_cranelift(&self, ctype: &bolide_parser::CType) -> types::Type {
        use bolide_parser::CType;
        match ctype {
            CType::Void => types::I64, // void 用 i64 占位
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
