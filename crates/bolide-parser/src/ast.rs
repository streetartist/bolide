//! AST 节点定义

/// 程序（顶层）
#[derive(Debug, Clone)]
pub struct Program {
    pub statements: Vec<Statement>,
}

/// 语句
#[derive(Debug, Clone)]
pub enum Statement {
    VarDecl(VarDecl),
    Assign(Assign),
    FuncDef(FuncDef),
    ClassDef(ClassDef),
    ValueDef(ValueDef),
    EnumDef(EnumDef),
    /// `trait Name { ... }`
    TraitDef(TraitDef),
    /// `impl Trait for Type { ... }`
    TraitImpl(TraitImpl),
    /// `macro name ...` 定义（展开阶段收集后剥离）
    MacroDef(MacroDef),
    /// `attr macro name ...` 定义
    AttrMacroDef(AttrMacroDef),
    /// `comptime fn name(...) { ... }`（展开期求值）
    ComptimeFn(ComptimeFn),
    /// 宏模板内 `$( stmts )*` / `$( stmts )+`
    MacroRep {
        body: Vec<Statement>,
        /// 0 = `*`, 1 = `+`
        min: usize,
    },
    If(IfStmt),
    While(WhileStmt),
    For(ForStmt),
    Pool(PoolStmt),
    Select(SelectStmt),
    AwaitScope(AwaitScopeStmt),
    SpawnSelect(SpawnSelectStmt),
    /// break; - 跳出最近一层循环
    Break,
    /// continue; - 进入最近一层循环的下一次迭代
    Continue,
    Return(Option<Expr>),
    /// throw expr; - 抛出异常
    Throw(Expr),
    /// yield expr; - 生成器产出（展开前）
    Yield(Expr),
    /// try { ... } catch { ... } - 异常捕获
    Try(TryStmt),
    /// with expr [as name], ... { body } — 上下文管理器
    With(WithStmt),
    Match(MatchStmt),
    Expr(Expr),
    Import(Import),
    ExternBlock(ExternBlock),
}

/// 属性 `@name` / `@name(args)`
#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    /// 位置参数：标识符或字符串内容（不含引号）
    pub args: Vec<AttrArg>,
}

#[derive(Debug, Clone)]
pub enum AttrArg {
    Ident(String),
    Str(String),
    Int(i64),
}

/// 宏片段分类符
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragKind {
    Expr,
    Ident,
    Lit,
    Block,
    Type,
    Path,
    Stmt,
    Tt,
    /// 属性宏接收的整项（class/fn/…）
    Item,
}

/// 宏模式中的一片
#[derive(Debug, Clone)]
pub enum PatPiece {
    /// `$name:kind`
    Bind { name: String, kind: FragKind },
    /// `$a:ident = $b:expr`
    EqBind {
        ident_name: String,
        expr_name: String,
        expr_kind: FragKind,
    },
    /// `$(...)*` / `$(...)+`，`sep` 为前导或片段间分隔符
    Rep {
        pieces: Vec<PatPiece>,
        /// 前导分隔（如 `$(, $x:expr)*` 中的 `,`）
        leading_sep: Option<char>,
        /// 重复之间的分隔
        inter_sep: Option<char>,
        min: usize, // 0 for *, 1 for +
    },
}

#[derive(Debug, Clone)]
pub struct MacroPattern {
    pub pieces: Vec<PatPiece>,
}

