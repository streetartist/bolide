//! Bolide AST → LLVM IR text.
//!
//! Expanding toward full language coverage (classes, ADTs, match, dict,
//! operator overload, try/throw, iterators) while Cranelift remains default.

use bolide_parser::{
    Assign, BinOp, Expr, ForStmt, FuncDef, IfStmt, Param, Pattern, Program, Statement, Type,
    UnaryOp, VarDecl, WhileStmt,
};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use super::frontend::{ExternSig, PreparedProgram};
use super::oop::{
    collect_adts, collect_classes, field_offset, field_type, method_full_name, type_llvm, AdtInfo,
    ClassInfo,
};
use crate::closure_capture::free_variables;
use crate::operator_overload::{binop_method, reflected_binop_method, unary_method};

pub fn emit_llvm_ir(prepared: &PreparedProgram) -> Result<String, String> {
    let mut cg = Codegen::new(prepared.modules.clone());
    cg.overloads = prepared.overloads.clone();
    for ext in &prepared.externs {
        cg.funcs
            .insert(ext.name.clone(), (ext.params.clone(), ext.ret));
        cg.extern_funcptr
            .insert(ext.name.clone(), ext.funcptr_params.clone());
        cg.extern_funcptr_sigs
            .insert(ext.name.clone(), ext.funcptr_sigs.clone());
        cg.extern_cstr
            .insert(ext.name.clone(), ext.cstr_params.clone());
        cg.extern_dynamic
            .insert(ext.name.clone(), ext.dynamic_params.clone());
        cg.extern_names.insert(ext.name.clone());
        cg.extern_decls.push(ext.clone());
    }
    cg.emit_program(&prepared.program)?;
    Ok(cg.finish())
}

/// High-level value kind for locals (beyond raw LLVM type).
#[derive(Clone, Debug)]
enum ValKind {
    Int,
    Float,
    Bool,
    Str,
    /// list element tag for bolide_list_new: 0=int 1=float 2=bool 3=str 4=ptr/object
    List(u8),
    /// list of class instances (element is object ptr)
    ListObj(String),
    /// nested list: `list<list<T>>`, carries the inner list's kind so that
    /// `grid[y][x]` type inference can resolve `T` (e.g. Str)
    NestedList(Box<ValKind>),
    Dict,
    Object(String),
    Adt(String),
    /// bolide_closure object
    Closure,
    /// raw function pointer (bare ABI), e.g. a FuncSig param not receiving closures
    RawFunc,
    /// a BolideDynamic value (dict values, `dynamic` vars) — conversions must unwrap
    Dynamic,
    /// a BolideBigInt heap object (ptr)
    BigInt,
    /// a BolideBytes heap object (ptr) — distinct from Ptr so bytes methods dispatch
    Bytes,
    Ptr,
}

#[derive(Clone)]
struct PendingClosure {
    name: String,
    params: Vec<Param>,
    return_type: Option<Type>,
    body: Vec<Statement>,
    /// (capture_name, kind)
    captures: Vec<(String, ValKind)>,
}

struct Codegen {
    modules: HashMap<String, String>,
    funcs: HashMap<String, (Vec<&'static str>, &'static str)>,
    /// original overloaded free-function name → candidate mangled names
    overloads: HashMap<String, Vec<String>>,
    /// constructor / free function return high-level kind
    func_ret_kind: HashMap<String, ValKind>,
    extern_decls: Vec<ExternSig>,
    /// extern name → per-param flag: is a `func(...)` callback (raw fn ptr)
    extern_funcptr: HashMap<String, Vec<bool>>,
    /// extern name → per-param full callback signature (params, ret) for trampolines
    extern_funcptr_sigs: HashMap<String, Vec<Option<(Vec<&'static str>, &'static str)>>>,
    /// extern name → per-param flag: is a `*c_char` (str → C string conversion)
    extern_cstr: HashMap<String, Vec<bool>>,
    /// extern name → per-param flag: is a `*dynamic` (wrap into a BolideDynamic)
    extern_dynamic: HashMap<String, Vec<bool>>,
    /// extern functions (runtime / native libs) — called by their raw C symbol
    extern_names: HashSet<String>,
    /// callback closure globals (`@__cb_N`) emitted in the module globals section
    cb_globals: Vec<String>,
    /// function/method name → params (for FuncSig param classification)
    func_params: HashMap<String, Vec<Param>>,
    /// variable names holding a spawn handle (await → thread_join)
    spawn_vars: HashSet<String>,
    /// (everything else is a raw function pointer, matching Cranelift)
    funcsig_closure_params: HashMap<String, HashSet<usize>>,
    strings: Vec<String>,
    body: String,
    tmp: usize,
    label: usize,
    locals: HashMap<String, (String, &'static str)>,
    local_kind: HashMap<String, ValKind>,
    mutable: HashMap<String, bool>,
    current_ret_ty: &'static str,
    current_ret_kind: ValKind,
    /// (continue_label, break_label)
    loop_stack: Vec<(String, String)>,
    classes: HashMap<String, ClassInfo>,
    adts: HashMap<String, AdtInfo>,
    /// method full name → (class, method)
    method_meta: HashMap<String, (String, String)>,
    /// whether program needs exception TLS checks
    needs_exc: bool,
    /// catch end labels for throw (innermost first)
    catch_stack: Vec<String>,
    pending_closures: Vec<PendingClosure>,
    closure_id: usize,
    /// free function → trampoline already emitted
    trampolines: HashMap<String, String>,
    /// trampoline function IR text (must not appear mid-function)
    trampoline_ir: String,
    /// captures active in current lifted function: name → env offset
    current_captures: HashMap<String, usize>,
    /// env ptr local name when compiling lifted closure
    current_env_slot: Option<String>,
    /// module-level globals: name → (llvm ty, kind, optional const init expr already lowered)
    global_vars: HashMap<String, (&'static str, ValKind)>,
    /// parameter names that are `ref` params — their `locals[name]` holds the
    /// caller's variable ADDRESS (ptr); reads deref, writes store through it.
    ref_params: HashSet<String>,
    /// ref param name → LLVM type of the pointed-to value (e.g. `ref x: bigint`
    /// → "ptr", `ref x: int` → "i64", `ref x: float` → "double").
    ref_param_val_ty: HashMap<String, &'static str>,
    /// ref param name → addr slot (the alloca holding the caller's address)
    ref_param_addr_slot: HashMap<String, String>,
    /// names of `async fn` — calls route to coroutine spawn (not direct call)
    async_funcs: HashSet<String>,
}

impl Codegen {
    fn new(modules: HashMap<String, String>) -> Self {
        let mut funcs = HashMap::new();
        // runtime helpers registered as callable names
        for (n, ps, r) in [
            ("bolide_print_int", vec!["i64"], "void"),
            ("bolide_print_float", vec!["double"], "void"),
            ("bolide_print_bool", vec!["i64"], "void"),
            ("bolide_print_string", vec!["ptr"], "void"),
            ("bolide_print_list", vec!["ptr"], "void"),
            ("bolide_string_new", vec!["ptr"], "ptr"),
            ("bolide_list_new", vec!["i8"], "ptr"),
            ("bolide_list_with_capacity", vec!["i8", "i64"], "ptr"),
            ("bolide_list_push", vec!["ptr", "i64"], "void"),
            ("bolide_list_pop", vec!["ptr"], "i64"),
            ("bolide_list_len", vec!["ptr"], "i64"),
            ("bolide_list_get", vec!["ptr", "i64"], "i64"),
            ("bolide_list_set", vec!["ptr", "i64", "i64"], "i64"),
            ("bolide_list_reserve", vec!["ptr", "i64"], "void"),
            ("bolide_list_resize", vec!["ptr", "i64", "i64"], "void"),
            ("bolide_list_clear", vec!["ptr"], "void"),
            ("bolide_list_is_empty", vec!["ptr"], "i64"),
            ("bolide_list_first", vec!["ptr"], "i64"),
            ("bolide_list_last", vec!["ptr"], "i64"),
            ("bolide_list_contains", vec!["ptr", "i64"], "i64"),
            ("bolide_list_index_of", vec!["ptr", "i64"], "i64"),
            ("bolide_list_count", vec!["ptr", "i64"], "i64"),
            ("bolide_list_reverse", vec!["ptr"], "void"),
            ("bolide_list_sort", vec!["ptr"], "void"),
            ("bolide_list_insert", vec!["ptr", "i64", "i64"], "void"),
            ("bolide_list_remove", vec!["ptr", "i64"], "i64"),
            ("bolide_string_len", vec!["ptr"], "i64"),
            ("bolide_string_from_int", vec!["i64"], "ptr"),
            ("bolide_string_from_float", vec!["double"], "ptr"),
            ("bolide_string_concat", vec!["ptr", "ptr"], "ptr"),
            ("object_alloc", vec!["i64"], "ptr"),
            ("object_set_class_tag", vec!["ptr", "i64"], "void"),
            ("object_class_tag", vec!["ptr"], "i64"),
            ("object_retain", vec!["ptr"], "void"),
            ("object_release", vec!["ptr"], "void"),
            ("bolide_dict_new", vec!["i8", "i8"], "ptr"),
            ("bolide_dict_set", vec!["ptr", "i64", "i64"], "void"),
            ("bolide_dict_get", vec!["ptr", "i64"], "i64"),
            ("bolide_dict_contains", vec!["ptr", "i64"], "i64"),
            ("bolide_dict_len", vec!["ptr"], "i64"),
            ("bolide_dict_remove", vec!["ptr", "i64"], "i64"),
            ("bolide_exception_set", vec!["ptr", "i64"], "void"),
            ("bolide_exception_get", vec![], "ptr"),
            ("bolide_exception_tag", vec![], "i64"),
            ("bolide_exception_pending", vec![], "i64"),
            ("bolide_throw_uncaught", vec!["ptr"], "void"),
            ("bolide_alloc", vec!["i64"], "ptr"),
            ("bolide_closure_new", vec!["ptr", "ptr", "i64", "ptr"], "ptr"),
            ("bolide_closure_fn_ptr", vec!["ptr"], "ptr"),
            ("bolide_closure_env_ptr", vec!["ptr"], "ptr"),
            ("bolide_closure_retain", vec!["ptr"], "void"),
            ("bolide_closure_release", vec!["ptr"], "void"),
            // BigInt / Decimal / Bytes
            ("bolide_bigint_debug_stats", vec![], "void"),
            ("bolide_bigint_from_i64", vec!["i64"], "ptr"),
            ("bolide_bigint_from_str", vec!["ptr", "i64"], "ptr"),
            ("bolide_decimal_from_i64", vec!["i64"], "ptr"),
            ("bolide_bytes_new", vec![], "ptr"),
            ("bolide_bytes_len", vec!["ptr"], "i64"),
            ("bolide_bytes_capacity", vec!["ptr"], "i64"),
            ("bolide_bytes_get", vec!["ptr", "i64"], "i64"),
            ("bolide_bytes_set", vec!["ptr", "i64", "i64"], "i64"),
            ("bolide_bytes_push", vec!["ptr", "i64"], "void"),
            ("bolide_bytes_extend", vec!["ptr", "ptr"], "void"),
            ("bolide_bytes_clone", vec!["ptr"], "ptr"),
            ("bolide_bytes_to_string_lossy", vec!["ptr"], "ptr"),
            ("bolide_tuple_debug_stats", vec![], "void"),
            // Channel
            ("bolide_channel_create", vec![], "ptr"),
            ("bolide_channel_create_buffered", vec!["i64"], "ptr"),
            ("bolide_channel_send", vec!["ptr", "i64"], "i64"),
            ("bolide_channel_recv", vec!["ptr"], "i64"),
            ("bolide_channel_close", vec!["ptr"], "void"),
            // Input / env
            ("bolide_input", vec![], "ptr"),
            ("bolide_input_prompt", vec!["ptr"], "ptr"),
            ("bolide_string_as_cstr", vec!["ptr"], "ptr"),
            ("bolide_string_from_bigint", vec!["ptr"], "ptr"),
            ("bolide_string_from_decimal", vec!["ptr"], "ptr"),
            // List extras
            ("bolide_list_clone", vec!["ptr"], "ptr"),
            ("bolide_list_extend", vec!["ptr", "ptr"], "void"),
            ("bolide_list_map", vec!["ptr", "ptr", "i64"], "ptr"),
            ("bolide_list_filter", vec!["ptr", "ptr"], "ptr"),
            // Dict extras
            ("bolide_dict_extend", vec!["ptr", "ptr"], "void"),
            // Math extras
            ("bolide_math_abs_i64", vec!["i64"], "i64"),
            ("bolide_math_round", vec!["double"], "double"),
            ("bolide_math_trunc", vec!["double"], "double"),
            ("bolide_math_exp", vec!["double"], "double"),
            ("bolide_math_ln", vec!["double"], "double"),
            ("bolide_math_tan", vec!["double"], "double"),
            ("bolide_math_min_i64", vec!["i64", "i64"], "i64"),
            ("bolide_math_max_i64", vec!["i64", "i64"], "i64"),
            ("bolide_math_min_f64", vec!["double", "double"], "double"),
            ("bolide_math_max_f64", vec!["double", "double"], "double"),
            ("bolide_math_clamp_i64", vec!["i64", "i64", "i64"], "i64"),
            ("bolide_math_clamp_f64", vec!["double", "double", "double"], "double"),
            // Dynamic wrappers (extern `*dynamic` params)
            ("bolide_dynamic_from_int", vec!["i64"], "ptr"),
            ("bolide_dynamic_from_float", vec!["double"], "ptr"),
            ("bolide_dynamic_from_bool", vec!["i64"], "ptr"),
            ("bolide_dynamic_from_string", vec!["ptr"], "ptr"),
            ("bolide_dynamic_from_list", vec!["ptr"], "ptr"),
            ("bolide_dynamic_from_dict", vec!["ptr"], "ptr"),
            ("bolide_dynamic_to_string", vec!["ptr"], "ptr"),
            ("bolide_dynamic_to_int", vec!["ptr"], "i64"),
            ("bolide_dynamic_to_float", vec!["ptr"], "double"),
            ("bolide_print_dynamic", vec!["ptr"], "void"),
            // BigInt
            ("bolide_print_bigint", vec!["ptr"], "void"),
            ("bolide_bigint_add", vec!["ptr", "ptr"], "ptr"),
            ("bolide_bigint_sub", vec!["ptr", "ptr"], "ptr"),
            ("bolide_bigint_mul", vec!["ptr", "ptr"], "ptr"),
            ("bolide_bigint_div", vec!["ptr", "ptr"], "ptr"),
            ("bolide_bigint_rem", vec!["ptr", "ptr"], "ptr"),
            ("bolide_bigint_neg", vec!["ptr"], "ptr"),
            ("bolide_bigint_eq", vec!["ptr", "ptr"], "i64"),
            ("bolide_bigint_ne", vec!["ptr", "ptr"], "i64"),
            ("bolide_bigint_lt", vec!["ptr", "ptr"], "i64"),
            ("bolide_bigint_le", vec!["ptr", "ptr"], "i64"),
            ("bolide_bigint_gt", vec!["ptr", "ptr"], "i64"),
            ("bolide_bigint_ge", vec!["ptr", "ptr"], "i64"),
            // Thread spawn / coroutine await
            ("bolide_thread_spawn_int", vec!["ptr"], "ptr"),
            ("bolide_thread_spawn_int_with_env", vec!["ptr", "ptr"], "ptr"),
            ("bolide_thread_spawn_float", vec!["ptr"], "ptr"),
            ("bolide_thread_spawn_float_with_env", vec!["ptr", "ptr"], "ptr"),
            ("bolide_thread_spawn_ptr", vec!["ptr"], "ptr"),
            ("bolide_thread_spawn_ptr_with_env", vec!["ptr", "ptr"], "ptr"),
            ("bolide_coroutine_await_int", vec!["ptr"], "i64"),
            ("bolide_coroutine_await_float", vec!["ptr"], "double"),
            ("bolide_coroutine_await_ptr", vec!["ptr"], "ptr"),
            ("bolide_coroutine_spawn_int", vec!["ptr"], "ptr"),
            ("bolide_coroutine_spawn_float", vec!["ptr"], "ptr"),
            ("bolide_coroutine_spawn_ptr", vec!["ptr"], "ptr"),
            ("bolide_coroutine_spawn_int_with_env", vec!["ptr", "ptr"], "ptr"),
            ("bolide_coroutine_spawn_float_with_env", vec!["ptr", "ptr"], "ptr"),
            ("bolide_coroutine_spawn_ptr_with_env", vec!["ptr", "ptr"], "ptr"),
            ("bolide_thread_join_int", vec!["ptr"], "i64"),
            ("bolide_thread_join_float", vec!["ptr"], "double"),
            ("bolide_thread_join_ptr", vec!["ptr"], "ptr"),
            ("bolide_thread_handle_free", vec!["ptr"], "void"),
        ] {
            funcs.insert(n.into(), (ps, r));
        }
        Self {
            modules,
            funcs,
            overloads: HashMap::new(),
            func_ret_kind: HashMap::new(),
            extern_decls: Vec::new(),
            extern_funcptr: HashMap::new(),
            extern_funcptr_sigs: HashMap::new(),
            extern_cstr: HashMap::new(),
            extern_dynamic: HashMap::new(),
            extern_names: HashSet::new(),
            cb_globals: Vec::new(),
            func_params: HashMap::new(),
            funcsig_closure_params: HashMap::new(),
            spawn_vars: HashSet::new(),
            strings: Vec::new(),
            body: String::new(),
            tmp: 0,
            label: 0,
            locals: HashMap::new(),
            local_kind: HashMap::new(),
            mutable: HashMap::new(),
            current_ret_ty: "i64",
            current_ret_kind: ValKind::Int,
            loop_stack: Vec::new(),
            classes: HashMap::new(),
            adts: HashMap::new(),
            method_meta: HashMap::new(),
            needs_exc: false,
            catch_stack: Vec::new(),
            pending_closures: Vec::new(),
            closure_id: 0,
            trampolines: HashMap::new(),
            trampoline_ir: String::new(),
            current_captures: HashMap::new(),
            current_env_slot: None,
            global_vars: HashMap::new(),
            ref_params: HashSet::new(),
            ref_param_val_ty: HashMap::new(),
            ref_param_addr_slot: HashMap::new(),
            async_funcs: HashSet::new(),
        }
    }

    fn finish(self) -> String {
        let mut out = String::new();
        out.push_str("; Bolide LLVM backend — generated IR\n");
        #[cfg(target_os = "windows")]
        out.push_str("target triple = \"x86_64-pc-windows-msvc\"\n\n");
        #[cfg(target_os = "linux")]
        out.push_str("target triple = \"x86_64-unknown-linux-gnu\"\n\n");
        #[cfg(target_os = "macos")]
        out.push_str("target triple = \"x86_64-apple-darwin\"\n\n");

        out.push_str(
            r#"declare void @bolide_print_int(i64)
declare void @bolide_print_float(double)
declare void @bolide_print_bool(i64)
declare void @bolide_print_string(ptr)
declare void @bolide_print_list(ptr)
declare void @bolide_println()
declare ptr @bolide_string_new(ptr)
declare i64 @bolide_string_len(ptr)
declare ptr @bolide_string_from_int(i64)
declare ptr @bolide_string_from_float(double)
declare ptr @bolide_string_from_bool(i64)
declare ptr @bolide_string_concat(ptr, ptr)
declare ptr @bolide_list_new(i8)
declare ptr @bolide_list_with_capacity(i8, i64)
declare void @bolide_list_push(ptr, i64)
declare i64 @bolide_list_pop(ptr)
declare i64 @bolide_list_len(ptr)
declare i64 @bolide_list_get(ptr, i64)
declare i64 @bolide_list_set(ptr, i64, i64)
declare void @bolide_list_reserve(ptr, i64)
declare void @bolide_list_resize(ptr, i64, i64)
declare void @bolide_list_clear(ptr)
declare i64 @bolide_list_is_empty(ptr)
declare i64 @bolide_list_first(ptr)
declare i64 @bolide_list_last(ptr)
declare i64 @bolide_list_contains(ptr, i64)
declare i64 @bolide_list_index_of(ptr, i64)
declare i64 @bolide_list_count(ptr, i64)
declare void @bolide_list_reverse(ptr)
declare void @bolide_list_sort(ptr)
declare void @bolide_list_insert(ptr, i64, i64)
declare i64 @bolide_list_remove(ptr, i64)
declare ptr @bolide_string_format(ptr, ptr, i64, ptr, ptr, i64)
declare i64 @bolide_string_to_int(ptr)
declare double @bolide_string_to_float(ptr)
declare ptr @bolide_env_args()
declare i64 @bolide_time_monotonic_ms()
declare i64 @bolide_time_now_ms()
declare i64 @bolide_time_now()
declare double @bolide_math_sqrt(double)
declare double @bolide_math_sin(double)
declare double @bolide_math_cos(double)
declare double @bolide_math_pow(double, double)
declare double @bolide_math_abs_f64(double)
declare double @bolide_math_floor(double)
declare double @bolide_math_ceil(double)
declare ptr @bolide_string_char_at(ptr, i64)
declare ptr @bolide_string_replace(ptr, ptr, ptr)
declare ptr @bolide_string_upper(ptr)
declare ptr @bolide_string_lower(ptr)
declare ptr @bolide_string_trim(ptr)
declare ptr @bolide_string_repeat(ptr, i64)
declare i64 @bolide_string_find(ptr, ptr)
declare i64 @bolide_string_contains(ptr, ptr)
declare i64 @bolide_string_starts_with(ptr, ptr)
declare i64 @bolide_string_ends_with(ptr, ptr)
declare i64 @bolide_string_count(ptr, ptr)
declare ptr @bolide_string_split(ptr, ptr)
declare ptr @bolide_string_slice(ptr, i64, i64, i64, i64)
declare i64 @bolide_string_eq(ptr, ptr)
declare i64 @bolide_string_compare(ptr, ptr)
declare ptr @object_alloc(i64)
declare void @object_set_class_tag(ptr, i64)
declare i64 @object_class_tag(ptr)
declare void @object_retain(ptr)
declare void @object_release(ptr)
declare ptr @bolide_dict_new(i8, i8)
declare void @bolide_dict_set(ptr, i64, i64)
declare i64 @bolide_dict_get(ptr, i64)
declare i64 @bolide_dict_contains(ptr, i64)
declare i64 @bolide_dict_len(ptr)
declare i64 @bolide_dict_is_empty(ptr)
declare i64 @bolide_dict_remove(ptr, i64)
declare ptr @bolide_dict_keys(ptr)
declare ptr @bolide_dict_values(ptr)
declare void @bolide_dict_clear(ptr)
declare void @bolide_exception_set(ptr, i64)
declare ptr @bolide_exception_get()
declare i64 @bolide_exception_tag()
declare i64 @bolide_exception_pending()
declare void @bolide_throw_uncaught(ptr)
declare ptr @bolide_alloc(i64)
declare ptr @bolide_closure_new(ptr, ptr, i64, ptr)
declare ptr @bolide_closure_fn_ptr(ptr)
declare ptr @bolide_closure_env_ptr(ptr)
declare void @bolide_closure_retain(ptr)
declare void @bolide_closure_release(ptr)
declare void @bolide_bigint_debug_stats()
declare ptr @bolide_bigint_from_i64(i64)
declare ptr @bolide_bigint_from_str(ptr, i64)
declare ptr @bolide_decimal_from_i64(i64)
declare ptr @bolide_bytes_new()
declare i64 @bolide_bytes_len(ptr)
declare i64 @bolide_bytes_capacity(ptr)
declare i64 @bolide_bytes_get(ptr, i64)
declare i64 @bolide_bytes_set(ptr, i64, i64)
declare void @bolide_bytes_push(ptr, i64)
declare void @bolide_bytes_extend(ptr, ptr)
declare ptr @bolide_bytes_clone(ptr)
declare ptr @bolide_bytes_to_string_lossy(ptr)
declare void @bolide_tuple_debug_stats()
declare ptr @bolide_channel_create()
declare ptr @bolide_channel_create_buffered(i64)
declare i64 @bolide_channel_send(ptr, i64)
declare i64 @bolide_channel_recv(ptr)
declare void @bolide_channel_close(ptr)
declare ptr @bolide_input()
declare ptr @bolide_input_prompt(ptr)
declare ptr @bolide_string_as_cstr(ptr)
declare ptr @bolide_string_from_bigint(ptr)
declare ptr @bolide_string_from_decimal(ptr)
declare ptr @bolide_list_clone(ptr)
declare void @bolide_list_extend(ptr, ptr)
declare ptr @bolide_list_map(ptr, ptr, i64)
declare ptr @bolide_list_filter(ptr, ptr)
declare void @bolide_dict_extend(ptr, ptr)
declare i64 @bolide_math_abs_i64(i64)
declare double @bolide_math_round(double)
declare double @bolide_math_trunc(double)
declare double @bolide_math_exp(double)
declare double @bolide_math_ln(double)
declare double @bolide_math_tan(double)
declare i64 @bolide_math_min_i64(i64, i64)
declare i64 @bolide_math_max_i64(i64, i64)
declare double @bolide_math_min_f64(double, double)
declare double @bolide_math_max_f64(double, double)
declare i64 @bolide_math_clamp_i64(i64, i64, i64)
declare double @bolide_math_clamp_f64(double, double, double)
declare ptr @bolide_dynamic_from_int(i64)
declare ptr @bolide_dynamic_from_float(double)
declare ptr @bolide_dynamic_from_bool(i64)
declare ptr @bolide_dynamic_from_string(ptr)
declare ptr @bolide_dynamic_from_list(ptr)
declare ptr @bolide_dynamic_from_dict(ptr)
declare ptr @bolide_dynamic_to_string(ptr)
declare i64 @bolide_dynamic_to_int(ptr)
declare double @bolide_dynamic_to_float(ptr)
declare void @bolide_print_dynamic(ptr)
declare void @bolide_print_bigint(ptr)
declare ptr @bolide_thread_spawn_int(ptr)
declare ptr @bolide_thread_spawn_int_with_env(ptr, ptr)
declare ptr @bolide_thread_spawn_float(ptr)
declare ptr @bolide_thread_spawn_float_with_env(ptr, ptr)
declare ptr @bolide_thread_spawn_ptr(ptr)
declare ptr @bolide_thread_spawn_ptr_with_env(ptr, ptr)
declare i64 @bolide_coroutine_await_int(ptr)
declare double @bolide_coroutine_await_float(ptr)
declare ptr @bolide_coroutine_await_ptr(ptr)
declare ptr @bolide_coroutine_spawn_int(ptr)
declare ptr @bolide_coroutine_spawn_float(ptr)
declare ptr @bolide_coroutine_spawn_ptr(ptr)
declare ptr @bolide_coroutine_spawn_int_with_env(ptr, ptr)
declare ptr @bolide_coroutine_spawn_float_with_env(ptr, ptr)
declare ptr @bolide_coroutine_spawn_ptr_with_env(ptr, ptr)
declare i64 @bolide_thread_join_int(ptr)
declare double @bolide_thread_join_float(ptr)
declare ptr @bolide_thread_join_ptr(ptr)
declare void @bolide_thread_handle_free(ptr)
declare ptr @bolide_bigint_add(ptr, ptr)
declare ptr @bolide_bigint_sub(ptr, ptr)
declare ptr @bolide_bigint_mul(ptr, ptr)
declare ptr @bolide_bigint_div(ptr, ptr)
declare ptr @bolide_bigint_rem(ptr, ptr)
declare ptr @bolide_bigint_neg(ptr)
declare i64 @bolide_bigint_eq(ptr, ptr)
declare i64 @bolide_bigint_ne(ptr, ptr)
declare i64 @bolide_bigint_lt(ptr, ptr)
declare i64 @bolide_bigint_le(ptr, ptr)
declare i64 @bolide_bigint_gt(ptr, ptr)
declare i64 @bolide_bigint_ge(ptr, ptr)

"#,
        );
        // declare any other collected externs not already listed
        for ext in &self.extern_decls {
            let known = [
                "bolide_print_int",
                "bolide_string_format",
                "bolide_env_args",
                "bolide_time_monotonic_ms",
                "bolide_math_sin",
                "bolide_math_cos",
                "bolide_math_sqrt",
            ];
            // always emit unique declare lines for all externs (duplicates ok for clang? no)
            let _ = known;
        }
        // emit declare for every extern sig (clang accepts redeclare if identical — skip dups by set)
        let mut seen = std::collections::HashSet::new();
        for ext in &self.extern_decls {
            if !seen.insert(ext.name.clone()) {
                continue;
            }
            // skip ones we already hard-coded above (avoid invalid redefinition when
            // the std module declares them with a slightly different C signature)
            if [
                "bolide_string_format",
                "bolide_string_to_int",
                "bolide_string_to_float",
                "bolide_string_from_int",
                "bolide_string_from_float",
                "bolide_string_concat",
                "bolide_string_len",
                "bolide_string_new",
                "bolide_env_args",
                "bolide_time_monotonic_ms",
                "bolide_time_now_ms",
                "bolide_time_now",
                "bolide_math_sqrt",
                "bolide_math_sin",
                "bolide_math_cos",
                "bolide_math_pow",
                "bolide_math_abs_f64",
                "bolide_math_floor",
                "bolide_math_ceil",
                // hardcoded declares added for direct runtime calls
                "bolide_bigint_debug_stats",
                "bolide_bigint_from_i64",
                "bolide_bigint_from_str",
                "bolide_decimal_from_i64",
                "bolide_bytes_new",
                "bolide_bytes_len",
                "bolide_bytes_capacity",
                "bolide_bytes_get",
                "bolide_bytes_set",
                "bolide_bytes_push",
                "bolide_bytes_extend",
                "bolide_bytes_clone",
                "bolide_bytes_to_string_lossy",
                "bolide_tuple_debug_stats",
                "bolide_channel_create",
                "bolide_channel_create_buffered",
                "bolide_channel_send",
                "bolide_channel_recv",
                "bolide_channel_close",
                "bolide_input",
                "bolide_input_prompt",
                "bolide_string_as_cstr",
                "bolide_string_from_bigint",
                "bolide_string_from_decimal",
                "bolide_list_clone",
                "bolide_list_extend",
                "bolide_list_map",
                "bolide_list_filter",
                "bolide_dict_extend",
                "bolide_math_abs_i64",
                "bolide_math_round",
                "bolide_math_trunc",
                "bolide_math_exp",
                "bolide_math_ln",
                "bolide_math_tan",
                "bolide_math_min_i64",
                "bolide_math_max_i64",
                "bolide_math_min_f64",
                "bolide_math_max_f64",
                "bolide_math_clamp_i64",
                "bolide_math_clamp_f64",
            ]
            .contains(&ext.name.as_str())
            {
                continue;
            }
            let mut ps = String::new();
            for (i, p) in ext.params.iter().enumerate() {
                if i > 0 {
                    ps.push_str(", ");
                }
                ps.push_str(p);
            }
            let _ = writeln!(out, "declare {} @{}({})", ext.ret, ext.name, ps);
        }


        for (i, s) in self.strings.iter().enumerate() {
            let escaped = llvm_escape(s);
            let len = s.len() + 1;
            let _ = writeln!(
                out,
                "@.str.{} = private unnamed_addr constant [{} x i8] c\"{}\\00\", align 1",
                i, len, escaped
            );
        }
        // module / top-level globals
        for (name, (ty, _)) in &self.global_vars {
            let g = llvm_func_name(name);
            match *ty {
                "double" => {
                    let _ = writeln!(out, "@{} = global double 0.0, align 8", g);
                }
                "ptr" => {
                    let _ = writeln!(out, "@{} = global ptr null, align 8", g);
                }
                _ => {
                    let _ = writeln!(out, "@{} = global i64 0, align 8", g);
                }
            }
        }
        // callback closure globals used by bare-call trampolines
        for g in &self.cb_globals {
            let _ = writeln!(out, "@{} = global ptr null, align 8", g);
        }
        out.push('\n');
        out.push_str(&self.body);
        out
    }

    fn emit_program(&mut self, program: &Program) -> Result<(), String> {
        self.needs_exc = program_needs_exceptions(program);
        self.classes = collect_classes(program)?;
        self.adts = collect_adts(program)?;

        // Register free functions
        for stmt in &program.statements {
            if let Statement::FuncDef(f) = stmt {
                let params: Vec<&'static str> = f
                    .params
                    .iter()
                    .map(|p| {
                        if p.mode == bolide_parser::ParamMode::Ref {
                            "ptr"
                        } else {
                            llvm_type_of(&Some(p.ty.clone()))
                        }
                    })
                    .collect();
                let ret = llvm_type_of(&f.return_type);
                self.funcs.insert(f.name.clone(), (params, ret));
                self.func_ret_kind
                    .insert(f.name.clone(), kind_of_type(&f.return_type));
                if f.is_async {
                    self.async_funcs.insert(f.name.clone());
                }
            }
        }

        // Register class constructors + methods
        for (cname, ci) in self.classes.clone() {
            let mut params: Vec<&'static str> = ci
                .fields
                .iter()
                .map(|f| type_llvm(&f.ty))
                .collect();
            self.funcs.insert(cname.clone(), (params, "ptr"));
            self.func_ret_kind
                .insert(cname.clone(), ValKind::Object(cname.clone()));

            for (mname, mdef) in &ci.methods {
                let full = method_full_name(&cname, mname);
                let has_self = mdef.params.first().map(|p| p.name.as_str()) == Some("self");
                let mut mparams: Vec<&'static str> = Vec::new();
                if !has_self {
                    mparams.push("ptr");
                }
                for p in &mdef.params {
                    if p.mode == bolide_parser::ParamMode::Ref {
                        mparams.push("ptr");
                    } else {
                        mparams.push(llvm_type_of(&Some(p.ty.clone())));
                    }
                }
                let ret = llvm_type_of(&mdef.return_type);
                self.funcs.insert(full.clone(), (mparams, ret));
                self.func_ret_kind
                    .insert(full.clone(), kind_of_type(&mdef.return_type));
                self.method_meta
                    .insert(full, (cname.clone(), mname.clone()));
            }
        }

        // Classify FuncSig params: which ones receive closure objects (capturing
        // lambdas). Everything else is a raw function pointer, so web/GUI callbacks
        // (bare functions forwarded through a std method to a `func(*c_void)` extern)
        // are passed as their bare address instead of a closure object.
        self.collect_funcsig_closure_params(program);

        // Every top-level `let`/`var` is a true LLVM global so non-main
        // functions can read/write it (mirrors the Cranelift backend, where
        // top-level bindings become globals rather than locals of `__main__`).
        for stmt in &program.statements {
            if let Statement::VarDecl(d) = stmt {
                let mut kind = if let Some(ref t) = d.ty {
                    kind_of_type(&Some(t.clone()))
                } else if let Some(ref v) = d.value {
                    // No explicit type → infer from the value: literals, closure
                    // literal, function-returning call, `channel()`/`bytes()`/
                    // class ctors → the global holds a ptr/closure object.
                    match v {
                        Expr::Float(_) | Expr::Decimal(_) => ValKind::Float,
                        Expr::String(_) => ValKind::Str,
                        Expr::Bool(_) => ValKind::Bool,
                        Expr::Int(_) => ValKind::Int,
                        Expr::Closure { .. } => ValKind::Closure,
                        _ => self.infer_kind(v),
                    }
                } else {
                    ValKind::Int
                };
                let ty = kind_to_llvm(&kind);
                self.global_vars.insert(d.name.clone(), (ty, kind));
            }
        }

        // Emit everything (full-language path; DCE optional later)
        let mut fns = String::new();
        std::mem::swap(&mut self.body, &mut fns);

        // Constructors
        for (cname, ci) in self.classes.clone() {
            self.emit_constructor(&cname, &ci)?;
        }
        // Methods
        for (cname, ci) in self.classes.clone() {
            for (_mname, mdef) in &ci.methods {
                self.emit_method(&cname, mdef)?;
            }
        }
        // Free functions (may queue closures)
        for stmt in &program.statements {
            if let Statement::FuncDef(f) = stmt {
                self.emit_function(f)?;
            }
        }
        // Lifted closures (may nest → drain until empty)
        self.drain_pending_closures()?;

        let mut emitted_fns = std::mem::take(&mut self.body);

        self.locals.clear();
        self.local_kind.clear();
        self.mutable.clear();
        self.loop_stack.clear();
        self.catch_stack.clear();
        self.current_captures.clear();
        self.current_env_slot = None;
        self.current_ret_ty = "i64";
        self.current_ret_kind = ValKind::Int;
        self.body = String::new();
        let _ = writeln!(self.body, "define i64 @bolide_main() {{");
        let _ = writeln!(self.body, "entry:");
        for stmt in &program.statements {
            if matches!(
                stmt,
                Statement::FuncDef(_)
                    | Statement::ClassDef(_)
                    | Statement::TraitDef(_)
                    | Statement::TraitImpl(_)
                    | Statement::MacroDef(_)
                    | Statement::AttrMacroDef(_)
                    | Statement::Import(_)
                    | Statement::ExternBlock(_)
                    | Statement::ValueDef(_)
                    | Statement::EnumDef(_)
                    | Statement::ComptimeFn(_)
            ) {
                continue;
            }
            // Top-level let/var: the global is declared zeroed above (so other
            // functions can read it), but its initializer STORE must run at this
            // source position. Hoisting every initializer to the top of main
            // reordered side-effecting inits (e.g. `let _r = run_heavy()` printed
            // before the banner). Mirrors Cranelift's in-place `compile_var_assign`.
            if let Statement::VarDecl(d) = stmt {
                if let Some(ref v) = d.value {
                    if matches!(v, Expr::Spawn(_, _) | Expr::SpawnThread(_, _)) {
                        self.spawn_vars.insert(d.name.clone());
                    }
                    if let Some((ty, _)) = self.global_vars.get(&d.name).cloned() {
                        let (val, vty) = if let (Some(Type::List(inner)), Expr::List(items)) =
                            (&d.ty, v)
                        {
                            let tag = match kind_of_type(&Some(Type::List(inner.clone()))) {
                                ValKind::List(t) => t,
                                ValKind::ListObj(_) => 4,
                                ValKind::NestedList(_) => 6,
                                _ => 0,
                            };
                            (self.emit_list_lit_tagged(items, tag)?.0, "ptr")
                        } else {
                            self.emit_expr(v)?
                        };
                        let val = self.cast_to(val, vty, ty)?;
                        let g = llvm_func_name(&d.name);
                        let _ = writeln!(
                            self.body,
                            "  store {} {}, ptr @{}, align 8",
                            ty, val, g
                        );
                    }
                }
                continue;
            }
            self.emit_stmt(stmt)?;
        }
        let _ = writeln!(self.body, "  ret i64 0");
        let _ = writeln!(self.body, "}}");
        let _ = writeln!(self.body);
        let _ = writeln!(self.body, "define i32 @main() {{");
        let _ = writeln!(self.body, "  %r = call i64 @bolide_main()");
        let _ = writeln!(self.body, "  %t = trunc i64 %r to i32");
        let _ = writeln!(self.body, "  ret i32 %t");
        let _ = writeln!(self.body, "}}");

        let main_body = std::mem::take(&mut self.body);

        // Closures created only in main
        self.drain_pending_closures()?;
        let late_lifts = std::mem::take(&mut self.body);

        let mut all = std::mem::take(&mut self.trampoline_ir);
        all.push_str(&emitted_fns);
        all.push_str(&late_lifts);
        all.push_str(&main_body);
        self.body = all;
        let _ = fns;
        Ok(())
    }

    fn drain_pending_closures(&mut self) -> Result<(), String> {
        // Nested closures enqueue while lifting — drain until stable
        for _ in 0..64 {
            let pending = std::mem::take(&mut self.pending_closures);
            if pending.is_empty() {
                return Ok(());
            }
            for pc in &pending {
                self.emit_lifted_closure(pc)?;
            }
        }
        Err("LLVM: too many nested closure lifting rounds".into())
    }

    fn emit_lifted_closure(&mut self, pc: &PendingClosure) -> Result<(), String> {
        self.locals.clear();
        self.local_kind.clear();
        self.mutable.clear();
        self.loop_stack.clear();
        self.catch_stack.clear();
        self.current_captures.clear();
        for (i, (name, kind)) in pc.captures.iter().enumerate() {
            self.current_captures.insert(name.clone(), i);
            self.local_kind.insert(name.clone(), kind.clone());
        }
        let ret = llvm_type_of(&pc.return_type);
        self.current_ret_ty = ret;
        self.current_ret_kind = kind_of_type(&pc.return_type);

        let mut params_s = String::from("ptr %env");
        for (i, p) in pc.params.iter().enumerate() {
            let ty = llvm_type_of(&Some(p.ty.clone()));
            let _ = write!(params_s, ", {} %arg_{}", ty, i);
        }
        let fname = llvm_func_name(&pc.name);
        let _ = writeln!(self.body, "define {} @{}({}) {{", ret, fname, params_s);
        let _ = writeln!(self.body, "entry:");

        // env slot
        let env_slot = self.fresh_local("__env");
        let _ = writeln!(self.body, "  {} = alloca ptr, align 8", env_slot);
        let _ = writeln!(self.body, "  store ptr %env, ptr {}, align 8", env_slot);
        self.current_env_slot = Some(env_slot.clone());

        // load captures into locals
        for (i, (name, kind)) in pc.captures.iter().enumerate() {
            let ty = kind_to_llvm(kind);
            let slot = self.fresh_local(name);
            let _ = writeln!(self.body, "  {} = alloca {}, align 8", slot, ty);
            let ep = self.fresh();
            let _ = writeln!(self.body, "  {} = load ptr, ptr {}, align 8", ep, env_slot);
            let fp = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = getelementptr i8, ptr {}, i64 {}",
                fp,
                ep,
                i * 8
            );
            if ty == "double" {
                let raw = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", raw, fp);
                let a = self.fresh();
                let _ = writeln!(self.body, "  {} = alloca i64, align 8", a);
                let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", raw, a);
                let d = self.fresh();
                let _ = writeln!(self.body, "  {} = load double, ptr {}, align 8", d, a);
                let _ = writeln!(self.body, "  store double {}, ptr {}, align 8", d, slot);
            } else if ty == "ptr" {
                let raw = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", raw, fp);
                let p = self.fresh();
                let _ = writeln!(self.body, "  {} = inttoptr i64 {} to ptr", p, raw);
                let _ = writeln!(self.body, "  store ptr {}, ptr {}, align 8", p, slot);
            } else {
                let raw = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", raw, fp);
                let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", raw, slot);
            }
            self.locals.insert(name.clone(), (slot, ty));
            self.mutable.insert(name.clone(), true);
        }

        for (i, p) in pc.params.iter().enumerate() {
            let ty = llvm_type_of(&Some(p.ty.clone()));
            let kind = kind_of_type(&Some(p.ty.clone()));
            let slot = self.fresh_local(&p.name);
            let _ = writeln!(self.body, "  {} = alloca {}, align 8", slot, ty);
            let _ = writeln!(
                self.body,
                "  store {} %arg_{}, ptr {}, align 8",
                ty, i, slot
            );
            self.locals.insert(p.name.clone(), (slot, ty));
            self.local_kind.insert(p.name.clone(), kind);
            self.mutable.insert(p.name.clone(), true);
        }

        let mut terminated = false;
        for s in &pc.body {
            if terminated {
                break;
            }
            terminated = self.emit_stmt(s)?;
        }
        if !terminated {
            match ret {
                "double" => {
                    let _ = writeln!(self.body, "  ret double 0.0");
                }
                "ptr" => {
                    let _ = writeln!(self.body, "  ret ptr null");
                }
                "void" => {
                    let _ = writeln!(self.body, "  ret void");
                }
                _ => {
                    let _ = writeln!(self.body, "  ret i64 0");
                }
            }
        }
        let _ = writeln!(self.body, "}}\n");
        self.current_env_slot = None;
        self.current_captures.clear();
        Ok(())
    }

    /// Create a closure object for a free function (trampoline).
    fn emit_func_as_closure(&mut self, func_name: &str) -> Result<(String, &'static str), String> {
        let tramp = if let Some(t) = self.trampolines.get(func_name) {
            t.clone()
        } else {
            let (param_tys, ret) = self.funcs.get(func_name).cloned().ok_or_else(|| {
                format!("cannot wrap unknown function '{}' as closure", func_name)
            })?;
            let tname = format!("__tramp_{}", llvm_func_name(func_name));
            // define trampoline in separate IR buffer (not mid-function)
            let mut def = String::new();
            let mut ps = String::from("ptr %env");
            for (i, t) in param_tys.iter().enumerate() {
                let _ = write!(ps, ", {} %a{}", t, i);
            }
            let _ = writeln!(def, "define {} @{}({}) {{", ret, tname, ps);
            let mut args = String::new();
            for (i, t) in param_tys.iter().enumerate() {
                if i > 0 {
                    args.push_str(", ");
                }
                let _ = write!(args, "{} %a{}", t, i);
            }
            let cal = llvm_func_name(func_name);
            if ret == "void" {
                let _ = writeln!(def, "  call void @{}({})", cal, args);
                let _ = writeln!(def, "  ret void");
            } else {
                let _ = writeln!(def, "  %r = call {} @{}({})", ret, cal, args);
                let _ = writeln!(def, "  ret {} %r", ret);
            }
            let _ = writeln!(def, "}}\n");
            self.trampoline_ir.push_str(&def);
            let mut tparams = vec!["ptr"];
            tparams.extend(param_tys.iter().copied());
            self.funcs.insert(tname.clone(), (tparams, ret));
            self.trampolines
                .insert(func_name.to_string(), tname.clone());
            tname
        };
        let clo = self.fresh();
        // tramp is a fully-mangled concrete symbol — do NOT re-mangle it
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_closure_new(ptr @{}, ptr null, i64 0, ptr null)",
            clo, tramp
        );
        Ok((clo, "ptr"))
    }

