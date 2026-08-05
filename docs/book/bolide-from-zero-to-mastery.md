# Bolide 从入门到精通

版本基准：Bolide 0.13.1

Bolide 是一门基于 Cranelift 的 JIT/AOT 编译型语言。它的语法追求直接、可读，运行模型追求原生性能，同时保留 Python 风格的高层表达能力：字符串和集合操作、切片、默认参数、具名参数、一等函数、闭包、类、异常、并发、FFI、Web 与 GUI 标准库。

这本书不是 API 清单，而是一条学习路径：从写出第一个程序开始，到理解类型系统、内存模型、并发、AOT 发布和与 C 互操作。

## 目录

1. 认识 Bolide
2. 安装、运行与项目结构
3. 第一个程序
4. 基础语法：变量、类型、表达式
5. 控制流、作用域与求值顺序
6. 函数、参数与泛型
7. 一等函数、闭包与高阶编程
8. 字符串、列表、字典、元组与切片
9. 类、对象与面向对象
10. 枚举、模式匹配与错误处理
11. 模块系统与包管理器
12. 类型系统与类型推断
13. 内存管理：ARC、生命周期、weak、unowned
14. 并发、线程、通道与 async/await
15. FFI 与 AOT 发布
16. Web、模板、数据库与 GUI 标准库
17. 报错诊断、调试与测试
18. 性能优化与工程实践
19. 综合项目：命令行任务管理器
20. 附录：常用语法速查

---

## 第 1 章 认识 Bolide

Bolide 的核心目标是把脚本语言的表达效率和原生编译语言的部署体验结合起来。

它有两种运行方式：

- JIT：`bolide run main.bl`，适合开发、调试、快速试验。
- AOT：`bolide compile main.bl -o main`，适合发布独立可执行文件。

Bolide 适合这些场景：

- 写性能足够好的命令行工具。
- 写需要原生部署的服务端程序。
- 写需要 C 互操作的系统边界代码。
- 写带 GUI 的小工具。
- 用高级语法组织中大型业务逻辑。

Bolide 文件通常使用 `.bl` 扩展名。

一个最小程序：

```bolide
print("hello Bolide");
```

JIT 运行：

```bash
bolide run hello.bl
```

AOT 编译：

```bash
bolide compile hello.bl -o hello
```

---

## 第 2 章 安装、运行与项目结构

从源码构建：

```bash
git clone https://github.com/streetartist/bolide.git
cd bolide
cargo build --release
```

运行示例：

```bash
cargo run --release -- run examples/hello.bl
```

发布包通常提供 `bolide` 或 `bolide.exe`。下载后可以直接运行：

```bash
bolide run your_program.bl
```

一个简单项目可以只有一个文件：

```text
hello.bl
```

稍复杂的项目建议使用包结构：

```text
my_app/
  bolide.toml
  src/
    main.bl
    util.bl
```

`bolide.toml` 描述包名和依赖。`bolide run` 和 `bolide compile` 会自动向上查找 `bolide.toml`，解析依赖并注入编译器。

常用命令：

```bash
bolide run src/main.bl
bolide compile src/main.bl -o app
bolide compile src/lib.bl --lib --header
bolide new my_app
bolide add dep-name@1.0.0
bolide install
```

---

## 第 3 章 第一个程序

创建 `hello.bl`：

```bolide
let name: str = "Bolide";
print("hello " + name);
```

运行：

```bash
bolide run hello.bl
```

Bolide 使用 `let` 定义变量。类型注解可以写，也可以在很多场景下省略：

```bolide
let a: int = 10;
let b = 20;
print(a + b);
```

读取用户输入：

```bolide
let name: str = input("name: ");
print("hello " + name);
```

`input()` 返回字符串。需要数字时显式转换：

```bolide
let raw: str = input("age: ");
let age: int = int(raw);
print(age + 1);
```

---

## 第 4 章 基础语法：变量、类型、表达式

### 变量

```bolide
let x: int = 42;
let pi: float = 3.14159;
let ok: bool = true;
let name: str = "Ada";
```

Bolide 的变量绑定使用 `let`。后续赋值使用 `=`：