#[derive(Debug, Clone)]
pub struct MacroArm {
    pub pattern: MacroPattern,
    /// 模板语句（可含 Splice）
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: String,
    pub is_export: bool,
    pub arms: Vec<MacroArm>,
    pub def_span_start: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct AttrMacroDef {
    pub name: String,
    pub is_export: bool,
    pub pattern: MacroPattern,
    pub body: Vec<Statement>,
    pub def_span_start: Option<usize>,
}

/// `comptime fn name(params) -> T { body }`
#[derive(Debug, Clone)]
pub struct ComptimeFn {
    pub name: String,
    pub params: Vec<(String, Type)>,
    pub return_type: Option<Type>,
    pub body: Vec<Statement>,
    pub def_span_start: Option<usize>,
}

/// 宏调用参数
#[derive(Debug, Clone)]
pub enum MacroArg {
    Expr(Expr),
    /// `name = expr`
    Named { name: String, value: Expr },
}

#[derive(Debug, Clone)]
pub enum MacroArgs {
    /// `name!(...)`
    Paren(Vec<MacroArg>),
    /// `name! { stmts }`
    Brace(Vec<Statement>),
}

/// `foo!(...)` / `m.foo!(...)`
#[derive(Debug, Clone)]
pub struct MacroInvoke {
    pub path: Vec<String>,
    pub args: MacroArgs,
    pub span_start: Option<usize>,
}

/// 模板拼接元数据 `$x:src`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpliceMeta {
    Src,
    Stringify,
    Line,
    File,
}

/// try/catch 语句
#[derive(Debug, Clone)]
pub struct TryStmt {
    pub try_body: Vec<Statement>,
    pub catch_clauses: Vec<CatchClause>,
    pub finally: Option<Vec<Statement>>,
}

/// with 语句（上下文管理器）
#[derive(Debug, Clone)]
pub struct WithStmt {
    pub items: Vec<WithItem>,
    pub body: Vec<Statement>,
}

/// `expr` 或 `expr as name`
#[derive(Debug, Clone)]
pub struct WithItem {
    pub expr: Expr,
    pub binding: Option<String>,
}

/// catch 子句
#[derive(Debug, Clone)]
pub struct CatchClause {
    pub var: String,
    pub ty: Type,
    pub body: Vec<Statement>,
}

/// 赋值语句
#[derive(Debug, Clone)]
pub struct Assign {
    pub target: Expr, // 可以是 Ident 或 Member
    pub value: Expr,
}

/// 变量声明
#[derive(Debug, Clone)]
pub struct VarDecl {
    pub name: String,
    pub mutable: bool,
    pub ty: Option<Type>,
    pub value: Option<Expr>,
    /// 宏模板中 `let $name = ...`：name 来自 splice
    pub name_is_splice: bool,
}

/// 函数定义
#[derive(Debug, Clone)]
pub struct FuncDef {
    pub name: String,
    pub is_async: bool,
    /// export fn：以裸名（无 mangling）导出，供 C 链接调用
    pub is_export: bool,
    /// inline fn：调用点内联展开函数体
    pub is_inline: bool,
    /// 泛型参数，如 `fn id<T>(x: T) -> T`
    pub type_params: Vec<String>,
    /// 泛型约束：`T: Drawable + Debug` → `("T", ["Drawable", "Debug"])`
    pub trait_bounds: Vec<(String, Vec<String>)>,
    pub params: Vec<Param>,
    /// Optional exception annotation: `throws IoError, ParseError`.
    /// This is intentionally advisory for now; compiler diagnostics may use it.
    pub throws: Vec<Type>,
    pub return_type: Option<Type>,
    /// 生命周期依赖: from x, y 表示返回值依赖于参数 x 和 y 的生命周期
    /// 当指定时，跳过 ARC 并执行生命周期检查
    pub lifetime_deps: Option<Vec<String>>,
    pub body: Vec<Statement>,
    /// 函数定义在源文件中的起始字节偏移（用于错误信息定位行号）
    pub def_span_start: Option<usize>,
    /// `@test` / `@route(...)` 等属性
    pub attrs: Vec<Attribute>,
}

/// 参数传递模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamMode {
    /// 默认借用：传递裸指针，不操作 RC
    Borrow,
    /// 接收所有权：传递裸指针，调用者置空本地变量
    Owned,
    /// 引用修改：传递指针的地址 (Object**)
    Ref,
}