    fn emit_closure_expr(
        &mut self,
        params: &[Param],
        return_type: &Option<Type>,
        body: &[Statement],
    ) -> Result<(String, &'static str), String> {
        let free = free_variables(params, body);
        let mut captures = Vec::new();
        for name in free {
            if let Some(kind) = self.local_kind.get(&name).cloned() {
                captures.push((name, kind));
            } else if self.funcs.contains_key(&name) {
                // free function referenced — treat as capture after wrapping? 
                // Skip: resolve at call via global funcs. For `f` free name in body as call,
                // Ident(f) works if f is free function. Don't capture.
            }
        }
        self.closure_id += 1;
        let cname = format!("__closure_{}", self.closure_id);
        // register signature (env + params)
        let mut mparams = vec!["ptr"];
        for p in params {
            mparams.push(llvm_type_of(&Some(p.ty.clone())));
        }
        let ret = llvm_type_of(return_type);
        self.funcs.insert(cname.clone(), (mparams, ret));
        self.func_ret_kind
            .insert(cname.clone(), kind_of_type(return_type));

        self.pending_closures.push(PendingClosure {
            name: cname.clone(),
            params: params.to_vec(),
            return_type: return_type.clone(),
            body: body.to_vec(),
            captures: captures.clone(),
        });

        // allocate env
        let env_size = (captures.len() * 8) as i64;
        let env = if env_size > 0 {
            let e = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call ptr @bolide_alloc(i64 {})",
                e, env_size
            );
            for (i, (name, kind)) in captures.iter().enumerate() {
                let (slot, ty) = self
                    .locals
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("capture '{}' not in locals", name))?;
                let v = self.fresh();
                let _ = writeln!(self.body, "  {} = load {}, ptr {}, align 8", v, ty, slot);
                let packed = self.pack_as_i64(v, ty)?;
                let fp = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = getelementptr i8, ptr {}, i64 {}",
                    fp,
                    e,
                    i * 8
                );
                let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", packed, fp);
                let _ = kind;
            }
            e
        } else {
            "null".to_string()
        };

        let clo = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_closure_new(ptr @{}, ptr {}, i64 {}, ptr null)",
            clo,
            llvm_func_name(&cname),
            env,
            env_size
        );
        Ok((clo, "ptr"))
    }

    /// Call a raw function pointer value (bare ABI `fn(args) -> ret`), used for
    /// FuncSig params that carry bare function addresses.
    fn emit_raw_func_call(
        &mut self,
        fn_ptr: &str,
        args: &[Expr],
        ret_ty: &'static str,
    ) -> Result<(String, &'static str), String> {
        let mut arg_s = String::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                arg_s.push_str(", ");
            }
            let (v, ty) = self.emit_expr(a)?;
            let _ = write!(arg_s, "{} {}", ty, v);
        }
        if ret_ty == "void" {
            let _ = writeln!(self.body, "  call void {}({})", fn_ptr, arg_s);
            Ok(("0".into(), "i64"))
        } else {
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call {} {}({})",
                d, ret_ty, fn_ptr, arg_s
            );
            Ok((d, ret_ty))
        }
    }

    fn emit_closure_call(
        &mut self,
        clo: &str,
        args: &[Expr],
        ret_ty: &'static str,
    ) -> Result<(String, &'static str), String> {
        let fptr = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_closure_fn_ptr(ptr {})",
            fptr, clo
        );
        let env = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_closure_env_ptr(ptr {})",
            env, clo
        );
        let mut arg_s = format!("ptr {}", env);
        // Infer arg types as i64/ptr/double from expr
        for a in args {
            let (v, ty) = self.emit_expr(a)?;
            // For FuncSig params, values should already be ptr
            let _ = write!(arg_s, ", {} {}", ty, v);
        }
        if ret_ty == "void" {
            let _ = writeln!(
                self.body,
                "  call void {}({})",
                fptr, arg_s
            );
            Ok(("0".into(), "i64"))
        } else {
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call {} {}({})",
                d, ret_ty, fptr, arg_s
            );
            Ok((d, ret_ty))
        }
    }

    fn emit_constructor(&mut self, cname: &str, ci: &ClassInfo) -> Result<(), String> {
        self.locals.clear();
        self.local_kind.clear();
        self.mutable.clear();
        self.loop_stack.clear();
        self.current_ret_ty = "ptr";
        self.current_ret_kind = ValKind::Object(cname.to_string());

        let mut params_s = String::new();
        for (i, f) in ci.fields.iter().enumerate() {
            if i > 0 {
                params_s.push_str(", ");
            }
            let ty = type_llvm(&f.ty);
            let _ = write!(params_s, "{} %arg_{}", ty, i);
        }
        let fname = llvm_func_name(cname);
        let _ = writeln!(self.body, "define ptr @{}({}) {{", fname, params_s);
        let _ = writeln!(self.body, "entry:");
        let size = ci.size as i64;
        let obj = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @object_alloc(i64 {})",
            obj, size
        );
        let _ = writeln!(
            self.body,
            "  call void @object_set_class_tag(ptr {}, i64 {})",
            obj, ci.tag
        );
        for (i, f) in ci.fields.iter().enumerate() {
            let fp = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = getelementptr i8, ptr {}, i64 {}",
                fp, obj, f.offset
            );
            let ty = type_llvm(&f.ty);
            let _ = writeln!(
                self.body,
                "  store {} %arg_{}, ptr {}, align 8",
                ty, i, fp
            );
        }
        let _ = writeln!(self.body, "  ret ptr {}", obj);
        let _ = writeln!(self.body, "}}\n");
        Ok(())
    }

    fn emit_method(&mut self, cname: &str, m: &FuncDef) -> Result<(), String> {
        // Methods are free functions with leading self: Class_method(ptr self, ...)
        let mut m2 = m.clone();
        m2.name = method_full_name(cname, &m.name);
        // prepend self param if not already present as first named "self"
        if m2.params.first().map(|p| p.name.as_str()) != Some("self") {
            m2.params.insert(
                0,
                bolide_parser::Param {
                    name: "self".into(),
                    ty: Type::Custom(cname.to_string()),
                    mode: bolide_parser::ParamMode::Borrow,
                    default_value: None,
                    is_variadic: false,
                    is_kw_variadic: false,
                },
            );
        } else {
            // ensure self type is class
            m2.params[0].ty = Type::Custom(cname.to_string());
        }
        self.emit_function(&m2)
    }

    /// `spawn fn(args)` / `spawn thread fn(args)` → thread handle (ptr).
    fn emit_spawn(
        &mut self,
        name: &str,
        args: &[Expr],
        force_thread: bool,
    ) -> Result<(String, &'static str), String> {
        let (_, ret) = self.funcs.get(name).cloned().ok_or_else(|| {
            format!("LLVM backend: cannot spawn unknown function '{}'", name)
        })?;
        let suffix = match ret {
            "double" => "float",
            "ptr" => "ptr",
            _ => "int",
        };
        let _ = force_thread;
        let cal = llvm_func_name(name);

        if args.is_empty() {
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call ptr @bolide_thread_spawn_{}(ptr @{})",
                d, suffix, cal
            );
            return Ok((d, "ptr"));
        }

        // Build env buffer + a bare trampoline `__spawn_tramp_N(env)` that unpacks
        // the args and calls the target.
        let n = self.tmp;
        self.tmp += 1;
        let tname = format!("__spawn_tramp_{}", n);
        let size = args.len() * 8;
        let env = self.fresh();
        let _ = writeln!(self.body, "  {} = call ptr @bolide_alloc(i64 {})", env, size);
        let params = self.func_params.get(name).cloned().unwrap_or_default();
        for (i, a) in args.iter().enumerate() {
            let (v, ty) = self.emit_expr(a)?;
            let arg_ty = params
                .get(i)
                .map(|p| llvm_type_of(&Some(p.ty.clone())))
                .unwrap_or("i64");
            let packed = match arg_ty {
                "double" => {
                    let v = self.cast_to(v, ty, "double")?;
                    let b = self.fresh();
                    let _ = writeln!(self.body, "  {} = bitcast double {} to i64", b, v);
                    b
                }
                "ptr" => self.cast_to(v, ty, "i64")?,
                _ => self.cast_to(v, ty, "i64")?,
            };
            let gep = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = getelementptr i8, ptr {}, i64 {}",
                gep, env, i * 8
            );
            let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", packed, gep);
        }

        // trampoline: define <ret> @__spawn_tramp_N(ptr %env)
        let mut def = String::new();
        let _ = writeln!(def, "define {} @{}(ptr %env) {{", ret, tname);
        let _ = writeln!(def, "entry:");
        let mut arg_s = String::new();
        for (i, a) in args.iter().enumerate() {
            let arg_ty = params
                .get(i)
                .map(|p| llvm_type_of(&Some(p.ty.clone())))
                .unwrap_or("i64");
            if i > 0 {
                arg_s.push_str(", ");
            }
            let gep = self.fresh();
            let _ = writeln!(
                def,
                "  {} = getelementptr i8, ptr %env, i64 {}",
                gep,
                i * 8
            );
            let raw = self.fresh();
            let _ = writeln!(def, "  {} = load i64, ptr {}, align 8", raw, gep);
            let _ = a;
            let v = match arg_ty {
                "double" => {
                    let d2 = self.fresh();
                    let _ = writeln!(def, "  {} = bitcast i64 {} to double", d2, raw);
                    d2
                }
                "ptr" => {
                    let d2 = self.fresh();
                    let _ = writeln!(def, "  {} = inttoptr i64 {} to ptr", d2, raw);
                    d2
                }
                _ => raw,
            };
            let _ = write!(arg_s, "{} {}", arg_ty, v);
        }
        if ret == "void" {
            let _ = writeln!(def, "  call void @{}({})", cal, arg_s);
            let _ = writeln!(def, "  ret void");
        } else {
            let r = self.fresh();
            let _ = writeln!(def, "  {} = call {} @{}({})", r, ret, cal, arg_s);
            let _ = writeln!(def, "  ret {} {}", ret, r);
        }
        let _ = writeln!(def, "}}\n");
        self.trampoline_ir.push_str(&def);

        let d = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_thread_spawn_{}_with_env(ptr @{}, ptr {})",
            d, suffix, tname, env
        );
        Ok((d, "ptr"))
    }

    /// `async_fn(args)` → 协程 spawn，返回 Future 指针（供 await 等待）。
    /// 与 emit_spawn 结构相同，但用 `bolide_coroutine_spawn_*` 而非线程 spawn。
    fn emit_coroutine_spawn(
        &mut self,
        name: &str,
        args: &[Expr],
    ) -> Result<(String, &'static str), String> {
        let (_, ret) = self.funcs.get(name).cloned().ok_or_else(|| {
            format!("LLVM backend: cannot spawn unknown async function '{}'", name)
        })?;
        let suffix = match ret {
            "double" => "float",
            "ptr" => "ptr",
            _ => "int",
        };
        let cal = llvm_func_name(name);

        if args.is_empty() {
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call ptr @bolide_coroutine_spawn_{}(ptr @{})",
                d, suffix, cal
            );
            return Ok((d, "ptr"));
        }

        // 有参数：env 缓冲区 + 裸 trampoline `__coro_tramp_N(env)` 解包参数调用目标
        let n = self.tmp;
        self.tmp += 1;
        let tname = format!("__coro_tramp_{}", n);
        let size = args.len() * 8;
        let env = self.fresh();
        let _ = writeln!(self.body, "  {} = call ptr @bolide_alloc(i64 {})", env, size);
        let params = self.func_params.get(name).cloned().unwrap_or_default();
        for (i, a) in args.iter().enumerate() {
            let (v, ty) = self.emit_expr(a)?;
            let arg_ty = params
                .get(i)
                .map(|p| llvm_type_of(&Some(p.ty.clone())))
                .unwrap_or("i64");
            let packed = match arg_ty {
                "double" => {
                    let v = self.cast_to(v, ty, "double")?;
                    let b = self.fresh();
                    let _ = writeln!(self.body, "  {} = bitcast double {} to i64", b, v);
                    b
                }
                "ptr" => self.cast_to(v, ty, "i64")?,
                _ => self.cast_to(v, ty, "i64")?,
            };
            let gep = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = getelementptr i8, ptr {}, i64 {}",
                gep, env, i * 8
            );
            let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", packed, gep);
        }

        // trampoline: define <ret> @__coro_tramp_N(ptr %env)
        let mut def = String::new();
        let _ = writeln!(def, "define {} @{}(ptr %env) {{", ret, tname);
        let _ = writeln!(def, "entry:");
        let mut arg_s = String::new();
        for (i, a) in args.iter().enumerate() {
            let arg_ty = params
                .get(i)
                .map(|p| llvm_type_of(&Some(p.ty.clone())))
                .unwrap_or("i64");
            if i > 0 {
                arg_s.push_str(", ");
            }
            let gep = self.fresh();
            let _ = writeln!(
                def,
                "  {} = getelementptr i8, ptr %env, i64 {}",
                gep, i * 8
            );
            let raw = self.fresh();
            let _ = writeln!(def, "  {} = load i64, ptr {}, align 8", raw, gep);
            let _ = a;
            let v = match arg_ty {
                "double" => {
                    let d2 = self.fresh();
                    let _ = writeln!(def, "  {} = bitcast i64 {} to double", d2, raw);
                    d2
                }
                "ptr" => {
                    let d2 = self.fresh();
                    let _ = writeln!(def, "  {} = inttoptr i64 {} to ptr", d2, raw);
                    d2
                }
                _ => raw,
            };
            let _ = write!(arg_s, "{} {}", arg_ty, v);
        }
        if ret == "void" {
            let _ = writeln!(def, "  call void @{}({})", cal, arg_s);
            let _ = writeln!(def, "  ret void");
        } else {
            let r = self.fresh();
            let _ = writeln!(def, "  {} = call {} @{}({})", r, ret, cal, arg_s);
            let _ = writeln!(def, "  ret {} {}", ret, r);
        }
        let _ = writeln!(def, "}}\n");
        self.trampoline_ir.push_str(&def);

        let d = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_coroutine_spawn_{}_with_env(ptr @{}, ptr {})",
            d, suffix, tname, env
        );
        Ok((d, "ptr"))
    }

    /// `await expr` → resolve the future/handle's result.
    fn emit_await(&mut self, inner: &Expr) -> Result<(String, &'static str), String> {
        // Determine if this is a spawn handle (thread_join) or a coroutine future.
        let is_spawn = matches!(inner, Expr::Spawn(_, _) | Expr::SpawnThread(_, _))
            || matches!(inner, Expr::Ident(n) if self.spawn_vars.contains(n));
        let (future, fty) = self.emit_expr(inner)?;
        let future = self.cast_to(future, fty, "ptr")?;

        // Result kind: from the spawned function's return kind when known.
        let ret_kind = match inner {
            Expr::Spawn(name, _) | Expr::SpawnThread(name, _) => {
                self.func_ret_kind.get(name).cloned()
            }
            Expr::Ident(n) => self
                .global_vars
                .get(n)
                .map(|(_, k)| k.clone())
                .or_else(|| self.local_kind.get(n).cloned()),
            _ => None,
        };
        self.emit_await_value(&future, ret_kind, is_spawn)
    }

    /// Await an already-emitted handle/future value.
    fn emit_await_value(
        &mut self,
        future: &str,
        ret_kind: Option<ValKind>,
        is_spawn: bool,
    ) -> Result<(String, &'static str), String> {
        let is_float = matches!(ret_kind, Some(ValKind::Float));
        let is_ptr = matches!(
            ret_kind,
            Some(
                ValKind::Str
                    | ValKind::Ptr
                    | ValKind::BigInt
                    | ValKind::Object(_)
                    | ValKind::List(_)
                    | ValKind::Dict
            )
        );
        let d = self.fresh();
        if is_spawn {
            let suffix = if is_float {
                "float"
            } else if is_ptr {
                "ptr"
            } else {
                "int"
            };
            let rty = if is_float { "double" } else if is_ptr { "ptr" } else { "i64" };
            let _ = writeln!(
                self.body,
                "  {} = call {} @bolide_thread_join_{}(ptr {})",
                d, rty, suffix, future
            );
            Ok((d, rty))
        } else {
            let _ = writeln!(
                self.body,
                "  {} = call i64 @bolide_coroutine_await_int(ptr {})",
                d, future
            );
            Ok((d, "i64"))
        }
    }

    /// Classify FuncSig params that receive closure objects (capturing lambdas);
    /// params not listed are treated as raw function pointers.
    fn collect_funcsig_closure_params(&mut self, program: &Program) {
        // 1) collect signatures
        for stmt in &program.statements {
            match stmt {
                Statement::FuncDef(f) => {
                    self.func_params.insert(f.name.clone(), f.params.clone());
                }
                Statement::ClassDef(c) => {
                    for m in &c.methods {
                        let mut params = vec![Param {
                            name: "self".to_string(),
                            ty: Type::Custom(c.name.clone()),
                            mode: bolide_parser::ParamMode::Borrow,
                            default_value: None,
                            is_variadic: false,
                            is_kw_variadic: false,
                        }];
                        params.extend(m.params.iter().cloned());
                        self.func_params
                            .insert(method_full_name(&c.name, &m.name), params);
                    }
                }
                _ => {}
            }
        }
        // 2) scan call sites; a FuncSig param receives a closure if any call passes
        //    a closure literal or a closure-returning expression to it
        let mut out: HashMap<String, HashSet<usize>> = HashMap::new();
        for _ in 0..8 {
            let before = out.clone();
            for stmt in &program.statements {
                match stmt {
                    Statement::FuncDef(f) => {
                        self.scan_closure_usage(&f.body, &mut out);
                    }
                    Statement::ClassDef(c) => {
                        for m in &c.methods {
                            self.scan_closure_usage(&m.body, &mut out);
                        }
                    }
                    Statement::VarDecl(d) => {
                        if let Some(v) = &d.value {
                            self.scan_closure_usage_expr(v, &mut out);
                        }
                    }
                    // top-level executable statements (the main body) too
                    other => {
                        self.scan_closure_usage(std::slice::from_ref(other), &mut out);
                    }
                }
            }
            if out == before {
                break;
            }
        }
        self.funcsig_closure_params = out;
    }

    fn scan_closure_usage(&mut self, stmts: &[Statement], out: &mut HashMap<String, HashSet<usize>>) {
        for s in stmts {
            match s {
                Statement::Expr(e) => self.scan_closure_usage_expr(e, out),
                Statement::Return(Some(e)) => self.scan_closure_usage_expr(e, out),
                Statement::VarDecl(d) => {
                    if let Some(v) = &d.value {
                        self.scan_closure_usage_expr(v, out);
                    }
                }
                Statement::Assign(a) => {
                    self.scan_closure_usage_expr(&a.value, out);
                }
                Statement::If(i) => {
                    self.scan_closure_usage(&i.then_body, out);
                    for (_, b) in &i.elif_branches {
                        self.scan_closure_usage(b, out);
                    }
                    if let Some(b) = &i.else_body {
                        self.scan_closure_usage(b, out);
                    }
                }
                Statement::While(w) => self.scan_closure_usage(&w.body, out),
                Statement::For(f) => self.scan_closure_usage(&f.body, out),
                Statement::Match(m) => {
                    for a in &m.arms {
                        self.scan_closure_usage(&a.body, out);
                    }
                }
                Statement::Try(t) => {
                    self.scan_closure_usage(&t.try_body, out);
                    for c in &t.catch_clauses {
                        self.scan_closure_usage(&c.body, out);
                    }
                }
                _ => {}
            }
        }
    }

    fn scan_closure_usage_expr(
        &mut self,
        e: &Expr,
        out: &mut HashMap<String, HashSet<usize>>,
    ) {
        match e {
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    if let Some(params) = self.func_params.get(name).cloned() {
                        for (i, arg) in args.iter().enumerate() {
                            if let Some(p) = params.get(i) {
                                if matches!(p.ty, Type::Func | Type::FuncSig(_, _)) {
                                    if self.expr_is_closure(arg) {
                                        out.entry(name.to_string())
                                            .or_default()
                                            .insert(i);
                                    }
                                }
                            }
                        }
                    }
                }
                for a in args {
                    self.scan_closure_usage_expr(a, out);
                }
                self.scan_closure_usage_expr(callee, out);
            }
            Expr::Closure { body, .. } => {
                self.scan_closure_usage(body, out);
            }
            Expr::List(items) | Expr::Tuple(items) => {
                for it in items {
                    self.scan_closure_usage_expr(it, out);
                }
            }
            Expr::Dict(pairs) => {
                for (k, v) in pairs {
                    self.scan_closure_usage_expr(k, out);
                    self.scan_closure_usage_expr(v, out);
                }
            }
            Expr::BinOp(l, _, r) => {
                self.scan_closure_usage_expr(l, out);
                self.scan_closure_usage_expr(r, out);
            }
            Expr::UnaryOp(_, x)
            | Expr::Member(x, _)
            | Expr::Index(x, _)
            | Expr::Await(x)
            | Expr::Propagate(x)
            | Expr::Raise(x) => self.scan_closure_usage_expr(x, out),
            _ => {}
        }
    }

    /// Is an expression a closure object (lambda literal or a call returning a func)?
    fn expr_is_closure(&self, e: &Expr) -> bool {
        match e {
            Expr::Closure { .. } => true,
            Expr::Call(c, _) => {
                if let Expr::Ident(n) = c.as_ref() {
                    if let Some(k) = self.func_ret_kind.get(n) {
                        return matches!(k, ValKind::Closure);
                    }
                }
                false
            }
            Expr::Ident(n) => self
                .local_kind
                .get(n)
                .map(|k| matches!(k, ValKind::Closure))
                .or_else(|| {
                    self.global_vars
                        .get(n)
                        .map(|(_, k)| matches!(k, ValKind::Closure))
                })
                .unwrap_or(false),
            _ => false,
        }
    }

    fn emit_function(&mut self, f: &FuncDef) -> Result<(), String> {
        self.locals.clear();
        self.local_kind.clear();
        self.mutable.clear();
        self.loop_stack.clear();
        self.catch_stack.clear();
        // ref 参数表是「当前函数」的局部状态：槽名只在当前函数里有效，
        // 不清除会让上个函数的 addr 槽泄漏进下个函数（use of undefined value）。
        self.ref_params.clear();
        self.ref_param_val_ty.clear();
        self.ref_param_addr_slot.clear();
        let ret = llvm_type_of(&f.return_type);
        self.current_ret_ty = ret;
        self.current_ret_kind = kind_of_type(&f.return_type);

        let mut params_s = String::new();
        for (i, p) in f.params.iter().enumerate() {
            if i > 0 {
                params_s.push_str(", ");
            }
            // ref 参数：签名上是「指向调用方变量的指针」，被调方原地读写。
            let ty = if p.mode == bolide_parser::ParamMode::Ref {
                "ptr"
            } else {
                llvm_type_of(&Some(p.ty.clone()))
            };
            let _ = write!(params_s, "{} %arg_{}", ty, i);
        }
        let fname = llvm_func_name(&f.name);
        let _ = writeln!(self.body, "define {} @{}({}) {{", ret, fname, params_s);
        let _ = writeln!(self.body, "entry:");

        for (i, p) in f.params.iter().enumerate() {
            if p.mode == bolide_parser::ParamMode::Ref {
                // ref：`%arg_i` 是调用方变量的地址。存进 addr 槽；读/写都经它解引用。
                let addr_slot = self.fresh_local(&p.name);
                let _ = writeln!(self.body, "  {} = alloca ptr, align 8", addr_slot);
                let _ = writeln!(
                    self.body,
                    "  store ptr %arg_{}, ptr {}, align 8",
                    i, addr_slot
                );
                self.locals
                    .insert(p.name.clone(), (addr_slot.clone(), "ptr"));
                self.local_kind
                    .insert(p.name.clone(), kind_of_type(&Some(p.ty.clone())));
                self.mutable.insert(p.name.clone(), true);
                self.ref_params.insert(p.name.clone());
                self.ref_param_val_ty
                    .insert(p.name.clone(), llvm_type_of(&Some(p.ty.clone())));
                self.ref_param_addr_slot
                    .insert(p.name.clone(), addr_slot);
                continue;
            }
            let ty = llvm_type_of(&Some(p.ty.clone()));
            let mut kind = kind_of_type(&Some(p.ty.clone()));
            // FuncSig/Func params are raw function pointers (bare address) UNLESS
            // the scan found a closure object being passed to them — matching
            // Cranelift, so web/GUI callbacks forwarded to a `func(*c_void)` extern
            // are passed as their bare address and invoked directly by the runtime.
            if matches!(p.ty, Type::Func | Type::FuncSig(_, _))
                && !self
                    .funcsig_closure_params
                    .get(&f.name)
                    .map(|s| s.contains(&i))
                    .unwrap_or(false)
            {
                kind = ValKind::RawFunc;
            }
            let slot = self.fresh_local(&p.name);
            let _ = writeln!(self.body, "  {} = alloca {}, align 8", slot, ty);
            let _ = writeln!(
                self.body,
                "  store {} %arg_{}, ptr {}, align 8",
                ty, i, slot
            );
            self.locals.insert(p.name.clone(), (slot, ty));
            self.local_kind.insert(p.name.clone(), kind);
            self.mutable.insert(p.name.clone(), true);
        }

        let mut terminated = false;
        for s in &f.body {
            if terminated {
                break;
            }
            terminated = self.emit_stmt(s)?;
        }
        if !terminated {
            match ret {
                "double" => {
                    let _ = writeln!(self.body, "  ret double 0.0");
                }
                "ptr" => {
                    let _ = writeln!(self.body, "  ret ptr null");
                }
                "void" => {
                    let _ = writeln!(self.body, "  ret void");
                }
                _ => {
                    let _ = writeln!(self.body, "  ret i64 0");
                }
            }
        }
        let _ = writeln!(self.body, "}}");
        let _ = writeln!(self.body);
        Ok(())
    }

    fn emit_stmt(&mut self, stmt: &Statement) -> Result<bool, String> {
        match stmt {
            Statement::VarDecl(d) => {
                self.emit_var_decl(d)?;
                Ok(false)
            }
            Statement::Assign(a) => {
                self.emit_assign(a)?;
                Ok(false)
            }
            Statement::Expr(e) => {
                let _ = self.emit_expr(e)?;
                Ok(false)
            }
            Statement::Return(None) => {
                // bare `return;` in a function with a declared return type falls
                // back to the type default (matches the implicit fallthrough)
                match self.current_ret_ty {
                    "double" => {
                        let _ = writeln!(self.body, "  ret double 0.0");
                    }
                    "ptr" => {
                        let _ = writeln!(self.body, "  ret ptr null");
                    }
                    "void" => {
                        let _ = writeln!(self.body, "  ret void");
                    }
                    _ => {
                        let _ = writeln!(self.body, "  ret i64 0");
                    }
                }
                Ok(true)
            }
            Statement::Return(Some(e)) => {
                let (v, ty) = self.emit_expr(e)?;
                let v = self.cast_to(v, ty, self.current_ret_ty)?;
                let _ = writeln!(self.body, "  ret {} {}", self.current_ret_ty, v);
                Ok(true)
            }
            Statement::If(i) => self.emit_if(i),
            Statement::While(w) => {
                self.emit_while(w)?;
                Ok(false)
            }
            Statement::For(f) => {
                self.emit_for(f)?;
                Ok(false)
            }
            Statement::Break => {
                let (_, brk) = self
                    .loop_stack
                    .last()
                    .ok_or("break outside loop")?
                    .clone();
                let _ = writeln!(self.body, "  br label %{}", brk);
                Ok(true)
            }
            Statement::Continue => {
                let (cont, _) = self
                    .loop_stack
                    .last()
                    .ok_or("continue outside loop")?
                    .clone();
                let _ = writeln!(self.body, "  br label %{}", cont);
                Ok(true)
            }
            Statement::Match(m) => self.emit_match(m),
            Statement::Throw(e) => {
                self.emit_throw(e)?;
                Ok(true)
            }
            Statement::Try(t) => self.emit_try(t),
            Statement::ClassDef(_)
            | Statement::TraitDef(_)
            | Statement::TraitImpl(_)
            | Statement::With(_)
            | Statement::Yield(_)
            | Statement::SpawnSelect(_)
            | Statement::Pool(_)
            | Statement::Select(_)
            | Statement::AwaitScope(_) => Ok(false), // already desugared / definitions
            _ => Ok(false),
        }
    }

    fn emit_var_decl(&mut self, d: &VarDecl) -> Result<(), String> {
        let kind = if let Some(ref t) = d.ty {
            kind_of_type(&Some(t.clone()))
        } else if let Some(ref v) = d.value {
            self.infer_kind(v)
        } else {
            ValKind::Int
        };
        let ty = kind_to_llvm(&kind);
        let slot = self.fresh_local(&d.name);
        let _ = writeln!(self.body, "  {} = alloca {}, align 8", slot, ty);
        // a local holding a spawn handle → await should use thread_join
        if let Some(ref v) = d.value {
            if matches!(v, Expr::Spawn(_, _) | Expr::SpawnThread(_, _)) {
                self.spawn_vars.insert(d.name.clone());
            }
        }
        if let Some(ref v) = d.value {
            // `let/var x: list<dict<..>> = [...]` — use the declared element tag so
            // e.g. an empty `[]` is a dict-tagged list and pushed dicts are read
            // back as dicts.
            let (val, vty) = if let (Some(Type::List(inner)), Expr::List(items)) = (&d.ty, v) {
                let tag = match kind_of_type(&Some(Type::List(inner.clone()))) {
                    ValKind::List(t) => t,
                    ValKind::ListObj(_) => 4,
                    ValKind::NestedList(_) => 6,
                    _ => 0,
                };
                (self.emit_list_lit_tagged(items, tag)?.0, "ptr")
            } else {
                self.emit_expr(v)?
            };
            let val = self.cast_to(val, vty, ty)?;
            let _ = writeln!(self.body, "  store {} {}, ptr {}, align 8", ty, val, slot);
        } else {
            match ty {
                "double" => {
                    let _ = writeln!(self.body, "  store double 0.0, ptr {}, align 8", slot);
                }
                "ptr" => {
                    let _ = writeln!(self.body, "  store ptr null, ptr {}, align 8", slot);
                }
                _ => {
                    let _ = writeln!(self.body, "  store i64 0, ptr {}, align 8", slot);
                }
            }
        }
        self.locals.insert(d.name.clone(), (slot, ty));
        self.local_kind.insert(d.name.clone(), kind);
        self.mutable.insert(d.name.clone(), d.mutable);
        Ok(())
    }

    fn emit_assign(&mut self, a: &Assign) -> Result<(), String> {
        match &a.target {
            Expr::Ident(name) => {
                // ref 参数赋值：经调用方变量的地址原地写回（无局部副本）
                if self.ref_params.contains(name) {
                    let addr_slot = self.ref_param_addr_slot.get(name).cloned().unwrap();
                    let val_ty = self.ref_param_val_ty.get(name).copied().unwrap_or("i64");
                    let (val, vty) = self.emit_expr(&a.value)?;
                    let val = self.cast_to(val, vty, val_ty)?;
                    let addr = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = load ptr, ptr {}, align 8",
                        addr, addr_slot
                    );
                    let _ = writeln!(
                        self.body,
                        "  store {} {}, ptr {}, align 8",
                        val_ty, val, addr
                    );
                    self.local_kind
                        .insert(name.clone(), self.infer_kind(&a.value));
                    return Ok(());
                }
                if let Some((slot, ty)) = self.locals.get(name).cloned() {
                    if self.mutable.get(name) == Some(&false) {
                        return Err(format!(
                            "Cannot assign to immutable binding '{}'; use var",
                            name
                        ));
                    }
                    let (val, vty) = self.emit_expr(&a.value)?;
                    let val = self.cast_to(val, vty, ty)?;
                    let _ = writeln!(self.body, "  store {} {}, ptr {}, align 8", ty, val, slot);
                    // update kind if RHS is list
                    let k = self.infer_kind(&a.value);
                    self.local_kind.insert(name.clone(), k);
                    return Ok(());
                }
                if let Some((ty, _)) = self.global_vars.get(name).cloned() {
                    let (val, vty) = self.emit_expr(&a.value)?;
                    let val = self.cast_to(val, vty, ty)?;
                    let g = llvm_func_name(name);
                    let _ = writeln!(self.body, "  store {} {}, ptr @{}, align 8", ty, val, g);
                    return Ok(());
                }
                Err(format!("Undefined variable: {}", name))
            }
            Expr::Index(base, idx) => {
                let base_kind = self.infer_kind(base);
                let (base_v, _) = self.emit_expr(base)?;
                let (ix, ixty) = self.emit_expr(idx)?;
                let (val, vty) = self.emit_expr(&a.value)?;
                match base_kind {
                    ValKind::Dict => {
                        let key = self.pack_as_i64(ix, ixty)?;
                        let k = self.infer_kind(&a.value);
                        let dyn_ = self.wrap_as_dynamic(val, vty, &k)?;
                        let packed = self.pack_as_i64(dyn_, "ptr")?;
                        let _ = writeln!(
                            self.body,
                            "  call void @bolide_dict_set(ptr {}, i64 {}, i64 {})",
                            base_v, key, packed
                        );
                        Ok(())
                    }
                    _ => {
                        let ix = self.cast_to(ix, ixty, "i64")?;
                        let packed = self.pack_list_value(val, vty, base)?;
                        let tag = match self.infer_kind(base) {
                            ValKind::List(t) => t,
                            _ => 0,
                        };
                        self.emit_list_set_inline(&base_v, &ix, &packed, tag)?;
                        Ok(())
                    }
                }
            }
            Expr::Member(base, field) => {
                let class_name = match self.infer_kind(base) {
                    ValKind::Object(n) => n,
                    other => {
                        return Err(format!(
                            "LLVM backend: field assign on non-object {:?}",
                            other
                        ))
                    }
                };
                let ci = self
                    .classes
                    .get(&class_name)
                    .ok_or_else(|| format!("Unknown class '{}'", class_name))?
                    .clone();
                let off = field_offset(&ci, field)
                    .ok_or_else(|| format!("Unknown field '{}.{}'", class_name, field))?;
                let fty = field_type(&ci, field).unwrap();
                let lty = type_llvm(fty);
                let (obj, _) = self.emit_expr(base)?;
                let (val, vty) = self.emit_expr(&a.value)?;
                let val = self.cast_to(val, vty, lty)?;
                let fp = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = getelementptr i8, ptr {}, i64 {}",
                    fp, obj, off
                );
                let _ = writeln!(self.body, "  store {} {}, ptr {}, align 8", lty, val, fp);
                Ok(())
            }
            _ => Err("LLVM backend: unsupported assignment target".into()),
        }
    }

    fn snapshot_scopes(
        &self,
    ) -> (
        HashMap<String, (String, &'static str)>,
        HashMap<String, ValKind>,
        HashMap<String, bool>,
    ) {
        (self.locals.clone(), self.local_kind.clone(), self.mutable.clone())
    }

    fn restore_scopes(
        &mut self,
        s: (
            HashMap<String, (String, &'static str)>,
            HashMap<String, ValKind>,
            HashMap<String, bool>,
        ),
    ) {
        self.locals = s.0;
        self.local_kind = s.1;
        self.mutable = s.2;
    }

    fn emit_if(&mut self, i: &bolide_parser::IfStmt) -> Result<bool, String> {
        let (cond, cty) = self.emit_expr(&i.condition)?;
        let cond_i1 = self.to_i1(cond, cty)?;
        let then_l = self.fresh_label("then");
        let else_l = self.fresh_label("else");
        let end_l = self.fresh_label("endif");
        let _ = writeln!(
            self.body,
            "  br i1 {}, label %{}, label %{}",
            cond_i1, then_l, else_l
        );

        // Block scoping: branch-local `let` shadows must not leak out.
        let saved = self.snapshot_scopes();
        let _ = writeln!(self.body, "{}:", then_l);
        let mut then_term = false;
        for s in &i.then_body {
            if then_term {
                break;
            }
            then_term = self.emit_stmt(s)?;
        }
        if !then_term {
            let _ = writeln!(self.body, "  br label %{}", end_l);
        }
        self.restore_scopes(saved);

        let _ = writeln!(self.body, "{}:", else_l);
        let mut else_term = false;
        if !i.elif_branches.is_empty() {
            let (c0, b0) = &i.elif_branches[0];
            let rest = i.elif_branches[1..].to_vec();
            let nested = bolide_parser::IfStmt {
                condition: c0.clone(),
                then_body: b0.clone(),
                elif_branches: rest,
                else_body: i.else_body.clone(),
            };
            else_term = self.emit_if(&nested)?;
        } else if let Some(ref eb) = i.else_body {
            let saved2 = self.snapshot_scopes();
            for s in eb {
                if else_term {
                    break;
                }
                else_term = self.emit_stmt(s)?;
            }
            self.restore_scopes(saved2);
        }
        if !else_term {
            let _ = writeln!(self.body, "  br label %{}", end_l);
        }

        // Both arms terminated → no join block (empty labels are invalid LLVM IR)
        if then_term && else_term {
            return Ok(true);
        }
        let _ = writeln!(self.body, "{}:", end_l);
        Ok(false)
    }

    fn emit_while(&mut self, w: &WhileStmt) -> Result<(), String> {
        let head = self.fresh_label("while");
        let body = self.fresh_label("while_body");
        let end = self.fresh_label("while_end");
        self.loop_stack.push((head.clone(), end.clone()));
        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", head);
        let (cond, cty) = self.emit_expr(&w.condition)?;
        let cond_i1 = self.to_i1(cond, cty)?;
        let _ = writeln!(
            self.body,
            "  br i1 {}, label %{}, label %{}",
            cond_i1, body, end
        );
        // Loop-body scoping (mirrors JIT enter_scope/leave_scope): a `let`
        // declared inside the body shadows an outer binding only within the body.
        let saved = self.snapshot_scopes();
        let _ = writeln!(self.body, "{}:", body);
        for s in &w.body {
            if self.emit_stmt(s)? {
                self.loop_stack.pop();
                // still need end label for break targets
                let _ = writeln!(self.body, "{}:", end);
                self.restore_scopes(saved);
                return Ok(());
            }
        }
        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", end);
        self.loop_stack.pop();
        self.restore_scopes(saved);
        Ok(())
    }

    fn emit_for(&mut self, f: &ForStmt) -> Result<(), String> {
        let var = f
            .vars
            .first()
            .cloned()
            .ok_or("for-loop missing variable")?;
        // range(...)
        if let Expr::Call(callee, args) = &f.iter {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "range" {
                    return self.emit_for_range(&var, args, &f.body);
                }
            }
        }
        // for k[, v] in dict
        if matches!(self.infer_kind(&f.iter), ValKind::Dict) {
            return self.emit_for_dict(&f.vars, &f.iter, &f.body);
        }
        // for x in iterator (class with next())
        if let ValKind::Object(ref cn) = self.infer_kind(&f.iter) {
            if self
                .classes
                .get(cn)
                .map(|c| c.methods.contains_key("next"))
                .unwrap_or(false)
            {
                return self.emit_for_iter(&var, &f.iter, cn, &f.body);
            }
        }
        // for x in list
        self.emit_for_list(&var, &f.iter, &f.body)
    }

    fn emit_for_dict(
        &mut self,
        vars: &[String],
        iter: &Expr,
        body: &[Statement],
    ) -> Result<(), String> {
        let key_var = vars
            .first()
            .cloned()
            .ok_or("for-dict missing key variable")?;
        let val_var = vars.get(1).cloned();
        let saved = self.snapshot_scopes();
        let (dict, _) = self.emit_expr(iter)?;
        // keys list
        let keys = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_dict_keys(ptr {})",
            keys, dict
        );
        // iterate keys as list of int/str
        let tag: u8 = 0; // packed as i64
        let idx_slot = self.fresh_local("__di");
        let _ = writeln!(self.body, "  {} = alloca i64, align 8", idx_slot);
        let _ = writeln!(self.body, "  store i64 0, ptr {}, align 8", idx_slot);
        let lenv = self.emit_list_len_inline(&keys)?;
        let len_slot = self.fresh_local("__dlen");
        let _ = writeln!(self.body, "  {} = alloca i64, align 8", len_slot);
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", lenv, len_slot);

        let k_slot = self.fresh_local(&key_var);
        let _ = writeln!(self.body, "  {} = alloca i64, align 8", k_slot);
        self.locals
            .insert(key_var.clone(), (k_slot.clone(), "i64"));
        self.local_kind.insert(key_var.clone(), ValKind::Int);
        self.mutable.insert(key_var.clone(), true);

        let v_slot = if let Some(ref vv) = val_var {
            let s = self.fresh_local(vv);
            let _ = writeln!(self.body, "  {} = alloca i64, align 8", s);
            self.locals.insert(vv.clone(), (s.clone(), "i64"));
            self.local_kind.insert(vv.clone(), ValKind::Int);
            self.mutable.insert(vv.clone(), true);
            Some(s)
        } else {
            None
        };

        let head = self.fresh_label("ford");
        let body_l = self.fresh_label("ford_body");
        let cont = self.fresh_label("ford_cont");
        let end_l = self.fresh_label("ford_end");
        self.loop_stack.push((cont.clone(), end_l.clone()));

        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", head);
        let i = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", i, idx_slot);
        let n = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", n, len_slot);
        let cmp = self.fresh();
        let _ = writeln!(self.body, "  {} = icmp slt i64 {}, {}", cmp, i, n);
        let _ = writeln!(
            self.body,
            "  br i1 {}, label %{}, label %{}",
            cmp, body_l, end_l
        );
        let _ = writeln!(self.body, "{}:", body_l);
        let raw_k = self.emit_list_get_inline(&keys, &i, tag)?;
        let _ = writeln!(
            self.body,
            "  store i64 {}, ptr {}, align 8",
            raw_k, k_slot
        );
        if let Some(ref vs) = v_slot {
            let val = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call i64 @bolide_dict_get(ptr {}, i64 {})",
                val, dict, raw_k
            );
            let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", val, vs);
        }
        for s in body {
            if self.emit_stmt(s)? {
                self.loop_stack.pop();
                let _ = writeln!(self.body, "{}:", end_l);
                self.restore_scopes(saved);
                return Ok(());
            }
        }
        let _ = writeln!(self.body, "  br label %{}", cont);
        let _ = writeln!(self.body, "{}:", cont);
        let i2 = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", i2, idx_slot);
        let i3 = self.fresh();
        let _ = writeln!(self.body, "  {} = add i64 {}, 1", i3, i2);
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", i3, idx_slot);
        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", end_l);
        self.loop_stack.pop();
        self.restore_scopes(saved);
        Ok(())
    }

    /// for x in gen  — desugared generators / Iterator classes: next() -> Option
    fn emit_for_iter(
        &mut self,
        var: &str,
        iter: &Expr,
        class_name: &str,
        body: &[Statement],
    ) -> Result<(), String> {
        let saved = self.snapshot_scopes();
        let (it, _) = self.emit_expr(iter)?;
        let it_slot = self.fresh_local("__it");
        let _ = writeln!(self.body, "  {} = alloca ptr, align 8", it_slot);
        let _ = writeln!(self.body, "  store ptr {}, ptr {}, align 8", it, it_slot);

        // Infer element kind from next() return: Option → field payload
        let next_full = method_full_name(class_name, "next");
        let elem_kind = ValKind::Int; // refined below if possible
        let mut elem_kind = elem_kind;
        if let Some(m) = self
            .classes
            .get(class_name)
            .and_then(|c| c.methods.get("next"))
        {
            if let Some(Type::Adt(n, args)) = &m.return_type {
                if n == "Option" && !args.is_empty() {
                    elem_kind = kind_of_type(&Some(args[0].clone()));
                } else if n != "Option" {
                    // might be monomorph name stored differently
                    elem_kind = kind_of_type(&m.return_type);
                }
            } else {
                elem_kind = kind_of_type(&m.return_type);
                // if next returns ptr Option, keep Int as default for payload
                if matches!(elem_kind, ValKind::Adt(_) | ValKind::Ptr) {
                    elem_kind = ValKind::Int;
                }
            }
        }
        let elem_ty = kind_to_llvm(&elem_kind);
        let var_slot = self.fresh_local(var);
        let _ = writeln!(self.body, "  {} = alloca {}, align 8", var_slot, elem_ty);
        self.locals
            .insert(var.to_string(), (var_slot.clone(), elem_ty));
        self.local_kind.insert(var.to_string(), elem_kind.clone());
        self.mutable.insert(var.to_string(), true);

        let head = self.fresh_label("fori");
        let body_l = self.fresh_label("fori_body");
        let cont = self.fresh_label("fori_cont");
        let end_l = self.fresh_label("fori_end");
        self.loop_stack.push((cont.clone(), end_l.clone()));

        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", head);
        let itv = self.fresh();
        let _ = writeln!(self.body, "  {} = load ptr, ptr {}, align 8", itv, it_slot);
        // call next
        let opt = self.fresh();
        let fname = llvm_func_name(&next_full);
        let _ = writeln!(
            self.body,
            "  {} = call ptr @{}(ptr {})",
            opt, fname, itv
        );
        // tag at offset 0: 0=Some, 1=None for Option enum order
        let tagp = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr i8, ptr {}, i64 0",
            tagp, opt
        );
        let tag = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", tag, tagp);
        let is_some = self.fresh();
        // Option: Some=0, None=1
        let _ = writeln!(self.body, "  {} = icmp eq i64 {}, 0", is_some, tag);
        let _ = writeln!(
            self.body,
            "  br i1 {}, label %{}, label %{}",
            is_some, body_l, end_l
        );

        let _ = writeln!(self.body, "{}:", body_l);
        // load payload at offset 8
        let fp = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr i8, ptr {}, i64 8",
            fp, opt
        );
        let raw = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = load {}, ptr {}, align 8",
            raw, elem_ty, fp
        );
        let _ = writeln!(
            self.body,
            "  store {} {}, ptr {}, align 8",
            elem_ty, raw, var_slot
        );
        for s in body {
            if self.emit_stmt(s)? {
                self.loop_stack.pop();
                let _ = writeln!(self.body, "{}:", end_l);
                self.restore_scopes(saved);
                return Ok(());
            }
        }
        let _ = writeln!(self.body, "  br label %{}", cont);
        let _ = writeln!(self.body, "{}:", cont);
        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", end_l);
        self.loop_stack.pop();
        self.restore_scopes(saved);
        let _ = next_full;
        Ok(())
    }

    fn emit_for_range(
        &mut self,
        var: &str,
        args: &[Expr],
        body: &[Statement],
    ) -> Result<(), String> {
        let (start_e, end_e, step_e) = match args.len() {
            1 => (Expr::Int(0), args[0].clone(), Expr::Int(1)),
            2 => (args[0].clone(), args[1].clone(), Expr::Int(1)),
            3 => (args[0].clone(), args[1].clone(), args[2].clone()),
            _ => return Err("range expects 1, 2, or 3 arguments".into()),
        };
        // 常量步长为 0 → 死循环，编译期拒绝（与 JIT 一致）
        if let Expr::Int(0) = &step_e {
            return Err("range() step cannot be 0".into());
        }
        // 步长符号只在编译期常量时确定（对齐 JIT is_negative_step）；非零负数 → 降序
        let is_neg = matches!(&step_e, Expr::Int(n) if *n < 0);
        // var i = start; while (i < end | i > end) { body; i = i + step }
        // Whole loop-body scope is snapshot/restored (JIT enter_scope/leave_scope):
        // the loop var + any `let` inside the body don't leak out of the loop.
        let saved = self.snapshot_scopes();
        let slot = self.fresh_local(var);
        let _ = writeln!(self.body, "  {} = alloca i64, align 8", slot);
        let (sv, sty) = self.emit_expr(&start_e)?;
        let sv = self.cast_to(sv, sty, "i64")?;
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", sv, slot);
        self.locals
            .insert(var.to_string(), (slot.clone(), "i64"));
        self.local_kind.insert(var.to_string(), ValKind::Int);
        self.mutable.insert(var.to_string(), true);

        let (endv, ety) = self.emit_expr(&end_e)?;
        let endv = self.cast_to(endv, ety, "i64")?;
        let end_slot = self.fresh_local("__range_end");
        let _ = writeln!(self.body, "  {} = alloca i64, align 8", end_slot);
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", endv, end_slot);

        let (stepv, stty) = self.emit_expr(&step_e)?;
        let stepv = self.cast_to(stepv, stty, "i64")?;
        let step_slot = self.fresh_local("__range_step");
        let _ = writeln!(self.body, "  {} = alloca i64, align 8", step_slot);
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", stepv, step_slot);

        let head = self.fresh_label("for");
        let body_l = self.fresh_label("for_body");
        let cont = self.fresh_label("for_cont");
        let end_l = self.fresh_label("for_end");
        self.loop_stack.push((cont.clone(), end_l.clone()));

        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", head);
        let iv = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", iv, slot);
        let ev = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", ev, end_slot);
        let cmp = self.fresh();
        if is_neg {
            // 负步长：i > end
            let _ = writeln!(self.body, "  {} = icmp sgt i64 {}, {}", cmp, iv, ev);
        } else {
            // 正步长：i < end
            let _ = writeln!(self.body, "  {} = icmp slt i64 {}, {}", cmp, iv, ev);
        }
        let _ = writeln!(
            self.body,
            "  br i1 {}, label %{}, label %{}",
            cmp, body_l, end_l
        );

        let _ = writeln!(self.body, "{}:", body_l);
        for s in body {
            if self.emit_stmt(s)? {
                self.loop_stack.pop();
                let _ = writeln!(self.body, "{}:", end_l);
                self.restore_scopes(saved);
                return Ok(());
            }
        }
        let _ = writeln!(self.body, "  br label %{}", cont);
        let _ = writeln!(self.body, "{}:", cont);
        let iv2 = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", iv2, slot);
        let st2 = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = load i64, ptr {}, align 8",
            st2, step_slot
        );
        let iv3 = self.fresh();
        let _ = writeln!(self.body, "  {} = add i64 {}, {}", iv3, iv2, st2);
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", iv3, slot);
        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", end_l);
        self.loop_stack.pop();
        self.restore_scopes(saved);
        Ok(())
    }

    fn emit_for_list(
        &mut self,
        var: &str,
        iter: &Expr,
        body: &[Statement],
    ) -> Result<(), String> {
        let saved = self.snapshot_scopes();
        let (list, _) = self.emit_expr(iter)?;
        let (tag, elem_kind) = match self.infer_kind(iter) {
            ValKind::List(t) => (t, list_tag_to_kind(t)),
            ValKind::NestedList(inner) => (6u8, *inner.clone()),
            ValKind::ListObj(n) => (4u8, ValKind::Object(n)),
            _ => (0u8, ValKind::Int),
        };
        let elem_ty = kind_to_llvm(&elem_kind);

        let idx_slot = self.fresh_local("__fi");
        let _ = writeln!(self.body, "  {} = alloca i64, align 8", idx_slot);
        let _ = writeln!(self.body, "  store i64 0, ptr {}, align 8", idx_slot);

        let lenv = self.emit_list_len_inline(&list)?;
        let len_slot = self.fresh_local("__flen");
        let _ = writeln!(self.body, "  {} = alloca i64, align 8", len_slot);
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", lenv, len_slot);

        let var_slot = self.fresh_local(var);
        let _ = writeln!(self.body, "  {} = alloca {}, align 8", var_slot, elem_ty);
        self.locals
            .insert(var.to_string(), (var_slot.clone(), elem_ty));
        self.local_kind.insert(var.to_string(), elem_kind.clone());
        self.mutable.insert(var.to_string(), true);

        let head = self.fresh_label("forl");
        let body_l = self.fresh_label("forl_body");
        let cont = self.fresh_label("forl_cont");
        let end_l = self.fresh_label("forl_end");
        self.loop_stack.push((cont.clone(), end_l.clone()));

        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", head);
        let i = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", i, idx_slot);
        let n = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", n, len_slot);
        let cmp = self.fresh();
        let _ = writeln!(self.body, "  {} = icmp slt i64 {}, {}", cmp, i, n);
        let _ = writeln!(
            self.body,
            "  br i1 {}, label %{}, label %{}",
            cmp, body_l, end_l
        );

        let _ = writeln!(self.body, "{}:", body_l);
        let raw = self.emit_list_get_inline(&list, &i, tag)?;
        let unpacked = self.unpack_list_value(raw, &elem_kind)?;
        let _ = writeln!(
            self.body,
            "  store {} {}, ptr {}, align 8",
            elem_ty, unpacked, var_slot
        );
        for s in body {
            if self.emit_stmt(s)? {
                self.loop_stack.pop();
                let _ = writeln!(self.body, "{}:", end_l);
                self.restore_scopes(saved);
                return Ok(());
            }
        }
        let _ = writeln!(self.body, "  br label %{}", cont);
        let _ = writeln!(self.body, "{}:", cont);
        let i2 = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", i2, idx_slot);
        let i3 = self.fresh();
        let _ = writeln!(self.body, "  {} = add i64 {}, 1", i3, i2);
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", i3, idx_slot);
        let _ = writeln!(self.body, "  br label %{}", head);
        let _ = writeln!(self.body, "{}:", end_l);
        self.loop_stack.pop();
        self.restore_scopes(saved);
        Ok(())
    }

    fn emit_expr(&mut self, expr: &Expr) -> Result<(String, &'static str), String> {
        match expr {
            Expr::Int(n) => Ok((format!("{}", n), "i64")),
            Expr::Float(f) => Ok((llvm_double_literal(*f), "double")),
            Expr::Bool(b) => Ok((if *b { "1".into() } else { "0".into() }, "i64")),
            Expr::String(s) => self.emit_string_lit(s),
            Expr::Ident(name) => {
                // ref 参数：locals[name] 存的是调用方变量的地址（ptr 槽），先取地址再解引用取值
                if self.ref_params.contains(name) {
                    let addr_slot = self.ref_param_addr_slot.get(name).cloned().unwrap();
                    let val_ty = self.ref_param_val_ty.get(name).copied().unwrap_or("i64");
                    let addr = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = load ptr, ptr {}, align 8",
                        addr, addr_slot
                    );
                    let val = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = load {}, ptr {}, align 8",
                        val, val_ty, addr
                    );
                    return Ok((val, val_ty));
                }
                if let Some((slot, ty)) = self.locals.get(name).cloned() {
                    let r = self.fresh();
                    let _ = writeln!(self.body, "  {} = load {}, ptr {}, align 8", r, ty, slot);
                    return Ok((r, ty));
                }
                if let Some((ty, kind)) = self.global_vars.get(name).cloned() {
                    let r = self.fresh();
                    let g = llvm_func_name(name);
                    let _ = writeln!(
                        self.body,
                        "  {} = load {}, ptr @{}, align 8",
                        r, ty, g
                    );
                    let _ = kind;
                    return Ok((r, ty));
                }
                // free function used as value → trampoline closure
                if self.funcs.contains_key(name) && !self.classes.contains_key(name) {
                    return self.emit_func_as_closure(name);
                }
                Err(format!("Undefined variable: {}", name))
            }
            Expr::List(items) => self.emit_list_lit(items),
            Expr::Dict(pairs) => self.emit_dict_lit(pairs),
            Expr::Index(base, idx) => self.emit_index(base, idx),
            Expr::BinOp(l, op, r) => self.emit_binop(l, *op, r),
            Expr::UnaryOp(op, e) => self.emit_unary(*op, e),
            Expr::Call(callee, args) => self.emit_call(callee, args),
            Expr::Member(base, member) => self.emit_member_load(base, member),
            Expr::None => Ok(("null".into(), "ptr")),
            Expr::Tuple(items) => {
                // simple: pack as list of mixed ints (limited)
                self.emit_list_lit(items)
            }
            Expr::BigInt(s) => {
                // BolideBigInt is a heap object (ptr). Small values via
                // from_i64, large via from_str.
                let d = self.fresh();
                if let Ok(n) = s.parse::<i64>() {
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_bigint_from_i64(i64 {})",
                        d, n
                    );
                } else {
                    let c = self.emit_cstr_ptr(s);
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_bigint_from_str(ptr {}, i64 {})",
                        d, c, s.len()
                    );
                }
                Ok((d, "ptr"))
            }
            Expr::Decimal(s) => s
                .parse::<f64>()
                .map(|f| (llvm_double_literal(f), "double"))
                .map_err(|_| format!("LLVM backend: bad decimal '{}'", s)),
            Expr::Closure {
                params,
                return_type,
                body,
            } => self.emit_closure_expr(params, return_type, body),
            Expr::NamedArg(_, e) => self.emit_expr(e),
            Expr::SpreadArg(e) | Expr::KwSpreadArg(e) => self.emit_expr(e),
            Expr::Propagate(e) => {
                // `expr?` — if Option/Result tag != 0 (None/Err), early-return None/Err
                let (v, _) = self.emit_expr(e)?;
                let tagp = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = getelementptr i8, ptr {}, i64 0",
                    tagp, v
                );
                let tag = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", tag, tagp);
                let bad = self.fresh();
                let _ = writeln!(self.body, "  {} = icmp ne i64 {}, 0", bad, tag);
                let err_l = self.fresh_label("prop_err");
                let ok_l = self.fresh_label("prop_ok");
                let _ = writeln!(
                    self.body,
                    "  br i1 {}, label %{}, label %{}",
                    bad, err_l, ok_l
                );
                let _ = writeln!(self.body, "{}:", err_l);
                // return the failing ADT as-is when ret is ptr; else 0
                if self.current_ret_ty == "ptr" {
                    let _ = writeln!(self.body, "  ret ptr {}", v);
                } else {
                    let _ = writeln!(self.body, "  ret {} 0", self.current_ret_ty);
                }
                let _ = writeln!(self.body, "{}:", ok_l);
                let fp = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = getelementptr i8, ptr {}, i64 8",
                    fp, v
                );
                let raw = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", raw, fp);
                Ok((raw, "i64"))
            }
            Expr::Raise(e) => {
                // `expr!` — unwrap or throw
                let (v, _) = self.emit_expr(e)?;
                let tagp = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = getelementptr i8, ptr {}, i64 0",
                    tagp, v
                );
                let tag = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", tag, tagp);
                let bad = self.fresh();
                let _ = writeln!(self.body, "  {} = icmp ne i64 {}, 0", bad, tag);
                let err_l = self.fresh_label("raise_err");
                let ok_l = self.fresh_label("raise_ok");
                let _ = writeln!(
                    self.body,
                    "  br i1 {}, label %{}, label %{}",
                    bad, err_l, ok_l
                );
                let _ = writeln!(self.body, "{}:", err_l);
                let _ = writeln!(
                    self.body,
                    "  call void @bolide_exception_set(ptr {}, i64 0)",
                    v
                );
                if let Some(catch_l) = self.catch_stack.last().cloned() {
                    let _ = writeln!(self.body, "  br label %{}", catch_l);
                } else {
                    let _ = writeln!(self.body, "  call void @bolide_throw_uncaught(ptr {})", v);
                    let _ = writeln!(self.body, "  unreachable");
                }
                let _ = writeln!(self.body, "{}:", ok_l);
                let fp = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = getelementptr i8, ptr {}, i64 8",
                    fp, v
                );
                let raw = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", raw, fp);
                Ok((raw, "i64"))
            }
            Expr::ValueConstruct(name, fields) => {
                // treat as class constructor by field order if class exists
                if self.classes.contains_key(name) {
                    let ci = self.classes.get(name).unwrap().clone();
                    let mut args = Vec::new();
                    for f in &ci.fields {
                        if let Some((_, e)) = fields.iter().find(|(n, _)| n == &f.name) {
                            args.push(e.clone());
                        } else {
                            args.push(Expr::Int(0));
                        }
                    }
                    return self.emit_named_call(name, &args);
                }
                Err(format!("LLVM: unknown value type '{}'", name))
            }
            Expr::Slice(base, start, end, _step) => {
                // list/string slice via runtime if available — simplified list rebuild
                let (b, _) = self.emit_expr(base)?;
                let s = if let Some(st) = start {
                    let (v, t) = self.emit_expr(st)?;
                    self.cast_to(v, t, "i64")?
                } else {
                    "0".into()
                };
                let e = if let Some(en) = end {
                    let (v, t) = self.emit_expr(en)?;
                    self.cast_to(v, t, "i64")?
                } else {
                    self.emit_list_len_inline(&b)?
                };
                // new list copy range
                let out = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_list_new(i8 0)",
                    out
                );
                let i_slot = self.fresh_local("__si");
                let _ = writeln!(self.body, "  {} = alloca i64, align 8", i_slot);
                let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", s, i_slot);
                let head = self.fresh_label("slice");
                let body_l = self.fresh_label("slice_b");
                let end_l = self.fresh_label("slice_e");
                let _ = writeln!(self.body, "  br label %{}", head);
                let _ = writeln!(self.body, "{}:", head);
                let iv = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", iv, i_slot);
                let cmp = self.fresh();
                let _ = writeln!(self.body, "  {} = icmp slt i64 {}, {}", cmp, iv, e);
                let _ = writeln!(
                    self.body,
                    "  br i1 {}, label %{}, label %{}",
                    cmp, body_l, end_l
                );
                let _ = writeln!(self.body, "{}:", body_l);
                let raw = self.emit_list_get_inline(&b, &iv, 0)?;
                let _ = writeln!(
                    self.body,
                    "  call void @bolide_list_push(ptr {}, i64 {})",
                    out, raw
                );
                let i2 = self.fresh();
                let _ = writeln!(self.body, "  {} = add i64 {}, 1", i2, iv);
                let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", i2, i_slot);
                let _ = writeln!(self.body, "  br label %{}", head);
                let _ = writeln!(self.body, "{}:", end_l);
                Ok((out, "ptr"))
            }
            Expr::ListComprehension {
                expr,
                vars,
                iter,
                filter,
            } => self.emit_list_comprehension(expr, vars, iter, filter),
            Expr::Spawn(name, args) => self.emit_spawn(name, args, false),
            Expr::SpawnThread(name, args) => self.emit_spawn(name, args, true),
            Expr::Await(inner) => self.emit_await(inner),
            Expr::SpawnAll(items) => {
                // spawn each task, await all, collect results into a list
                let list = self.fresh();
                let _ = writeln!(self.body, "  {} = call ptr @bolide_list_new(i8 0)", list);
                for it in items {
                    let (name, args) = match it {
                        Expr::Call(callee, args) => match callee.as_ref() {
                            Expr::Ident(n) => (n.clone(), args.clone()),
                            _ => continue,
                        },
                        Expr::Spawn(n, args) | Expr::SpawnThread(n, args) => {
                            (n.clone(), args.clone())
                        }
                        _ => continue,
                    };
                    let (h, _) = self.emit_spawn(&name, &args, false)?;
                    let ret_kind = self.func_ret_kind.get(&name).cloned();
                    let (res, rty) = self.emit_await_value(&h, ret_kind, true)?;
                    let packed = self.pack_as_i64(res, rty)?;
                    let _ = writeln!(
                        self.body,
                        "  call void @bolide_list_push(ptr {}, i64 {})",
                        list, packed
                    );
                }
                Ok((list, "ptr"))
            }
            other => Err(format!(
                "LLVM backend: unsupported expression {:?}",
                std::mem::discriminant(other)
            )),
        }
    }

    /// `[expr for var in iter if cond]` → 合成 for 循环，复用 emit_for 的迭代逻辑
    /// （与 Cranelift 的 compile_list_comprehension 一致）。
    fn emit_list_comprehension(
        &mut self,
        expr: &Expr,
        vars: &[String],
        iter: &Expr,
        filter: &Option<Box<Expr>>,
    ) -> Result<(String, &'static str), String> {
        if vars.len() != 1 {
            return Err(
                "LLVM: list comprehension with multiple loop variables not supported yet".into(),
            );
        }
        // 结果列表存入合成局部变量，`push` 走正常 list 方法路径。
        // 元素类型从推导式表达式推断（决定 list tag 与 push 打包方式）。
        // 推导式表达式会引用循环变量，须先临时绑定其元素类型（同 Cranelift
        // compile_list_comprehension 的做法），否则 `s + "!"` 被误判为 Int。
        let loop_var_kind = match self.infer_kind(iter) {
            ValKind::List(t) => list_tag_to_kind(t),
            ValKind::ListObj(n) => ValKind::Object(n),
            _ => ValKind::Int,
        };
        let saved_var_kind = self.local_kind.get(&vars[0]).cloned();
        self.local_kind
            .insert(vars[0].clone(), loop_var_kind.clone());
        let elem_kind = self.infer_kind(expr);
        match saved_var_kind {
            Some(k) => {
                self.local_kind.insert(vars[0].clone(), k);
            }
            None => {
                self.local_kind.remove(&vars[0]);
            }
        }
        let elem_tag: u8 = match &elem_kind {
            ValKind::Float => 1,
            ValKind::Bool => 2,
            ValKind::Str => 3,
            ValKind::Object(_) | ValKind::Adt(_) | ValKind::Dict | ValKind::Ptr => 4,
            _ => 0,
        };
        let result_list_kind = match &elem_kind {
            ValKind::Object(n) => ValKind::ListObj(n.clone()),
            ValKind::Adt(n) => ValKind::ListObj(n.clone()),
            _ => ValKind::List(elem_tag),
        };
        let result_name = format!("__lc_{}", self.tmp);
        let result_slot = self.fresh_local(&result_name);
        let _ = writeln!(self.body, "  {} = alloca ptr, align 8", result_slot);
        let new_l = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_list_new(i8 {})",
            new_l, elem_tag
        );
        let _ = writeln!(self.body, "  store ptr {}, ptr {}, align 8", new_l, result_slot);
        self.locals
            .insert(result_name.clone(), (result_slot.clone(), "ptr"));
        self.local_kind
            .insert(result_name.clone(), result_list_kind);
        self.mutable.insert(result_name.clone(), true);

        // result.push(expr)
        let push_expr = Expr::Call(
            Box::new(Expr::Member(
                Box::new(Expr::Ident(result_name.clone())),
                "push".into(),
            )),
            vec![expr.clone()],
        );
        let body = if let Some(f) = filter {
            vec![Statement::If(IfStmt {
                condition: (**f).clone(),
                then_body: vec![Statement::Expr(push_expr)],
                elif_branches: vec![],
                else_body: None,
            })]
        } else {
            vec![Statement::Expr(push_expr)]
        };

        let for_stmt = ForStmt {
            vars: vars.to_vec(),
            iter: (*iter).clone(),
            body,
        };
        self.emit_for(&for_stmt)?;

        let d = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = load ptr, ptr {}, align 8",
            d, result_slot
        );
        Ok((d, "ptr"))
    }

    fn emit_member_load(
        &mut self,
        base: &Expr,
        member: &str,
    ) -> Result<(String, &'static str), String> {
        // Enum.Variant bare (zero-field) → construct
        if let Expr::Ident(enum_name) = base {
            if let Some(adt) = self.adts.get(enum_name).cloned() {
                if let Some(v) = adt.variants.iter().find(|v| v.name == member) {
                    if v.fields.is_empty() {
                        return self.emit_adt_construct(enum_name, &v.name, &[]);
                    }
                }
            }
        }
        let kind = self.infer_kind(base);
        match kind {
            ValKind::Object(class_name) => {
                let ci = self
                    .classes
                    .get(&class_name)
                    .ok_or_else(|| format!("Unknown class '{}'", class_name))?
                    .clone();
                let off = field_offset(&ci, member)
                    .ok_or_else(|| format!("Unknown field '{}.{}'", class_name, member))?;
                let fty = field_type(&ci, member).unwrap().clone();
                let lty = type_llvm(&fty);
                let (obj, _) = self.emit_expr(base)?;
                let fp = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = getelementptr i8, ptr {}, i64 {}",
                    fp, obj, off
                );
                let r = self.fresh();
                let _ = writeln!(self.body, "  {} = load {}, ptr {}, align 8", r, lty, fp);
                Ok((r, lty))
            }
            _ => {
                if let Expr::Ident(mod_name) = base {
                    if self.modules.contains_key(mod_name) {
                        return Err(format!(
                            "LLVM backend: call {}.{}(...) as a function",
                            mod_name, member
                        ));
                    }
                }
                Err(format!(
                    "LLVM backend: member '{}' not supported on {:?}",
                    member, kind
                ))
            }
        }
    }

    fn emit_dict_lit(&mut self, pairs: &[(Expr, Expr)]) -> Result<(String, &'static str), String> {
        let key_tag: u8 = if pairs
            .first()
            .map(|(k, _)| matches!(self.infer_kind(k), ValKind::Str))
            .unwrap_or(false)
        {
            3
        } else {
            0
        };
        // Dict values are stored as BolideDynamic wrappers (value_type=9) so that
        // reads (`row["key"]`) return a uniform dynamic the runtime can convert,
        // and `where_eq` comparisons see the same representation as the query
        // value. (Using the FIRST value's type for a mixed dict would store e.g. a
        // bool as a string → the runtime reads it as a pointer and crashes.)
        let use_dynamic = true;
        let val_tag: u8 = 9;
        let d = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_dict_new(i8 {}, i8 {})",
            d, key_tag, val_tag
        );
        for (k, v) in pairs {
            let (kv, kty) = self.emit_expr(k)?;
            let (vv, vty) = self.emit_expr(v)?;
            let key = self.pack_as_i64(kv, kty)?;
            let val = if use_dynamic {
                let dyn_ = self.fresh();
                let from_fn = match self.infer_kind(v) {
                    ValKind::Str => Some("bolide_dynamic_from_string"),
                    ValKind::Float => Some("bolide_dynamic_from_float"),
                    ValKind::Bool => Some("bolide_dynamic_from_bool"),
                    ValKind::Dict => Some("bolide_dynamic_from_dict"),
                    ValKind::List(_) | ValKind::ListObj(_) | ValKind::NestedList(_) => {
                        Some("bolide_dynamic_from_list")
                    }
                    _ => None,
                };
                if let Some(fn_name) = from_fn {
                    let v = self.cast_to(vv, vty, "ptr")?;
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @{}(ptr {})",
                        dyn_, fn_name, v
                    );
                    self.pack_as_i64(dyn_, "ptr")?
                } else {
                    let v = self.cast_to(vv, vty, "i64")?;
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_dynamic_from_int(i64 {})",
                        dyn_, v
                    );
                    self.pack_as_i64(dyn_, "ptr")?
                }
            } else {
                self.pack_as_i64(vv, vty)?
            };
            let _ = writeln!(
                self.body,
                "  call void @bolide_dict_set(ptr {}, i64 {}, i64 {})",
                d, key, val
            );
        }
        Ok((d, "ptr"))
    }

    fn emit_adt_construct(
        &mut self,
        adt_name: &str,
        variant: &str,
        args: &[Expr],
    ) -> Result<(String, &'static str), String> {
        let adt = self
            .adts
            .get(adt_name)
            .ok_or_else(|| format!("Unknown enum '{}'", adt_name))?
            .clone();
        let v = adt
            .variants
            .iter()
            .find(|v| v.name == variant)
            .ok_or_else(|| format!("Unknown variant '{}.{}'", adt_name, variant))?
            .clone();
        if args.len() != v.fields.len() {
            return Err(format!(
                "{}.{} expects {} args, got {}",
                adt_name,
                variant,
                v.fields.len(),
                args.len()
            ));
        }
        let obj = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @object_alloc(i64 {})",
            obj, adt.size
        );
        // store variant tag at offset 0
        let tp = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr i8, ptr {}, i64 0",
            tp, obj
        );
        let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", v.tag, tp);
        for (i, arg) in args.iter().enumerate() {
            let (val, vty) = self.emit_expr(arg)?;
            let lty = type_llvm(&v.fields[i].ty);
            // Generic fields: store as i64/ptr best-effort
            let store_ty = if lty == "double" {
                "double"
            } else if vty == "ptr" || lty == "ptr" {
                "ptr"
            } else {
                "i64"
            };
            let val = self.cast_to(val, vty, store_ty)?;
            let fp = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = getelementptr i8, ptr {}, i64 {}",
                fp, obj, v.fields[i].offset
            );
            let _ = writeln!(
                self.body,
                "  store {} {}, ptr {}, align 8",
                store_ty, val, fp
            );
        }
        Ok((obj, "ptr"))
    }

    fn pack_as_i64(&mut self, v: String, ty: &str) -> Result<String, String> {
        if ty == "i64" {
            return Ok(v);
        }
        if ty == "double" {
            // bitcast via ptr
            let a = self.fresh();
            let _ = writeln!(self.body, "  {} = alloca double, align 8", a);
            let _ = writeln!(self.body, "  store double {}, ptr {}, align 8", v, a);
            let r = self.fresh();
            let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", r, a);
            return Ok(r);
        }
        // ptr → i64
        self.cast_to(v, ty, "i64")
    }

    fn unpack_from_i64(
        &mut self,
        v: String,
        kind: &ValKind,
    ) -> Result<(String, &'static str), String> {
        match kind {
            ValKind::Float => {
                let a = self.fresh();
                let _ = writeln!(self.body, "  {} = alloca i64, align 8", a);
                let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", v, a);
                let r = self.fresh();
                let _ = writeln!(self.body, "  {} = load double, ptr {}, align 8", r, a);
                Ok((r, "double"))
            }
            ValKind::Str | ValKind::List(_) | ValKind::Dict | ValKind::Object(_) | ValKind::Adt(_)
            | ValKind::Ptr => {
                let r = self.cast_to(v, "i64", "ptr")?;
                Ok((r, "ptr"))
            }
            _ => Ok((v, "i64")),
        }
    }

    /// Emit a C string constant (`@.str.N`) and return the char* pointer.
    fn emit_cstr_ptr(&mut self, s: &str) -> String {
        let id = self.strings.len();
        self.strings.push(s.to_string());
        let len = s.len() + 1;
        let tmp = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr inbounds [{} x i8], ptr @.str.{}, i64 0, i64 0",
            tmp, len, id
        );
        tmp
    }

    fn emit_string_lit(&mut self, s: &str) -> Result<(String, &'static str), String> {
        let id = self.strings.len();
        self.strings.push(s.to_string());
        let len = s.len() + 1;
        let tmp = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr inbounds [{} x i8], ptr @.str.{}, i64 0, i64 0",
            tmp, len, id
        );
        let sreg = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_string_new(ptr {})",
            sreg, tmp
        );
        Ok((sreg, "ptr"))
    }

    fn emit_list_lit(&mut self, items: &[Expr]) -> Result<(String, &'static str), String> {
        let tag = if items.is_empty() {
            0u8
        } else {
            match self.infer_kind(&items[0]) {
                ValKind::Float => 1,
                ValKind::Bool => 2,
                ValKind::Str => 3,
                _ => 0,
            }
        };
        self.emit_list_lit_tagged(items, tag)
    }

    /// Create a list literal with an explicit element tag (used when the declared
    /// type fixes the element kind, e.g. `var x: list<dict<..>> = []` must be a
    /// dict-tagged list so pushed dicts are read back as dicts, not ints).
    fn emit_list_lit_tagged(
        &mut self,
        items: &[Expr],
        tag: u8,
    ) -> Result<(String, &'static str), String> {
        let list = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_list_new(i8 {})",
            list, tag
        );
        for it in items {
            let (v, ty) = self.emit_expr(it)?;
            let packed = match tag {
                1 => {
                    // float bits as i64
                    let v = self.cast_to(v, ty, "double")?;
                    let b = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = bitcast double {} to i64",
                        b, v
                    );
                    b
                }
                _ => self.cast_to(v, ty, "i64")?,
            };
            let _ = writeln!(
                self.body,
                "  call void @bolide_list_push(ptr {}, i64 {})",
                list, packed
            );
        }
        Ok((list, "ptr"))
    }

    fn emit_index(
        &mut self,
        base: &Expr,
        idx: &Expr,
    ) -> Result<(String, &'static str), String> {
        let base_kind = self.infer_kind(base);
        let (base_v, _) = self.emit_expr(base)?;
        let (ix, ixty) = self.emit_expr(idx)?;
        if matches!(base_kind, ValKind::Dict) {
            let key = self.pack_as_i64(ix, ixty)?;
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call i64 @bolide_dict_get(ptr {}, i64 {})",
                d, base_v, key
            );
            // dict values are stored as BolideDynamic wrappers → return the ptr
            let p = self.fresh();
            let _ = writeln!(self.body, "  {} = inttoptr i64 {} to ptr", p, d);
            return Ok((p, "ptr"));
        }
        if matches!(base_kind, ValKind::Str) {
            let ix = self.cast_to(ix, ixty, "i64")?;
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call ptr @bolide_string_char_at(ptr {}, i64 {})",
                d, base_v, ix
            );
            return Ok((d, "ptr"));
        }
        if matches!(base_kind, ValKind::Bytes) {
            let ix = self.cast_to(ix, ixty, "i64")?;
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call i64 @bolide_bytes_get(ptr {}, i64 {})",
                d, base_v, ix
            );
            return Ok((d, "i64"));
        }
        let ix = self.cast_to(ix, ixty, "i64")?;
        let tag = match base_kind {
            ValKind::List(t) => t,
            ValKind::NestedList(_) => 6, // list element
            _ => 0,
        };
        let kind = match &base_kind {
            ValKind::NestedList(inner) => *inner.clone(),
            _ => list_tag_to_kind(tag),
        };
        let raw = self.emit_list_get_inline(&base_v, &ix, tag)?;
        let v = self.unpack_list_value(raw, &kind)?;
        // nested list element is a list pointer stored as i64 → inttoptr
        if matches!(base_kind, ValKind::NestedList(_)) {
            let p = self.fresh();
            let _ = writeln!(self.body, "  {} = inttoptr i64 {} to ptr", p, v);
            return Ok((p, "ptr"));
        }
        Ok((v, kind_to_llvm(&kind)))
    }

    fn emit_match(&mut self, m: &bolide_parser::MatchStmt) -> Result<bool, String> {
        let scrut_kind = self.infer_kind(&m.expr);
        let (scrut, _) = self.emit_expr(&m.expr)?;
        let adt_name = match scrut_kind {
            ValKind::Adt(n) => n,
            ValKind::Object(n) if self.adts.contains_key(&n) => n,
            // next() returns Option ptr often inferred as Ptr/Object — try Option
            ValKind::Ptr | ValKind::Object(_) => {
                if self.adts.contains_key("Option") {
                    "Option".into()
                } else {
                    return Err(format!(
                        "LLVM match: unsupported scrutinee kind {:?}",
                        scrut_kind
                    ));
                }
            }
            other => {
                return Err(format!(
                    "LLVM match: unsupported scrutinee kind {:?}",
                    other
                ))
            }
        };
        let adt = self
            .adts
            .get(&adt_name)
            .ok_or_else(|| format!("Unknown enum '{}'", adt_name))?
            .clone();

        let tagp = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr i8, ptr {}, i64 0",
            tagp, scrut
        );
        let tag = self.fresh();
        let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", tag, tagp);

        let end_l = self.fresh_label("match_end");
        let mut all_term = true;
        let mut need_end = false;
        let mut open_next: Option<String> = None;
        for (i, arm) in m.arms.iter().enumerate() {
            // Close previous next-label fallthrough: start matching this arm there
            if let Some(prev) = open_next.take() {
                let _ = writeln!(self.body, "{}:", prev);
            }
            let body_l = self.fresh_label(&format!("match_arm{}", i));
            let next_l = self.fresh_label(&format!("match_next{}", i));
            match &arm.pattern {
                Pattern::Wildcard | Pattern::Bind(_) => {
                    let _ = writeln!(self.body, "  br label %{}", body_l);
                    let _ = writeln!(self.body, "{}:", body_l);
                    if let Pattern::Bind(name) = &arm.pattern {
                        let slot = self.fresh_local(name);
                        let _ = writeln!(self.body, "  {} = alloca ptr, align 8", slot);
                        let _ = writeln!(
                            self.body,
                            "  store ptr {}, ptr {}, align 8",
                            scrut, slot
                        );
                        self.locals.insert(name.clone(), (slot, "ptr"));
                        self.local_kind
                            .insert(name.clone(), ValKind::Adt(adt_name.clone()));
                        self.mutable.insert(name.clone(), true);
                    }
                    let mut term = false;
                    for s in &arm.body {
                        if term {
                            break;
                        }
                        term = self.emit_stmt(s)?;
                    }
                    if !term {
                        all_term = false;
                        need_end = true;
                        let _ = writeln!(self.body, "  br label %{}", end_l);
                    }
                    // catch-all next is dead if we always enter body
                    let _ = writeln!(self.body, "{}:", next_l);
                    need_end = true;
                    let _ = writeln!(self.body, "  br label %{}", end_l);
                    open_next = None;
                    break;
                }
                Pattern::Variant {
                    enum_name,
                    variant,
                    fields,
                } => {
                    if let Some(en) = enum_name {
                        if en != &adt_name {
                            return Err(format!(
                                "match arm {}.{} for value of {}",
                                en, variant, adt_name
                            ));
                        }
                    }
                    let vinfo = adt
                        .variants
                        .iter()
                        .find(|v| v.name == *variant)
                        .ok_or_else(|| format!("Unknown variant '{}.{}'", adt_name, variant))?
                        .clone();
                    let cmp = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = icmp eq i64 {}, {}",
                        cmp, tag, vinfo.tag
                    );
                    let _ = writeln!(
                        self.body,
                        "  br i1 {}, label %{}, label %{}",
                        cmp, body_l, next_l
                    );
                    let _ = writeln!(self.body, "{}:", body_l);
                    for (fi, pat) in fields.iter().enumerate() {
                        if let Pattern::Bind(name) = pat {
                            let off = vinfo.fields[fi].offset;
                            let fty = &vinfo.fields[fi].ty;
                            let lty = type_llvm(fty);
                            let load_ty = if matches!(fty, Type::Generic(_)) {
                                "i64"
                            } else {
                                lty
                            };
                            let fp = self.fresh();
                            let _ = writeln!(
                                self.body,
                                "  {} = getelementptr i8, ptr {}, i64 {}",
                                fp, scrut, off
                            );
                            let raw = self.fresh();
                            let _ = writeln!(
                                self.body,
                                "  {} = load {}, ptr {}, align 8",
                                raw, load_ty, fp
                            );
                            let slot = self.fresh_local(name);
                            let store_ty = if load_ty == "double" {
                                "double"
                            } else if load_ty == "ptr" {
                                "ptr"
                            } else {
                                "i64"
                            };
                            let _ = writeln!(self.body, "  {} = alloca {}, align 8", slot, store_ty);
                            let _ = writeln!(
                                self.body,
                                "  store {} {}, ptr {}, align 8",
                                store_ty, raw, slot
                            );
                            self.locals.insert(name.clone(), (slot, store_ty));
                            self.local_kind
                                .insert(name.clone(), kind_of_type(&Some(fty.clone())));
                            if matches!(fty, Type::Generic(_)) {
                                self.local_kind.insert(name.clone(), ValKind::Int);
                            }
                            self.mutable.insert(name.clone(), true);
                        }
                    }
                    let mut term = false;
                    for s in &arm.body {
                        if term {
                            break;
                        }
                        term = self.emit_stmt(s)?;
                    }
                    if !term {
                        all_term = false;
                        need_end = true;
                        let _ = writeln!(self.body, "  br label %{}", end_l);
                    }
                    open_next = Some(next_l);
                }
                Pattern::None => {
                    let none_tag = adt
                        .variants
                        .iter()
                        .find(|v| v.name == "None")
                        .map(|v| v.tag)
                        .unwrap_or(1);
                    let cmp = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = icmp eq i64 {}, {}",
                        cmp, tag, none_tag
                    );
                    let _ = writeln!(
                        self.body,
                        "  br i1 {}, label %{}, label %{}",
                        cmp, body_l, next_l
                    );
                    let _ = writeln!(self.body, "{}:", body_l);
                    let mut term = false;
                    for s in &arm.body {
                        if term {
                            break;
                        }
                        term = self.emit_stmt(s)?;
                    }
                    if !term {
                        all_term = false;
                        need_end = true;
                        let _ = writeln!(self.body, "  br label %{}", end_l);
                    }
                    open_next = Some(next_l);
                }
                _ => {
                    return Err("LLVM match: unsupported pattern".into());
                }
            }
        }
        if let Some(prev) = open_next {
            let _ = writeln!(self.body, "{}:", prev);
            need_end = true;
            let _ = writeln!(self.body, "  br label %{}", end_l);
        }
        if need_end {
            let _ = writeln!(self.body, "{}:", end_l);
            if all_term {
                // join only from dead next labels
                let _ = writeln!(self.body, "  unreachable");
                return Ok(true);
            }
            return Ok(false);
        }
        // no join needed — every live path returned
        Ok(all_term && !m.arms.is_empty())
    }

    fn emit_throw(&mut self, e: &Expr) -> Result<(), String> {
        let (v, ty) = self.emit_expr(e)?;
        let ptr = if ty == "ptr" {
            v
        } else {
            self.cast_to(v, ty, "ptr")?
        };
        let tag = match self.infer_kind(e) {
            ValKind::Object(n) => self.classes.get(&n).map(|c| c.tag).unwrap_or(0),
            _ => 0,
        };
        let _ = writeln!(
            self.body,
            "  call void @bolide_exception_set(ptr {}, i64 {})",
            ptr, tag
        );
        if let Some(catch_l) = self.catch_stack.last().cloned() {
            let _ = writeln!(self.body, "  br label %{}", catch_l);
        } else {
            let _ = writeln!(self.body, "  call void @bolide_throw_uncaught(ptr {})", ptr);
            let _ = writeln!(self.body, "  unreachable");
        }
        Ok(())
    }

    fn emit_try(&mut self, t: &bolide_parser::TryStmt) -> Result<bool, String> {
        let catch_l = self.fresh_label("catch");
        let end_l = self.fresh_label("try_end");
        let try_l = self.fresh_label("try_body");
        self.catch_stack.push(catch_l.clone());
        let _ = writeln!(self.body, "  br label %{}", try_l);
        let _ = writeln!(self.body, "{}:", try_l);
        // try 块是独立作用域：块内 `let` 遮蔽不得泄漏到 catch/finally/之后
        let saved_try = self.snapshot_scopes();
        let mut try_term = false;
        for s in &t.try_body {
            if try_term {
                break;
            }
            try_term = self.emit_stmt(s)?;
        }
        self.restore_scopes(saved_try);
        if !try_term {
            let _ = writeln!(self.body, "  br label %{}", end_l);
        }
        self.catch_stack.pop();

        let _ = writeln!(self.body, "{}:", catch_l);
        // bind first catch clause variable (catch 也是独立作用域)
        if let Some(cc) = t.catch_clauses.first() {
            let saved_catch = self.snapshot_scopes();
            let ex = self.fresh();
            let _ = writeln!(self.body, "  {} = call ptr @bolide_exception_get()", ex);
            let slot = self.fresh_local(&cc.var);
            let _ = writeln!(self.body, "  {} = alloca ptr, align 8", slot);
            let _ = writeln!(self.body, "  store ptr {}, ptr {}, align 8", ex, slot);
            self.locals.insert(cc.var.clone(), (slot, "ptr"));
            self.local_kind
                .insert(cc.var.clone(), kind_of_type(&Some(cc.ty.clone())));
            self.mutable.insert(cc.var.clone(), true);
            let mut term = false;
            for s in &cc.body {
                if term {
                    break;
                }
                term = self.emit_stmt(s)?;
            }
            self.restore_scopes(saved_catch);
            if !term {
                let _ = writeln!(self.body, "  br label %{}", end_l);
            }
        } else {
            let _ = writeln!(self.body, "  br label %{}", end_l);
        }

        if let Some(fin) = &t.finally {
            // simplified: finally after catch/try join
            let fin_l = self.fresh_label("finally");
            // re-route end to finally first — patch by inserting finally before end
            let _ = writeln!(self.body, "  br label %{}", fin_l);
            let _ = writeln!(self.body, "{}:", fin_l);
            let saved_fin = self.snapshot_scopes();
            for s in fin {
                let _ = self.emit_stmt(s)?;
            }
            self.restore_scopes(saved_fin);
            let _ = writeln!(self.body, "  br label %{}", end_l);
        }

        let _ = writeln!(self.body, "{}:", end_l);
        Ok(false)
    }

    // BolideList layout (matches runtime + Cranelift offsets):
    //   +0  RcHeader (16 bytes)
    //   +16 data: *mut u8
    //   +24 len:  usize (i64 on LP64)
    const LIST_DATA_OFF: i64 = 16;
    const LIST_LEN_OFF: i64 = 24;

    /// Inline list.len() — load field, no runtime call.
    fn emit_list_len_inline(&mut self, list: &str) -> Result<String, String> {
        let len_addr = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
            len_addr,
            list,
            Self::LIST_LEN_OFF
        );
        let len = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = load i64, ptr {}, align 8",
            len, len_addr
        );
        Ok(len)
    }

    /// Inline list[i] load with bounds check (OOB → 0), same semantics as Cranelift.
    /// `tag`: 0=int, 1=float, 2=bool, 3=str (str still 8-byte pointer slots).
    fn emit_list_get_inline(
        &mut self,
        list: &str,
        index: &str,
        tag: u8,
    ) -> Result<String, String> {
        let len = self.emit_list_len_inline(list)?;
        let inb = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = icmp ult i64 {}, {}",
            inb, index, len
        );

        let data_addr = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
            data_addr,
            list,
            Self::LIST_DATA_OFF
        );
        let data = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = load ptr, ptr {}, align 8",
            data, data_addr
        );

        let (elem_ty, scale) = if tag == 2 {
            ("i8", 1i64)
        } else {
            ("i64", 8i64)
        };
        let off = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = mul i64 {}, {}",
            off, index, scale
        );
        let ep = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
            ep, data, off
        );
        let loaded = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = load {}, ptr {}, align 1",
            loaded, elem_ty, ep
        );
        let value = if tag == 2 {
            let z = self.fresh();
            let _ = writeln!(self.body, "  {} = zext i8 {} to i64", z, loaded);
            z
        } else {
            loaded
        };
        let sel = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = select i1 {}, i64 {}, i64 0",
            sel, inb, value
        );
        Ok(sel)
    }

    /// Inline list[i] = value with bounds check (OOB write ignored).
    fn emit_list_set_inline(
        &mut self,
        list: &str,
        index: &str,
        packed_i64: &str,
        tag: u8,
    ) -> Result<(), String> {
        let len = self.emit_list_len_inline(list)?;
        let inb = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = icmp ult i64 {}, {}",
            inb, index, len
        );
        let do_store = self.fresh_label("lset");
        let cont = self.fresh_label("lset_end");
        let _ = writeln!(
            self.body,
            "  br i1 {}, label %{}, label %{}",
            inb, do_store, cont
        );
        let _ = writeln!(self.body, "{}:", do_store);

        let data_addr = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
            data_addr,
            list,
            Self::LIST_DATA_OFF
        );
        let data = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = load ptr, ptr {}, align 8",
            data, data_addr
        );
        let (elem_ty, scale) = if tag == 2 {
            ("i8", 1i64)
        } else {
            ("i64", 8i64)
        };
        let off = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = mul i64 {}, {}",
            off, index, scale
        );
        let ep = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
            ep, data, off
        );
        if tag == 2 {
            let narrow = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = trunc i64 {} to i8",
                narrow, packed_i64
            );
            let _ = writeln!(
                self.body,
                "  store i8 {}, ptr {}, align 1",
                narrow, ep
            );
        } else {
            let _ = writeln!(
                self.body,
                "  store i64 {}, ptr {}, align 8",
                packed_i64, ep
            );
        }
        let _ = writeln!(self.body, "  br label %{}", cont);
        let _ = writeln!(self.body, "{}:", cont);
        Ok(())
    }

    fn pack_list_value(
        &mut self,
        v: String,
        ty: &str,
        base: &Expr,
    ) -> Result<String, String> {
        match self.infer_kind(base) {
            ValKind::List(1) => {
                let v = self.cast_to(v, ty, "double")?;
                // bitcast via memory (LLVM may reject bitcast double→i64 on some targets)
                let a = self.fresh();
                let _ = writeln!(self.body, "  {} = alloca double, align 8", a);
                let _ = writeln!(self.body, "  store double {}, ptr {}, align 8", v, a);
                let b = self.fresh();
                let _ = writeln!(self.body, "  {} = load i64, ptr {}, align 8", b, a);
                Ok(b)
            }
            ValKind::List(3) | ValKind::List(4) | ValKind::ListObj(_) => {
                self.cast_to(v, ty, "i64") // ptr → i64
            }
            _ => self.cast_to(v, ty, "i64"),
        }
    }

    fn unpack_list_value(&mut self, raw: String, kind: &ValKind) -> Result<String, String> {
        match kind {
            ValKind::Float => {
                let a = self.fresh();
                let _ = writeln!(self.body, "  {} = alloca i64, align 8", a);
                let _ = writeln!(self.body, "  store i64 {}, ptr {}, align 8", raw, a);
                let b = self.fresh();
                let _ = writeln!(self.body, "  {} = load double, ptr {}, align 8", b, a);
                Ok(b)
            }
            ValKind::Str
            | ValKind::Ptr
            | ValKind::Object(_)
            | ValKind::Adt(_)
            | ValKind::Closure
            | ValKind::Dict
            | ValKind::List(_)
            | ValKind::NestedList(_) => {
                // list elements are stored as i64 (packed ptr); nested list /
                // object elements must be re-cast to ptr for iteration/access
                let b = self.fresh();
                let _ = writeln!(self.body, "  {} = inttoptr i64 {} to ptr", b, raw);
                Ok(b)
            }
            _ => Ok(raw),
        }
    }

    /// `"tpl {}".format(a, b)` → bolide_string_format
    fn emit_string_format(
        &mut self,
        template: String,
        args: &[Expr],
    ) -> Result<(String, &'static str), String> {
        let n = args.len();
        // alloca [N x ptr]
        let arr = self.fresh_local("__fmt_args");
        if n == 0 {
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call ptr @bolide_string_format(ptr {}, ptr null, i64 0, ptr null, ptr null, i64 0)",
                d, template
            );
            return Ok((d, "ptr"));
        }
        let _ = writeln!(
            self.body,
            "  {} = alloca [{} x ptr], align 8",
            arr, n
        );
        for (i, a) in args.iter().enumerate() {
            let sp = self.expr_to_string_ptr(a)?;
            let gep = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = getelementptr inbounds [{} x ptr], ptr {}, i64 0, i64 {}",
                gep, n, arr, i
            );
            let _ = writeln!(self.body, "  store ptr {}, ptr {}, align 8", sp, gep);
        }
        let base = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = getelementptr inbounds [{} x ptr], ptr {}, i64 0, i64 0",
            base, n, arr
        );
        let d = self.fresh();
        let _ = writeln!(
            self.body,
            "  {} = call ptr @bolide_string_format(ptr {}, ptr {}, i64 {}, ptr null, ptr null, i64 0)",
            d, template, base, n
        );
        Ok((d, "ptr"))
    }

    fn expr_to_string_ptr(&mut self, expr: &Expr) -> Result<String, String> {
        let (v, ty) = self.emit_expr(expr)?;
        match self.infer_kind(expr) {
            ValKind::Str => Ok(v),
            ValKind::Float => {
                let v = self.cast_to(v, ty, "double")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_string_from_float(double {})",
                    d, v
                );
                Ok(d)
            }
            ValKind::Bool => {
                let v = self.cast_to(v, ty, "i64")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_string_from_bool(i64 {})",
                    d, v
                );
                Ok(d)
            }
            ValKind::Dynamic => {
                let v = self.cast_to(v, ty, "ptr")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_dynamic_to_string(ptr {})",
                    d, v
                );
                Ok(d)
            }
            _ => {
                if ty == "ptr" {
                    return Ok(v);
                }
                let v = self.cast_to(v, ty, "i64")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_string_from_int(i64 {})",
                    d, v
                );
                Ok(d)
            }
        }
    }

    fn emit_binop(
        &mut self,
        l: &Expr,
        op: BinOp,
        r: &Expr,
    ) -> Result<(String, &'static str), String> {
        // BigInt arithmetic / comparison → runtime bigint ops
        if matches!(self.infer_kind(l), ValKind::BigInt)
            || matches!(self.infer_kind(r), ValKind::BigInt)
        {
            let (lv, _) = self.emit_expr(l)?;
            let (rv, _) = self.emit_expr(r)?;
            let lv = self.cast_to(lv, "ptr", "ptr")?;
            let rv = self.cast_to(rv, "ptr", "ptr")?;
            let name = match op {
                BinOp::Add => "bolide_bigint_add",
                BinOp::Sub => "bolide_bigint_sub",
                BinOp::Mul => "bolide_bigint_mul",
                BinOp::Div => "bolide_bigint_div",
                BinOp::Mod => "bolide_bigint_rem",
                BinOp::Eq => "bolide_bigint_eq",
                BinOp::Ne => "bolide_bigint_ne",
                BinOp::Lt => "bolide_bigint_lt",
                BinOp::Le => "bolide_bigint_le",
                BinOp::Gt => "bolide_bigint_gt",
                BinOp::Ge => "bolide_bigint_ge",
                _ => {
                    return Err("LLVM backend: unsupported bigint binop".into());
                }
            };
            let d = self.fresh();
            let ret = if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                "i64"
            } else {
                "ptr"
            };
            let _ = writeln!(
                self.body,
                "  {} = call {} @{}(ptr {}, ptr {})",
                d, ret, name, lv, rv
            );
            return Ok((d, ret));
        }
        // string + string → concat
        if matches!(op, BinOp::Add)
            && matches!(self.infer_kind(l), ValKind::Str)
            && matches!(self.infer_kind(r), ValKind::Str)
        {
            let (lv, _) = self.emit_expr(l)?;
            let (rv, _) = self.emit_expr(r)?;
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call ptr @bolide_string_concat(ptr {}, ptr {})",
                d, lv, rv
            );
            return Ok((d, "ptr"));
        }

        // string comparisons must compare contents, not pointer identity
        if matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        ) && matches!(self.infer_kind(l), ValKind::Str)
            && matches!(self.infer_kind(r), ValKind::Str)
        {
            let (lv, lt) = self.emit_expr(l)?;
            let (rv, rt) = self.emit_expr(r)?;
            let lv = self.cast_to(lv, lt, "ptr")?;
            let rv = self.cast_to(rv, rt, "ptr")?;
            let z = self.fresh();
            if matches!(op, BinOp::Eq | BinOp::Ne) {
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_string_eq(ptr {}, ptr {})",
                    z, lv, rv
                );
                if matches!(op, BinOp::Ne) {
                    let d = self.fresh();
                    let _ = writeln!(self.body, "  {} = xor i64 {}, 1", d, z);
                    return Ok((d, "i64"));
                }
                return Ok((z, "i64"));
            }
            let cmp = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call i64 @bolide_string_compare(ptr {}, ptr {})",
                cmp, lv, rv
            );
            let pred = match op {
                BinOp::Lt => "slt",
                BinOp::Le => "sle",
                BinOp::Gt => "sgt",
                BinOp::Ge => "sge",
                _ => unreachable!(),
            };
            let b1 = self.fresh();
            let _ = writeln!(self.body, "  {} = icmp {} i64 {}, 0", b1, pred, cmp);
            let d = self.fresh();
            let _ = writeln!(self.body, "  {} = zext i1 {} to i64", d, b1);
            return Ok((d, "i64"));
        }

        // Class operator overload: left.__op__(right) or right.__rop__(left)
        let lk = self.infer_kind(l);
        let rk = self.infer_kind(r);
        if let ValKind::Object(ref cn) = lk {
            if let Some(m) = binop_method(&op) {
                if self
                    .classes
                    .get(cn)
                    .map(|c| c.methods.contains_key(m))
                    .unwrap_or(false)
                {
                    let full = method_full_name(cn, m);
                    return self.emit_named_call(&full, &[l.clone(), r.clone()]);
                }
            }
        }
        if let ValKind::Object(ref cn) = rk {
            if let Some(m) = reflected_binop_method(&op) {
                if self
                    .classes
                    .get(cn)
                    .map(|c| c.methods.contains_key(m))
                    .unwrap_or(false)
                {
                    let full = method_full_name(cn, m);
                    return self.emit_named_call(&full, &[r.clone(), l.clone()]);
                }
            }
        }

        let (lv, lt) = self.emit_expr(l)?;
        let (rv, rt) = self.emit_expr(r)?;
        if lt == "double" || rt == "double" {
            let lv = self.cast_to(lv, lt, "double")?;
            let rv = self.cast_to(rv, rt, "double")?;
            let dst = self.fresh();
            match op {
                BinOp::Add => {
                    let _ = writeln!(self.body, "  {} = fadd double {}, {}", dst, lv, rv);
                    return Ok((dst, "double"));
                }
                BinOp::Sub => {
                    let _ = writeln!(self.body, "  {} = fsub double {}, {}", dst, lv, rv);
                    return Ok((dst, "double"));
                }
                BinOp::Mul => {
                    let _ = writeln!(self.body, "  {} = fmul double {}, {}", dst, lv, rv);
                    return Ok((dst, "double"));
                }
                BinOp::Div => {
                    let _ = writeln!(self.body, "  {} = fdiv double {}, {}", dst, lv, rv);
                    return Ok((dst, "double"));
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let pred = match op {
                        BinOp::Eq => "oeq",
                        BinOp::Ne => "one",
                        BinOp::Lt => "olt",
                        BinOp::Le => "ole",
                        BinOp::Gt => "ogt",
                        BinOp::Ge => "oge",
                        _ => unreachable!(),
                    };
                    let _ = writeln!(
                        self.body,
                        "  {} = fcmp {} double {}, {}",
                        dst, pred, lv, rv
                    );
                    let z = self.fresh();
                    let _ = writeln!(self.body, "  {} = zext i1 {} to i64", z, dst);
                    return Ok((z, "i64"));
                }
                _ => return Err("LLVM backend: unsupported float binop".into()),
            }
        }

        let lv = self.cast_to(lv, lt, "i64")?;
        let rv = self.cast_to(rv, rt, "i64")?;
        let dst = self.fresh();
        match op {
            BinOp::Add => {
                let _ = writeln!(self.body, "  {} = add i64 {}, {}", dst, lv, rv);
            }
            BinOp::Sub => {
                let _ = writeln!(self.body, "  {} = sub i64 {}, {}", dst, lv, rv);
            }
            BinOp::Mul => {
                let _ = writeln!(self.body, "  {} = mul i64 {}, {}", dst, lv, rv);
            }
            BinOp::Div => {
                let _ = writeln!(self.body, "  {} = sdiv i64 {}, {}", dst, lv, rv);
            }
            BinOp::Mod => {
                let _ = writeln!(self.body, "  {} = srem i64 {}, {}", dst, lv, rv);
            }
            BinOp::Eq => {
                let _ = writeln!(self.body, "  {} = icmp eq i64 {}, {}", dst, lv, rv);
                let z = self.fresh();
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", z, dst);
                return Ok((z, "i64"));
            }
            BinOp::Ne => {
                let _ = writeln!(self.body, "  {} = icmp ne i64 {}, {}", dst, lv, rv);
                let z = self.fresh();
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", z, dst);
                return Ok((z, "i64"));
            }
            BinOp::Lt => {
                let _ = writeln!(self.body, "  {} = icmp slt i64 {}, {}", dst, lv, rv);
                let z = self.fresh();
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", z, dst);
                return Ok((z, "i64"));
            }
            BinOp::Le => {
                let _ = writeln!(self.body, "  {} = icmp sle i64 {}, {}", dst, lv, rv);
                let z = self.fresh();
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", z, dst);
                return Ok((z, "i64"));
            }
            BinOp::Gt => {
                let _ = writeln!(self.body, "  {} = icmp sgt i64 {}, {}", dst, lv, rv);
                let z = self.fresh();
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", z, dst);
                return Ok((z, "i64"));
            }
            BinOp::Ge => {
                let _ = writeln!(self.body, "  {} = icmp sge i64 {}, {}", dst, lv, rv);
                let z = self.fresh();
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", z, dst);
                return Ok((z, "i64"));
            }
            BinOp::And | BinOp::BitAnd => {
                let _ = writeln!(self.body, "  {} = and i64 {}, {}", dst, lv, rv);
            }
            BinOp::Or | BinOp::BitOr => {
                let _ = writeln!(self.body, "  {} = or i64 {}, {}", dst, lv, rv);
            }
            BinOp::Xor => {
                let _ = writeln!(self.body, "  {} = xor i64 {}, {}", dst, lv, rv);
            }
            BinOp::Shl => {
                let _ = writeln!(self.body, "  {} = shl i64 {}, {}", dst, lv, rv);
            }
            BinOp::Shr => {
                let _ = writeln!(self.body, "  {} = ashr i64 {}, {}", dst, lv, rv);
            }
            _ => return Err("LLVM backend: unsupported binop".into()),
        }
        Ok((dst, "i64"))
    }

    fn emit_unary(&mut self, op: UnaryOp, e: &Expr) -> Result<(String, &'static str), String> {
        if let ValKind::Object(ref cn) = self.infer_kind(e) {
            if let Some(m) = unary_method(&op) {
                if self
                    .classes
                    .get(cn)
                    .map(|c| c.methods.contains_key(m))
                    .unwrap_or(false)
                {
                    let full = method_full_name(cn, m);
                    return self.emit_named_call(&full, &[e.clone()]);
                }
            }
        }
        let (v, ty) = self.emit_expr(e)?;
        match op {
            UnaryOp::Neg => {
                if ty == "double" {
                    let d = self.fresh();
                    let _ = writeln!(self.body, "  {} = fneg double {}", d, v);
                    Ok((d, "double"))
                } else {
                    let d = self.fresh();
                    let _ = writeln!(self.body, "  {} = sub i64 0, {}", d, v);
                    Ok((d, "i64"))
                }
            }
            UnaryOp::Not => {
                let v = self.cast_to(v, ty, "i64")?;
                let c = self.fresh();
                let _ = writeln!(self.body, "  {} = icmp eq i64 {}, 0", c, v);
                let z = self.fresh();
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", z, c);
                Ok((z, "i64"))
            }
        }
    }

    fn flatten_args(args: &[Expr]) -> Vec<Expr> {
        let mut out = Vec::new();
        for a in args {
            match a {
                Expr::NamedArg(_, e) => out.push(*e.clone()),
                Expr::SpreadArg(e) | Expr::KwSpreadArg(e) => out.push(*e.clone()),
                other => out.push(other.clone()),
            }
        }
        out
    }

    /// Compute, per raw call arg, the parameter index it targets (named args by
    /// name, positional args in order). Keeps SOURCE order so the callee
    /// evaluates args in source order while binding each to the right parameter.
    /// Mirrors the Cranelift backend's `normalize_args_for_params`.
    fn arg_target_indices(
        params: &[Param],
        raw_args: &[Expr],
        call_name: &str,
    ) -> Result<Vec<usize>, String> {
        let mut used = vec![false; params.len()];
        let mut next_pos = 0usize;
        let mut out = Vec::with_capacity(raw_args.len());
        for a in raw_args {
            match a {
                Expr::NamedArg(name, _) => {
                    let i = params
                        .iter()
                        .position(|p| !p.is_variadic && !p.is_kw_variadic && p.name == *name)
                        .ok_or_else(|| {
                            format!("{} has no parameter named '{}'", call_name, name)
                        })?;
                    if used[i] {
                        return Err(format!(
                            "{} got multiple values for argument '{}'",
                            call_name, name
                        ));
                    }
                    used[i] = true;
                    out.push(i);
                }
                _ => {
                    while next_pos < params.len() && used[next_pos] {
                        next_pos += 1;
                    }
                    let i = next_pos.min(params.len());
                    if i < params.len() {
                        used[i] = true;
                        next_pos += 1;
                    }
                    out.push(i);
                }
            }
        }
        Ok(out)
    }

    fn emit_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
    ) -> Result<(String, &'static str), String> {
        let raw_args = args.to_vec();
        let args = Self::flatten_args(args);
        let args = args.as_slice();
        if let Expr::Ident(name) = callee {
            // local or global that is a closure / func value
            let is_func_value = self.locals.contains_key(name)
                || self
                    .global_vars
                    .get(name)
                    .map(|(_, k)| matches!(k, ValKind::Closure | ValKind::Ptr))
                    .unwrap_or(false)
                || self
                    .local_kind
                    .get(name)
                    .map(|k| matches!(k, ValKind::Closure))
                    .unwrap_or(false);
            if is_func_value && !self.funcs.contains_key(name) && !self.classes.contains_key(name)
            {
                let (clo, _) = self.emit_expr(callee)?;
                // raw FuncSig param (bare function pointer) → direct call; a
                // closure object → closure ABI call
                if self
                    .local_kind
                    .get(name)
                    .map(|k| matches!(k, ValKind::RawFunc))
                    .unwrap_or(false)
                    || self
                        .global_vars
                        .get(name)
                        .map(|(_, k)| matches!(k, ValKind::RawFunc))
                        .unwrap_or(false)
                {
                    return self.emit_raw_func_call(&clo, args, "i64");
                }
                return self.emit_closure_call(&clo, args, "i64");
            }
            if name == "print" {
                return self.emit_print(args);
            }
            if name == "int" && args.len() == 1 {
                let (v, ty) = self.emit_expr(&args[0])?;
                if matches!(self.infer_kind(&args[0]), ValKind::Dynamic) {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_dynamic_to_int(ptr {})",
                        d, v
                    );
                    return Ok((d, "i64"));
                }
                if ty == "ptr" || matches!(self.infer_kind(&args[0]), ValKind::Str) {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_string_to_int(ptr {})",
                        d, v
                    );
                    return Ok((d, "i64"));
                }
                return Ok((self.cast_to(v, ty, "i64")?, "i64"));
            }
            if name == "float" && args.len() == 1 {
                let (v, ty) = self.emit_expr(&args[0])?;
                if matches!(self.infer_kind(&args[0]), ValKind::Dynamic) {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call double @bolide_dynamic_to_float(ptr {})",
                        d, v
                    );
                    return Ok((d, "double"));
                }
                if ty == "ptr" || matches!(self.infer_kind(&args[0]), ValKind::Str) {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call double @bolide_string_to_float(ptr {})",
                        d, v
                    );
                    return Ok((d, "double"));
                }
                return Ok((self.cast_to(v, ty, "double")?, "double"));
            }
            if name == "str" && args.len() == 1 {
                let (v, ty) = self.emit_expr(&args[0])?;
                let d = self.fresh();
                if matches!(self.infer_kind(&args[0]), ValKind::Dynamic) {
                    // dict value / dynamic → unwrap to its string form
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_dynamic_to_string(ptr {})",
                        d, v
                    );
                    return Ok((d, "ptr"));
                }
                if matches!(self.infer_kind(&args[0]), ValKind::BigInt) {
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_from_bigint(ptr {})",
                        d, v
                    );
                    return Ok((d, "ptr"));
                }
                if matches!(self.infer_kind(&args[0]), ValKind::Bool) {
                    // bools are stored as i64 0/1 in LLVM; convert to "true"/"false"
                    let v = self.cast_to(v, ty, "i64")?;
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_from_bool(i64 {})",
                        d, v
                    );
                } else if ty == "double" {
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_from_float(double {})",
                        d, v
                    );
                } else if ty == "ptr" {
                    return Ok((v, "ptr"));
                } else {
                    let v = self.cast_to(v, ty, "i64")?;
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_from_int(i64 {})",
                        d, v
                    );
                }
                return Ok((d, "ptr"));
            }
            if name == "range" {
                return Err("range only valid in for-loops".into());
            }
            if name == "bigint_debug_stats" && args.is_empty() {
                let _ = writeln!(self.body, "  call void @bolide_bigint_debug_stats()");
                return Ok(("0".into(), "i64"));
            }
            if name == "tuple_debug_stats" && args.is_empty() {
                let _ = writeln!(self.body, "  call void @bolide_tuple_debug_stats()");
                return Ok(("0".into(), "i64"));
            }
            if name == "bytes" && args.is_empty() {
                let d = self.fresh();
                let _ = writeln!(self.body, "  {} = call ptr @bolide_bytes_new()", d);
                return Ok((d, "ptr"));
            }
            if name == "channel" {
                if args.is_empty() {
                    let d = self.fresh();
                    let _ = writeln!(self.body, "  {} = call ptr @bolide_channel_create()", d);
                    return Ok((d, "ptr"));
                } else if args.len() == 1 {
                    // channel(capacity)
                    let (v, ty) = self.emit_expr(&args[0])?;
                    let v = self.cast_to(v, ty, "i64")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_channel_create_buffered(i64 {})",
                        d, v
                    );
                    return Ok((d, "ptr"));
                }
                return Err("channel expects 0 or 1 arguments".into());
            }
            if name == "input" && args.is_empty() {
                let d = self.fresh();
                let _ = writeln!(self.body, "  {} = call ptr @bolide_input()", d);
                return Ok((d, "ptr"));
            }
            if name == "bigint" && args.len() == 1 {
                let (v, ty) = self.emit_expr(&args[0])?;
                if ty == "ptr" {
                    // bigint("123") — string → bigint via string_from_bigint? No: parse digits.
                    return Err("LLVM: bigint(string) not supported yet".into());
                }
                let v = self.cast_to(v, ty, "i64")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_bigint_from_i64(i64 {})",
                    d, v
                );
                return Ok((d, "ptr"));
            }
            if name == "decimal" && args.len() == 1 {
                let (v, ty) = self.emit_expr(&args[0])?;
                if ty == "double" {
                    // decimal(float) unsupported for now
                    return Err("LLVM: decimal(float) not supported yet".into());
                }
                let v = self.cast_to(v, ty, "i64")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_decimal_from_i64(i64 {})",
                    d, v
                );
                return Ok((d, "ptr"));
            }
            // async 函数调用 → 协程 spawn（返回 Future 指针，供 await）
            if self.async_funcs.contains(name) {
                return self.emit_coroutine_spawn(name, &raw_args);
            }
            // Class constructor
            if self.classes.contains_key(name) {
                return self.emit_named_call(name, &raw_args);
            }
            // free function
            if self.funcs.contains_key(name) {
                return self.emit_named_call(name, &raw_args);
            }
            // maybe closure in local under different path
            if self.locals.contains_key(name) {
                let (clo, _) = self.emit_expr(callee)?;
                return self.emit_closure_call(&clo, args, "i64");
            }
            return self.emit_named_call(name, &raw_args);
        }
        // obj.method(args) or Enum.Variant(args) or module.fn(args)
        if let Expr::Member(base, method) = callee {
            if let Expr::Ident(mod_name) = base.as_ref() {
                if self.adts.contains_key(mod_name) {
                    return self.emit_adt_construct(mod_name, method, args);
                }
                if let Some(prefix) = self.modules.get(mod_name).cloned() {
                    let candidates = [
                        format!("{}{}", prefix, method),
                        format!("@{}_{}", mod_name, method),
                        format!("{}_{}", mod_name, method),
                    ];
                    for fname in &candidates {
                        if self.funcs.contains_key(fname) {
                            return self.emit_named_call(fname, &raw_args);
                        }
                    }
                    // still try primary mangled name for better error
                    return self.emit_named_call(&candidates[0], &raw_args);
                }
            }
            return self.emit_method_call(base, method, args);
        }
        // call a computed closure value: (expr)(args)
        let (clo, _) = self.emit_expr(callee)?;
        self.emit_closure_call(&clo, args, "i64")
    }

    /// Generate a bare-call trampoline for a closure passed to a `func(...)`
    /// extern param. The runtime invokes it as `extern "C" fn(params) -> ret`,
    /// so the trampoline loads the closure object from a global `@__cb_N`,
    /// extracts fn_ptr/env_ptr, and calls it with the closure ABI `(env, args)`.
    /// Returns the trampoline name to pass as the callback address.
    fn emit_funcptr_trampoline(
        &mut self,
        sig: &(Vec<&'static str>, &'static str),
        closure_val: String,
    ) -> Result<String, String> {
        let n = self.tmp;
        self.tmp += 1;
        let gname = format!("__cb_{}", n);
        let tname = format!("__cb_tramp_{}", n);
        self.cb_globals.push(gname.clone());

        let (params, ret) = sig;
        let mut params_s = String::new();
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                params_s.push_str(", ");
            }
            let _ = write!(params_s, "{} %a{}", p, i);
        }
        let mut call_args = String::from("ptr %env");
        for (i, p) in params.iter().enumerate() {
            let _ = write!(call_args, ", {} %a{}", p, i);
        }
        let mut def = String::new();
        let _ = writeln!(def, "define {} @{}({}) {{", ret, tname, params_s);
        let _ = writeln!(def, "entry:");
        let _ = writeln!(def, "  %clo = load ptr, ptr @{}, align 8", gname);
        let _ = writeln!(def, "  %env = call ptr @bolide_closure_env_ptr(ptr %clo)");
        let _ = writeln!(def, "  %fn = call ptr @bolide_closure_fn_ptr(ptr %clo)");
        if *ret == "void" {
            let _ = writeln!(def, "  call void %fn({})", call_args);
            let _ = writeln!(def, "  ret void");
        } else {
            let _ = writeln!(def, "  %r = call {} %fn({})", ret, call_args);
            let _ = writeln!(def, "  ret {} %r", ret);
        }
        let _ = writeln!(def, "}}");
        let _ = writeln!(def);
        self.trampoline_ir.push_str(&def);

        // store the closure into the global so the trampoline can find it
        let _ = writeln!(
            self.body,
            "  store ptr {}, ptr @{}, align 8",
            closure_val, gname
        );
        Ok(tname)
    }

    /// Wrap an already-emitted value into a BolideDynamic, returning a ptr.
    /// `kind` is the value's inferred ValKind; `ty` is its LLVM type.
    fn wrap_as_dynamic(&mut self, v: String, ty: &str, kind: &ValKind) -> Result<String, String> {
        let d = self.fresh();
        let from_fn = match ty {
            "double" => "bolide_dynamic_from_float",
            "ptr" => match kind {
                ValKind::Dict => "bolide_dynamic_from_dict",
                ValKind::List(_) | ValKind::ListObj(_) | ValKind::NestedList(_) => {
                    "bolide_dynamic_from_list"
                }
                _ => "bolide_dynamic_from_string",
            },
            _ => {
                if matches!(kind, ValKind::Bool) {
                    "bolide_dynamic_from_bool"
                } else {
                    "bolide_dynamic_from_int"
                }
            }
        };
        let arg = if ty == "ptr" {
            v
        } else if ty == "double" {
            v
        } else {
            self.cast_to(v, ty, "i64")?
        };
        let _ = writeln!(
            self.body,
            "  {} = call ptr @{}({})",
            d, from_fn, if ty == "ptr" { format!("ptr {}", arg) } else if ty == "double" { format!("double {}", arg) } else { format!("i64 {}", arg) }
        );
        Ok(d)
    }

    /// Map a possibly module-qualified class name (`gui.Ui`, `@gui_Ui`) to the
    /// class name actually registered by `collect_classes` (`Ui`), trying the
    /// raw name, the module-prefixed name, and the short name.
    fn resolve_class_name(&self, name: &str) -> Option<String> {
        if self.classes.contains_key(name) {
            return Some(name.to_string());
        }
        if let Some((module, short)) = name.split_once('.') {
            let prefixed = format!("@{}_{}", module, short);
            if self.classes.contains_key(&prefixed) {
                return Some(prefixed);
            }
            if self.classes.contains_key(short) {
                return Some(short.to_string());
            }
        }
        if let Some(stripped) = name.strip_prefix('@') {
            if self.classes.contains_key(stripped) {
                return Some(stripped.to_string());
            }
        }
        None
    }

    /// Resolve a class method name on a class, walking the parent chain.
    fn find_class_method(&self, cn: &str, method: &str) -> Option<String> {
        let mut cur = self.resolve_class_name(cn);
        while let Some(c) = cur {
            if let Some(ci) = self.classes.get(&c) {
                if ci.methods.contains_key(method) {
                    return Some(method_full_name(&c, method));
                }
                cur = ci.parent.clone();
            } else {
                break;
            }
        }
        None
    }

    fn emit_method_call(
        &mut self,
        base: &Expr,
        method: &str,
        args: &[Expr],
    ) -> Result<(String, &'static str), String> {
        let (obj, _) = self.emit_expr(base)?;
        let base_kind = self.infer_kind(base);
        // String methods
        if matches!(base_kind, ValKind::Str) {
            match method {
                "replace" if args.len() == 2 => {
                    let (a, aty) = self.emit_expr(&args[0])?;
                    let (b, bty) = self.emit_expr(&args[1])?;
                    let a = self.cast_to(a, aty, "ptr")?;
                    let b = self.cast_to(b, bty, "ptr")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_replace(ptr {}, ptr {}, ptr {})",
                        d, obj, a, b
                    );
                    return Ok((d, "ptr"));
                }
                "upper" | "to_upper" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_upper(ptr {})",
                        d, obj
                    );
                    return Ok((d, "ptr"));
                }
                "lower" | "to_lower" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_lower(ptr {})",
                        d, obj
                    );
                    return Ok((d, "ptr"));
                }
                "trim" | "strip" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_trim(ptr {})",
                        d, obj
                    );
                    return Ok((d, "ptr"));
                }
                "repeat" if args.len() == 1 => {
                    let (n, nty) = self.emit_expr(&args[0])?;
                    let n = self.cast_to(n, nty, "i64")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_repeat(ptr {}, i64 {})",
                        d, obj, n
                    );
                    return Ok((d, "ptr"));
                }
                "find" | "index_of" if args.len() == 1 => {
                    let (a, aty) = self.emit_expr(&args[0])?;
                    let a = self.cast_to(a, aty, "ptr")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_string_find(ptr {}, ptr {})",
                        d, obj, a
                    );
                    return Ok((d, "i64"));
                }
                "contains" | "includes" if args.len() == 1 => {
                    let (a, aty) = self.emit_expr(&args[0])?;
                    let a = self.cast_to(a, aty, "ptr")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_string_contains(ptr {}, ptr {})",
                        d, obj, a
                    );
                    return Ok((d, "i64"));
                }
                "starts_with" if args.len() == 1 => {
                    let (a, aty) = self.emit_expr(&args[0])?;
                    let a = self.cast_to(a, aty, "ptr")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_string_starts_with(ptr {}, ptr {})",
                        d, obj, a
                    );
                    return Ok((d, "i64"));
                }
                "ends_with" if args.len() == 1 => {
                    let (a, aty) = self.emit_expr(&args[0])?;
                    let a = self.cast_to(a, aty, "ptr")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_string_ends_with(ptr {}, ptr {})",
                        d, obj, a
                    );
                    return Ok((d, "i64"));
                }
                "count" if args.len() == 1 => {
                    let (a, aty) = self.emit_expr(&args[0])?;
                    let a = self.cast_to(a, aty, "ptr")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_string_count(ptr {}, ptr {})",
                        d, obj, a
                    );
                    return Ok((d, "i64"));
                }
                "split" if args.len() == 1 => {
                    let (a, aty) = self.emit_expr(&args[0])?;
                    let a = self.cast_to(a, aty, "ptr")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_split(ptr {}, ptr {})",
                        d, obj, a
                    );
                    return Ok((d, "ptr"));
                }
                "slice" | "substring" if args.len() == 2 => {
                    let (a, at) = self.emit_expr(&args[0])?;
                    let (b, bt) = self.emit_expr(&args[1])?;
                    let a = self.cast_to(a, at, "i64")?;
                    let b = self.cast_to(b, bt, "i64")?;
                    let d = self.fresh();
                    // 运行时签名 (ptr, start, end, step, flags)；step=1, flags=both
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_slice(ptr {}, i64 {}, i64 {}, i64 1, i64 3)",
                        d, obj, a, b
                    );
                    return Ok((d, "ptr"));
                }
                "format" => return self.emit_string_format(obj, args),
                "char_at" if args.len() == 1 => {
                    let (ix, ixty) = self.emit_expr(&args[0])?;
                    let ix = self.cast_to(ix, ixty, "i64")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_string_char_at(ptr {}, i64 {})",
                        d, obj, ix
                    );
                    return Ok((d, "ptr"));
                }
                "len" | "length" | "size" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_string_len(ptr {})",
                        d, obj
                    );
                    return Ok((d, "i64"));
                }
                _ => {}
            }
        }
        // Bytes methods (dispatch before generic ptr/list fallback)
        if matches!(base_kind, ValKind::Bytes) {
            match method {
                "len" | "length" | "size" => {
                    if !args.is_empty() {
                        return Err(format!("bytes.{} expects 0 arguments", method));
                    }
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_bytes_len(ptr {})",
                        d, obj
                    );
                    return Ok((d, "i64"));
                }
                "capacity" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_bytes_capacity(ptr {})",
                        d, obj
                    );
                    return Ok((d, "i64"));
                }
                "get" if args.len() == 1 => {
                    let (ix, ixty) = self.emit_expr(&args[0])?;
                    let ix = self.cast_to(ix, ixty, "i64")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_bytes_get(ptr {}, i64 {})",
                        d, obj, ix
                    );
                    return Ok((d, "i64"));
                }
                "set" if args.len() == 2 => {
                    let (ix, ixty) = self.emit_expr(&args[0])?;
                    let (v, vty) = self.emit_expr(&args[1])?;
                    let ix = self.cast_to(ix, ixty, "i64")?;
                    let v = self.cast_to(v, vty, "i64")?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_bytes_set(ptr {}, i64 {}, i64 {})",
                        d, obj, ix, v
                    );
                    return Ok((d, "i64"));
                }
                "push" | "append" if args.len() == 1 => {
                    let (v, vty) = self.emit_expr(&args[0])?;
                    let v = self.cast_to(v, vty, "i64")?;
                    let _ = writeln!(
                        self.body,
                        "  call void @bolide_bytes_push(ptr {}, i64 {})",
                        obj, v
                    );
                    return Ok(("0".into(), "i64"));
                }
                "extend" if args.len() == 1 => {
                    let (o, oty) = self.emit_expr(&args[0])?;
                    let o = self.cast_to(o, oty, "ptr")?;
                    let _ = writeln!(
                        self.body,
                        "  call void @bolide_bytes_extend(ptr {}, ptr {})",
                        obj, o
                    );
                    return Ok(("0".into(), "i64"));
                }
                "copy" | "clone" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_bytes_clone(ptr {})",
                        d, obj
                    );
                    return Ok((d, "ptr"));
                }
                "to_string_lossy" | "to_string" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_bytes_to_string_lossy(ptr {})",
                        d, obj
                    );
                    return Ok((d, "ptr"));
                }
                _ => {
                    return Err(format!(
                        "LLVM backend: unknown Bytes method '{}'",
                        method
                    ))
                }
            }
        }
        // Dict methods first (avoid list-layout loads on dict ptrs)
        if matches!(base_kind, ValKind::Dict) {
            match method {
                "len" | "length" | "size" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_dict_len(ptr {})",
                        d, obj
                    );
                    return Ok((d, "i64"));
                }
                "is_empty" | "empty" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_dict_is_empty(ptr {})",
                        d, obj
                    );
                    return Ok((d, "i64"));
                }
                "contains" if args.len() == 1 => {
                    let (k, kty) = self.emit_expr(&args[0])?;
                    let key = self.pack_as_i64(k, kty)?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_dict_contains(ptr {}, i64 {})",
                        d, obj, key
                    );
                    return Ok((d, "i64"));
                }
                "get" if args.len() == 1 => {
                    let (k, kty) = self.emit_expr(&args[0])?;
                    let key = self.pack_as_i64(k, kty)?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_dict_get(ptr {}, i64 {})",
                        d, obj, key
                    );
                    return Ok((d, "i64"));
                }
                "set" if args.len() == 2 => {
                    let (k, kty) = self.emit_expr(&args[0])?;
                    let (v, vty) = self.emit_expr(&args[1])?;
                    let key = self.pack_as_i64(k, kty)?;
                    let k2 = self.infer_kind(&args[1]);
                    let dyn_ = self.wrap_as_dynamic(v, vty, &k2)?;
                    let val = self.pack_as_i64(dyn_, "ptr")?;
                    let _ = writeln!(
                        self.body,
                        "  call void @bolide_dict_set(ptr {}, i64 {}, i64 {})",
                        obj, key, val
                    );
                    return Ok(("0".into(), "i64"));
                }
                "remove" if args.len() == 1 => {
                    let (k, kty) = self.emit_expr(&args[0])?;
                    let key = self.pack_as_i64(k, kty)?;
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call i64 @bolide_dict_remove(ptr {}, i64 {})",
                        d, obj, key
                    );
                    return Ok((d, "i64"));
                }
                "keys" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_dict_keys(ptr {})",
                        d, obj
                    );
                    return Ok((d, "ptr"));
                }
                "values" => {
                    let d = self.fresh();
                    let _ = writeln!(
                        self.body,
                        "  {} = call ptr @bolide_dict_values(ptr {})",
                        d, obj
                    );
                    return Ok((d, "ptr"));
                }
                "clear" => {
                    let _ = writeln!(self.body, "  call void @bolide_dict_clear(ptr {})", obj);
                    return Ok(("0".into(), "i64"));
                }
                _ => {}
            }
        }
        // Class object method: dispatch BEFORE the generic list/string arms, which
        // would otherwise mis-handle same-named methods (e.g. `set` on a class vs
        // the list `set(index, value)` builtin).
        if let ValKind::Object(ref cn) = &base_kind {
            if let Some(full) = self.find_class_method(cn, method) {
                let mut call_args = vec![base.clone()];
                call_args.extend(args.iter().cloned());
                return self.emit_named_call(&full, &call_args);
            }
        }
        match method {
            "len" | "length" | "size" => {
                match base_kind {
                    ValKind::Str => {
                        let d = self.fresh();
                        let _ = writeln!(
                            self.body,
                            "  {} = call i64 @bolide_string_len(ptr {})",
                            d, obj
                        );
                        Ok((d, "i64"))
                    }
                    _ => {
                        // list: inline field load
                        let d = self.emit_list_len_inline(&obj)?;
                        Ok((d, "i64"))
                    }
                }
            }
            "push" | "append" => {
                if args.len() != 1 {
                    return Err("push expects 1 arg".into());
                }
                let (v, ty) = self.emit_expr(&args[0])?;
                let packed = self.pack_list_value(v, ty, base)?;
                let _ = writeln!(
                    self.body,
                    "  call void @bolide_list_push(ptr {}, i64 {})",
                    obj, packed
                );
                Ok(("0".into(), "i64"))
            }
            "pop" => {
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_list_pop(ptr {})",
                    d, obj
                );
                let kind = match self.infer_kind(base) {
                    ValKind::List(t) => list_tag_to_kind(t),
                    _ => ValKind::Int,
                };
                let v = self.unpack_list_value(d, &kind)?;
                Ok((v, kind_to_llvm(&kind)))
            }
            "resize" => {
                if args.len() != 2 {
                    return Err("resize expects (len, fill)".into());
                }
                let (n, nty) = self.emit_expr(&args[0])?;
                let n = self.cast_to(n, nty, "i64")?;
                let (f, fty) = self.emit_expr(&args[1])?;
                let f = self.pack_list_value(f, fty, base)?;
                let _ = writeln!(
                    self.body,
                    "  call void @bolide_list_resize(ptr {}, i64 {}, i64 {})",
                    obj, n, f
                );
                Ok(("0".into(), "i64"))
            }
            "reserve" => {
                if args.len() != 1 {
                    return Err("reserve expects 1 arg".into());
                }
                let (n, nty) = self.emit_expr(&args[0])?;
                let n = self.cast_to(n, nty, "i64")?;
                let _ = writeln!(
                    self.body,
                    "  call void @bolide_list_reserve(ptr {}, i64 {})",
                    obj, n
                );
                Ok(("0".into(), "i64"))
            }
            "clear" => {
                let _ = writeln!(self.body, "  call void @bolide_list_clear(ptr {})", obj);
                Ok(("0".into(), "i64"))
            }
            "is_empty" | "empty" => {
                let len = self.emit_list_len_inline(&obj)?;
                let z = self.fresh();
                let _ = writeln!(self.body, "  {} = icmp eq i64 {}, 0", z, len);
                let d = self.fresh();
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", d, z);
                Ok((d, "i64"))
            }
            "first" => {
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_list_first(ptr {})",
                    d, obj
                );
                let kind = match self.infer_kind(base) {
                    ValKind::List(t) => list_tag_to_kind(t),
                    _ => ValKind::Int,
                };
                let v = self.unpack_list_value(d, &kind)?;
                Ok((v, kind_to_llvm(&kind)))
            }
            "last" => {
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_list_last(ptr {})",
                    d, obj
                );
                let kind = match self.infer_kind(base) {
                    ValKind::List(t) => list_tag_to_kind(t),
                    _ => ValKind::Int,
                };
                let v = self.unpack_list_value(d, &kind)?;
                Ok((v, kind_to_llvm(&kind)))
            }
            "reverse" => {
                let _ = writeln!(self.body, "  call void @bolide_list_reverse(ptr {})", obj);
                Ok(("0".into(), "i64"))
            }
            "sort" => {
                let _ = writeln!(self.body, "  call void @bolide_list_sort(ptr {})", obj);
                Ok(("0".into(), "i64"))
            }
            "contains" => {
                if args.len() != 1 {
                    return Err("contains expects 1 arg".into());
                }
                let (v, ty) = self.emit_expr(&args[0])?;
                let packed = self.pack_list_value(v, ty, base)?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_list_contains(ptr {}, i64 {})",
                    d, obj, packed
                );
                Ok((d, "i64"))
            }
            "get" => {
                if args.len() != 1 {
                    return Err("get expects 1 arg".into());
                }
                let (ix, ixty) = self.emit_expr(&args[0])?;
                let ix = self.cast_to(ix, ixty, "i64")?;
                let tag = match self.infer_kind(base) {
                    ValKind::List(t) => t,
                    _ => 0,
                };
                let kind = list_tag_to_kind(tag);
                let raw = self.emit_list_get_inline(&obj, &ix, tag)?;
                let v = self.unpack_list_value(raw, &kind)?;
                Ok((v, kind_to_llvm(&kind)))
            }
            "format" => self.emit_string_format(obj, args),
            "char_at" => {
                if args.len() != 1 {
                    return Err("char_at expects 1 arg".into());
                }
                let (ix, ixty) = self.emit_expr(&args[0])?;
                let ix = self.cast_to(ix, ixty, "i64")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_string_char_at(ptr {}, i64 {})",
                    d, obj, ix
                );
                Ok((d, "ptr"))
            }
            "set" => {
                if args.len() != 2 {
                    return Err("set expects 2 args".into());
                }
                let (ix, ixty) = self.emit_expr(&args[0])?;
                let ix = self.cast_to(ix, ixty, "i64")?;
                let (v, ty) = self.emit_expr(&args[1])?;
                let packed = self.pack_list_value(v, ty, base)?;
                let tag = match self.infer_kind(base) {
                    ValKind::List(t) => t,
                    _ => 0,
                };
                self.emit_list_set_inline(&obj, &ix, &packed, tag)?;
                Ok(("1".into(), "i64"))
            }
            "insert" => {
                if args.len() != 2 {
                    return Err("insert expects (index, value)".into());
                }
                let (ix, ixty) = self.emit_expr(&args[0])?;
                let ix = self.cast_to(ix, ixty, "i64")?;
                let (v, ty) = self.emit_expr(&args[1])?;
                let packed = self.pack_list_value(v, ty, base)?;
                let _ = writeln!(
                    self.body,
                    "  call void @bolide_list_insert(ptr {}, i64 {}, i64 {})",
                    obj, ix, packed
                );
                Ok(("0".into(), "i64"))
            }
            "remove" => {
                if args.len() != 1 {
                    return Err("remove expects 1 arg".into());
                }
                let (ix, ixty) = self.emit_expr(&args[0])?;
                let ix = self.cast_to(ix, ixty, "i64")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_list_remove(ptr {}, i64 {})",
                    d, obj, ix
                );
                let kind = match self.infer_kind(base) {
                    ValKind::List(t) => list_tag_to_kind(t),
                    _ => ValKind::Int,
                };
                let v = self.unpack_list_value(d, &kind)?;
                return Ok((v, kind_to_llvm(&kind)));
            }
            "index_of" | "index" | "find" => {
                if args.len() != 1 {
                    return Err("index_of expects 1 arg".into());
                }
                let (v, ty) = self.emit_expr(&args[0])?;
                let packed = self.pack_list_value(v, ty, base)?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_list_index_of(ptr {}, i64 {})",
                    d, obj, packed
                );
                return Ok((d, "i64"));
            }
            "count" => {
                if args.len() != 1 {
                    return Err("count expects 1 arg".into());
                }
                let (v, ty) = self.emit_expr(&args[0])?;
                let packed = self.pack_list_value(v, ty, base)?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_list_count(ptr {}, i64 {})",
                    d, obj, packed
                );
                return Ok((d, "i64"));
            }
            "extend" => {
                if args.len() != 1 {
                    return Err("extend expects 1 arg".into());
                }
                let (o, oty) = self.emit_expr(&args[0])?;
                let o = self.cast_to(o, oty, "ptr")?;
                let _ = writeln!(
                    self.body,
                    "  call void @bolide_list_extend(ptr {}, ptr {})",
                    obj, o
                );
                return Ok(("0".into(), "i64"));
            }
            "clone" => {
                if args.len() != 0 {
                    return Err("clone expects 0 args".into());
                }
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_list_clone(ptr {})",
                    d, obj
                );
                return Ok((d, "ptr"));
            }
            "send" => {
                // channel.send(value) → bolide_channel_send(chan, packed)
                if args.len() != 1 {
                    return Err("send expects 1 arg".into());
                }
                let (v, ty) = self.emit_expr(&args[0])?;
                let packed = self.cast_to(v, ty, "i64")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_channel_send(ptr {}, i64 {})",
                    d, obj, packed
                );
                return Ok((d, "i64"));
            }
            "recv" => {
                if args.len() != 0 {
                    return Err("recv expects 0 args".into());
                }
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call i64 @bolide_channel_recv(ptr {})",
                    d, obj
                );
                return Ok((d, "i64"));
            }
            "close" => {
                let _ = writeln!(self.body, "  call void @bolide_channel_close(ptr {})", obj);
                return Ok(("0".into(), "i64"));
            }
            other => {
                // Class method call (walk parent chain for inherited methods)
                if let ValKind::Object(ref cn) = self.infer_kind(base) {
                    if let Some(full) = self.find_class_method(cn, other) {
                        let mut call_args = vec![base.clone()];
                        call_args.extend(args.iter().cloned());
                        return self.emit_named_call(&full, &call_args);
                    }
                }
                // dict helpers
                if matches!(self.infer_kind(base), ValKind::Dict) {
                    match other {
                        "len" | "length" | "size" => {
                            let d = self.fresh();
                            let _ = writeln!(
                                self.body,
                                "  {} = call i64 @bolide_dict_len(ptr {})",
                                d, obj
                            );
                            return Ok((d, "i64"));
                        }
                        "contains" if args.len() == 1 => {
                            let (k, kty) = self.emit_expr(&args[0])?;
                            let key = self.pack_as_i64(k, kty)?;
                            let d = self.fresh();
                            let _ = writeln!(
                                self.body,
                                "  {} = call i64 @bolide_dict_contains(ptr {}, i64 {})",
                                d, obj, key
                            );
                            return Ok((d, "i64"));
                        }
                        "get" if args.len() == 1 => {
                            let (k, kty) = self.emit_expr(&args[0])?;
                            let key = self.pack_as_i64(k, kty)?;
                            let d = self.fresh();
                            let _ = writeln!(
                                self.body,
                                "  {} = call i64 @bolide_dict_get(ptr {}, i64 {})",
                                d, obj, key
                            );
                            return Ok((d, "i64"));
                        }
                        "remove" if args.len() == 1 => {
                            let (k, kty) = self.emit_expr(&args[0])?;
                            let key = self.pack_as_i64(k, kty)?;
                            let d = self.fresh();
                            let _ = writeln!(
                                self.body,
                                "  {} = call i64 @bolide_dict_remove(ptr {}, i64 {})",
                                d, obj, key
                            );
                            return Ok((d, "i64"));
                        }
                        "keys" => {
                            let d = self.fresh();
                            let _ = writeln!(
                                self.body,
                                "  {} = call ptr @bolide_dict_keys(ptr {})",
                                d, obj
                            );
                            return Ok((d, "ptr"));
                        }
                        "values" => {
                            let d = self.fresh();
                            let _ = writeln!(
                                self.body,
                                "  {} = call ptr @bolide_dict_values(ptr {})",
                                d, obj
                            );
                            return Ok((d, "ptr"));
                        }
                        "clear" => {
                            let _ = writeln!(
                                self.body,
                                "  call void @bolide_dict_clear(ptr {})",
                                obj
                            );
                            return Ok(("0".into(), "i64"));
                        }
                        _ => {}
                    }
                }
                Err(format!(
                    "LLVM backend: method '{}' not supported yet",
                    other
                ))
            }
        }
    }

    fn emit_named_call(
        &mut self,
        name: &str,
        args: &[Expr],
    ) -> Result<(String, &'static str), String> {
        let overload_owner;
        let name: &str = if let Some(candidates) = self.overloads.get(name).cloned() {
            overload_owner = self
                .resolve_overload(&candidates, args)
                .unwrap_or_else(|| candidates[0].clone());
            overload_owner.as_str()
        } else {
            name
        };
        let resolved = if self.funcs.contains_key(name) {
            name.to_string()
        } else if self.classes.contains_key(name) {
            name.to_string()
        } else {
            // tolerate missing @ prefix
            let alt = if name.starts_with('@') {
                name.to_string()
            } else {
                format!("@{}", name)
            };
            if self.funcs.contains_key(&alt) {
                alt
            } else if name.starts_with('@') {
                // module-rewritten class ctor: @time_Timer → Timer
                if let Some(short) = name.rsplit('_').next() {
                    if self.classes.contains_key(short) || self.funcs.contains_key(short) {
                        short.to_string()
                    } else {
                        name.to_string()
                    }
                } else {
                    name.to_string()
                }
            } else {
                // exact llvm name only — never fuzzy ends_with (matches "r" → BitXor)
                name.to_string()
            }
        };
        let (param_tys, ret) = self.funcs.get(&resolved).cloned().ok_or_else(|| {
            format!(
                "LLVM backend: unknown function '{}' (try cranelift for full language)",
                name
            )
        })?;
        // 命名参数：算出每个实参的目标参数位（保持源码求值顺序，绑定到正确参数位）
        let (flat_args, target_indices) = if let Some(params) = self.func_params.get(&resolved) {
            let indices = Self::arg_target_indices(params, args, &resolved)?;
            (Self::flatten_args(args), indices)
        } else {
            (Self::flatten_args(args), (0..args.len()).collect())
        };
        if flat_args.len() != param_tys.len() {
            return Err(format!(
                "function '{}' expects {} args, got {}",
                resolved,
                param_tys.len(),
                flat_args.len()
            ));
        }
        // 每个参数位预先算好的调用参数字符串（"i64 %v" / "ptr @g"）；按源码顺序求值填入，
        // 最后按参数顺序拼装 call。这样命名参数既绑定正确，又保持源码求值顺序。
        let mut prepared: Vec<Option<String>> = vec![None; param_tys.len()];
        let funcptr_flags = self.extern_funcptr.get(&resolved).cloned();
        for (src_idx, a) in flat_args.iter().enumerate() {
            let ti = target_indices[src_idx];
            // func(...) callback param on an extern: the runtime invokes it as a raw
            // `extern "C" fn(...)`, so a bare function name must be passed as its
            // address — NOT wrapped into a closure object (which the runtime would
            // call as a bare pointer and crash, e.g. gui.run(root), web handlers).
            if funcptr_flags.as_ref().map(|f| f.get(ti)).flatten() == Some(&true) {
                if let Expr::Ident(fname) = a {
                    // only bare top-level functions have a stable symbol; local
                    // closures fall through to the closure-object path
                    if self.funcs.contains_key(fname) && !self.locals.contains_key(fname) {
                        let link = llvm_func_name(fname);
                        prepared[ti] = Some(format!("ptr @{}", link));
                        continue;
                    }
                }
                // forwarded/local closure: the runtime invokes it as a raw
                // `extern "C" fn(...)`. A top-level-function closure now has
                // fn_ptr = the bare function, so passing the closure object
                // directly makes the runtime call `@func(obj)` — correct, and
                // avoids a per-method shared trampoline global (which would make
                // every route dispatch to the same closure).
                let (v, ty) = self.emit_expr(a)?;
                let v = self.cast_to(v, ty, "ptr")?;
                prepared[ti] = Some(format!("ptr {}", v));
                continue;
            }
            // raw FuncSig param (bare function pointer, not closure-expecting):
            // a bare function name is passed as its address so the callee can
            // forward it to a `func(*c_void)` extern (web/GUI callbacks).
            let is_raw_funcsig = self
                .func_params
                .get(&resolved)
                .and_then(|params| params.get(ti))
                .map(|p| matches!(p.ty, Type::Func | Type::FuncSig(_, _)))
                .unwrap_or(false)
                && !self
                    .funcsig_closure_params
                    .get(&resolved)
                    .map(|s| s.contains(&ti))
                    .unwrap_or(false);
            if is_raw_funcsig {
                if let Expr::Ident(fname) = a {
                    if self.funcs.contains_key(fname) && !self.locals.contains_key(fname) {
                        let link = llvm_func_name(fname);
                        prepared[ti] = Some(format!("ptr @{}", link));
                        continue;
                    }
                }
            }
            // ref 参数：传递调用方变量的地址（局部 alloca 槽 / 全局符号 / 上层 ref 地址），
            // 被调方经指针原地读写，返回即写回。
            let param_is_ref = self
                .func_params
                .get(&resolved)
                .and_then(|params| params.get(ti))
                .map(|p| p.mode == bolide_parser::ParamMode::Ref)
                .unwrap_or(false);
            if param_is_ref {
                if let Expr::Ident(vname) = a {
                    if self.ref_params.contains(vname) {
                        // 转发 ref 参数：取出其持有的地址
                        let addr_slot = self.ref_param_addr_slot.get(vname).cloned().unwrap();
                        let addr = self.fresh();
                        let _ = writeln!(
                            self.body,
                            "  {} = load ptr, ptr {}, align 8",
                            addr, addr_slot
                        );
                        prepared[ti] = Some(format!("ptr {}", addr));
                        continue;
                    }
                    if let Some((slot, _)) = self.locals.get(vname).cloned() {
                        // 局部变量：alloca 的地址就是指针
                        prepared[ti] = Some(format!("ptr {}", slot));
                        continue;
                    }
                    if self.global_vars.contains_key(vname) {
                        let g = llvm_func_name(vname);
                        prepared[ti] = Some(format!("ptr @{}", g));
                        continue;
                    }
                }
                return Err(format!(
                    "ref parameter must be a variable, got {:?}",
                    a
                ));
            }
            let (v, ty) = self.emit_expr(a)?;
            // `*dynamic` param: wrap the arg into a BolideDynamic. Pick the wrapper
            // from the EMITTED type (`ty`) rather than infer_kind, since a `dynamic`-
            // typed value forwarded here is a pointer even though infer_kind may
            // call it Int.
            if self
                .extern_dynamic
                .get(&resolved)
                .map(|flags| flags.get(ti) == Some(&true))
                .unwrap_or(false)
            {
                let k = self.infer_kind(a);
                if matches!(k, ValKind::Dynamic) {
                    let v = self.cast_to(v, ty, "ptr")?;
                    prepared[ti] = Some(format!("ptr {}", v));
                } else {
                    let dyn_ = self.wrap_as_dynamic(v, ty, &k)?;
                    prepared[ti] = Some(format!("ptr {}", dyn_));
                }
                continue;
            }
            // user-function `dynamic` param: wrap the arg so the callee sees a
            // BolideDynamic (e.g. `where_eq(..., value: dynamic)` receives `true`
            // as a bool dynamic rather than raw `1`).
            let param_is_dynamic = self
                .func_params
                .get(&resolved)
                .and_then(|params| params.get(ti))
                .map(|p| matches!(p.ty, Type::Dynamic))
                .unwrap_or(false);
            if param_is_dynamic {
                let k = self.infer_kind(a);
                if matches!(k, ValKind::Dynamic) {
                    let v = self.cast_to(v, ty, "ptr")?;
                    prepared[ti] = Some(format!("ptr {}", v));
                } else {
                    let dyn_ = self.wrap_as_dynamic(v, ty, &k)?;
                    prepared[ti] = Some(format!("ptr {}", dyn_));
                }
                continue;
            }
            // `*c_char` param: convert a Bolide str (ptr) to a C string pointer
            if self
                .extern_cstr
                .get(&resolved)
                .map(|flags| flags.get(ti) == Some(&true))
                .unwrap_or(false)
            {
                let v = self.cast_to(v, ty, "ptr")?;
                let d = self.fresh();
                let _ = writeln!(
                    self.body,
                    "  {} = call ptr @bolide_string_as_cstr(ptr {})",
                    d, v
                );
                prepared[ti] = Some(format!("ptr {}", d));
                continue;
            }
            let v = self.cast_to(v, ty, param_tys[ti])?;
            prepared[ti] = Some(format!("{} {}", param_tys[ti], v));
        }
        // 按参数顺序拼装调用参数（跳过未填充的槽）
        let mut arg_s = String::new();
        for slot in prepared.iter().flatten() {
            if !arg_s.is_empty() {
                arg_s.push_str(", ");
            }
            arg_s.push_str(slot);
        }
        // Externs (runtime/native libs) are called by their raw C symbol — never
        // give them the `bolide_user_` namespace prefix (e.g. `extern fn abs(c_int)`).
        let fname = if self.extern_names.contains(&resolved) {
            resolved.clone()
        } else {
            llvm_func_name(&resolved)
        };
        if ret == "void" {
            let _ = writeln!(self.body, "  call void @{}({})", fname, arg_s);
            Ok(("0".into(), "i64"))
        } else {
            let d = self.fresh();
            let _ = writeln!(
                self.body,
                "  {} = call {} @{}({})",
                d, ret, fname, arg_s
            );
            // Prefer high-level return kind when known
            if let Some(k) = self.func_ret_kind.get(&resolved) {
                let _ = k;
            }
            Ok((d, ret))
        }
    }

    fn emit_print(&mut self, args: &[Expr]) -> Result<(String, &'static str), String> {
        for a in args {
            let (v, ty) = self.emit_expr(a)?;
            match self.infer_kind(a) {
                ValKind::List(_) => {
                    let _ = writeln!(self.body, "  call void @bolide_print_list(ptr {})", v);
                }
                ValKind::Str => {
                    let _ = writeln!(self.body, "  call void @bolide_print_string(ptr {})", v);
                }
                ValKind::Object(_)
                | ValKind::Adt(_)
                | ValKind::Dict
                | ValKind::Ptr
                | ValKind::Closure
                | ValKind::ListObj(_) => {
                    // print pointer as int for debugging
                    let iv = self.cast_to(v, ty, "i64")?;
                    let _ = writeln!(self.body, "  call void @bolide_print_int(i64 {})", iv);
                }
                ValKind::Float => {
                    let v = self.cast_to(v, ty, "double")?;
                    let _ = writeln!(
                        self.body,
                        "  call void @bolide_print_float(double {})",
                        v
                    );
                }
                ValKind::Bool => {
                    let v = self.cast_to(v, ty, "i64")?;
                    let _ = writeln!(self.body, "  call void @bolide_print_bool(i64 {})", v);
                }
                ValKind::Dynamic => {
                    let v = self.cast_to(v, ty, "ptr")?;
                    let _ = writeln!(self.body, "  call void @bolide_print_dynamic(ptr {})", v);
                }
                ValKind::BigInt => {
                    let v = self.cast_to(v, ty, "ptr")?;
                    let _ = writeln!(self.body, "  call void @bolide_print_bigint(ptr {})", v);
                }
                _ => {
                    if ty == "double" {
                        let _ = writeln!(
                            self.body,
                            "  call void @bolide_print_float(double {})",
                            v
                        );
                    } else if ty == "ptr" {
                        let _ = writeln!(self.body, "  call void @bolide_print_string(ptr {})", v);
                    } else {
                        let v = self.cast_to(v, ty, "i64")?;
                        let _ = writeln!(self.body, "  call void @bolide_print_int(i64 {})", v);
                    }
                }
            }
        }
        // each bolide_print_* runtime fn already appends its own newline
        Ok(("0".into(), "i64"))
    }

    fn to_i1(&mut self, v: String, ty: &str) -> Result<String, String> {
        let v = self.cast_to(v, ty, "i64")?;
        let c = self.fresh();
        let _ = writeln!(self.body, "  {} = icmp ne i64 {}, 0", c, v);
        Ok(c)
    }

    fn cast_to(
        &mut self,
        v: String,
        from: &str,
        to: &'static str,
    ) -> Result<String, String> {
        if from == to {
            return Ok(v);
        }
        let d = self.fresh();
        match (from, to) {
            ("i64", "double") => {
                let _ = writeln!(self.body, "  {} = sitofp i64 {} to double", d, v);
            }
            ("double", "i64") => {
                let _ = writeln!(self.body, "  {} = fptosi double {} to i64", d, v);
            }
            ("i1", "i64") => {
                let _ = writeln!(self.body, "  {} = zext i1 {} to i64", d, v);
            }
            ("ptr", "i64") => {
                let _ = writeln!(self.body, "  {} = ptrtoint ptr {} to i64", d, v);
            }
            ("i64", "ptr") => {
                let _ = writeln!(self.body, "  {} = inttoptr i64 {} to ptr", d, v);
            }
            _ => return Err(format!("LLVM backend: cannot cast {} → {}", from, to)),
        }
        Ok(d)
    }

    /// Picks the best-matching overload for a free-function call by comparing
    /// each candidate's registered param LLVM types against the inferred arg kinds.
    fn resolve_overload(&self, candidates: &[String], args: &[Expr]) -> Option<String> {
        if candidates.len() == 1 {
            return Some(candidates[0].clone());
        }
        let arg_kinds: Vec<ValKind> = args.iter().map(|a| self.infer_kind(a)).collect();
        let mut best: Option<(usize, &String)> = None;
        for cand in candidates {
            // 用实际 Bolide 参数类型评分（LLVM 类型都是 ptr/i64，str 与 bytes 无法区分）
            let Some(params) = self.func_params.get(cand).cloned() else {
                // 没有元数据：退化为按 LLVM 参数类型个数匹配
                if let Some((ptys, _)) = self.funcs.get(cand) {
                    if ptys.len() == arg_kinds.len() {
                        best.get_or_insert((0, cand));
                    }
                }
                continue;
            };
            if params.len() != arg_kinds.len() {
                continue;
            }
            let score = params
                .iter()
                .zip(arg_kinds.iter())
                .filter(|(p, k)| Self::param_matches_kind(&p.ty, k))
                .count();
            if best.map(|(s, _)| score > s).unwrap_or(true) {
                best = Some((score, cand));
            }
        }
        best.map(|(_, name)| name.clone())
    }

    /// 判断实参 ValKind 是否与形参 Bolide Type 匹配（重载解析用）。
    fn param_matches_kind(ty: &Type, k: &ValKind) -> bool {
        match (ty, k) {
            (Type::Int, ValKind::Int) | (Type::BigInt, ValKind::BigInt) => true,
            (Type::Float, ValKind::Float) | (Type::Decimal, ValKind::Float) => true,
            (Type::Bool, ValKind::Bool) => true,
            (Type::Str, ValKind::Str) => true,
            (Type::Bytes, ValKind::Bytes) => true,
            (Type::Custom(n), ValKind::Object(on)) => n == on,
            (Type::List(_), ValKind::List(_) | ValKind::ListObj(_) | ValKind::NestedList(_)) => true,
            (Type::Dict(_, _), ValKind::Dict) => true,
            (Type::Dynamic, ValKind::Dynamic) => true,
            // 指针型形参接受任何对象/容器/闭包
            (Type::Str, ValKind::Object(_))
            | (Type::Bytes, ValKind::Object(_))
            | (Type::Ptr, _) => true,
            _ => false,
        }
    }

    fn infer_kind(&self, expr: &Expr) -> ValKind {
        match expr {
            Expr::Float(_) | Expr::Decimal(_) => ValKind::Float,
            Expr::String(_) => ValKind::Str,
            Expr::Bool(_) => ValKind::Bool,
            Expr::Int(_) => ValKind::Int,
            Expr::BigInt(_) => ValKind::BigInt,
            Expr::List(items) => {
                if items.is_empty() {
                    ValKind::List(0)
                } else {
                    match self.infer_kind(&items[0]) {
                        ValKind::Float => ValKind::List(1),
                        ValKind::Bool => ValKind::List(2),
                        ValKind::Str => ValKind::List(3),
                        ValKind::Object(n) => ValKind::ListObj(n),
                        ValKind::Adt(n) => ValKind::ListObj(n),
                        ValKind::Closure | ValKind::Dict | ValKind::Ptr => ValKind::List(4),
                        _ => ValKind::List(0),
                    }
                }
            }
            Expr::Closure { .. } => ValKind::Closure,
            Expr::Dict(_) => ValKind::Dict,
            Expr::Tuple(items) => self.infer_kind(&Expr::List(items.clone())),
            Expr::ListComprehension { expr, .. } => match self.infer_kind(expr) {
                ValKind::Float => ValKind::List(1),
                ValKind::Bool => ValKind::List(2),
                ValKind::Str => ValKind::List(3),
                ValKind::Object(n) => ValKind::ListObj(n),
                ValKind::Adt(n) => ValKind::ListObj(n),
                _ => ValKind::List(0),
            },
            Expr::Ident(n) => self
                .local_kind
                .get(n)
                .cloned()
                .or_else(|| self.global_vars.get(n).map(|(_, k)| k.clone()))
                .unwrap_or(ValKind::Int),
            Expr::Index(base, _) => match self.infer_kind(base) {
                ValKind::List(t) => list_tag_to_kind(t),
                ValKind::NestedList(inner) => *inner,
                ValKind::ListObj(cn) => ValKind::Object(cn),
                ValKind::Dict => ValKind::Dynamic,
                ValKind::Str => ValKind::Str,
                _ => ValKind::Int,
            },
            Expr::Member(base, member) => {
                if let Expr::Ident(en) = base.as_ref() {
                    if self.adts.contains_key(en) {
                        return ValKind::Adt(en.clone());
                    }
                }
                match self.infer_kind(base) {
                    ValKind::Object(cn) => {
                        if let Some(ci) = self.classes.get(&cn) {
                            if let Some(fty) = field_type(ci, member) {
                                return kind_of_type(&Some(fty.clone()));
                            }
                        }
                        ValKind::Int
                    }
                    _ => ValKind::Int,
                }
            }
            Expr::BinOp(l, BinOp::Add, r)
                if matches!(self.infer_kind(l), ValKind::Str)
                    && matches!(self.infer_kind(r), ValKind::Str) =>
            {
                ValKind::Str
            }
            Expr::BinOp(l, op, r) => {
                // overloaded ops may return non-int
                if let ValKind::Object(ref cn) = self.infer_kind(l) {
                    if let Some(m) = binop_method(&op) {
                        if let Some(md) = self.classes.get(cn).and_then(|c| c.methods.get(m)) {
                            return kind_of_type(&md.return_type);
                        }
                    }
                }
                // comparisons produce bool (printed as true/false)
                if matches!(
                    op,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                ) {
                    return ValKind::Bool;
                }
                if matches!(self.infer_kind(l), ValKind::Float)
                    || matches!(self.infer_kind(r), ValKind::Float)
                {
                    ValKind::Float
                } else {
                    ValKind::Int
                }
            }
            Expr::Call(c, args) => {
                if let Expr::Ident(n) = c.as_ref() {
                    if n == "str" {
                        return ValKind::Str;
                    }
                    if n == "float" {
                        return ValKind::Float;
                    }
                    if n == "int" {
                        return ValKind::Int;
                    }
                    if n == "channel" {
                        return ValKind::Ptr;
                    }
                    if n == "bytes" {
                        return ValKind::Bytes;
                    }
                    if n == "bolide_env_args" {
                        return ValKind::List(3);
                    }
                    // calling a closure-valued variable returns a heap object
                    if self
                        .global_vars
                        .get(n)
                        .map(|(_, k)| matches!(k, ValKind::Closure | ValKind::Ptr))
                        .unwrap_or(false)
                        || self
                            .local_kind
                            .get(n)
                            .map(|k| matches!(k, ValKind::Closure | ValKind::Ptr))
                            .unwrap_or(false)
                    {
                        return ValKind::Ptr;
                    }
                    if let Some(k) = self.func_ret_kind.get(n) {
                        return k.clone();
                    }
                    if self.classes.contains_key(n) {
                        return ValKind::Object(n.clone());
                    }
                }
                if let Expr::Member(base, method) = c.as_ref() {
                    if let Expr::Ident(en) = base.as_ref() {
                        if self.adts.contains_key(en) {
                            return ValKind::Adt(en.clone());
                        }
                    }
                    if method == "format"
                        || method == "replace"
                        || method == "upper"
                        || method == "lower"
                        || method == "trim"
                        || method == "repeat"
                        || method == "slice"
                        || method == "char_at"
                        || method == "substring"
                        || method == "to_upper"
                        || method == "to_lower"
                    {
                        return ValKind::Str;
                    }
                    if method == "split" {
                        return ValKind::List(3);
                    }
                    if method == "contains"
                        || method == "includes"
                        || method == "starts_with"
                        || method == "ends_with"
                        || method == "is_empty"
                        || method == "empty"
                    {
                        return ValKind::Bool;
                    }
                    if method == "find"
                        || method == "count"
                        || method == "len"
                        || method == "index_of"
                        || method == "index"
                    {
                        return ValKind::Int;
                    }
                    if method == "args" {
                        if let Expr::Ident(m) = base.as_ref() {
                            if m == "env" || self.modules.contains_key(m) {
                                return ValKind::List(3);
                            }
                        }
                    }
                    if method == "sin"
                        || method == "cos"
                        || method == "sqrt"
                        || method == "pow"
                        || method == "floor"
                        || method == "ceil"
                    {
                        return ValKind::Float;
                    }
                    if method == "monotonic_ms" || method == "now_ms" || method == "now" {
                        return ValKind::Int;
                    }
                    if let ValKind::Object(ref cn) = self.infer_kind(base) {
                        if let Some(resolved) = self.resolve_class_name(cn) {
                            let full = method_full_name(&resolved, method);
                            if let Some(k) = self.func_ret_kind.get(&full) {
                                return k.clone();
                            }
                            if let Some(md) =
                                self.classes.get(&resolved).and_then(|c| c.methods.get(method))
                            {
                                return kind_of_type(&md.return_type);
                            }
                        }
                    }
                    let _ = args;
                }
                if let Expr::Ident(n) = c.as_ref() {
                    if let Some((_, ret)) = self.funcs.get(n) {
                        return match *ret {
                            "double" => ValKind::Float,
                            "ptr" => {
                                if n.contains("args") {
                                    ValKind::List(3)
                                } else if n.contains("string") {
                                    ValKind::Str
                                } else {
                                    ValKind::Ptr
                                }
                            }
                            _ => ValKind::Int,
                        };
                    }
                }
                ValKind::Int
            }
            _ => ValKind::Int,
        }
    }

    fn fresh(&mut self) -> String {
        let n = self.tmp;
        self.tmp += 1;
        format!("%t{}", n)
    }

    fn fresh_local(&mut self, name: &str) -> String {
        let n = self.tmp;
        self.tmp += 1;
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("%{}.{}", safe, n)
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        let n = self.label;
        self.label += 1;
        format!("{}_{}", prefix, n)
    }
}