```bolide
let n: int = 1;
n = n + 1;
n += 10;
```

### 基本类型

常用内建类型：

| 类型 | 含义 |
| --- | --- |
| `int` | 64 位整数 |
| `float` | 双精度浮点 |
| `bool` | 布尔值 |
| `str` | 字符串 |
| `bigint` | 任意精度整数 |
| `decimal` | 十进制定点/高精度数 |
| `dynamic` | 动态值 |
| `none` | 空值字面量 |

示例：

```bolide
let big: bigint = 123456789012345678901234567890b;
let money: decimal = 19.99d;
let empty = none;
```

### 表达式与运算符

```bolide
let a = 10 + 3 * 2;
let b = (10 + 3) * 2;
let c = a > b;
let d = true and not false;
let e = false or expensive_check();
```

`and` / `or` 是短路求值：右侧只在需要时执行。

### 类型转换

```bolide
let a: int = int("123");
let b: float = float(10);
let c: str = str(3.14);
let d: bigint = bigint(100);
let e: decimal = decimal(3.14);
```

---

## 第 5 章 控制流、作用域与求值顺序

### if / elif / else

```bolide
let n = int(input("n: "));

if n > 0 {
    print("positive");
} elif n < 0 {
    print("negative");
} else {
    print("zero");
}
```

### while

```bolide
let i = 0;
while i < 5 {
    print(i);
    i += 1;
}
```

### for

```bolide
for i in range(5) {
    print(i);
}

let nums: list<int> = [10, 20, 30];
for n in nums {
    print(n);
}
```

### break / continue

```bolide
for i in range(10) {
    if i == 3 {
        continue;
    }
    if i == 8 {
        break;
    }
    print(i);
}
```

### 作用域

块会创建局部作用域。内层变量不会污染外层：

```bolide
let x = 1;

if true {
    let x = 2;
    print(x); // 2
}

print(x); // 1
```

### 求值顺序

函数参数、具名参数、变长参数和复合表达式按源码顺序求值。依赖副作用时，应把复杂表达式拆成临时变量，让顺序更清晰：

```bolide
let a = next();
let b = next();
print(pair(a, b));
```

---

## 第 6 章 函数、参数与泛型

### 定义函数

```bolide
fn add(a: int, b: int) -> int {
    return a + b;
}

print(add(1, 2));
```

无返回值函数可以省略返回类型：

```bolide
fn greet(name: str) {
    print("hello " + name);
}
```

### 默认参数

```bolide
fn greet(name: str = "world", punctuation: str = "!") {
    print("hello " + name + punctuation);
}

greet();
greet("Bolide");
```

### 具名参数

```bolide
greet(name="Ada", punctuation=".");
greet(punctuation="?", name="Bob");
```

Bolide 同时接受 `name=value` 和 `name: value` 风格的具名实参。

### 变长参数

```bolide
fn total(base: int = 0, *nums: int) -> int {
    let sum = base;
    for n in nums {
        sum += n;
    }
    return sum;
}

print(total(10, 1, 2, 3));
```

关键字变长参数：

```bolide
fn show(**opts: int) {
    print(opts);
}

show(width=100, height=80);
```

### 参数模式

Bolide 支持普通借用、所有权传递和引用修改：

```bolide
fn take(owned value: list<int>) {
    print(value);
}

fn bump(ref n: int) {
    n += 1;
}
```

### 泛型函数

```bolide
fn id<T>(x: T) -> T {
    return x;
}

print(id<int>(10));
print(id<str>("hello"));
```

通常调用点可以推断类型：

```bolide
print(id(10));
print(id("hello"));
```

---

## 第 7 章 一等函数、闭包与高阶编程

函数是值：可以赋给变量、作为参数、作为返回值、放进集合。

```bolide
fn add1(x: int) -> int { return x + 1; }
fn double(x: int) -> int { return x * 2; }

let f = add1;
print(f(10)); // 11

let g: func(int) -> int = double;
print(g(10)); // 20
```

### 函数作为参数

```bolide
fn apply(callback: func(int) -> int, x: int) -> int {
    return callback(x);
}

print(apply(double, 21)); // 42
```