/// 参数
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub mode: ParamMode,
    pub default_value: Option<Expr>,
    /// True for `*name: T`; the parameter is compiled as `list<T>`.
    pub is_variadic: bool,
    /// True for `**name: T`; the parameter is compiled as `dict<str, T>`.
    pub is_kw_variadic: bool,
}

/// 类定义
#[derive(Debug, Clone)]
pub struct ClassDef {
    pub name: String,
    /// 主父类（字段布局 + `super` 链）；多继承时仅第一个可有字段
    pub parent: Option<String>,
    /// 额外父类 / mixin：必须无字段，方法在展开期并入本类（避免钻石布局）
    pub mixins: Vec<String>,
    pub fields: Vec<ClassField>,
    pub methods: Vec<FuncDef>,
    pub attrs: Vec<Attribute>,
    /// 本类已 `impl` 的 trait 名（由 trait 脱糖写入）
    pub impl_traits: Vec<String>,
}

/// trait 定义
#[derive(Debug, Clone)]
pub struct TraitDef {
    pub name: String,
    /// 父 trait：`trait Child: Parent + Other`
    pub supers: Vec<String>,
    pub methods: Vec<TraitMethod>,
    pub attrs: Vec<Attribute>,
}

/// trait 方法：可无默认实现，或仅签名（要求 impl 提供）
#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub func: FuncDef,
    /// false = 必须由 `impl` 实现
    pub has_default: bool,
}

/// `impl Trait for Type { ... }`
#[derive(Debug, Clone)]
pub struct TraitImpl {
    pub trait_name: String,
    pub type_name: String,
    pub methods: Vec<FuncDef>,
}

/// 值类型定义（栈上，零分配）
#[derive(Debug, Clone)]
pub struct ValueDef {
    pub name: String,
    pub fields: Vec<ValueField>,
    pub attrs: Vec<Attribute>,
}

/// 值类型字段
#[derive(Debug, Clone)]
pub struct ValueField {
    pub name: String,
    pub ty: Type,
}

/// enum/union 定义（代数数据类型）
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub variants: Vec<EnumVariant>,
    /// `union` 与 `enum` 共享同一 ADT 表示；该标志保留源码意图。
    pub is_union: bool,
    pub attrs: Vec<Attribute>,
}

/// enum/union variant
#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<EnumVariantField>,
}

/// variant 字段。`name == None` 表示元组式字段，如 `Some(T)`。
#[derive(Debug, Clone)]
pub struct EnumVariantField {
    pub name: Option<String>,
    pub ty: Type,
}

/// 类字段
#[derive(Debug, Clone)]
pub struct ClassField {
    pub name: String,
    pub ty: Type,
    pub default_value: Option<Expr>,
}

/// If 语句
#[derive(Debug, Clone)]
pub struct IfStmt {
    pub condition: Expr,
    pub then_body: Vec<Statement>,
    pub elif_branches: Vec<(Expr, Vec<Statement>)>,
    pub else_body: Option<Vec<Statement>>,
}

/// While 语句
#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub condition: Expr,
    pub body: Vec<Statement>,
}

/// For 语句
#[derive(Debug, Clone)]
pub struct ForStmt {
    pub vars: Vec<String>,
    pub iter: Expr,
    pub body: Vec<Statement>,
}

/// 线程池块: pool(n) { ... }
#[derive(Debug, Clone)]
pub struct PoolStmt {
    pub size: Expr,
    pub body: Vec<Statement>,
}

/// Select 语句: select { v = ch.recv() => { ... } }
#[derive(Debug, Clone)]
pub struct SelectStmt {
    pub branches: Vec<SelectBranch>,
}

/// Select 分支
#[derive(Debug, Clone)]
pub enum SelectBranch {
    /// 接收分支: var = channel.recv() => { body }
    Recv {
        var: String,
        channel: String,
        body: Vec<Statement>,
    },
    /// 超时分支: timeout(ms) => { body }
    Timeout {
        duration: Expr,
        body: Vec<Statement>,
    },
    /// 默认分支: default => { body }
    Default { body: Vec<Statement> },
}