fn llvm_type_of(ty: &Option<Type>) -> &'static str {
    match ty {
        Some(t) => type_llvm(t),
        None => "i64",
    }
}

fn kind_of_type(ty: &Option<Type>) -> ValKind {
    match ty {
        Some(Type::Float) | Some(Type::Decimal) => ValKind::Float,
        Some(Type::Bool) => ValKind::Bool,
        Some(Type::Str) => ValKind::Str,
        Some(Type::List(inner)) => match inner.as_ref() {
            Type::Float => ValKind::List(1),
            Type::Bool => ValKind::List(2),
            Type::Str => ValKind::List(3),
            Type::Custom(n) | Type::Dyn(n) => ValKind::ListObj(n.clone()),
            Type::Adt(n, _) => ValKind::ListObj(n.clone()),
            // list<list<T>> — carry the inner list kind so grid[y][x] resolves
            Type::List(_) => ValKind::NestedList(Box::new(kind_of_list_inner(inner))),
            Type::Dict(_, _) => ValKind::List(8),
            Type::Func | Type::FuncSig(_, _) => ValKind::List(4),
            _ => ValKind::List(0),
        },
        Some(Type::Dict(_, _)) => ValKind::Dict,
        Some(Type::Dynamic) => ValKind::Dynamic,
        Some(Type::Tuple(elems)) => {
            // tuples are packed as lists in the LLVM backend
            let tag = match elems.first() {
                Some(Type::Float) | Some(Type::Decimal) => 1,
                Some(Type::Bool) => 2,
                Some(Type::Str) => 3,
                _ => 0,
            };
            ValKind::List(tag)
        }
        Some(Type::BigInt) => ValKind::BigInt,
        Some(Type::Custom(n)) | Some(Type::Dyn(n)) => ValKind::Object(n.clone()),
        Some(Type::Adt(n, _)) => ValKind::Adt(n.clone()),
        Some(Type::Func) | Some(Type::FuncSig(_, _)) => ValKind::Closure,
        Some(Type::Bytes) => ValKind::Bytes,
        _ => ValKind::Int,
    }
}