### 返回函数

```bolide
fn pick(which: int) -> func(int) -> int {
    if which == 0 {
        return add1;
    }
    return double;
}

let f = pick(1);
print(f(7)); // 14
```

### 闭包

闭包使用 `fn(...) -> T { ... }` 字面量，会自动捕获局部变量：

```bolide
fn make_adder(delta: int) -> func(int) -> int {
    return fn(x: int) -> int {
        return x + delta;
    };
}

let add10 = make_adder(10);
print(add10(5)); // 15
```

闭包适合延迟执行、回调、组合逻辑。

### map / filter

```bolide
fn double(x: int) -> int { return x * 2; }
fn is_even(x: int) -> bool { return x % 2 == 0; }

let nums: list<int> = [1, 2, 3, 4];
print(nums.map(double));     // [2, 4, 6, 8]
print(nums.filter(is_even)); // [2, 4]
```

---

## 第 8 章 字符串、列表、字典、元组与切片

### 字符串

```bolide
let s = "Hello, World";

print(s.len());
print(s.upper());
print(s.lower());
print(s.contains("World"));
print(s.find("World"));
print(s.replace("World", "Bolide"));
print(s.trim());
```

字符串可以拼接：

```bolide
let name = "Ada";
print("hello " + name);
```

### 列表

```bolide
let nums: list<int> = [1, 2, 3];
nums.push(4);
print(nums[0]);
print(nums.len());
```

遍历：

```bolide
for n in nums {
    print(n);
}
```

### 字典

```bolide
let ages: dict<str, int> = {"Ada": 36, "Bob": 28};
print(ages["Ada"]);
ages["Cara"] = 31;
```

常见用法是配置表、索引表、计数器。

### 元组

```bolide
let point: (int, int) = (3, 4);
print(point[0]);
print(point[1]);
```

### 切片

列表、字符串、元组支持 Python 风格切片：

```bolide
let nums = [0, 1, 2, 3, 4, 5];
print(nums[1:4]);
print(nums[:3]);
print(nums[2:]);
print(nums[::2]);
```

---

## 第 9 章 类、对象与面向对象

类用于组织数据和行为：

```bolide
class Counter {
    value: int = 0;

    fn inc(self) {
        self.value += 1;
    }

    fn get(self) -> int {
        return self.value;
    }
}

let c = Counter();
c.inc();
c.inc();
print(c.get()); // 2
```

字段可以有默认值，也可以通过构造参数初始化：

```bolide
class Point {
    x: int;
    y: int;
}

let p = Point(3, 4);
print(p.x);
```

### 继承

```bolide
class Animal {
    name: str;

    fn speak(self) -> str {
        return self.name + " makes a sound";
    }
}

class Dog: Animal {
    fn speak(self) -> str {
        return self.name + " barks";
    }
}
```

对象适合表达拥有状态的实体；纯计算优先用函数。

---

## 第 10 章 枚举、模式匹配与错误处理

### 枚举与 union

Bolide 支持代数数据类型风格的 `enum` / `union`：

```bolide
enum OptionInt {
    Some(int),
    None,
}
```

模式匹配：

```bolide
fn describe(x: OptionInt) -> str {
    match x {
        OptionInt.Some(v) => {
            return "value=" + str(v);
        },
        OptionInt.None() => {
            return "none";
        },
    }
}
```

### throw / try / catch / finally

异常用于非预期、需要跨层中止的错误。`throw` 只能抛出 `Error` 或 `Error` 子类，`catch` 也只能按 `Error` 体系捕获。

```bolide
try {
    throw Error("boom");
} catch (e: Error) {
    print("caught: " + e.message);
} finally {
    print("cleanup");
}
```

`finally` 无论是否抛错都会执行，适合释放资源、关闭连接、记录日志。

可选的 `throws` 注解用于说明函数可能抛出的异常：

```bolide
fn load_config(path: str) throws Error -> str {
    throw Error("missing config: " + path);
}
```

### 自定义错误类

```bolide
class AppError: Error {}

try {
    throw AppError("not found");
} catch (e: AppError) {
    print(e.message);
}
```

