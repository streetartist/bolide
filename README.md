<p align="center">
  <img src="./bolide_logo.png" alt="Bolide Logo" width="200">
  <br>
  <b style="font-size: 32px;">Bolide</b>
  <br>
  <i>现代化 JIT/AOT 编译型编程语言</i>
  <br>
</p>

<p align="center">
  <a href="https://opensource.org/licenses/MIT">
    <img src="https://img.shields.io/badge/License-MIT-brightgreen.svg" alt="License: MIT">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/version-0.14.1-blue.svg" alt="Version">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/platform-windows%20%7C%20linux-lightgrey.svg" alt="Platform">
  </a>
</p>

<p align="center">
  <img src="./calculator.png" alt="Bolide GUI Calculator" width="300">
</p>

---

**Bolide** 是一门现代化编程语言，基于 **Cranelift** 实现 JIT/AOT 编译，兼具简洁语法与原生性能。

## 特性

- **JIT / AOT** - Cranelift 即时编译与原生可执行文件；AOT 可单文件部署
- **一等函数与闭包** - 函数作值、`map`/`filter`、闭包字面量与捕获
- **泛型与 trait** - 单态化泛型、`trait` / `impl`、`T: Trait` 约束、`dyn Trait` 运行时多态
- **宏与装饰器** - 声明式 / 属性宏（调用带 `!`）、Python 风格 `@` 装饰器与 `with`
- **生成器** - `yield` 懒迭代器（`next() -> Option`），`for` 可直接遍历
- **运算符重载** - class 上 `__add__` / `__eq__` / 位运算 / 一元等，支持右操作数反射
- **值类型与内联** - `value` 零堆聚合；`inline fn` 与热路径自动内联小叶子函数
- **函数参数** - 默认值、具名调用、`*args` / `**kwargs`
- **默认不可变绑定** - `let` 不可变，`var` 可变；`list[i]` **始终边界检查**
- **字符串与切片** - 内置方法；字符串 / 列表 / 元组 Python 风格切片
- **异步与并发** - async/await、线程、通道、线程池、原子与同步原语
- **双向 FFI** - 调 C（含回调）；`export fn` 编译为静态库供 C 调用
- **模块与包** - 命名空间 import；`bolide.toml` 依赖与短路径 `std/fs`
- **标准库** - Web / HTTP / GUI / JSON / DB / CLI 等（见 `std/README.md`）
- **源码级报错** - 文件名、行列、源码片段与修复提示
- **内存管理** - ARC + 生命周期注解 + weak/unowned

## 快速开始

完整教程见：[Bolide 从入门到精通](docs/book/bolide-from-zero-to-mastery.md)。

### 从源码构建

```bash
# 克隆仓库
git clone https://github.com/streetartist/bolide.git
cd bolide

# 构建
cargo build --release

# 运行程序
cargo run --release -- run examples/hello.bl

# 特性演示（可选）
cargo run --release -- run examples/neon_lang.bl
cargo run --release -- run examples/starfield.bl
```

### 使用 Release 版本

下载对应平台的 Release 包后：

```bash
# Windows
bolide.exe run your_program.bl

# Linux / macOS
./bolide run your_program.bl
```

### AOT 编译

将 Bolide 程序编译为独立的原生可执行文件：

```bash
# 编译为可执行文件
bolide compile your_program.bl -o your_program

# Windows 会生成 your_program.exe
# Linux/macOS 会生成 your_program

# 直接运行编译后的程序
./your_program
```

AOT 编译的优势：
- **无需运行时** - 生成的可执行文件可独立运行
- **更快启动** - 跳过 JIT 编译阶段
- **便于分发** - 单文件部署，无依赖

### 源码级错误诊断

`bolide run`、`bolide compile` 和 REPL 都会输出带源码位置的诊断信息。语法错误使用解析器保留的精确位置；常见语义错误（未定义变量/函数/通道、未知方法、缺少必填参数、导入失败等）会定位到最相关的源码 token，并附带简短修复提示。

例如：

```bolide
let x = missing_name + 1;
print(x);
```

运行后会得到类似输出：

```text
Error: bolide::compile

  × Compile error: Undefined variable or function: missing_name
   ╭─[example.bl:1:9]
 1 │ let x = missing_name + 1;
   ·         ──────┬─────
   ·               ╰── 'missing_name' is not defined
 2 │ print(x);
   ╰────
  help: Define the name before using it, or check for a spelling/import mistake.
```

这套诊断覆盖 JIT 运行、AOT 编译和静态库编译入口；REPL 中也会显示 `<repl>:行:列`、源码行和 caret 标注。

#### 编译为静态库（C 调用 Bolide）

除可执行文件外，AOT 还可将 Bolide 编译为 C 可链接的静态库：

```bash
# 生成静态库 (.lib / .a) 并同时输出 C 头文件 (.h)
bolide compile mathlib.bl --lib --header

# Windows 生成 mathlib.lib + mathlib.h
# Linux/macOS 生成 libmathlib.a + mathlib.h
```

