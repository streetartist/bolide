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
    /// try { ... } catch { ... } - 异常捕获
    Try(TryStmt),
    Match(MatchStmt),
    Expr(Expr),
    Import(Import),
    ExternBlock(ExternBlock),
}

/// try/catch 语句
#[derive(Debug, Clone)]
pub struct TryStmt {
    pub try_body: Vec<Statement>,
    pub catch_clauses: Vec<CatchClause>,
    pub finally: Option<Vec<Statement>>,
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
    pub parent: Option<String>, // 父类名（继承）
    pub fields: Vec<ClassField>,
    pub methods: Vec<FuncDef>,
}

/// 值类型定义（栈上，零分配）
#[derive(Debug, Clone)]
pub struct ValueDef {
    pub name: String,
    pub fields: Vec<ValueField>,
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
    Weak(Box<Type>),    // 弱引用: weak T
    Unowned(Box<Type>), // 无主引用: unowned T
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