异常会跨函数传播到最近的匹配 `catch`：

```bolide
class ParseError: Error {}

fn parse_config() {
    throw ParseError("bad syntax");
}

try {
    parse_config();
} catch (e: ParseError) {
    print("parse failed: " + e.message);
} catch (e: Error) {
    print("other error: " + e.message);
}
```

### Option / Result 与 ?

值可能缺失时用 `Option<T>`，可恢复错误用 `Result<T, E>`：

```bolide
fn parse_id(raw: str) -> Result<int, Error> {
    if raw.len() == 0 {
        return Result.Err(Error("empty id"));
    }
    return Result.Ok(int(raw));
}

fn load_id(raw: str) -> Result<int, Error> {
    let id: int = parse_id(raw)?;
    return Result.Ok(id + 1);
}
```

`expr?` 会解包 `Result.Ok(v)` / `Option.Some(v)`；遇到 `Result.Err(e)` / `Option.None()` 时从当前函数早返回。

### Result 与异常互转

`expr!` 把 `Result.Err(e)` 升级为异常，适合调用点确认失败不可继续的场景：

```bolide
fn init() {
    let id: int = parse_id("42")!;
    print(id);
}
```

`try { ... }` 也可以作为表达式，把块内异常捕获成 `Result<T, Error>`：

```bolide
let result: Result<int, Error> = try {
    let id: int = parse_id("42")!;
    id + 10;
};
```

实践建议：

- 缺失值用 `Option<T>`，可恢复错误用 `Result<T, E>`。
- 跨层异常使用明确的错误类。
- 只有非预期错误才用异常，不把 `catch` 当普通业务分支。
- `finally` 只做清理，不写复杂业务逻辑。

---

## 第 11 章 模块系统与包管理器

导入本地文件：

```bolide
import "math_utils.bl";
```

导入模块路径：

```bolide
import math.utils;
```

别名：

```bolide
import "long/path/to/mod.bl" as mod;
```

包结构：

```text
my_app/
  bolide.toml
  src/
    main.bl
```

创建项目：

```bash
bolide new my_app
```

添加依赖：

```bash
bolide add name@1.0.0
bolide add https://github.com/org/pkg.git --tag v1.0.0
bolide add ../local_pkg --path
```

安装依赖：

```bash
bolide install
```

工程建议：

- 一个文件只放一类概念。
- 公共函数放在 `src/lib.bl` 或 `src/*.bl`。
- 示例程序放 `examples/`。
- 回归测试放 `tests/`。

---

## 第 12 章 类型系统与类型推断

Bolide 是静态类型语言，但很多地方可以推断类型。

```bolide
let x = 1;        // int
let s = "hello";  // str
let xs = [1, 2];  // list<int>
```

复杂边界建议显式标注：

```bolide
let callbacks: list<func(int) -> int> = [add1, double];
let table: dict<str, list<int>> = {"a": [1, 2]};
```

函数签名建议明确写出：

```bolide
fn score(name: str, values: list<int>) -> int {
    let total = 0;
    for v in values {
        total += v;
    }
    return total;
}
```

### dynamic

`dynamic` 适合边界数据、快速原型和异构值：

```bolide
let x: dynamic = 42;
print(x);
```

不要把 `dynamic` 当作默认类型。核心业务数据应尽量使用静态类型。

### 函数签名类型

```bolide
let f: func(int, int) -> int = add;
```

函数值流转时，显式签名能让错误更早暴露，也能帮助代码阅读。

---

## 第 13 章 内存管理：ARC、生命周期、weak、unowned

Bolide 使用 ARC 引用计数管理对象生命周期。普通对象在最后一个强引用离开作用域后释放。

多数代码不需要手动管理内存：

```bolide
class Node {
    name: str;
}

let n = Node("root");
print(n.name);
```

### owned

`owned` 表示把值的所有权交给函数：

```bolide
fn consume(owned xs: list<int>) {
    print(xs);
}
```

调用后原变量不再拥有该值。

### ref

`ref` 允许函数修改调用方变量：