/// await scope 语句: await scope { ... }
#[derive(Debug, Clone)]
pub struct AwaitScopeStmt {
    pub body: Vec<Statement>,
}

/// 并行 select 语句
#[derive(Debug, Clone)]
pub struct SpawnSelectStmt {
    pub branches: Vec<SpawnSelectBranch>,
}

/// 并行 select 分支
#[derive(Debug, Clone)]
pub enum SpawnSelectBranch {
    /// 带绑定: var = expr => { body }
    Bind {
        var: String,
        expr: Expr,
        body: Vec<Statement>,
    },
    /// 不带绑定: expr => { body }
    Expr { expr: Expr, body: Vec<Statement> },
}

/// match 语句
#[derive(Debug, Clone)]
pub struct MatchStmt {
    pub expr: Expr,
    pub arms: Vec<MatchArm>,
}

/// match 分支
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Vec<Statement>,
}

/// 模式匹配 pattern
#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    Bind(String),
    Int(i64),
    Bool(bool),
    String(String),
    None,
    /// 元组模式: `(a, b, _)`（主要用于将来 match；解构声明另有脱糖路径）
    Tuple(Vec<Pattern>),
    Variant {
        enum_name: Option<String>,
        variant: String,
        fields: Vec<Pattern>,
    },
}

/// Import 语句
#[derive(Debug, Clone)]
pub struct Import {
    pub path: Vec<String>,         // 模块路径 (如 math.utils)
    pub file_path: Option<String>, // 文件路径 (如 "utils.bl")
    pub alias: Option<String>,
}

/// 表达式
#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    BigInt(String),  // 存储原始字符串以支持任意大数
    Decimal(String), // 存储原始字符串以支持任意精度
    Ident(String),
    BinOp(Box<Expr>, BinOp, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    /// Only valid inside call argument lists: `name: expr`.
    NamedArg(String, Box<Expr>),
    /// Only valid inside call argument lists: `*expr`.
    SpreadArg(Box<Expr>),
    /// Only valid inside call argument lists: `**expr`.
    KwSpreadArg(Box<Expr>),
    Index(Box<Expr>, Box<Expr>),
    /// 切片: base[start:end:step]，三段均可缺省（None 表示采用默认边界）
    Slice(
        Box<Expr>,
        Option<Box<Expr>>,
        Option<Box<Expr>>,
        Option<Box<Expr>>,
    ),
    Member(Box<Expr>, String),
    List(Vec<Expr>),
    /// 字典字面量: {key: value, ...}
    Dict(Vec<(Expr, Expr)>),
    /// spawn func(args) - 在线程池或新线程执行函数
    Spawn(String, Vec<Expr>),
    /// spawn thread func(args) - 强制在独立系统线程执行函数
    SpawnThread(String, Vec<Expr>),
    /// await expr - 等待异步结果
    Await(Box<Expr>),
    /// spawn all { expr, ... } - 并行启动多个任务并等待全部
    SpawnAll(Vec<Expr>),
    /// expr? - propagate Result.Err / Option.None with early return
    Propagate(Box<Expr>),
    /// expr! - unwrap Result.Ok or throw Result.Err as Error
    Raise(Box<Expr>),
    /// value 类型构造: TypeName { field: expr, ... }
    ValueConstruct(String, Vec<(String, Expr)>),
    /// try { ... } expression - convert thrown Error into Result.Err
    TryExpr(Vec<Statement>),
    /// 元组字面量: (expr, expr, ...)
    Tuple(Vec<Expr>),
    /// 闭包表达式: fn(params) -> ret { body }
    Closure {
        params: Vec<Param>,
        return_type: Option<Type>,
        body: Vec<Statement>,
    },
    /// 列表推导式: [expr for var in iter if cond]
    ListComprehension {
        expr: Box<Expr>,
        vars: Vec<String>,
        iter: Box<Expr>,
        filter: Option<Box<Expr>>,
    },
    /// 宏调用 `name!(...)` / `m.name!(...)`（展开前）
    MacroInvoke(MacroInvoke),
    /// 宏模板拼接 `$name` / `$name:src`
    Splice {
        name: String,
        meta: Option<SpliceMeta>,
    },
    /// `comptime { ... }` 编译期求值块（展开期折叠为常量）
    Comptime(Vec<Statement>),
    None,
}