fn kind_to_llvm(k: &ValKind) -> &'static str {
    match k {
        ValKind::Float => "double",
        ValKind::Str
        | ValKind::List(_)
        | ValKind::ListObj(_)
        | ValKind::NestedList(_)
        | ValKind::Dict
        | ValKind::Object(_)
        | ValKind::Adt(_)
        | ValKind::Closure
        | ValKind::RawFunc
        | ValKind::Dynamic
        | ValKind::BigInt
        | ValKind::Bytes
        | ValKind::Ptr => "ptr",
        ValKind::Int | ValKind::Bool => "i64",
    }
}

fn program_needs_exceptions(program: &Program) -> bool {
    fn walk_s(s: &Statement) -> bool {
        match s {
            Statement::Throw(_) | Statement::Try(_) => true,
            Statement::If(i) => {
                i.then_body.iter().any(walk_s)
                    || i.elif_branches.iter().any(|(_, b)| b.iter().any(walk_s))
                    || i.else_body
                        .as_ref()
                        .map(|b| b.iter().any(walk_s))
                        .unwrap_or(false)
            }
            Statement::While(w) => w.body.iter().any(walk_s),
            Statement::For(f) => f.body.iter().any(walk_s),
            Statement::FuncDef(f) => f.body.iter().any(walk_s),
            Statement::ClassDef(c) => c.methods.iter().any(|m| m.body.iter().any(walk_s)),
            Statement::Match(m) => m.arms.iter().any(|a| a.body.iter().any(walk_s)),
            _ => false,
        }
    }
    program.statements.iter().any(walk_s)
}