```bolide
fn inc(ref n: int) {
    n += 1;
}

let x = 1;
inc(x);
print(x); // 2
```

### 生命周期 from

返回值依赖参数生命周期时，可用 `from` 注解表达：

```bolide
fn first(ref xs: list<str>) -> str from xs {
    return xs[0];
}
```

这类注解帮助编译器防止悬垂引用。

### weak 与 unowned

`weak<T>` 不延长对象生命周期，访问时会检查对象是否还活着。

`unowned<T>` 适合你确定对象一定还活着的场景；Bolide 仍会做存活检查，避免未定义行为。

实践建议：

- 默认使用普通强引用。
- 只有为避免引用环时使用 `weak`。
- `unowned` 只用于明确的父子生命周期关系。
- 复杂对象图先画所有权图，再编码。

---

## 第 14 章 并发、线程、通道与 async/await

### spawn

```bolide
fn work(x: int) -> int {
    return x * 2;
}

let task: Task<int> = spawn work(21);
let result: int = await task;
print(result);
```

`spawn` 返回热任务 `Task<T>`，任务已提交执行；等待结果统一使用 `await`。

需要明确使用独立系统线程时，写 `spawn thread`：

```bolide
let blocking: Task<int> = spawn thread work(21);
print(await blocking);
```

### async 函数

```bolide
async fn fetch(id: int) -> int {
    return id * 10;
}

let f: Future<int> = fetch(3);
print(await f);
```

`async fn` 调用返回冷 `Future<T>`，不会自动并行执行；`await` 时才等待它完成。

### spawn all

```bolide
fn a() -> int { return 10; }
fn b() -> int { return 20; }

let results: (int, int) = spawn all {
    a(),
    b()
};

print(results[0]);
print(results[1]);
```

`spawn all` 会同时启动多个任务，并在表达式结束时等待全部结果。

### spawn select

```bolide
spawn select {
    value = work(1) => {
        print(value);
    }
    other = work(2) => {
        print(other);
    }
}
```

`spawn select` 同时启动多个分支，只执行最先完成的分支处理块。

### await scope

```bolide
async fn background(id: int) -> int {
    return id * 10;
}

await scope {
    let f1: Future<int> = background(1);
    let f2: Future<int> = background(2);
    // 不显式 await 时，scope 结束也会等待已登记的 Future
}
```

### 线程池

```bolide
pool(4) {
    let t1: Task<int> = spawn work(1);
    let t2: Task<int> = spawn work(2);
    print(await t1);
    print(await t2);
}
```

`pool(n)` 限制块内普通 `spawn` 的并发度，离开块时会同步未完成任务。

### 通道

```bolide
let ch: channel<int> = channel();

fn producer(c: channel<int>) -> int {
    c.send(42);
    return 0;
}

let t: Task<int> = spawn producer(ch);
let value: int = ch.recv();
print(value);
await t;
```

### select

```bolide
select {
    x = ch1.recv() => {
        print(x);
    }
    y = ch2.recv() => {
        print(y);
    }
    timeout(1000) => {
        print("timeout");
    }
    default => {
        print("nothing ready");
    }
}
```

并发实践：

- 共享可变状态越少越好。
- 优先用通道传递消息。
- `spawn` 传入对象时注意所有权语义。
- 任务边界要明确处理错误。

---

## 第 15 章 FFI 与 AOT 发布

Bolide 可以调用 C，也可以编译成 C 可链接的静态库。

### 调用 C 函数

```bolide
extern "lib:c" {
    fn puts(s: *c_char) -> c_int;
}
```

JIT 开发期通常使用动态库：

```bolide
extern "dyn:c" {
    fn puts(s: *c_char) -> c_int;
    // C 函数指针类型：func(...)（fn 只用于声明/字面量）
    // fn qsort(..., cmp: func(*c_void, *c_void) -> c_int);
}

extern "dyn:m" {
    fn sqrt(x: f64) -> f64;
}
```

AOT 链接期可使用 `lib:name`。固定宽度优先 `i32`/`f64`；平台相关写 `c_int`/`c_size_t`。不透明指针写 `*c_void`。

### export fn

把 Bolide 函数导出给 C：