/// 二元运算符
#[derive(Debug, Clone, Copy)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Shl,    // <<
    Shr,    // >>
    BitAnd, // &
    BitOr,  // |
    Xor,    // ^
}

/// 一元运算符
#[derive(Debug, Clone, Copy)]
pub enum UnaryOp {
    Neg,
    Not,
}

/// 类型
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    Str,
    Bytes,
    BigInt,
    Decimal,
    Dynamic,
    Ptr,
    Channel(Box<Type>),                    // 泛型 channel<T>
    Future,                                // spawn 返回的句柄类型
    Func,                                  // 函数类型（简单版本，无签名）
    FuncSig(Vec<Type>, Option<Box<Type>>), // 带签名的函数类型: func(params) -> return_type
    List(Box<Type>),
    Dict(Box<Type>, Box<Type>), // dict<K, V>
    Tuple(Vec<Type>),           // 元组类型: (T1, T2, ...)
    /// 泛型参数类型，如 `T`
    Generic(String),
    /// 已应用的代数数据类型，如 `Option<int>` / `Result<int, str>`
    Adt(String, Vec<Type>),
    Custom(String),
    /// trait 对象：`dyn Drawable`（运行时多态，底层为对象指针 + class tag 分派）
    Dyn(String),
    Weak(Box<Type>),    // 弱引用: weak T
    Unowned(Box<Type>), // 无主引用: unowned T
}

/// 合成 dyn 包装类名：`dyn Drawable` → class `__Dyn_Drawable`
pub fn dyn_trait_class_name(trait_name: &str) -> String {
    format!("__Dyn_{}", trait_name)
}

/// 若类型/类名为 `__Dyn_Trait`，返回 trait 名
pub fn dyn_trait_from_class_name(class_name: &str) -> Option<&str> {
    class_name.strip_prefix("__Dyn_")
}

/// FFI extern 块
#[derive(Debug, Clone)]
pub struct ExternBlock {
    pub lib_path: String,
    pub declarations: Vec<ExternDecl>,
}

/// extern 声明项
#[derive(Debug, Clone)]
pub enum ExternDecl {
    Function(ExternFunc),
    Struct(ExternStruct),
    TypeAlias(String, CType),
}

/// extern 函数声明
#[derive(Debug, Clone)]
pub struct ExternFunc {
    pub name: String,
    pub params: Vec<CParam>,
    pub return_type: Option<CType>,
    pub variadic: bool, // 支持可变参数 (...)
}

/// C 函数参数
#[derive(Debug, Clone)]
pub struct CParam {
    pub name: String,
    pub ty: CType,
}

/// extern 结构体
#[derive(Debug, Clone)]
pub struct ExternStruct {
    pub name: String,
    pub fields: Vec<CField>,
}

/// C 结构体字段
#[derive(Debug, Clone)]
pub struct CField {
    pub name: String,
    pub ty: CType,
}

/// C 类型系统
#[derive(Debug, Clone, PartialEq)]
pub enum CType {
    // 基本类型
    Void,
    Char,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    LongLong,
    ULongLong,
    Float,
    Double,
    Bool,
    // 固定宽度整数
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    // 特殊类型
    SizeT,
    PtrDiffT,
    // 指针类型
    Ptr(Box<CType>),
    // 数组类型
    Array(Box<CType>, usize),
    // 函数指针 (回调)
    FuncPtr {
        params: Vec<CType>,
        return_type: Box<CType>,
    },
    // 自定义结构体
    Struct(String),
}