fn walk_func_calls(
    f: &FuncDef,
    reachable: &mut std::collections::HashSet<String>,
    program: &Program,
) {
    let mut stack = Vec::new();
    for s in &f.body {
        walk_stmt_collect(s, &mut stack);
    }
    let mut defs: HashMap<String, &FuncDef> = HashMap::new();
    for stmt in &program.statements {
        if let Statement::FuncDef(ff) = stmt {
            defs.insert(ff.name.clone(), ff);
        }
    }
    while let Some(name) = stack.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        if let Some(ff) = defs.get(&name) {
            for s in &ff.body {
                walk_stmt_collect(s, &mut stack);
            }
        }
    }
}

fn walk_stmt_collect(stmt: &Statement, out: &mut Vec<String>) {
    fn collect_calls(expr: &Expr, out: &mut Vec<String>) {
        match expr {
            Expr::Call(c, args) => {
                match c.as_ref() {
                    Expr::Ident(n) => out.push(n.clone()),
                    Expr::Member(base, m) => {
                        if let Expr::Ident(mod_name) = base.as_ref() {
                            out.push(format!("{}_{}", mod_name, m));
                            out.push(m.clone());
                        }
                        collect_calls(base, out);
                    }
                    other => collect_calls(other, out),
                }
                for a in args {
                    collect_calls(a, out);
                }
            }
            Expr::BinOp(l, _, r) => {
                collect_calls(l, out);
                collect_calls(r, out);
            }
            Expr::UnaryOp(_, e) | Expr::Member(e, _) => collect_calls(e, out),
            Expr::Index(b, i) => {
                collect_calls(b, out);
                collect_calls(i, out);
            }
            Expr::List(items) | Expr::Tuple(items) => {
                for i in items {
                    collect_calls(i, out);
                }
            }
            _ => {}
        }
    }
    match stmt {
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_calls(e, out)
        }
        Statement::VarDecl(d) => {
            if let Some(v) = &d.value {
                collect_calls(v, out);
            }
        }
        Statement::Assign(a) => {
            collect_calls(&a.target, out);
            collect_calls(&a.value, out);
        }
        Statement::If(i) => {
            collect_calls(&i.condition, out);
            for s in &i.then_body {
                walk_stmt_collect(s, out);
            }
            for (c, b) in &i.elif_branches {
                collect_calls(c, out);
                for s in b {
                    walk_stmt_collect(s, out);
                }
            }
            if let Some(b) = &i.else_body {
                for s in b {
                    walk_stmt_collect(s, out);
                }
            }
        }
        Statement::While(w) => {
            collect_calls(&w.condition, out);
            for s in &w.body {
                walk_stmt_collect(s, out);
            }
        }
        Statement::For(f) => {
            collect_calls(&f.iter, out);
            for s in &f.body {
                walk_stmt_collect(s, out);
            }
        }
        Statement::Match(m) => {
            collect_calls(&m.expr, out);
            for a in &m.arms {
                for s in &a.body {
                    walk_stmt_collect(s, out);
                }
            }
        }
        Statement::Try(t) => {
            for s in &t.try_body {
                walk_stmt_collect(s, out);
            }
            for c in &t.catch_clauses {
                for s in &c.body {
                    walk_stmt_collect(s, out);
                }
            }
            if let Some(f) = &t.finally {
                for s in f {
                    walk_stmt_collect(s, out);
                }
            }
        }
        _ => {}
    }
}