```bolide
export fn add(a: int, b: int) -> int {
    return a + b;
}
```

编译静态库并生成头文件：

```bash
bolide compile mathlib.bl --lib --header
```

产物：

- Windows：`mathlib.lib`、`mathlib.h`
- Linux/macOS：`libmathlib.a`、`mathlib.h`

C 端链接时需要同时链接 Bolide runtime。

### AOT 发布

```bash
bolide compile src/main.bl -o app
```

发布前检查：

- 是否依赖本地相对路径文件。
- 是否需要动态库。
- 是否需要 runtime 静态库参与链接。
- 是否在目标平台验证过。

---

## 第 16 章 Web、模板、数据库与 GUI 标准库

### Web

Bolide 的 Web 标准库支持路由、请求、响应、静态文件和会话。

典型结构：

```bolide
import "std/web/web.bl" as web;

fn index(req: web.Request) -> web.Response {
    return web.text("hello");
}

let app = web.App();
app.get("/", index);
app.run("127.0.0.1", 8080);
```

AOT 编译后可以作为单文件服务发布。

### 模板

模板适合生成 HTML：

```bolide
let html = render("hello {{name}}", {"name": "Bolide"});
print(html);
```

### 数据库

数据库标准库提供基本 CRUD、查询和错误信息：

```bolide
let db = open_db("app.db");
db.exec("create table if not exists items(name text)");
db.exec("insert into items(name) values('Bolide')");
print(db.all("select * from items"));
```

### GUI

GUI 标准库以声明式回调绘制界面：

```bolide
import "std/gui/gui.bl" as gui;

fn root(ui: gui.Ui) {
    ui.label("Hello Bolide");
}

gui.run("App", 420, 280, root);
```

GUI 开发建议先用 JIT 快速迭代，发布时用 AOT。

---

## 第 17 章 报错诊断、调试与测试

Bolide 0.13.1 起，CLI 提供源码级报错诊断。`run`、`compile`、`compile --lib` 和 REPL 都会显示：

- 错误阶段：`bolide::parse` 或 `bolide::compile`
- 文件名
- 行列号
- 源码片段
- caret 标注
- 针对性 help

示例：

```bolide
let x = missing_name + 1;
print(x);
```

输出：

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

常见错误：

| 错误 | 原因 | 处理 |
| --- | --- | --- |
| Undefined variable | 名字未定义或拼写错误 | 检查作用域、导入和拼写 |
| Undefined function | 函数不存在或未导入 | 定义函数或导入模块 |
| Unknown method | 接收者类型没有该方法 | 检查类型和方法名 |
| missing required argument | 调用缺少必填参数 | 补充位置参数或具名参数 |
| Failed to parse module | import 的文件语法错误 | 直接运行被导入文件查看位置 |

### 测试

一个测试文件可以用打印 `PASS` / `FAIL` 的方式表达：

```bolide
fn expect(label: str, ok: bool) {
    if ok {
        print("PASS " + label);
    } else {
        print("FAIL " + label);
    }
}

expect("add", 1 + 1 == 2);
```

运行：

```bash
bolide run tests/test_add.bl
```

建议：

- 语义边界写小测试。
- 对函数值、闭包、所有权、异常、并发写回归测试。
- JIT 和 AOT 都要覆盖核心路径。

---

## 第 18 章 性能优化与工程实践

性能优化先测量，再修改。

### 优先级

1. 减少不必要的分配。
2. 避免在热路径使用 `dynamic`。
3. 大集合处理时减少中间列表。
4. 频繁调用的函数写清楚类型签名。
5. 发布时使用 AOT。

### 字符串

大量拼接时，尽量把片段组织好，避免循环里反复创建大字符串。

```bolide
let out = "";
for item in items {
    out = out + render_item(item);
}
```

如果性能敏感，应改成批量渲染或模板。

### 集合

`map` / `filter` 清晰，但在超热路径中可能产生中间集合。必要时使用显式循环。

### 并发

线程不是越多越快。对于 CPU 密集任务，线程池大小通常接近核心数；对于 IO 密集任务，可以更高，但要用压测确认。

