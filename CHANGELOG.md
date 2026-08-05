# Changelog

## 0.14.1

### 语言与编译器

- **宏系统**：声明式宏、属性宏、`@derive` 扩展；展开在 monomorph 之前。
- **装饰器与 `with`**：Python 风格装饰器链与上下文管理器（enter/exit）。
- **生成器**：`yield` 懒状态机（`next() -> Option`），支持 `elif` / `for` / `break` / `continue` / 类方法生成器。
- **运算符重载**：二元算术/比较/位运算、一元 `-`/`not`/`!`、右操作数反射（`__radd__` 等）。
- **Trait 与多继承安全子集**：`trait` / `impl Trait for Class`、默认方法、泛型约束 `T: Trait`、`dyn Trait` 运行时多态（class-tag 分派）；主父类 + 无字段 mixin 多继承。
- **协议自动满足**：类上有 `__add__`/`next` 等即视为 `Add`/`Iterator` 等 bound。
- **解析修复**：`dyn` 词边界，避免 `dynamic` 被拆成 `dyn amic`；`!` 一元与 `not` 并存。
- **方法分派修复**：类方法 tag 分派仅限静态类型及其子类，避免跨类重载 ABI 错位；重载按实参类型匹配，禁止类型失败后仅按参数个数回退。
- **import**：`std/fs` 等短路径自动解析到 `std/fs/fs.bl`（兼容旧长路径）。

### 标准库

- 新模块：`option`、`result`、`traits`、`macros`、`prelude`；`std/README.md` 索引。
- 加厚：`time`/`iter`/`text`/`assert`/`collections`/`env`/`encoding`/`sort`/`uuid`/`math`/`random`/`log`/`io`/`path`/`process`。
- 优化：`cli`（`parse_or_exit`、粘连短选项、`help_flag`）、`gui`（`run_default`/`progress_pct` 等）、`web`/`http`（便捷响应工厂、`listen`/`cors_open`、客户端 timeout 等）。
- 文档：`docs/standard-library.md` 与 README 标准库章节更新为短路径。

### 测试

- 新增宏/装饰器/生成器/trait/运算符/`dynamic` vs `dyn`/标准库核心与 cli-web 等回归用例。

### 性能

- 无 `throw`/`try`/`?` 的程序跳过调用点异常 pending 检查（递归数值代码收益大）。
- `list.len()` 内联；新增 `list.reserve` / `list.resize`（批量填充）。
- 常用 `bolide_math_*` 降为 Cranelift 指令（sqrt/fabs/floor/min/max 等）。
- 自动内联小叶子函数（标量参数/返回值、单出口、含 if/while）。
- `list[i]` 保持边界检查（越界读 0、越界写忽略）；不以牺牲安全换速度。
- bench 套件与 `bench/README.md` 更新参考比值（几何平均约 1.3x vs C `-O3`）。

### 示例

- `examples/neon_lang.bl`、`examples/starfield.bl` 特性演示。

## 0.13.7

- 修复 `list + list` 在 JIT/AOT 两个后端中的 lowering 漏洞：此前会错误落到整数 `iadd`，导致 `rows = rows + [row]` 这类嵌套列表构造在运行期静默退出。
- 为列表拼接补齐类型推导和回归用例，覆盖 `list<list<int>>` 这种嵌套场景。

## 0.13.6

- 新增 `value` 值类型语法，支持轻量聚合类型的按值构造、字段访问、局部变量、函数参数和返回值，并打通 JIT/AOT 两个后端。
- 新增 `inline fn` 语法与 AST 级内联展开，适合数值计算和其他热路径上的短小辅助函数。
- 修复 `value` 与 `inline` 组合时的类型推导和名称捕获问题，补齐一批对应测试与示例，包括 `raytracer_vt.bl`。
- AOT 现已支持值类型调用、返回和局部/全局存储链路，`bolide compile examples/raytracer_vt.bl -o examples/raytracer_vt.exe` 可成功生成原生可执行文件。
- 编译器错误信息改进：导入模块内函数体的编译错误现在会显示实际源文件名、函数名和行号
  （"in 'file.bl' (function '@module_fn' at line N)"），不再指向 import 行。
- AST 增加源位置信息：`FuncDef` 新增 `def_span_start` 字段记录函数定义在源文件中的字节偏移。

## 0.13.5
- `std/json` 新增完整 JSON 解析器与序列化器（运行时实现，JIT/AOT 双后端）：
  `parse(text) -> dynamic`、`stringify(value)` / `stringify_pretty(value, indent)`、
  `get_path(value, "a.b.0")`，以及容器访问 `get`/`at`/`keys`/`length`、类型自省
  `type_of`/`is_*`、标量取值 `as_int`/`as_float`/`as_str`/`as_bool`/`as_array`。
  对象解析为 `dict<str, dynamic>`、数组解析为 `list<dynamic>`，整数走 `int`、含小数或
  指数走 `float`；支持 `\uXXXX`（含代理对）转义，解析失败返回 `null` 并可由
  `parse_error()` 读取原因。原有 JSON 生成辅助（`escape`/`quote`/`object`/`dict_*` 等）保持不变。

## 0.13.4