/// Kind of the element type of a list (used to build `list<list<T>>` kinds).
fn kind_of_list_inner(inner: &Type) -> ValKind {
    kind_of_type(&Some(inner.clone()))
}

fn list_tag_to_kind(tag: u8) -> ValKind {
    match tag {
        1 => ValKind::Float,
        2 => ValKind::Bool,
        3 => ValKind::Str,
        4 => ValKind::Ptr,
        8 => ValKind::Dict,
        _ => ValKind::Int,
    }
}

/// Functions called from top-level statements (transitively).
fn reachable_functions(program: &Program) -> std::collections::HashSet<String> {
    let mut defs: HashMap<String, &FuncDef> = HashMap::new();
    for stmt in &program.statements {
        if let Statement::FuncDef(f) = stmt {
            defs.insert(f.name.clone(), f);
        }
    }
    let mut stack = Vec::new();
    let mut seen = std::collections::HashSet::new();

    fn collect_calls(expr: &Expr, out: &mut Vec<String>, modules_hint: bool) {
        match expr {
            Expr::Call(c, args) => {
                match c.as_ref() {
                    Expr::Ident(n) => out.push(n.clone()),
                    Expr::Member(base, m) => {
                        if let Expr::Ident(mod_name) = base.as_ref() {
                            // env.args → @env_args style may already be mangled only on def side;
                            // calls stay as Member until emit — also record bare and @mod_m
                            out.push(format!("@{}_{}", mod_name, m));
                            out.push(m.clone());
                        }
                        collect_calls(base, out, modules_hint);
                    }
                    other => collect_calls(other, out, modules_hint),
                }
                for a in args {
                    collect_calls(a, out, modules_hint);
                }
            }
            Expr::BinOp(l, _, r) => {
                collect_calls(l, out, modules_hint);
                collect_calls(r, out, modules_hint);
            }
            Expr::UnaryOp(_, e) | Expr::Member(e, _) => {
                collect_calls(e, out, modules_hint);
            }
            Expr::Index(b, i) => {
                collect_calls(b, out, modules_hint);
                collect_calls(i, out, modules_hint);
            }
            Expr::List(items) | Expr::Tuple(items) => {
                for i in items {
                    collect_calls(i, out, modules_hint);
                }
            }
            _ => {}
        }
    }

    fn walk_stmt(stmt: &Statement, out: &mut Vec<String>) {
        match stmt {
            Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
                collect_calls(e, out, false)
            }
            Statement::VarDecl(d) => {
                if let Some(v) = &d.value {
                    collect_calls(v, out, false);
                }
            }
            Statement::Assign(a) => {
                collect_calls(&a.target, out, false);
                collect_calls(&a.value, out, false);
            }
            Statement::If(i) => {
                collect_calls(&i.condition, out, false);
                for s in &i.then_body {
                    walk_stmt(s, out);
                }
                for (c, b) in &i.elif_branches {
                    collect_calls(c, out, false);
                    for s in b {
                        walk_stmt(s, out);
                    }
                }
                if let Some(b) = &i.else_body {
                    for s in b {
                        walk_stmt(s, out);
                    }
                }
            }
            Statement::While(w) => {
                collect_calls(&w.condition, out, false);
                for s in &w.body {
                    walk_stmt(s, out);
                }
            }
            Statement::For(f) => {
                collect_calls(&f.iter, out, false);
                for s in &f.body {
                    walk_stmt(s, out);
                }
            }
            _ => {}
        }
    }

    for stmt in &program.statements {
        if !matches!(stmt, Statement::FuncDef(_)) {
            walk_stmt(stmt, &mut stack);
        }
    }

    while let Some(name) = stack.pop() {
        // resolve name to a def key
        let key = if defs.contains_key(&name) {
            name.clone()
        } else if let Some(k) = defs.keys().find(|k| {
            k == &&name || k.ends_with(&format!("_{}", name)) || llvm_func_name(k) == llvm_func_name(&name)
        }) {
            k.clone()
        } else {
            continue;
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        if let Some(f) = defs.get(&key) {
            for s in &f.body {
                walk_stmt(s, &mut stack);
            }
        }
    }
    seen
}