### 工程实践

- 公共边界显式写类型。
- 错误类型用类或 enum 建模。
- 模块之间只暴露必要函数。
- 测试覆盖语义边界，不只覆盖 happy path。
- 发布前用 `bolide compile` 验证 AOT 路径。

---

## 第 19 章 综合项目：命令行任务管理器

本章把前面的知识组合成一个小项目。

目标：

- 添加任务。
- 列出任务。
- 标记完成。

### 数据模型

```bolide
class TodoItem {
    id: int;
    title: str;
    done: bool = false;
}
```

### 基本操作

```bolide
fn add_task(tasks: list<TodoItem>, title: str) -> list<TodoItem> {
    let id = tasks.len() + 1;
    tasks.push(TodoItem(id, title, false));
    return tasks;
}

fn list_tasks(tasks: list<TodoItem>) {
    for item in tasks {
        let mark = " ";
        if item.done {
            mark = "x";
        }
        print(str(item.id) + ". [" + mark + "] " + item.title);
    }
}

fn finish(tasks: list<TodoItem>, id: int) -> list<TodoItem> {
    for item in tasks {
        if item.id == id {
            item.done = true;
        }
    }
    return tasks;
}
```

### 主程序

```bolide
let tasks: list<TodoItem> = [];

tasks = add_task(tasks, "learn Bolide");
tasks = add_task(tasks, "write a program");
tasks = finish(tasks, 1);

list_tasks(tasks);
```

运行：

```bash
bolide run todo.bl
```

编译：

```bash
bolide compile todo.bl -o todo
```

扩展方向：

- 用文件保存任务。
- 用数据库标准库存储任务。
- 用 Web 标准库做浏览器界面。
- 用 GUI 标准库做桌面界面。

---

## 第 20 章 附录：常用语法速查

### 变量

```bolide
let x: int = 1;
let y = 2;
x += 1;
```

### 函数

```bolide
fn add(a: int, b: int) -> int {
    return a + b;
}
```

### 默认参数与具名参数

```bolide
fn greet(name: str = "world") {
    print(name);
}

greet(name="Bolide");
```

### 列表

```bolide
let xs: list<int> = [1, 2, 3];
xs.push(4);
print(xs[0]);
```

### 字典

```bolide
let m: dict<str, int> = {"a": 1};
m["b"] = 2;
```

### 类

```bolide
class Point {
    x: int;
    y: int;
}

let p = Point(1, 2);
```

### 闭包

```bolide
let f: func(int) -> int = fn(x: int) -> int {
    return x + 1;
};
```

### 异常

```bolide
try {
    throw Error("error");
} catch (e: Error) {
    print(e.message);
} finally {
    print("cleanup");
}
```

### Result / Option

```bolide
fn read_id(raw: str) -> Result<int, Error> {
    let id: int = int(raw);
    return Result.Ok(id);
}

fn checked_id(raw: str) -> Result<int, Error> {
    let id: int = read_id(raw)?;
    return Result.Ok(id + 1);
}
```

### 并发与通道

```bolide
fn work(x: int) -> int {
    return x * 2;
}

async fn fetch(id: int) -> int {
    return id * 10;
}

let task: Task<int> = spawn work(21);
let value: int = await task;

let f: Future<int> = fetch(1);
print(await f);

let ch: channel<int> = channel();
ch.send(42);
print(ch.recv());
```

### AOT

```bash
bolide compile main.bl -o main
```

### 静态库

```bolide
export fn add(a: int, b: int) -> int {
    return a + b;
}
```

```bash
bolide compile lib.bl --lib --header
```

---

## 结语

学会 Bolide 的关键不是记住所有语法，而是掌握几条主线：

- 用静态类型描述核心数据。
- 用函数和闭包表达可组合逻辑。
- 用类管理有状态对象。
- 用模块和包管理工程边界。
- 用 ARC、所有权和生命周期避免资源错误。
- 用 JIT 快速开发，用 AOT 稳定发布。

读完这本书后，你已经具备从零写出 Bolide 程序、组织中型项目、诊断错误、编写测试、接入 C、发布原生可执行文件的完整路径。