库模式下不会生成 `main` 入口；仅 `export fn` 标记的函数以裸名导出（无名称修饰），
其余函数保持内部命名空间隔离。生成的 `.h` 只声明 `export fn` 函数。链接时需同时带上
Bolide 运行时静态库（`bolide_runtime.lib` / `libbolide_runtime.a`）。详见下方
[FFI](#ffi-c-语言互操作) 一节。

## 语法示例

### 变量与类型

`let` 声明不可变绑定；需要重新赋值或原地修改容器/对象字段时使用 `var`。

```bolide
let x: int = 42;
let pi: float = 3.14159;
let name: str = "Bolide";
let flag: bool = true;
let big: bigint = 123456789012345678901234567890b;
let precise: decimal = 3.14159265358979d;

// 数字字面量支持下划线分隔
let million: int = 1_000_000;

// 字符串支持转义序列: \" \\ \n \t \r \0
let quoted: str = "he said \"hi\"\nsecond line";

// f-string 插值（`{expr}` 求值后转成字符串拼接；`{{` / `}}` 表示字面量花括号）
// 插值内可含普通字符串与嵌套 f-string
let id: int = 42;
print(f"user={name} id={id}");   // user=Bolide id=42
print(f"sum={1 + 2}");           // sum=3
print(f"brace={{ok}}");          // brace={ok}
print(f"quoted={"hi"}");         // quoted=hi
print(f"nested={f"inner"}");     // nested=inner

// 元组 / 字段解构；`_` 丢弃
let t = (10, 20, 30);
let (a, b, c) = t;
let (x, _, z) = t;
_ = a + b;                       // 求值但丢弃结果

class Point {
    x: int;
    y: int;
}
let p: Point = Point(3, 4);
let Point { x, y } = p;          // 字段同名绑定
let Point { x: px, y: _ } = p;   // 重命名 / 丢弃字段

// if let / while let（enum/union 模式，脱糖为 match）
fn find(flag: int) -> Option<int> {
    if flag > 0 { return Option.Some(flag); }
    return Option.None();
}
if let Option.Some(v) = find(7) {
    print(v);
} else {
    print(0);
}

var i: int = 1;
fn take(n: int) -> Option<int> {
    if n <= 3 { return Option.Some(n); }
    return Option.None();
}
while let Option.Some(v) = take(i) {
    print(v);
    i += 1;
}

var counter: int = 0;
counter += 1;

var items: list<int> = [1, 2];
items.push(3);
```

### 用户输入

使用 `input()` 函数从标准输入读取用户输入（类似 Python）：

```bolide
// 带提示的输入
let name: str = input("请输入你的名字: ");
print(name);

// 无提示的输入
let content: str = input();
```

### 类型转换

Bolide 提供了完整的类型转换函数：

```bolide
// int() - 转整数
let a: int = int(3.7);       // float -> int (截断) = 3
let b: int = int("123");     // str -> int = 123
let c: int = int(999B);      // bigint -> int = 999
let d: int = int(45.6D);     // decimal -> int = 45

// float() - 转浮点数
let e: float = float(100);       // int -> float = 100.0
let f: float = float("2.718");   // str -> float = 2.718
let g: float = float(1.5D);      // decimal -> float = 1.5

// str() - 转字符串
let h: str = str(12345);         // int -> str = "12345"
let i: str = str(3.14159);       // float -> str = "3.14159"
let j: str = str(true);          // bool -> str = "true"
let k: str = str(123456789B);    // bigint -> str = "123456789"
let l: str = str(99.99D);        // decimal -> str = "99.99"

// bigint() 和 decimal()
let m: bigint = bigint(100);     // int -> bigint
let n: decimal = decimal(3.14);  // float -> decimal
```

### 字符串方法

`str` 提供常用库函数，方法调用风格类似 Python：

```bolide
let s: str = "Hello, World";

print(s.len());              // 12
print(s.upper());            // HELLO, WORLD
print(s.lower());            // hello, world
print(s.contains("World"));  // 1
print(s.find("World"));      // 7
print(s.starts_with("Hell"));// 1
print(s.ends_with("rld"));   // 1
print(s.replace("l", "L"));  // HeLLo, WorLd
print(s.count("l"));         // 3

print("  trim me  ".trim()); // trim me
print("ab".repeat(3));       // ababab
print(s.substring(0, 5));    // Hello
print(s.char_at(1));         // e

let parts: list<str> = "a,b,c".split(",");
print(parts);                // ["a", "b", "c"]
```

常用别名包括 `length()`/`size()`、`strip()`、`index_of()`、`includes()`、`substr()`。
字符串索引和切片按 Unicode 码点处理；`len()` 当前返回 UTF-8 字节长度。

### 函数

```bolide
fn add(a: int, b: int) -> int {
    return a + b;
}

fn greet(name: str = "world", punctuation: str = "!") {
    print("hello " + name + punctuation);
}

greet();                         // 使用默认值
greet(name="Bolide");            // 具名参数，等价于 name: "Bolide"
greet(punctuation="?", name="B");// 具名参数可调整顺序

fn total(base: int = 10, *nums: int, **opts: int) -> int {
    var sum: int = base;
    for n in nums {              // nums 的类型是 list<int>
        sum += n;
    }
    if opts.contains("bonus") {  // opts 的类型是 dict<str, int>
        sum += opts["bonus"];
    }
    return sum;
}

let xs: list<int> = [2, 3];
let kwargs: dict<str, int> = {"bonus": 4};

print(total());                  // 10
print(total(1, *xs, **kwargs));  // 10
print(total(base=5, bonus=7));   // 12，未知具名参数进入 **opts
```

函数形参支持：
- `name: T = expr`：默认值；
- `*args: T`：接收多余位置参数，函数体中类型为 `list<T>`；
- `**kwargs: T`：接收多余具名参数，函数体中类型为 `dict<str, T>`；
- 调用侧支持 `name=value`、`name: value`、`*list_expr`、`**dict_expr`。

`*args` 必须位于普通参数之后，`**kwargs` 必须是最后一个参数。没有对应形参或
`**kwargs` 时，未知具名参数会编译报错。

### 值类型

使用 `value` 可以定义轻量聚合类型。它们适合 `Vec2`/`Vec3`/颜色/小型记录这类高频数据，
字段通过点号访问，可直接用于局部变量、函数参数和返回值。

```bolide
value Vec3 { x: float; y: float; z: float; }

let a: Vec3 = Vec3 { x: 1.0, y: 2.0, z: 3.0 };
let b: Vec3 = Vec3 { x: 4.0, y: 5.0, z: 6.0 };

fn dot(lhs: Vec3, rhs: Vec3) -> float {
    return lhs.x * rhs.x + lhs.y * rhs.y + lhs.z * rhs.z;
}

print(dot(a, b));
print(a.x);
```

### 宏（Macros）

宏在**类型检查之前**展开为普通 Bolide 代码。**调用必须带 `!`**（`assert!(x)` 会展宏，`assert(x)` 永远当函数）。

```bolide
// 内置
assert!(x > 0);
assert_eq!(a, b);
let v = dbg!(1 + 2);       // 打印调试信息并返回值
let s = stringify!(1 + 2); // "(1 + 2)"
todo!("later");            // 抛出带位置的 Error

// 自定义：pattern 后直接 quote { ... } 或 { ... }
macro twice($x:expr) quote {
    ($x) + ($x);
}
print(twice!(21));  // 42

macro log_pair($a:expr, $b:expr) {
    print($a);
    print($b);
}
log_pair!(1, 2);

// 多 arm + ident = expr
macro bind {
    ($name:ident = $val:expr) => {
        let $name = $val;
    },
}
bind!(n = 10);

// 属性
@derive(Debug, Eq)
class Point {
    x: int;
    y: int;
}
let p: Point = Point(1, 2);
print(p.debug());
print(p.eq(Point(1, 2)));

@test
fn test_add() {
    assert!(1 + 1 == 2);
}
```

**导出与导入**（`export macro` 合并进调用方后可用短名；也可用 `mod.name!`）：

```bolide
// lib.bl
export macro add1($x:expr) quote { ($x) + 1; }

// main.bl
import "lib.bl" as lib;
print(add1!(41));       // export 短名
print(lib.add1!(9));    // 限定路径
```

**属性宏**（函数体前插入模板）：

```bolide
attr macro traced($item:item) {
    print("enter");
}
@traced
fn work() { print("body"); }
```

**模板体重复 `$(...)*`**：

```bolide
macro sum_all($x:expr $(, $rest:expr)*) quote {
    (fn() -> int {
        var __s = $x;
        $(
            __s = __s + $rest;
        )*
        return __s;
    })();
}
print(sum_all!(1, 2, 3, 4));  // 10

// $n:lit 控制重复次数
macro print_n($n:lit, $msg:expr) {
    $( print($msg); )*
}
print_n!(3, "hi");
```

**comptime / comptime fn**：

```bolide
comptime fn fact(n: int) -> int {
    if n <= 1 { return 1; }
    return n * fact(n - 1);
}
let F: int = comptime { fact(5); };  // 120
```

**类属性**：`@derive(Debug, Eq, Clone, Default)`、`@getters`、自定义 `attr macro` 可按字段 `$(...)*` 生成方法（`self.$field` / `fn $field`）。

### 生成器（yield，懒求值）

含 `yield` 的函数是**生成器**：返回迭代器对象，按需 `next()`，也可用 `for` 遍历（支持无限序列）。

```bolide
fn count_to(n: int) {
    var i: int = 0;
    while i < n {
        yield i;
        i = i + 1;
    }
}

for x in count_to(4) {
    print(x);   // 0 1 2 3
}

// 手动拉取
let g = count_to(2);
match g.next() {
    Option.Some(v) => { print(v); },
    Option.None() => {},
}

// 无限生成器
fn naturals() {
    var n: int = 0;
    while true {
        yield n;
        n = n + 1;
    }
}
let nats = naturals();
// 只取前几个：反复 nats.next()

// bare return 结束生成
fn early(n: int) {
    var i: int = 0;
    while i < n {
        if i == 2 { return; }
        yield i;
        i = i + 1;
    }
}
```

协议：`next() -> Option<T>`（`Some` 产出值，`None` 结束）。  
实现为状态机迭代器类；生成器体内支持 `while` / `if`/`elif`/`else` / `for`（`range` 与列表）/ `break` / `continue`，以及 **类方法生成器**（`self` 捕获为迭代器上的 `__owner`）。

```bolide
// elif / for / break / continue
fn filtered(n: int) {
    for i in range(n) {
        if i % 2 == 1 { continue; }
        if i > 4 { break; }
        yield i;
    }
}

// 类方法
class Counter {
    start: int;
    fn count(n: int) {
        var i: int = 0;
        while i < n {
            yield self.start + i;
            i = i + 1;
        }
    }
}
for x in Counter(10).count(3) { print(x); }  // 10 11 12
```

### 装饰器与上下文管理器（Python 风格）

**运行时装饰器**（`@name` 且不是内置/`attr macro` 时）——与 Python 相同：`deco(f) -> f`：

```bolide
fn logged(f: func() -> int) -> func() -> int {
    return fn() -> int {
        print("before");
        let r: int = f();
        print("after");
        return r;
    };
}

@logged
fn answer() -> int {
    return 42;
}
print(answer());  // before / after / 42

// 工厂装饰器
fn repeat(n: int) -> func(func() -> int) -> func() -> int {
    return fn(f: func() -> int) -> func() -> int {
        return fn() -> int {
            var i: int = 0;
            var last: int = 0;
            while i < n {
                last = f();
                i = i + 1;
            }
            return last;
        };
    };
}
@repeat(3)
fn step() -> int { print("step"); return 1; }
```

多层 `@a @b fn f` 等价于 `f = a(b(f))`（外层后应用）。  
编译期仍用 `@derive` / `@test` / `attr macro`（同名时编译期优先）。

**上下文管理器 `with`**：

```bolide
class Resource {
    name: str;
    fn enter() -> str { print("enter"); return self.name; }
    fn exit() { print("exit"); }
}

with Resource("db") as r {
    print(r);
}
// 多个：with A() as x, B() as y { ... }
```

协议：`enter()`（返回值可 `as` 绑定）、`exit()`（在 `finally` 中调用）。

```bash
bolide expand your_file.bl
```

详见 [docs/decorator-with-design.md](docs/decorator-with-design.md)、[docs/macro-design.md](docs/macro-design.md)。

### 内联函数

使用 `inline fn` 可以把短小函数在调用点展开，适合数值运算和热路径辅助函数。

```bolide
inline fn v3_add(a: Vec3, b: Vec3) -> Vec3 {
    return Vec3 { x: a.x + b.x, y: a.y + b.y, z: a.z + b.z };
}

inline fn sq(x: float) -> float {
    return x * x;
}

let sum = v3_add(
    Vec3 { x: 1.0, y: 2.0, z: 3.0 },
    Vec3 { x: 4.0, y: 5.0, z: 6.0 },
);
print(sum.y);
print(sq(3.0));
```

当前 `inline fn` 最适合“若干 `let` 绑定 + 单个 `return` 表达式”的短函数。

### 泛型函数

函数名后可使用 `<T>` 或 `<T, U>` 声明类型参数。调用时无需显式写类型实参，编译器会根据实参类型推断，并在 JIT/AOT 后端编译前单态化为具体函数实例。

```bolide
fn id<T>(x: T) -> T {
    return x;
}

fn pair<T, U>(a: T, b: U) -> (T, U) {
    return (a, b);
}

fn wrap<T>(x: T) -> list<T> {
    return [x];
}

print(id(42));           // 42
print(id("hello"));      // hello
print(pair(10, "x"));    // (10, "x")
print(wrap(7));          // [7]

let n: int = id(100);
let s: str = id("bolide");
print(n);
print(s);
```

当前支持顶层泛型函数与 **class 泛型方法** 的直接调用，以及 **泛型函数作为一等值**（赋值、传参）；编译器按期望的 `func(...)` 类型单态化。

```bolide
fn id<T>(x: T) -> T { return x; }

// 直接调用
print(id(42));
print(id("hi"));

// 作为值：需要可推断的 concrete 函数类型
let f: func(int) -> int = id;
print(f(1));

fn apply(cb: func(int) -> int, x: int) -> int {
    return cb(x);
}
print(apply(id, 7));

// 泛型方法
class Box {
    value: int;
    fn map<U>(f: func(int) -> U) -> U {
        return f(self.value);
    }
}
fn double(x: int) -> int { return x * 2; }
let b: Box = Box(21);
print(b.map(double));  // 42
```

无类型注解时不能把裸泛型名当值用（无法确定实例），例如 `let f = id;` 会报错并提示补上 `func(...)` 标注。

一等函数值（含 `list<func(...)>`、从函数返回、再调用）统一为**闭包对象 ABI**：裸函数指针在存入/返回时会自动 wrap，调用走 `(env, ...args)` 适配器。

### 一等函数 (First-Class Functions)

函数是一等值：可以赋给变量、作为参数传递、从函数返回、存入列表。无需类型标注，编译器会自动推断函数签名。

约定上，`fn` 只用于函数声明和函数字面量；`func(T...) -> R` 是唯一的函数值类型（含 C 函数指针签名）。不要把 C 函数指针、trampoline 或 `*c_void` 暴露成普通业务类型，它们属于 FFI/编译器内部实现细节。

```bolide
fn add1(x: int) -> int { return x + 1; }
fn double(x: int) -> int { return x * 2; }

// 函数赋给变量（无需类型标注），再调用
let f = add1;
print(f(10));            // 11

// 显式函数类型标注（可选）
let g: func(int) -> int = double;
print(g(10));            // 20

// 函数作为参数：用户定义高阶函数
fn apply(callback: func(int) -> int, x: int) -> int {
    return callback(x);
}
print(apply(double, 21));  // 42

// 返回函数
fn pick(which: int) -> func(int) -> int {
    if which == 0 { return add1; }
    return double;
}
print(pick(0)(7));       // 8
print(pick(1)(7));       // 14

// 函数存入列表，按下标取出调用
let fns: list<func(int) -> int> = [add1, double];
print(fns[0](5));        // 6
print(fns[1](5));        // 10
```

### 闭包 (Closures)

闭包使用 `fn(...) -> T { ... }` 表达式创建，类型仍然是 `func(...) -> T`。闭包可以捕获外层局部变量，也可以作为参数传递或从函数返回。

```bolide
// 闭包字面量赋给变量
let double: func(int) -> int = fn(x: int) -> int {
    return x * 2;
};
print(double(21));       // 42

// 自动捕获外层变量
let n: int = 10;
let add_n = fn(x: int) -> int {
    return x + n;
};
print(add_n(5));         // 15

// 闭包作为参数
fn apply(callback: func(int) -> int, x: int) -> int {
    return callback(x);
}
print(apply(fn(x: int) -> int { return x * 3; }, 7)); // 21

// 返回闭包，形成高阶函数
fn make_adder(n: int) -> func(int) -> int {
    return fn(x: int) -> int {
        return x + n;
    };
}

let add5 = make_adder(5);
print(add5(10));         // 15

// 捕获 ARC 管理的对象也会自动保活
let prefix: str = "val:";
let label = fn(x: int) -> str {
    return prefix + str(x);
};
print(label(7));         // val:7
```

### 高阶列表方法 (map / filter)

`map` 对每个元素应用回调（可改变元素类型），`filter` 保留回调返回真的元素。回调可以是任意命名函数。

```bolide
fn double(x: int) -> int { return x * 2; }
fn is_even(x: int) -> bool { return x % 2 == 0; }
fn label(n: int) -> str { return "n=" + str(n); }

let nums: list<int> = [1, 2, 3, 4];

// map: 元素变换
print(nums.map(double));     // [2, 4, 6, 8]

// filter: 元素过滤
print(nums.filter(is_even)); // [2, 4]

// 跨类型 map (int -> str)
print(nums.map(label));      // ["n=1", "n=2", "n=3", "n=4"]

// float 回调同样支持
fn scale(x: float) -> float { return x * 2.0; }
let fs: list<float> = [1.5, 2.5, 3.5];
print(fs.map(scale));        // [3, 5, 7]
```

> 注意：`map`/`filter` 的类型推断在**函数内**完整可用；顶层（全局作用域）调用建议显式标注结果类型。

### 控制流

```bolide
// if-elif-else
if x > 0 {
    print("positive");
} elif x < 0 {
    print("negative");
} else {
    print("zero");
}

// for 循环 - Python 风格 range
for i in range(5) { print(i); }           // 0, 1, 2, 3, 4
for i in range(3, 7) { print(i); }        // 3, 4, 5, 6
for i in range(0, 10, 2) { print(i); }    // 0, 2, 4, 6, 8
for i in range(10, 0, -2) { print(i); }   // 10, 8, 6, 4, 2 (负步长)

// for 循环 - 列表遍历
let nums: list<int> = [10, 20, 30];
for n in nums {
    print(n);
}

// while 循环
var x: int = 5;
while x > 0 {
    x = x - 1;
}

// 复合赋值
var n: int = 10;
n += 5;   // 15
n -= 3;   // 12
n *= 2;   // 24
n /= 4;   // 6
n %= 4;   // 2

// break / continue
var total: int = 0;
for i in range(10) {
    if i == 3 {
        continue;  // 跳过本次迭代
    }
    if i == 6 {
        break;     // 跳出循环
    }
    total += i;
}

// for 循环 - 字典遍历 (Python 风格)
let scores = {"Alice": 100, "Bob": 85};
for k, v in scores {
    print(k);  // 键
    print(v);  // 值
}
```

### 切片

字符串、列表和元组支持 `seq[start:end:step]` 切片语法。`start`、`end`、
`step` 都可省略，负索引和负步长可用于从末尾索引或反向遍历：

```bolide
let text: str = "Hello, World";
print(text[0:5]);       // Hello
print(text[7:]);        // World
print(text[:5]);        // Hello
print(text[::2]);       // Hlo ol
print(text[::-1]);      // dlroW ,olleH
print(text[-1]);        // d

let nums: list<int> = [10, 20, 30, 40, 50];
print(nums[1:4]);       // [20, 30, 40]
print(nums[-2:]);       // [40, 50]
print(nums[::-1]);      // [50, 40, 30, 20, 10]

let t: (int, int, int, int) = (1, 2, 3, 4);
let mid: (int, int) = t[1:3];
print(mid);             // (2, 3)
```

单下标访问仍使用 `seq[index]`。字符串单下标返回单字符 `str`，字符串切片按
Unicode 码点截取。

### 列表操作

Bolide 提供了丰富的 Python 风格列表操作。

**边界检查**：`list[i]` 读写始终检查下标；越界读返回 `0`/`0.0`，越界写忽略。需要预分配时用 `reserve` / `resize`：

```bolide
var flags: list<int> = [];
flags.resize(1000, 0);   // 长度 1000，填充 0
flags.reserve(2000);     // 只扩容不改长度
```

```bolide
var nums: list<int> = [3, 1, 4, 1, 5, 9];

// 基本操作
nums.push(10);           // 追加元素
let x: int = nums.pop(); // 弹出最后一个元素
print(nums.len());       // 获取长度

// 索引访问（有边界检查）
print(nums[0]);          // 获取元素
nums[0] = 100;           // 设置元素

// 插入和删除
nums.insert(1, 42);      // 在索引 1 处插入
let removed: int = nums.remove(2);  // 移除索引 2 的元素

// 搜索
print(nums.contains(4)); // 是否包含值 (返回 0 或 1)
print(nums.index_of(4)); // 查找索引 (找不到返回 -1)
print(nums.count(1));    // 统计出现次数

// 工具方法
print(nums.first());     // 第一个元素
print(nums.last());      // 最后一个元素
print(nums.is_empty());  // 是否为空

// 修改操作
nums.reverse();          // 原地反转
nums.sort();             // 原地排序

// 切片和扩展
let sliced: list<int> = nums[1:4];   // 切片 [1:4)，也可用 nums.slice(1, 4)
let every2: list<int> = nums[::2];   // 步长切片
let rev: list<int> = nums[::-1];     // 反向切片
let more: list<int> = [100, 200];
nums.extend(more);       // 扩展列表

// 复制和清空
let copy: list<int> = nums.copy();  // 复制列表
nums.clear();            // 清空列表

// 直接打印列表
print(nums);             // 输出: [1, 2, 3, ...]
```

### 字典 (Dictionaries)

Bolide 支持强类型和混合类型的动态字典，语法类似于 Python：

```bolide
// 强类型字典
var scores: dict<str, int> = {"Alice": 100, "Bob": 90};
print(scores["Alice"]);  // 100

// 混合类型字典 (自动推导为 dict<dynamic, dynamic>)
// 支持异构键和值，自动进行装箱处理
let profile = {"name": "Bolide", 1: "Version", "active": true};
print(profile["name"]);  // "Bolide"
print(profile[1]);       // "Version"

// 常用操作
scores["Charlie"] = 95;     // 插入/更新
scores.remove("Bob");       // 删除
print(scores.len());        // 获取长度
print(scores.contains("Alice")); // 检查键是否存在
print(scores.keys());       // 获取所有键
print(scores.values());     // 获取所有值
```

### Async/Await

```bolide
async fn fetch_data(id: int) -> int {
    return id * 10;
}

// 创建冷 Future；不会自动并行执行
let f1: Future<int> = fetch_data(1);
let f2: Future<int> = fetch_data(2);

// await 只等待单个 Future
let r1: int = await f1;
let r2: int = await f2;
```

### 高级并发特性

#### Spawn All (并行等待)

```bolide
fn fetch_a() -> int { return 100; }
fn fetch_b() -> int { return 200; }

// 并发执行所有任务并等待结果（返回元组）
let results: (int, int) = spawn all {
    fetch_a(),
    fetch_b()
};
print(results[0]);  // 100
print(results[1]);  // 200

// 类型标注可以省略，编译器会根据各任务返回类型自动推断为元组
let inferred = spawn all {
    fetch_a(),
    fetch_b()
};
print(inferred[0]);  // 100
```

#### Spawn Select (竞态等待)

```bolide
// 并行启动所有任务，等待第一个完成的任务
spawn select {
    res1 = task_fast() => {
        print("fast finished");
    }
    res2 = task_slow() => {
        print("slow finished");
    }
}
```

### 多线程与并行

#### Spawn & Await

使用 `spawn` 启动热任务，返回 `Task<T>`；等待统一使用 `await`。
`pool` 块内的普通 `spawn` 进入当前线程池，`spawn thread` 会显式使用独立系统线程。

```bolide
fn heavy_work(id: int) -> int {
    // 耗时计算...
    return id * id;
}

// 启动独立系统线程
let t: Task<int> = spawn thread heavy_work(10);

// 等待线程结束并获取结果
let result: int = await t;
```

#### 线程池 (Thread Pool)

使用 `pool` 块将任务分发到指定大小的线程池中执行：

```bolide
pool(4) {
    // 这些任务将在4个工作线程中并发执行
    let t1: Task<int> = spawn heavy_work(1);
    let t2: Task<int> = spawn heavy_work(2);
    print(await t1);
    print(await t2);
}
// pool 块结束时会自动等待所有任务完成
```

#### 通道 (Channels)

线程间安全的通信机制。通道收发采用方法风格，与字符串、列表等保持一致：

```bolide
// 创建通道
let ch: channel<int> = channel();

// 定义发送函数
fn sender(c: channel<int>) {
    c.send(42);
}

// 启动发送线程
spawn sender(ch);

let val: int = ch.recv();  // 接收数据
ch.recv();                 // 纯同步：接收并丢弃返回值
```

#### 原子与同步

`std/atomic` 提供 `AtomicInt`、`AtomicBool`；`std/sync` 提供值语义的
`Mutex`、`RwLock` 和 `Once`。锁内值通过 `dynamic` 承载。锁 API 不暴露裸 guard，使用 `get()` 读取副本，
用 `set`/`swap`/`add_int` 等方法在运行时锁内完成修改。

```bolide
import "std/atomic" as atomic;
import "std/sync" as sync;

let counter: atomic.AtomicInt = atomic.new_int(0);
counter.add(1);
print(counter.get());

let lock: sync.Mutex = sync.mutex(10);
lock.add_int(5);
print(lock.get());
```

#### Channel Select (多路复用)

使用 `select` 语句处理多个通道操作，支持超时和默认分支：

```bolide
select {
    val1 = ch1.recv() => {
        print("Received from ch1");
    }
    timeout(100) => {
        print("Timed out");
    }
    default => {
        print("No data available");
    }
}
```

### 模块系统

模块按文件导入，所有符号位于以文件名命名的命名空间内（不污染全局命名空间）：

```bolide
// math_utils.bl
fn add(a: int, b: int) -> int {
    return a + b;
}

// main.bl
import "math_utils.bl";

let result: int = math_utils.add(10, 20);
print(result);  // 30

// 使用 as 指定别名
import "math_utils.bl" as mu;
print(mu.add(1, 2));  // 3
```

**导入路径解析规则**（确定性顺序，不依赖进程工作目录）：

1. 绝对路径按原样使用；
2. 相对路径基于**导入方源文件所在目录**解析；
3. **包管理器依赖**（`bolide.toml` 中声明的依赖，见下方[包管理器](#包管理器)）；
4. `BOLIDE_HOME` 环境变量指向的目录（开发期可指向仓库根，以便 `import "std/..."`）；
5. `bolide` 可执行文件所在目录（发行版布局：`std/` 与可执行文件同级）。

## 包管理器

Bolide 内置轻量级包管理器，支持在 `bolide.toml` 中声明依赖，从 **git 仓库**、
**本地路径**或 **registry 索引**获取，并以简洁的 `import <包名>;` 语法使用。

### 创建项目

```bash
bolide new myapp
```

生成骨架：

```
myapp/
  bolide.toml
  src/
    main.bl
```

### bolide.toml

```toml
[package]
name = "myapp"          # 必填，作为依赖被引用时的命名空间
version = "0.1.0"       # 必填
description = "..."     # 可选
authors = ["..."]       # 可选
license = "MIT"         # 可选
lib = "src/lib.bl"      # 可选，包入口文件（默认 src/lib.bl）

[dependencies]
# git 依赖
http = { git = "https://github.com/bolide-lang/http.git", ref = "v1.2.0" }
# 本地路径依赖（monorepo 开发，改动即时生效）
utils = { path = "../utils" }
# registry 依赖
db = { version = "0.3.0", registry = "https://registry.bolide.dev" }
```

### 命令

```bash
bolide add ../utils --path                 # 添加本地路径依赖
bolide add https://github.com/x/y.git --tag v1.0   # 添加 git 依赖
bolide add http@1.2.0                       # 添加 registry 依赖
bolide install                              # 解析依赖并生成 bolide.lock
bolide publish                              # 校验包（registry 上传暂未实现）
```

`bolide add` 会把依赖写入 `bolide.toml` 并自动运行 `install`。

### 使用依赖

依赖以包名作为命名空间，无需写相对路径：

```bolide
// src/main.bl
import utils;                 // 解析到 utils 包的入口文件

fn main() -> int {
    print(utils.greet());     // 调用依赖包导出的函数
    return 0;
}
```

也支持别名与子文件导入：

```bolide
import utils as u;            // 别名
import "utils/extra.bl";      // 导入包内的其他源文件（相对包源码目录）
```

`bolide run` / `bolide compile` 会自动向上查找 `bolide.toml`，解析依赖后注入编译器，
因此 JIT 与 AOT 两种模式都能使用包依赖。缓存位于
`%LOCALAPPDATA%\bolide`（Windows）或 `~/.cache/bolide`（Linux/macOS），
可用 `BOLIDE_CACHE_DIR` 环境变量覆盖。

### 标识符与内置函数隔离

运行时内置函数位于编译器内部的 `@_` 命名空间（`@` 不是合法标识符字符），
与用户代码完全隔离：

- 用户函数可以使用任何合法标识符，包括 `print_bigint`、`list_push` 这类与
  运行时内部函数同名的名字，不会冲突或递归；
- 运行时内部 ABI（如 `list_push`、`object_alloc`）**不暴露**给用户代码，
  列表/字典操作请使用方法语法（`xs.push(3)`）；
- 用户可直接调用的内置函数（`print`、`input`、`range`、`str`/`int`/`float`
  等类型转换、`channel`）是显式白名单，不受隔离影响。

### 变量与作用域

- `let` 声明不可变绑定，声明后不能重新赋值，也不能通过该绑定做
  `xs[i] = ...`、`xs.push(...)`、`dict.set(...)` 这类原地修改；
- `var` 声明可变绑定，允许重新赋值、复合赋值和容器原地修改；
- 顶层声明是**全局变量**，函数内可读取；需要在函数/回调中修改的全局状态应使用 `var`；
- 函数内声明是**局部变量**，同名时遮蔽全局变量；
- 全局变量与局部变量能力一致：可作 `ref` 实参、可作通道用于
  `send`/`recv` 收发与 `select`、支持 `float` 等全部类型。

### 类与面向对象

```bolide
class Point {
    x: int;
    y: int;

    fn distance() -> int {
        return self.x * self.x + self.y * self.y;
    }

    fn move_by(dx: int, dy: int) {
        self.x = self.x + dx;
        self.y = self.y + dy;
    }
}

// 使用构造函数直接初始化字段
let p: Point = Point(3, 4);
print(p.distance());  // 25

p.move_by(1, 1);
print(p.x);  // 4
print(p.y);  // 5

// 继承
class Animal {
    age: int;
    fn get_age() -> int { return self.age; }
}

class Dog: Animal {
    name: int;
    fn bark() -> int { return 100; }
}

let dog: Dog = Dog(3, 42);  // age=3, name=42
print(dog.get_age());  // 3 (继承的方法)
print(dog.bark());     // 100
```

### Trait

`trait` 约定一组方法；`impl Trait for Class` 把实现注入到类上。无方法体的为**必须实现**；带默认体的可省略。

```bolide
trait Drawable {
    fn draw();
    fn label() -> str { return "shape"; }
}

class Circle {
    r: int;
}

impl Drawable for Circle {
    fn draw() { print(self.r); }
}

let c = Circle(3);
c.draw();
print(c.label());  // 默认实现
```

#### 泛型约束

```bolide
fn paint<T: Drawable>(x: T) {
    x.draw();
}

// 多 bound
fn paint_count<T: Drawable + Countable>(x: T) {
    x.draw();
    print(x.count());
}
```

单态化时检查实参类型是否 `impl` 了对应 trait；未实现则编译报错并提示 `impl Trait for Type`。  
目标类型目前为 **class**（方法注入 + 静态分发）。`impl From<A> for B` 仍为 `?` 错误转换专用语法。

#### dyn Trait（运行时多态）

```bolide
fn paint(d: dyn Drawable) {
    d.draw();
}

paint(Circle(3));
paint(Box(5));

let d: dyn Drawable = Circle(1);
d.draw();
```

`dyn Trait` 在编译期改写为合成类 `__Dyn_Trait`（承载方法签名），运行时仍是普通对象指针；方法调用按 **class tag** 分派到真实实现类。传入的 class 须已 `impl` 该 trait（或满足协议方法）。

#### Supertrait

```bolide
trait Countable: Drawable {
    fn count() -> int;
}
// impl Countable for C 时，C 须已实现 Drawable（或已具备其方法）
```

#### 协议 trait（自动满足）

类上若存在对应方法，自动视为实现了下列协议，可用于 `T: Trait` 约束，无需手写 `impl`：

| 协议 | 方法 |
|------|------|
| `Add` / `Sub` / `Mul` / `Div` / `Mod` | `__add__` … |
| `Eq` / `Ord` | `__eq__` / `__lt__` |
| `BitAnd` / `BitOr` / `BitXor` / `Shl` / `Shr` | `__and__` … |
| `Neg` / `Not` | `__neg__` / `__not__` |
| `Iterator` | `next`（通常返回 `Option<T>`） |

任意带 `next()` 的 class 都可用于 `for x in it { ... }`（与生成器相同的 `Option` 循环脱糖）。

### 多继承（安全子集）

```bolide
class Child: Primary, Mixin1, Mixin2 { }
```

- **第一个父类（Primary）**：唯一参与**字段布局**与 `super` 链（可有字段）
- **其余父类（Mixin）**：必须**无字段**；方法复制进子类，以子类 `self` 编译
- 两个 mixin 提供同名方法且子类未覆盖 → **编译错误**（强制显式覆盖消歧）

这样得到「多继承行为」，避免钻石继承的字段/布局问题。能力组合更推荐 **trait**；mixin 适合无状态的工具类。

### 运算符重载

在 class 上定义 Python 风格 dunder 方法即可重载运算符。左操作数优先 `left.__op__(right)`；若左操作数没有对应方法，则尝试右操作数反射（如 `int + Vec` → `Vec.__radd__(int)`）。`+=` 等复合赋值脱糖为 `a = a + b`，会间接触发重载。

| 运算符 | 方法 | 反射（右操作数） |
|--------|------|------------------|
| `+` `-` `*` `/` `%` | `__add__` … `__mod__` | `__radd__` … `__rmod__` |
| `==` `!=` | `__eq__` `__ne__` | 同名（参数对调） |
| `<` `<=` `>` `>=` | `__lt__` … `__ge__` | 对偶 `__gt__` … `__le__` |
| `&` `\|` `^` `<<` `>>` | `__and__` `__or__` `__xor__` `__lshift__` `__rshift__` | `__rand__` … `__rrshift__` |
| 一元 `-` `!` | `__neg__` `__not__` | — |

```bolide
class Vec {
    x: int;
    y: int;
    fn __add__(o: Vec) -> Vec { return Vec(self.x + o.x, self.y + o.y); }
    fn __radd__(n: int) -> int { return n + self.x + self.y; }
    fn __neg__() -> Vec { return Vec(0 - self.x, 0 - self.y); }
    fn __eq__(o: Vec) -> bool {
        return self.x == o.x && self.y == o.y;
    }
}
print(Vec(1, 2) + Vec(3, 4));  // Vec
print(10 + Vec(1, 2));         // 13 via __radd__
print(-Vec(1, 2));
```

逻辑短路 `&&` / `||` **不**支持重载（保持短路语义）。一元逻辑非写作 `not x` 或 `!x`（均触发 `__not__`）。

### FFI (C 语言互操作)

Bolide 支持**双向** C 互操作：既能调用 C 库，也能被 C 程序调用。

#### Bolide 调 C

```bolide
// 动态加载 C 标准库函数。源码写逻辑库名，不写 .dll/.so/.dylib。
extern "dyn:c" {
    fn abs(x: c_int) -> c_int;
}

extern "dyn:m" {
    fn sqrt(x: f64) -> f64;
}

let a: int = abs(-42);      // 42
let b: float = sqrt(16.0);  // 4.0

// C 函数指针类型：func(...)（fn 只用于声明/字面量）
extern "dyn:c" {
    fn qsort(
        base: *c_void,
        n: c_size_t,
        size: c_size_t,
        cmp: func(*c_void, *c_void) -> c_int
    );
}

// 支持回调函数
fn my_callback(a: int, b: int) -> int {
    return a + b;
}
let r: int = test_callback(my_callback, 10, 20);
```

#### C ABI 类型写法

`extern` 签名使用独立的 C ABI 类型空间（**不是** Bolide 的 `int`/`float`）：

| 类别 | 写法 | 说明 |
|------|------|------|
| 平台相关整数 | `c_int`, `c_uint`, `c_long`, `c_size_t`, … | 宽度随平台/C ABI 变化 |
| 平台相关浮点 | `c_float`, `c_double` | 与 C `float`/`double` 一致 |
| 固定宽度 | `i8`…`i64`, `u8`…`u64`, `f32`, `f64` | 可移植绑定优先 |
| 字符/字节 | `c_char`, `c_uchar` | C 字符串：`*c_char` |
| 指针 | `*T`, `*c_void` | Bolide 侧不透明句柄用 `ptr` 存 |
| 函数指针 | `func(T...) -> R` | 与用户级函数值类型关键字一致 |

注意：
- Bolide `int` 是 64 位，**不等于** C `int`；raw 绑定请写 `c_int` 或 `i32`。
- 调用时编译器会做常见转换：`int`→`c_int` 截断、`str`→`*c_char`、窄整数返回拓宽为 `int`。
- 标准库对外 API 应继续用 `int`/`float`/`str` 等语言类型；只有 raw `extern` 才写 C ABI 类型。
- 不接受历史别名：`void`/`char`/`long`/`size_t`、以及类型位上的 `fn(...)`。

#### 外部库标识

`extern "..."` 中的库名使用跨平台标识，避免把 Windows/Linux/macOS 文件名写进 Bolide 源码：

| 标识 | 用途 | AOT | JIT | 说明 |
|------|------|-----|-----|------|
| `bolide` | Bolide runtime 内置函数 | 直接链接 | 直接链接 | 仅标准库内部使用 |
| `lib:name` | 静态库或导入库链接 | 支持 | 不支持 | Windows 映射为 `name.lib`，Unix 映射为 `-lname` |
| `dyn:name` | 运行时动态加载 | 支持 | 支持 | Windows 映射为 `name.dll`，Linux 为 `libname.so`，macOS 为 `libname.dylib` |
| `auto:name` | JIT 动态加载，AOT 原生链接 | 支持 | 支持 | JIT 等同 `dyn:name`；AOT 等同 `lib:name` |

常用别名：
- `dyn:c` / `dyn:m`: C 标准库 / 数学库动态加载；Windows 解析到 `msvcrt.dll`，Linux 解析到 `libc.so.6` / `libm.so.6`，macOS 解析到 `libSystem.B.dylib`。
- `lib:c` / `lib:m`: AOT 链接 C 标准库 / 数学库；Windows 使用 `msvcrt.lib`，Unix 使用 `-lc` / `-lm`。
- `auto:c` / `auto:m`: JIT 时按 `dyn:c` / `dyn:m` 加载，AOT 时按 `lib:c` / `lib:m` 链接。

注意点：
- 用户代码不要写 `extern "xxx.dll"`、`extern "libxxx.so"` 或 `extern "xxx.dylib"`。这些平台路径不可移植；需要动态加载时写 `dyn:name`。
- `lib:name` 是 AOT-only。JIT 没有链接阶段，不能链接 `.lib` / `.a` / `-lxxx`；开发期需要 JIT 运行时请使用 `dyn:name`。
- AOT 中 `dyn:name` 不会传给系统 linker，而是在生成代码里通过 runtime 动态加载。最终程序仍要求目标机器能找到对应动态库。
- `auto:name` 适合“开发期 JIT 用动态库，发布期 AOT 用静态库/导入库”的单源码写法；AOT 是否真正单文件取决于 `name.lib` / `libname.a` 是否是真静态库，若只是导入库仍需要对应动态库。
- 必要时 AOT 可以使用平台 linker 能识别的显式库参数（如 `foo.lib`、`libfoo.a`、`-lfoo`）作为逃生口；可移植代码优先使用 `lib:name`。
- 标准库 wrapper 应隐藏原始 C ABI。普通 Bolide API 应使用 `int`、`float`、`str`、`bytes` 等语言类型；只有写 raw `extern` 绑定时才需要 `c_int`、`f64`、`*c_char`、`*c_void`、`func(...)` 等 C ABI 类型（见上表）。

#### C 调 Bolide

用 `export fn` 标记要暴露给 C 的函数（裸符号名，无 name mangling），再用
`--lib` 编译为静态库、`--header` 生成 C 头文件：

```bolide
// mathlib.bl —— export 的函数以裸名导出供 C 链接
export fn add(a: int, b: int) -> int { return a + b; }
export fn scale(x: float, k: float) -> float { return x * k; }

fn internal_helper() -> int { return 1; }  // 不带 export，不导出
```

```bash
# 编译为静态库并生成头文件
bolide compile mathlib.bl --lib --header
# 产物: mathlib.lib (Windows) / libmathlib.a (Linux), mathlib.h
```

生成的 `mathlib.h`：

```c
/* Auto-generated by Bolide compiler. Do not edit. */
#ifndef BOLIDE_EXPORTS_H
#define BOLIDE_EXPORTS_H
#ifdef __cplusplus
extern "C" {
#endif

long long add(long long a, long long b);
double scale(double x, double k);

#ifdef __cplusplus
}
#endif
#endif /* BOLIDE_EXPORTS_H */
```

C 端链接 Bolide 库 + 运行时库即可调用：

```c
#include "mathlib.h"
#include <stdio.h>

int main(void) {
    printf("add(3,4) = %lld\n", add(3, 4));        // 7
    printf("scale(2.5,4.0) = %f\n", scale(2.5, 4.0)); // 10.0
    return 0;
}
```

```bash
# 链接时同时带上 mathlib 库与 bolide 运行时静态库
cl main.c mathlib.lib bolide_runtime.lib   # Windows (MSVC)
cc main.c libmathlib.a libbolide_runtime.a # Linux
```

> **C 互调 ABI 约定**: 跨 C 边界仅保证**数值与指针签名**稳定——`int`/`bool`
> 映射为 `long long`，`float` 映射为 `double`，其余复合类型（`str`/`list`/
> 对象等）按运行时内部指针（`void*`）传递，C 端无法安全构造。如需 C 友好的
> 导出函数，请使用纯数值/指针签名。

### 错误处理 (try/catch/throw)

Bolide 提供了轻量级的异常处理机制：`throw` 抛出 `Error` 或其子类，`try/catch` 捕获异常，支持可选的 `finally` 清理块。可恢复错误推荐用 `Result<T, E>` 或 `Option<T>` 建模。

#### 基本用法

```bolide
try {
    print("in try body");
    throw Error("boom");
    print("after throw (will not print)");
} catch (e: Error) {
    print("caught: " + e.message);
}
print("after try/catch");
```

#### 语法

```
throw_stmt = { "throw" ~ expr ~ ";" }
try_stmt  = { "try" ~ block ~ catch_clause+ ~ finally? }
catch_clause = { "catch" ~ "(" ~ ident ~ ":" ~ type_expr ~ ")" ~ block }
throws_clause = { "throws" ~ type_expr ~ ("," ~ type_expr)* }
try_expr = { "try" ~ block }
postfix_expr = { primary ~ (... | "?" | "!")* }
```

- **`throw`** 只能抛出 `Error` 或 `Error` 子类
- **`catch (e: T)`** 只能捕获 `Error` 或 `Error` 子类，支持子类匹配（编译器自动展开）
- **`finally`** 块无论是否抛出异常都会执行，适合资源清理
- **`throws`** 是可选函数注解，用于声明函数可能抛出的异常类型：
  `fn load(path: str) throws IoError, ParseError -> Config { ... }`
- **`expr?`** 解包 `Result.Ok(v)` / `Option.Some(v)`，遇到 `Err` / `None` 时从当前函数早返回
- **`expr!`** 解包 `Result.Ok(v)`，遇到 `Err(e)` 时把 `e` 作为异常抛出（`E` 必须是 `Error` 或子类）
- **`try { ... }` 表达式** 捕获块内抛出的异常并返回 `Result<T, Error>`；最后一个表达式语句作为 `Ok` 值，没有表达式时为 `Ok(0)`

#### Result / Option 与 `?`

`Result<T, E>` 和 `Option<T>` 是内置泛型 ADT，适合表达可恢复错误和值缺失：

```bolide
fn parse_number(text: str) -> Result<int, Error> {
    if text.len() == 0 {
        return Result.Err(Error("empty input"));
    }
    return Result.Ok(int(text));
}

fn load_value() -> Result<int, Error> {
    let value: int = parse_number("42")?;
    return Result.Ok(value + 1);
}
```

`expr?` 的规则：

- `Result.Ok(v)?` 得到 `v`
- `Result.Err(e)?` 从当前函数早返回 `Result.Err(e)`
- `Option.Some(v)?` 得到 `v`
- `Option.None()?` 从当前函数早返回 `Option.None()`

`?` 要求当前函数也返回 `Result` 或 `Option`。当两边都是 `Result` 但 **错误类型不同** 时，需要 `impl From<Src> for Dst`，`?` 会在传播 `Err` 时自动调用转换函数。

#### 错误类型转换与 `From`

```bolide
class IoError: Error {}
class AppError: Error {}

impl From<IoError> for AppError {
    fn from(e: IoError) -> AppError {
        return AppError("wrapped: " + e.message);
    }
}

fn read() -> Result<str, IoError> {
    return Result.Err(IoError("disk full"));
}

fn load() -> Result<str, AppError> {
    let text: str = read()?;   // IoError → AppError
    return Result.Ok(text);
}
```

- 错误类型相同时，`?` 直接早返回原 `Result`（无需 From）。
- 类型不同且缺少 `impl From` 时，编译器给出实现提示。
- `impl From` 脱糖为内部函数 `__from_Src_for_Dst`；用户一般只写 `impl` 语法。

#### `!`：Result 转异常

当调用点认为错误不应继续作为普通 `Result` 传播时，可以用 `!` 把 `Err` 升级为异常：

```bolide
fn init() {
    let value: int = parse_number("42")!;
    print(value);
}
```

`expr!` 的规则：

- `Result.Ok(v)!` 得到 `v`
- `Result.Err(e)!` 执行 `throw e`
- `e` 的类型必须是 `Error` 或 `Error` 子类

#### `try` 表达式：异常转 Result

`try { ... }` 表达式会把块内抛出的异常捕获为 `Result.Err(error)`：

```bolide
let result: Result<int, Error> = try {
    let value: int = parse_number("42")!;
    value + 1;
};
```

最后一个表达式语句作为 `Result.Ok(value)` 的值；如果块内没有最后表达式，则返回 `Result.Ok(0)`。当前不会自动扁平化 `Result<Result<T, E>, Error>`。

#### 内置 Error 类

编译器内置一个 `Error` 类（单字段 `message: str`），无需声明即可直接使用。
也可以定义子类继承 `Error`，由基类 `catch` 统一捕获：

```bolide
// 直接抛出/捕获内置 Error
try {
    throw Error("boom");
} catch (e: Error) {
    print(e.message);   // boom
}

// 自定义子类继承 Error，被基类 catch 捕获（子类匹配）
class MyError: Error {}

try {
    throw MyError("custom failure");
} catch (e: Error) {
    print(e.message);   // custom failure
}
```

> 若用户自定义了同名 `Error` 类，则以用户定义为准（内置类被跳过）。

#### 嵌套 try/catch

```bolide
try {
    try {
        throw Error("inner");
    } catch (e: Error) {
        print("inner catch: " + e.message);
    }
    print("after inner try");
} catch (e: Error) {
    print("outer catch (should not reach)");
}
```

#### 重新抛出 (Rethrow)

```bolide
try {
    try {
        throw Error("77");
    } catch (e: Error) {
        print("rethrowing: " + e.message);
        throw e;  // 重新抛出
    }
} catch (e: Error) {
    print("outer catch: " + e.message);  // 77
}
```

#### finally 清理

```bolide
fn open_file(path: str) -> int {
    // ... open file, return handle
    return 1;
}

fn close_file(handle: int) {
    // ... close file
}

let handle: int = open_file("data.txt");
try {
    // ... 可能抛出异常的代码
    throw Error("something went wrong");
} catch (e: Error) {
    print("error: " + e.message);
} finally {
    close_file(handle);  // 一定会执行
    print("cleanup done");
}
```

#### 实现原理

不使用 `setjmp/longjmp` 或 OS 栈展开。当前实现采用显式异常传播 ABI：

- 同一函数内：编译器维护 **catch 落点栈**，`throw` 将异常值和类型标签存入 thread-local，然后跳转到最近的 catch 块
- 跨函数调用：callee 抛出异常后把 pending exception 留在线程局部状态中并返回默认值；caller 在用户函数/方法/闭包调用后检查 pending exception，再跳到当前函数的最近 catch 或继续向上传播
- `finally` 会在本地跳转、重抛和跨函数传播路径上执行

类型标签机制：
- 异常对象必须是 `Error` 或其子类
- 自定义类按声明顺序分配 ID（≥100）
- `catch (e: T)` 的类型过滤在编译器展开为标签比较的 OR 链（含 T 的所有已知子类）

> **当前状态**: 跨函数异常传播已支持 JIT 和 AOT，但 `throws` 仍是签名注解和工具元数据，暂未实现 checked-exception 式强制诊断。

## 类型系统

| 类型 | 说明 | 示例 |
|------|------|------|
| `int` | 64位整数 | `let x: int = 42;` |
| `float` | 64位浮点数 | `let pi: float = 3.14;` |
| `bool` | 布尔值 | `let flag: bool = true;` |
| `str` | 字符串 | `let s: str = "hello";` |
| `bigint` | 任意精度整数 | `let b: bigint = 999b;` |
| `decimal` | 高精度小数 | `let d: decimal = 3.14d;` |
| `list<T>` | 泛型列表 | `let l: list<int> = [1, 2, 3];` |
| `tuple` | 元组 | `let t: tuple = (1, 2, 3);` |
| `channel<T>` | 通道 | `let ch: channel<int> = channel();` |
| `dict<K, V>` | 字典 | `let d: dict<str, int> = {"a": 1};` |
| `dynamic` | 动态类型 | (运行时自动推导) |
| `Future<T>` | 冷协程 Future | `let f: Future<int> = async_fn();` |
| `Task<T>` | 已启动任务句柄 | `let t: Task<int> = spawn work();` |
| `func(T...) -> R` | 函数类型 | `let f: func(int) -> int = double;` |


## 内存管理

Bolide 使用 **ARC (自动引用计数)** 作为默认内存管理方式。引用计数是**原子操作**（与 Swift/Rust Arc 相同的内存序），跨线程传递对象不会产生计数竞争。同时提供生命周期注解和弱引用来处理特殊场景。

### 生命周期注解 (from)

使用 `from` 关键字指定返回值的生命周期依赖，跳过 ARC 开销：

```bolide
// 返回值的生命周期依赖于参数 x
fn get_value(ref x: bigint) -> bigint from x {
    return x;
}

let a: bigint = 100B;
let b: bigint = get_value(a);  // b 借用 a，不增加引用计数
```

编译器会对借用施加以下检查，违反时**编译报错**：

- `from` 函数的返回值必须来源于声明的参数；
- 借用变量在来源离开作用域后不可继续存活（悬空检测）；
- 借用存活期间，**禁止对来源变量重新赋值**（旧对象会被释放）；
- 借用值**不允许逃逸**：不能存入列表/字典/元组/对象字段、不能通过
  `push`/`insert` 等存储型方法进入容器、不能通过通道发送或作为 `spawn`
  参数跨线程传递、不能从未声明 `from` 的函数返回。

需要让借用值逃逸时，请显式拷贝（如 `bigint(b)`）后再存储。

### weak 引用

`weak` 引用不增加强引用计数，用于打破循环引用：

```bolide
class Node {
    value: int;
}

let obj: Node = Node(42);

let w: weak Node = obj;  // weak 引用，不增加强引用计数
print(w.value);          // 访问前自动检查对象是否存活
```

weak 引用会持有对象头（弱引用计数），对象被释放后访问 weak 引用会触发
**确定性的运行时错误并中止程序**（带诊断信息），而不是未定义行为：

```text
runtime error: weak/unowned reference accessed after object was deallocated
```

### unowned 引用

`unowned` 引用同样不增加强引用计数，语义上假设对象始终存在。与 weak 相同，
访问时会进行存活检查——对象已释放时**立刻报错中止**，绝不会读到悬空内存：

```bolide
let obj: Node = Node(42);
let u: unowned Node = obj;  // unowned 引用
print(u.value);             // 对象存活时直接访问
```

> weak 与 unowned 的区别是语义意图：weak 表示"对象可能先于引用消亡"（典型如
> 子节点回指父节点），unowned 表示"对象保证活得比引用久"。两者目前都采用
> trap 式检查保证内存安全；未来引入可选类型后，weak 将支持 nil 分支处理。

### 并发安全

- 所有引用计数（强/弱）均为原子操作，跨线程 retain/release 无数据竞争；
- `spawn` 会拒绝明显共享可变的 `list`、`dict`、`bytes` 和 `dynamic` 参数；
- 已确认不可变的 `let` 绑定可以作为 `spawn` 参数传入；运行时会按类型 clone/retain，
  例如 `list`/`dict`/`bytes` 会传递独立容器副本；
- 需要在线程间共享并修改状态时，优先使用 channel、`std/atomic` 或 `std/sync`；
- `pool(0)` 会钳制为至少 1 个 worker；线程/任务句柄的重复 await/join 会被运行时同步保护。


## 项目结构

```
bolide/
├── crates/
│   ├── bolide-cli/       # 命令行入口
│   ├── bolide-compiler/  # JIT 编译器 (Cranelift)
│   ├── bolide-parser/    # 词法/语法分析器 (PEG)
│   └── bolide-runtime/   # 运行时库
├── vscode-bolide/        # VS Code 插件
├── examples/             # 示例程序
└── README.md
```

## 标准库实现方式

### 常用标准库模块

导入推荐短路径（`import "std/fs" as fs;`，兼容旧的 `std/fs/fs.bl`）。索引见 `std/README.md`，教程见 `docs/standard-library.md`。

- `std/collections`：`IntSet`、`StringSet`、`Queue`、`Stack`、`Deque`、`Counter`、优先队列。
- `std/iter`：序列、`take`/`drop`/`chunk`/`sum`/`zip`/`unique` 等。
- `std/option` / `std/result` / `std/traits`：Option/Result 工具与标准协议。
- `std/prelude`：常用模块一键导入。
- `std/arena`：`Arena` / `BufferArena`。
- `std/atomic` · `std/sync`：原子类型与 Mutex/RwLock/Once。
- `std/json` · `std/csv` · `std/html` · `std/template` · `std/text`：文本与 Web 输出。
- `std/fs` · `std/path` · `std/io` · `std/process` · `std/env` · `std/time`：系统与 IO。
- `std/http` · `std/web` · `std/gui`：HTTP 客户端、Web 服务、GUI。

Bolide 标准库通常由 `.bl` 包装层加底层实现组成。用户侧通过 `import "std/..."` 使用稳定的 Bolide API，底层实现由工具链处理。

1. **Rust runtime 内置库**
   - 适合语言核心标准库、跨平台能力，以及需要和 Bolide 运行时对象配合的功能。
   - 底层实现位于 `crates/bolide-runtime/src/`，通过 `extern "bolide"` 暴露给 `.bl` 包装层。
   - AOT 时随 `bolide_runtime.lib` / `libbolide_runtime.a` 静态链接进最终程序；JIT 时由当前 `bolide` 进程直接解析 runtime 符号。
   - 例如 `std/fs`、`std/web`、`std/gui`（底层 `.bl` 包装 + runtime）当前都使用这种模式。

2. **独立静态标准库**
   - 适合保持为独立模块、又希望由 Bolide 工具链自动管理的平台能力或较大组件。
   - `.bl` 包装层使用 `extern "std:name"`；CLI 根据标准库名和目标平台自动查找对应实现，可以是 C 源、对象文件或静态库。
   - AOT 时实现会被编译/链接为最终可执行文件的一部分；JIT 开发期由工具链按同一标准库名解析可用实现。
   - 用户代码只依赖标准库 import 和 Bolide 类型，发布形态由编译器和 CLI 决定。

3. **外部 C 库 FFI**
   - 适合绑定系统 API、第三方 C 库，或直接使用已有原生库。
   - 静态/导入库使用 `extern "lib:name"`，AOT 时按平台映射为 `name.lib` 或 `-lname`。
   - 动态加载使用 `extern "dyn:name"`，AOT/JIT 都按平台解析并运行时加载，例如 `dyn:c`、`dyn:m`。

选择原则：和 `str`、列表、对象、线程、文件系统等运行时模型紧密相关的功能优先放入
Rust runtime；需要独立演进但仍属于 Bolide 标准库体验的组件使用独立静态标准库；绑定已有系统库或第三方库时使用外部 C FFI。

## Web 标准库

Bolide 提供 `std/web`（短路径；旧路径 `std/web/web.bl` 仍可用），目标是用简洁 API 写出接近 FastAPI 使用体验、但能 AOT
编译为原生可执行文件的 Web 服务。底层实现位于 runtime，AOT 时会静态链接进最终程序；
发布 Web 应用时可以保持单文件可执行程序形态。

```bolide
import "std/web" as web;

fn index(req: web.Request) -> web.Response {
    return web.html("<h1>Hello Bolide</h1><p>path=" + req.path() + "</p>");
}

fn hello(req: web.Request) -> web.Response {
    return web.text("hello " + req.path_param("name"));
}

let app: web.App = web.app();
app.get("/", index);
app.get_async("/hello/{name}", hello);
app.static_files("/static", "public");

app.listen(8080);  // 或 app.run("127.0.0.1", 8080)
```

当前 Web 标准库支持：
- HTTP 方法：`GET`、`POST`、`PUT`、`PATCH`、`DELETE`、`HEAD`、`OPTIONS`、`TRACE`、`CONNECT`。
- 路由：精确路径、`/posts/{id}` 动态路径参数、静态文件目录。
- 请求读取：method、target、path、query、version、header、cookie、query 参数、form 参数、path 参数、body 文本和 bytes。
- 响应构造：text、html、json、bytes、empty、redirect，自定义 status/header/cookie。
- 会话：session id、get/set/contains/remove/clear/destroy/regenerate，cookie 由标准库封装。
- 并发服务：默认自动选择 worker 和 acceptor；`set_workers(n)` 仅作为压测或部署调优 override；`*_async` 路由当前走同一高性能 worker/reactor 路径，保留 API 语义以便后续扩展。
- 连接处理：HTTP/1.1 keep-alive、Content-Length、HEAD 无 body、405/OPTIONS 自动能力；AOT 服务在 Windows/Unix 上使用更大的 listen backlog 和多 acceptor，减少高并发短连接突发下的 connect refused。

JIT 适合开发期快速调试：

```bash
bolide run examples/blog/main.bl
```

AOT 适合发布：

```bash
bolide compile examples/blog/main.bl -o examples/blog/main.exe
examples/blog/main.exe
```

### Web 性能

仓库内提供本地压测工具：

```powershell
# 构建并对比 hello 服务
.\bench\http_bench.ps1 -Target CompareHello -Requests 100000 -Concurrency 1024

# 压测内存版博客示例
.\bench\http_bench.ps1 -Target BolideFastBlog -Requests 50000 -Concurrency 1024 `
  -Paths "/,/about,/login,/posts/1,/posts/2,/posts/3"
```

当前在 Windows 本机 loopback、AOT release、keep-alive 开启的参考结果：

| 场景 | 并发 | 请求数 | 结果 |
|------|------|--------|------|
| `bench/http_bolide_hello_sync.bl` | 1024 | 100000 | 约 150k RPS，0 errors |
| `bench/http_bolide_hello_async.bl` | 1024 | 100000 | 约 157k RPS，0 errors |
| `examples/blog/main.bl` 原始博客多页面 | 512 | 50000 | 约 57k RPS，0 errors |
| `examples/blog/main.bl` 原始博客多页面 | 1024 | 50000 | 约 51k-55k RPS，0 errors |
| `bench/http_bolide_blog_fast.bl` 多页面 | 512 | 30000 | 约 122k RPS，0 errors |
| `bench/http_bolide_blog_fast.bl` 多页面 | 1024 | 50000 | 约 106k RPS，0 errors |

Go 对照程序位于 `bench/http_go_hello.go` 和 `bench/http_go_blog.go`，可用同一压测工具复现：

```powershell
# Go hello vs Bolide hello
.\bench\http_bench.ps1 -Target CompareHello -Requests 100000 -Concurrency 1024

# Go 小博客 vs Bolide 原始博客示例
.\bench\http_bench.ps1 -Target CompareBlog -Requests 50000 -Concurrency 512

# Go 小博客 vs Bolide 内存版快速博客
.\bench\http_bench.ps1 -Target CompareFastBlog -Requests 50000 -Concurrency 512 `
  -Paths "/,/about,/login,/posts/1,/posts/2,/posts/3"
```

本机参考对比：

| 场景 | 条件 | Go | Bolide | 说明 |
|------|------|----|--------|------|
| Hello HTTP | 1024 并发 | 约 137k RPS | sync 约 150k RPS；async 约 157k RPS | 这组主要测 HTTP reactor 和路由开销 |
| 小博客页面 | 512 并发 | 约 47k RPS | 热缓存后约 57k | 架构不完全相同：Go 小博客是内存数据 + `html/template`；Bolide 版包含文件模板、文件数据库和更完整的后台/登录/评论流程，但是带热缓存。 |
| 内存版博客页面 | 512 并发 | 约 47k RPS | 约 122k RPS | Bolide fast 版使用内存数据和直接 HTML 生成，不是模板引擎严格同构对比 |

## 模板与数据库标准库

Bolide 现在提供轻量模板引擎和文件数据库，目标是让小型 Web 应用可以只依赖标准库完成。
当前实现由 `.bl` wrapper 隐藏底层 ABI，用户侧只接触 `str`、`dict<str, dynamic>`、
`list<dict<str, dynamic>>` 和 `Database` 等 Bolide 类型，不需要直接处理 `ptr`。

```bolide
import "std/db" as db;
import "std/template" as template;

let database: db.Database = db.open("data/blog");
database.create_table("posts", "title,slug,body,published");

let row: dict<str, dynamic> = {
    "title": "Hello Bolide",
    "slug": "hello-bolide",
    "body": "A first post.",
    "published": true,
};
database.insert("posts", row);

let posts: list<dict<str, dynamic>> = database.all("posts");
let html: str = template.render(
    "<h1>{{ title }}</h1>{% for post in posts %}<article>{{ post.title }}</article>{% endfor %}",
    {
        "title": "Blog",
        "posts": posts,
    }
);
print(html);
database.close();
```

模板语法保持小而明确：
- `{{ expr }}` 默认 HTML 转义，适合页面文本输出。
- `{!! expr !!}` 输出原始 HTML，只应对可信内容使用。
- `{% if cond %}...{% else %}...{% endif %}` 支持条件渲染。
- `{% for post in posts %}...{% endfor %}` 支持遍历列表。
- `post.title` 这类点路径可以读取字典和对象字段。

数据库 API 使用目录作为存储位置，表按文件保存，支持 `create_table`、`insert`、`update`、
`delete`、`get`、`all`、`where_eq`、`count` 和 `last_error`。`all

完整博客示例见 `examples/blog/`，包含文章列表、详情页、
关于页、后台列表、新建、编辑、删除、种子数据和响应式页面样式。开发时可用：

```bash
cd examples/blog
bolide run main.bl
```

示例会启动本地 Web 服务，模板位于 `examples/blog/templates/`，数据库默认写入
`examples/blog/data/`；发布前建议再用 AOT 编译验证。

## VS Code 插件

Bolide 提供了 VS Code 插件，支持语法高亮和一键运行。

### 安装方法

#### 方法 1: 复制到扩展目录（推荐）

将 `vscode-bolide` 文件夹复制到 VS Code 扩展目录：

- **Windows**: `%USERPROFILE%\.vscode\extensions\`
- **macOS**: `~/.vscode/extensions/`
- **Linux**: `~/.vscode/extensions/`

然后重启 VS Code。

#### 方法 2: 打包为 VSIX 安装

```bash
cd vscode-bolide
npm install
npm install -g @vscode/vsce
vsce package
```

然后在 VS Code 中按 `Ctrl+Shift+P`，输入 "Install from VSIX"，选择生成的 `.vsix` 文件。

### 配置

在 VS Code 设置中配置 Bolide 可执行文件路径：

```json
{
  "bolide.executablePath": "D:\\Project\\bolide_new\\target\\release\\bolide.exe"
}
```

### 使用

1. 打开 `.bl` 文件
2. 按 `Ctrl+Shift+R` 运行当前文件

## GUI 开发

Bolide 提供 `std/gui`（短路径；旧路径 `std/gui/gui.bl` 仍可用），当前后端为 runtime 中的 `egui/eframe`。GUI 程序使用声明式回调渲染：应用状态保存在 Bolide 全局变量或对象中，`gui.run(title, width, height, root)` 创建窗口并在每一帧调用 `root(ui)` 绘制界面。JIT 可用于开发期快速运行，AOT 会把 GUI runtime 静态链接进最终可执行文件。

GUI 标准库的用户层只接触 Bolide 类型：`gui.Ui`、`str`、`int`、`bool` 和 `func(gui.Ui)` 回调。底层窗口、平台事件循环和 egui 对象由 runtime 管理。

### 布局模型

布局 API 借鉴 Tkinter 的 `pack/grid/place` 思路，同时保留 egui 的自动排版能力：

- `pad(x, y, child)`：给子布局添加内边距。
- `pack_top/pack_left/pack_right/pack_bottom(spacing, child)`：按方向排列一组控件。
- `row(child)` / `column(child)`：水平或垂直排列。
- `grid(id, columns, striped, child)`：表格式布局；放在 `fill(...)` 中时会按可用宽高自动分配单元格。
- `fill(child)`、`fill_width(child)`、`fill_height(child)`：让子布局占满当前可用空间。
- `left/right/centered(child)` 和 `align(mode, child)`：控制子布局和文本对齐。
- `frame(title, child)`、`scroll(id, height, child)`、`indent(id, child)`、`collapsing(title, child)`：常用容器。
- `width/height/size(..., child)` 与 `place(...)`：用于固定尺寸或绝对位置的局部区域。

普通按钮、文本输入、滑条、进度条等控件会根据所在布局使用可用宽度；在 `row`、`pack_left`、`pack_right` 这类水平布局中按内容紧凑排列，在 `fill + grid` 场景下会撑满网格单元格并按高度放大文字。

### 基本示例

```bolide
import "std/gui" as gui;

var count: int = 0;
var status: str = "准备";

fn toolbar(ui: gui.Ui) {
    if ui.button("增加") {
        count = count + 1;
        status = "计数 " + str(count);
    }
    if ui.button("清零") {
        count = 0;
        status = "已清零";
    }
}

fn status_line(ui: gui.Ui) {
    ui.strong(status);
}

fn body(ui: gui.Ui) {
    ui.heading("Bolide GUI");
    ui.pack_left(8, toolbar);
    ui.space(12);
    ui.right(status_line);
}

fn root(ui: gui.Ui) {
    ui.pad(16, 16, root_padded);
}

fn root_padded(ui: gui.Ui) {
    ui.fill(body);
}

gui.run("Bolide GUI", 420, 280, root);
```

### 计算器示例

完整计算器位于 `examples/calculator.bl`。它展示了更接近桌面应用的布局方式：上方显示区固定高度，底部按键区使用 `fill(buttons_panel)`，按键通过 `grid("calculator-buttons", 4, false, buttons_grid)` 自动撑满窗口剩余空间。

运行 JIT 版本：

```bash
bolide run examples/calculator.bl
```

编译 AOT 版本：

```bash
bolide compile examples/calculator.bl -o examples/calculator.exe
examples/calculator.exe
```

### 常用控件

- 文本：`label`、`heading`、`small`、`strong`。
- 命令与选择：`button`、`selectable`、`link`。
- 输入：`text_input`、`password_input`、`multiline_input`、`checkbox`、`slider`。
- 状态：`progress`、`separator`、`space`。
- 尺寸查询：`available_width()`、`available_height()`。
- 重绘：`request_repaint()`。

### 实现与发布

`std/gui` 通过 `extern "bolide"` 调用 runtime 中的 GUI 后端。AOT 链接时 GUI 后端随 Bolide runtime 静态进入最终程序；Windows 下 runtime 会配置 winit event loop 兼容 AOT 入口线程。中文显示通过系统 CJK 字体回退处理。

## 许可证

MIT License