/// Formats an `f64` as an LLVM IR double literal, which (unlike Rust's `Debug`
/// output) always requires a decimal point even in exponential notation
/// (e.g. `1e308` must be written `1.0e308`).
fn llvm_double_literal(f: f64) -> String {
    let s = format!("{:?}", f);
    if s.contains('.') {
        return s;
    }
    match s.find(['e', 'E']) {
        Some(epos) => format!("{}.0{}", &s[..epos], &s[epos..]),
        None => format!("{}.0", s),
    }
}

fn llvm_func_name(name: &str) -> String {
    let n = name.strip_prefix('@').unwrap_or(name);
    let mangled: String = n
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // "main" is reserved for the generated C entry point wrapper.
    if mangled == "main" {
        "bolide_user_main".to_string()
    } else if mangled.starts_with("bolide_") || mangled.starts_with("object_") {
        // runtime / extern symbols stay raw
        mangled
    } else {
        // namespace user functions so they can't collide with C/CRT/WinSock
        // symbols (e.g. a Bolide fn named `send` vs ws2_32's `send`)
        format!("bolide_user_{}", mangled)
    }
}

fn llvm_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\22"),
            c if (32..127).contains(&c) => out.push(c as char),
            c => out.push_str(&format!("\\{:02X}", c)),
        }
    }
    out
}
