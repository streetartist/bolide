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
    If(IfStmt),
    While(WhileStmt),
    For(ForStmt),
    Pool(PoolStmt),
    Select(SelectStmt),
    AwaitScope(AwaitScopeStmt),
    AsyncSelect(AsyncSelectStmt),
    Send(SendStmt),
    Return(Option<Expr>),
    Expr(Expr),
    Import(Import),
    ExternBlock(ExternBlock),
}

/// 赋值语句
#[derive(Debug, Clone)]
pub struct Assign {
    pub target: Expr,  // 可以是 Ident 或 Member
    pub value: Expr,
}

/// 变量声明
#[derive(Debug, Clone)]
pub struct VarDecl {
    pub name: String,
    pub ty: Option<Type>,
    pub value: Option<Expr>,
}

/// 函数定义
#[derive(Debug, Clone)]
pub struct FuncDef {
    pub name: String,
    pub is_async: bool,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    /// 生命周期依赖: from x, y 表示返回值依赖于参数 x 和 y 的生命周期
    /// 当指定时，跳过 ARC 并执行生命周期检查
    pub lifetime_deps: Option<Vec<String>>,
    pub body: Vec<Statement>,
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
}

/// 类定义
#[derive(Debug, Clone)]
pub struct ClassDef {
    pub name: String,
    pub parent: Option<String>,  // 父类名（继承）
    pub fields: Vec<ClassField>,
    pub methods: Vec<FuncDef>,
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
    pub var: String,
    pub iter: Expr,
    pub body: Vec<Statement>,
}

/// 线程池块: pool(n) { ... }
#[derive(Debug, Clone)]
pub struct PoolStmt {
    pub size: Expr,
    pub body: Vec<Statement>,
}

/// Select 语句: select { x <- ch => { ... } }
#[derive(Debug, Clone)]
pub struct SelectStmt {
    pub branches: Vec<SelectBranch>,
}

/// Select 分支
#[derive(Debug, Clone)]
pub enum SelectBranch {
    /// 接收分支: var <- channel => { body }
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
    Default {
        body: Vec<Statement>,
    },
}

/// await scope 语句: await scope { ... }
#[derive(Debug, Clone)]
pub struct AwaitScopeStmt {
    pub body: Vec<Statement>,
}

/// 协程 select 语句
#[derive(Debug, Clone)]
pub struct AsyncSelectStmt {
    pub branches: Vec<AsyncSelectBranch>,
}

/// 协程 select 分支
#[derive(Debug, Clone)]
pub enum AsyncSelectBranch {
    /// 带绑定: var = expr => { body }
    Bind {
        var: String,
        expr: Expr,
        body: Vec<Statement>,
    },
    /// 不带绑定: expr => { body }
    Expr {
        expr: Expr,
        body: Vec<Statement>,
    },
}

/// 通道发送: ch <- val;
#[derive(Debug, Clone)]
pub struct SendStmt {
    pub channel: String,
    pub value: Expr,
}

/// Import 语句
#[derive(Debug, Clone)]
pub struct Import {
    pub path: Vec<String>,      // 模块路径 (如 math.utils)
    pub file_path: Option<String>,  // 文件路径 (如 "utils.bl")
    pub alias: Option<String>,
}

/// 表达式
#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    BigInt(String),     // 存储原始字符串以支持任意大数
    Decimal(String),    // 存储原始字符串以支持任意精度
    Ident(String),
    BinOp(Box<Expr>, BinOp, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Member(Box<Expr>, String),
    List(Vec<Expr>),
    /// spawn func(args) - 在新线程执行函数
    Spawn(String, Vec<Expr>),
    /// <- ch - 从通道接收
    Recv(String),
    /// await expr - 等待异步结果
    Await(Box<Expr>),
    /// await all { expr, ... } - 并发等待多个
    AwaitAll(Vec<Expr>),
    /// 元组字面量: (expr, expr, ...)
    Tuple(Vec<Expr>),
    None,
}

/// 二元运算符
#[derive(Debug, Clone, Copy)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    Eq, Ne, Lt, Le, Gt, Ge,
    And, Or,
}

/// 一元运算符
#[derive(Debug, Clone, Copy)]
pub enum UnaryOp {
    Neg, Not,
}

/// 类型
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    Str,
    BigInt,
    Decimal,
    Dynamic,
    Ptr,
    Channel(Box<Type>),  // 泛型 channel<T>
    Future,  // spawn 返回的句柄类型
    Func,    // 函数类型（简单版本，无签名）
    FuncSig(Vec<Type>, Option<Box<Type>>),  // 带签名的函数类型: func(params) -> return_type
    List(Box<Type>),
    Tuple(Vec<Type>),  // 元组类型: (T1, T2, ...)
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
    pub variadic: bool,  // 支持可变参数 (...)
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
    I8, U8, I16, U16, I32, U32, I64, U64,
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