- 新增一批实用标准库：`std/assert`、`std/text`、`std/csv`、`std/encoding`、`std/http`、`std/uuid`、`std/table`、`std/cache`。
- 新增 `std/regex`，绑定 Rust `regex`，支持匹配、提取、捕获、替换、切分和正则转义，并补齐 JIT/AOT 符号注册。
- 改进 HTTP 客户端错误设计：DNS、连接、TLS、超时、非法 URL 等请求层错误不再静默吞掉，可通过 `ClientResponse.error()` / `http.Response.error` 获取。
- 统一内置 bool 返回表现：字符串/列表/字典的 `contains`、`is_empty`、`starts_with`、`ends_with`，以及 `list.set`、`channel.send` 等现在按 `bool` 暴露，`str(...)` 输出 `true/false`。
- 新增标准库教程 `docs/standard-library.md`，覆盖当前 `std/` 模块的导入、示例、API 速查和常见组合。

## 0.13.3

- 新增 `std/cli`，支持命令行 flag、option、必填参数、位置参数、错误收集和 help 文本生成。
- 新增爬虫相关标准库：`std/url`、`std/html`、`std/crawler`，覆盖 URL 解析与相对链接归一化、轻量 HTML 抽取、抓取队列与去重辅助。
- 扩展标准库和运行时绑定：`std/env`、`std/time`、`std/random`、`std/process`、`std/math`，并补齐 JIT/AOT 运行时符号注册。
- 改进解析器关键字处理，允许大多数关键字在普通标识符位置作为上下文关键字使用。
- 改进函数重载和模块全局变量访问路径，修复按参数类型解析重载函数、模块函数和模块变量的若干问题。

## 0.12.2

- 修复类方法动态分派、`super` 调用，以及相关语义回归测试中的几处已知问题。
- 补充并通过了一组新的语义测试，覆盖作用域遮蔽、短路、闭包捕获、默认参数、`match`、`try/finally`、数值转换与容器行为。

## Unreleased（0.8.2 之后）

### 语法与解析器

- **关键字词边界守卫**：`not`/`and`/`or`/`in`/`as`/`from`/`await`/`spawn`
  及内建类型名不再误匹配标识符前缀（`notable`、`a orbit`、`class interval`
  等现在都能正确解析）
- **字符串转义**：支持 `\"` `\\` `\n` `\t` `\r` `\0`，字符串中可以包含引号
- **`break` / `continue`**：JIT 与 AOT 全链路支持，循环采用 latch 块结构，
  `continue` 保证执行步进；提前跳出路径正确释放作用域内 RC 变量
- **复合赋值**：`+=` `-=` `*=` `/=` `%=`（parser 层脱糖）
- **尾随逗号**：列表/字典/元组字面量、调用实参、参数表
- **数字下划线分隔**：`1_000_000`
- **`await` 优先级收紧**：绑定到后缀层级，`await a + b` 解析为 `(await a) + b`
- **链式比较改为语法错误**：`a < b < c` 不再静默产生错误语义

### 类型推断

- `await` / `await all` 支持省略类型标注（直接调用、future 变量、spawn
  三种形式；`await all` 推断为元组类型）
- 全局变量静态推断与函数内推断对齐：列表/字典字面量扫描元素推断统一类型，
  函数调用查返回类型，async 函数调用推断为 `future`

### 内存安全

- **weak/unowned 安全化**：对象头加入弱引用计数，强引用归零后进入僵尸态
  （分配保留至弱引用归零）；访问已释放对象触发确定性运行时错误并中止，
  替代之前的 use-after-free 未定义行为
- **`from` 借用检查收紧**（编译期拒绝）：借用存活期间禁止对来源变量重新
  赋值/重声明；借用值禁止逃逸（容器/字段/通道/spawn/存储型方法/非 from
  函数返回）
- **类对象释放修复**：释放时先检查指针非 null（修复全局类变量首次初始化
  段错误），且仅在最后一个强引用（refcount==1）时清理字段（修复共享对象
  字段重复释放）；自引用类字段（链表等）不再崩溃

### 并发安全

- **引用计数全部原子化**：统一到共享 `RcHeader`（`AtomicU32`，与
  `std::sync::Arc` 相同内存序），跨线程 retain/release 无数据竞争；
  weak upgrade 改为 CAS 消除 TOCTOU

### 命名空间与模块

- **内置函数隔离**：运行时内置符号统一加 `@_` 前缀（非法标识符字符），
  用户函数可使用任意合法名字（如 `print_bigint`）不再与内置冲突/递归；
  运行时内部 ABI（`list_push` 等）不再暴露给用户代码
- **import 路径解析**：确定性顺序（绝对路径 → 源文件目录 → BOLIDE_HOME →
  可执行文件目录），不依赖进程工作目录
- **`import ... as` 别名**：实现别名到真实模块名的 AST 重写，同一文件可
  多次导入使用不同别名

### 作用域与全局变量

- 函数内 `let` 声明局部变量并**遮蔽**同名全局（之前会直接写全局，类型
  不一致时产生 verifier 错误）
- 全局变量补齐能力：可作 `ref` 实参（原地修改+旧值释放）、可作通道
  （`<-` 收发、`select`）、float 等非指针类型修复（按实际类型 load）、
  生命周期借用追踪（全局悬空检测生效）
- `async select` 绑定变量按分支协程返回类型推断（之前硬编码 int 且
  类型穿透到同名全局）

### 测试

- JIT：tests/ 106/106、examples 18/18 全通过（基线为 86/104 + 多个示例
  段错误）；4 个预期编译错误的 lifetime 示例正确报错
- AOT 对齐进行中：61/106，差距清单见 todo.md

### 文档

- README 修正与实现不符的承诺（weak 自动 nil、`let (a,b) = await all`），
  新增：转义/复合赋值/break-continue 示例、模块路径解析规则、内置函数
  隔离说明、变量作用域语义、内存管理真实语义（trap 式检查）、from 借用
  检查清单
