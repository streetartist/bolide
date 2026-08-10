# Bolide 宏编程设计

> 目标：**强大够用，但默认好学**。  
> 原则：同一门语言、同一套语法、分层能力、编译期可预测、错误可读。

版本：设计已拍板（好用优先 + **宏调用强制 `!`**）。配合 Bolide 0.13.x 语法与编译管线（pest → AST → monomorph → JIT/AOT）。

---

## 0. 一句话

Bolide 宏 = **声明式模式宏（学 10 分钟）** + **`quote` 卫生拼接（日常 DSL）** + **`comptime` 同语言编译期计算（真正强大）** + **属性宏 `@...`（结构化元数据）**。

用户不需要学第二门“宏语言”，也不需要写独立进程式 proc-macro crate。

### 已拍板（怎么好用怎么来）

| 议题 | 决议 | 理由 |
|------|------|------|
| 调用语法 | **强制 `name!(...)`**，无 `!` 永不触发宏 | 与函数一眼区分；报错、补全、重载都干净 |
| 命名空间 | 宏与函数**同一命名空间**，**允许同名** | `assert` 函数 + `assert!` 宏可并存；import 不用特殊语法 |
| 卫生泄漏 | **`expose $name;`**（不用 `:unhygienic` 后缀） | 读起来像语句，意图明确 |
| `@derive` 产物 | 生成 **class / value 方法** | 贴合现有 OOP 与调用习惯 `p.debug()` |
| 属性写法 | `@name` / `@name(...)`，未知属性**硬错误** | 不静默吞掉拼写错误 |
| 导入 | 普通 `import`，用 `m.assert!(...)` 或导入后 `assert!(...)` | 无 `import macro` 第二套规则 |
| 内置 desugar 顺序 | **宏展开 → 内置 desugar → 类型/单态 → codegen** | 宏可生成 if-let / `?` 等，享受后续糖 |
| 无 `!` 的糖 | **永不提供** `assert(x)` 当宏 | 用户已确认强制 `!` |

---

## 1. 设计动机与取舍

### 1.1 为什么要宏

| 场景 | 没有宏时的痛点 | 宏能提供的 |
|------|----------------|------------|
| `assert` / `todo` / `unreachable` | 手写 `if` + 错误信息 | 源码位置、表达式原文 |
| `@derive(Debug, Eq)` | 样板代码爆炸 | 结构化生成 |
| HTML / SQL / 路由 DSL | 字符串拼接或冗长 API | 领域语法 |
| 测试 / 基准脚手架 | 复制粘贴 | 统一包装 |
| 编译期配置、表驱动代码 | 运行时算或外部脚本 | 可复现的生成 |

### 1.2 明确不做什么

| 不做 | 原因 |
|------|------|
| 完整 C 预处理器 `#define` | 无卫生、难调试、破坏模块边界 |
| 强制全员学 TokenStream / 独立宏 crate | 学习曲线陡，与 Bolide“脚本感”冲突 |
| 运行期“宏”（eval 源码） | 与 AOT、静态诊断冲突 |
| 无限制改写任意类型检查后 IR | 难诊断、难稳定 ABI |

### 1.3 参考与差异

| 语言 | 借鉴 | 不照搬 |
|------|------|--------|
| Rust `macro_rules!` | 模式匹配、片段分类符 | 避免过度复杂的 TT muncher 文化 |
| Elixir `quote`/`unquote` | 卫生、可读 AST 拼接 | 不用 BEAM 那套 |
| Zig `comptime` | **同语言**编译期执行 | 不把整个类型系统拖成 comptime 图灵沼泽 |
| Nim macros | AST 操作力 | 降低裸 AST 节点心智负担 |
| Crystal | 宏内插自然 | 避免字符串宏为主 |

Bolide 定位：**默认像 Elixir/Zig 一样好写；需要时有 Nim/Rust 级控制力。**

---

## 2. 心智模型（三层）

```text
┌─────────────────────────────────────────────────────────┐
│  L3  属性宏 @name(...)          结构化、可组合的元数据     │
├─────────────────────────────────────────────────────────┤
│  L2  comptime fn / block        普通 Bolide 在编译期跑    │
├─────────────────────────────────────────────────────────┤
│  L1  quote { ... } + $splice    卫生模板，拼出 AST         │
├─────────────────────────────────────────────────────────┤
│  L0  macro name(...) { ... }    声明式模式宏（入门主路径） │
└─────────────────────────────────────────────────────────┘
         ↓ 全部在「类型检查之前」展开为普通 Bolide AST
pest ──► parse ──► expand macros ──► 现有 convert/语义/codegen
```

**铁律：宏展开结果必须是合法 Bolide AST。**  
宏不发明新运行时语义；只是“会写代码的编译期函数”。

---

## 3. L0 — 声明式模式宏（默认教这个）

### 3.1 语法

```bolide
// 单模式：pattern 后直接 quote { ... } 或 { ... }
// （不要写成 macro name(...) { quote { ... } } 双层花括号）
macro assert($cond:expr) quote {
    if not ($cond) {
        throw Error(f"assertion failed: {$cond:src}");
    }
}

// 多模式（按定义顺序匹配，第一条命中）
macro debug {
    () => {
        print("[debug]");
    },
    ($msg:expr) => {
        print(f"[debug] {$msg}");
    },
    ($name:ident = $val:expr) => {
        print(f"[debug] {$name:src} = {$val}");
        let $name = $val;
    },
}

// 调用：必须带 !（见 §7）
assert!(x > 0);
debug!();
debug!("here");
debug!(n = 1 + 2);
```

### 3.2 片段分类符（Fragment Specifiers）

刻意**少而稳**，先 8 个，覆盖 95% 场景：

| 分类符 | 匹配 | 说明 |
|--------|------|------|
| `ident` | 标识符 | 变量名、类型名、字段名 |
| `expr` | 表达式 | 不吃掉外层语句分隔 |
| `stmt` | 单条语句 | 含末尾 `;` 的规范形式 |
| `block` | `{ ... }` | 语句块 |
| `type` | 类型表达式 | `list<int>`、`func(int)->int` |
| `path` | `a.b.c` | 模块/成员路径 |
| `lit` | 字面量 | 数、字符串、bool |
| `tt` | 单个 token 树 | 进阶：括号平衡的一块 |

**暂不引入** `item`/`pat`/`lifetime` 等 Rust 全套；需要时用 `tt` + `comptime` 解析。

### 3.3 重复与分隔

```bolide
// $(...)*  零或多次
// $(...),*  逗号分隔重复（允许尾逗号）
// $(...)+  一或多次

macro max_all($x:expr $(, $rest:expr)*) {
    quote {
        {
            var __m = $x;
            $(
                if $rest > __m { __m = $rest; }
            )*
            __m
        }
    }
}

print(max_all!(1, 5, 3, 9));  // 9
```

重复标记与 Elixir/Rust 接近，但**禁止**任意 token 作分隔符花样；只允许 `,` `;` 和空格连接。降低“宏宏相噬”的复杂度。

### 3.4 卫生（Hygiene）— 默认开启

```bolide
macro swap($a:ident, $b:ident) {
    quote {
        let __tmp = $a;   // __tmp 默认不泄漏到调用处
        $a = $b;
        $b = __tmp;
    }
}

var x = 1;
var y = 2;
swap(x, y);
// print(__tmp);  // 编译错误：未定义
```

规则：

1. **宏内引入的绑定**默认带宏展开色（expansion color），对调用处不可见。  
2. **`$capture` 拼进来的标识符**保持调用处颜色（调用方传入的名字就是调用方的）。  
3. 需要故意泄漏时用 **`expose`**（唯一写法）：

```bolide
macro let_mut($name:ident, $val:expr) {
    quote {
        var __v = $val;
        expose $name;          // 把 $name 以调用处颜色引入
        let $name = __v;
    }
}
```

**默认卫生 = 少踩坑；`expose` = 故意把名字交给调用方（builder DSL 等）。**

### 3.5 元数据插值

在 `quote` / f-string 风格中支持宏专用格式：

| 写法 | 含义 |
|------|------|
| `$x` | 按 AST 节点嵌入 |
| `$x:src` | 嵌入该节点的源码文本（`str` 字面量） |
| `$x:stringify` | 同 `:src` 的别名，偏文档友好 |
| `$x:line` / `$x:file` | 调用点行列 / 文件（编译期常量） |

```bolide
macro todo($msg:lit) {
    quote {
        throw Error(f"TODO at {$msg:file}:{$msg:line}: {$msg}");
    }
}

todo!("wire up auth");
```

---

## 4. L1 — `quote` / `$`：AST 模板

所有宏体最终产出 `Ast`（语句列表或表达式）。`quote { ... }` 是**写模板**的主方式，不是字符串。

```bolide
// quote 的结果类型是 Ast（编译期类型）
let body: Ast = quote {
    print("hello");
    return 1;
};
```

### 4.1 反引用 / 拼接

```bolide
comptime fn wrap_in_if(cond: Ast, then_body: Ast) -> Ast {
    return quote {
        if $cond {
            $then_body
        }
    };
}
```

- `$cond`：嵌入表达式节点  
- `$then_body`：若节点是 block/stmt 列表，按语法位置嵌入  
- `$$`：字面 `$`（极少需要）

### 4.2 为什么不是字符串拼接

```bolide
// ❌ 禁止作为主路径（可保留 comptime 调试用 raw，但不进稳定 API）
let s = "print(" + name + ");";
parse(s);

// ✅ 稳定路径
quote { print($name); }
```

字符串宏会破坏：
- 卫生  
- IDE 高亮与跳转  
- 错误定位（展开后“怪代码从哪来”）

---

## 5. L2 — `comptime`：同语言编译期（真正强大）

### 5.1 语法

```bolide
// 编译期函数：参数/返回仅限 comptime 可表示的值
comptime fn field_names(def: TypeInfo) -> list<str> {
    return def.fields.map(fn(f) -> str { return f.name; });
}

// 编译期块：可出现在宏体、常量、部分类型位置
let SIZE: int = comptime {
    var n = 0;
    for i in 0..10 { n += i; }
    n
};
```

### 5.2 编译期可操作的值

**第一阶段允许：**

| 类型 | 用途 |
|------|------|
| 基础：`int` `bool` `str` `float` | 配置、循环次数 |
| `list<T>` `dict<str,T>`（T 也是 comptime） | 表驱动生成 |
| `Ast` `TypeInfo` `Span` | 宏核心 |
| 函数指针（comptime 纯函数） | 组合 |

**第一阶段禁止（或仅内建）：**

- 任意 IO（文件读可选 `comptime_read` 白名单）  
- 线程 / 异步 / FFI 调用外部库  
- 持有运行时对象身份

失败时编译错误，不带入运行时。

### 5.3 用 comptime 写“过程宏”

```bolide
// 声明式宏不够时，macro 体可以是 comptime 代码
macro getters {
    ($cls:ident { $( $field:ident : $ty:type ),* }) => comptime {
        var items: list<Ast> = [];
        $(
            items.push(quote {
                fn $field(self) -> $ty {
                    return self.$field;
                }
            });
        )*
        // 拼接多个 item
        return Ast.items(items);
    },
}

// 使用
getters!(Point { x: int, y: int });
```

用户心智：**还是 Bolide**，只是多了 `Ast` API 和 `quote`。

### 5.4 最小 `Ast` API（稳定面）

```bolide
// 构造
Ast.ident("x")
Ast.lit_int(1)
Ast.lit_str("hi")
Ast.call(func, args)           // args: list<Ast>
Ast.block(stmts)               // stmts: list<Ast>
Ast.let_bind(name, expr)
Ast.func(name, params, ret, body)
Ast.class(name, fields, methods)
Ast.parse_expr("1 + 2")        // 仅用于工具场景；主路径仍用 quote
Ast.parse_stmts("...")

// 解构 / 查询
node.kind() -> AstKind         // Ident | Call | Block | ...
node.as_ident() -> str?
node.children() -> list<Ast>
node.span() -> Span

// 列表工具
Ast.items(list<Ast>)           // 展平为语句序列
```

API 保持**小而文档化**；高级遍历用普通 `for` + `match`。

---

## 6. L3 — 属性宏 `@`

### 6.1 语法

```bolide
@derive(Debug, Eq)
class Point {
    x: int;
    y: int;
}

@route("GET", "/users/:id")
fn get_user(id: int) -> Response { ... }

@test
fn test_add() {
    assert!(add(1, 2) == 3);
}
```

### 6.2 属性展开协议

属性宏是接收 **被标注 AST + 参数** 的 comptime 函数：

```bolide
// 内置或用户定义
attr macro derive($item:item, $($trait:ident),+) {
    comptime {
        var out: list<Ast> = [$item];  // 保留原定义
        $(
            out.push(gen_impl($trait, $item));
        )+
        return Ast.items(out);
    }
}
```

约定：

1. 属性**从上到下**应用；每个可改写/包裹/追加。  
2. 未知属性 = 编译错误（不静默忽略）。  
3. 内置属性白名单：`derive` `test` `inline`（可与现有 `inline fn` 统一）`export`（可选统一）。

### 6.3 `@derive` 第一批 trait

**产物形态（已拍板）：一律生成类型上的方法**，调用为 `p.debug()` / `p.eq(q)`，不生成游离顶层函数。

| Trait | 生成方法 |
|-------|----------|
| `Debug` | `fn debug(self) -> str` |
| `Eq` | `fn eq(self, other: Self) -> bool`（字段递归） |
| `Hash` | 若语言后续有哈希协议再补 |
| `Default` | 关联式 `fn default() -> Self`（或静态方法，与 class 模型对齐） |

用户自定义 derive：`comptime fn derive_MyTrait(item: Ast) -> Ast`，协议与内置相同（改写/追加方法后返回 item）。

---

## 7. 调用形态与解析优先级

### 7.1 调用：强制 `!`（硬规则）

```bolide
// ✅ 唯一合法的宏调用
let y = max_all!(1, 2, 3);
assert!(x > 0);
dbg!(compute());

// ❌ 永远不是宏（即便存在 macro assert）
assert(x > 0);     // 按普通函数解析；无此函数则“未定义函数”
max_all(1, 2, 3);  // 同上

// 属性（无 !，用 @）
@test
fn t() { ... }

@derive(Debug, Eq)
class Point { x: int; y: int; }
```

规则写死：

1. **定义**用 `macro name` / `attr macro name` / `export macro name`。  
2. **调用**必须是 `name!(...)`（或 `name! { ... }` 块形，见下）。  
3. **`name(...)` 永不展开宏**——与函数调用同形，走函数解析。  
4. 宏与函数**可以同名**：`fn assert(...)` 与 `macro assert` 并存时，`assert(...)` 调函数，`assert!(...)` 展宏。  
5. 属性用 `@name`，不写 `!`（`@` 本身已标明元层面）。

块形调用（可选糖，好用时保留，仍带 `!`）：

```bolide
// 适合“宏体像代码块”的 DSL
html! {
    div {
        h1 { "Hi" }
    }
}
// 解析为 name! { tt... }，与 name!(...) 等价，只是括号形态不同
```

### 7.2 语句宏 vs 表达式宏

由模式与 `quote` 根节点决定：

- `quote { stmt; stmt; }` → 语句位置展开  
- `quote { expr }` / `quote(expr)` → 表达式位置  

位置不对 → 明确报错：`macro foo expands to statements, but was used as expression`。

---

## 8. 模块、可见性与导入

```bolide
// macros.bl
export macro assert($cond:expr) { ... }

export attr macro test($item:item) { ... }

// main.bl
import "macros.bl" as m;

m.assert!(true);
```

规则（已拍板）：

1. 宏与函数**同一命名空间**，**靠 `!` 区分调用**；不设 `import macro { ... }` 第二套导入。  
2. `import "m.bl" as m;` 后写 `m.assert!(...)`；`import` 若将来支持按名导入，则 `assert!` 与 `assert` 可分别/同时导入（实现期：导入符号时函数与宏可同键共存）。  
3. 宏展开在**调用方模块**卫生环境下进行；路径颜色按定义方/调用方规则。  
4. 递归展开：**深度上限 128** + 同一调用栈环检测，超限硬错误并打印展开栈。

---

## 9. 错误诊断（宏好不好用取决于报错）

### 9.1 必须有的体验

```text
Error: bolide::macro

  × assertion failed pattern: argument is not expr
   ╭─[app.bl:10:12]
10 │ assert!(let x = 1);
   ·            ───────
   ·            ╰── expected expression
   ╰────
  help: assert! expects `assert!(<expr>)`
```

展开后错误（宏写错了）：

```text
Error: bolide::compile

  × Undefined variable: __oops
   ╭─[app.bl:10:5]
10 │ assert!(x > 0);
   ·     ───────────
   ·     ╰── expanded from macro `assert`
   ╭─[std/macros.bl:4:9]  (expanded)
 4 │     if not (__oops) {
   ·             ───┬──
   ·                ╰── not found
```

要求：

1. **调用点**始终在主诊断里  
2. **展开栈**可 `--macro-trace` 展开  
3. `quote` 内语法错误指向宏定义处  

### 9.2 `expand` 工具

```bash
bolide expand app.bl            # 打印宏展开后源码
bolide expand app.bl --macro assert
```

和 JIT/AOT 同源 AST，pretty-print 回 Bolide 子集。

---

## 10. 与现有编译管线的衔接

```text
源码
  │
  ▼
pest 解析（含 macro / quote / comptime / attr 语法）
  │
  ▼
原始 AST（含 MacroDef / MacroCall / Comptime / Attr）
  │
  ▼
【新增】MacroExpander
  · 收集 export macro
  · 迭代展开 MacroCall / Attr（深度限制）
  · 执行 comptime 子集（解释器或 MIR 轻量求值）
  · 输出 纯净 AST（无宏节点）
  │
  ▼
现有 convert 后处理 / 语义检查 / monomorph / JIT|AOT
```

注意：

- **f-string、解构、`impl From`、if-let** 等已有 desugar 保持；宏展开**先于或并入**同一 expand 阶段，顺序写死并文档化：  
  `宏展开 → 内置 desugar → 类型/单态 → 代码生成`  
- 宏**看不到** monomorph 之后的实例（避免依赖后端）；泛型代码可对“未单态 AST”生成泛型形态。  
- `inline fn` 继续是后端优化；`macro` 是语法层生成。两者不合并，但文档里对比清楚。

---

## 11. 标准库宏（第一批“教会用户”的例子）

```bolide
// std/macros.bl（示意）

export macro assert($cond:expr) { ... }
export macro assert_eq($a:expr, $b:expr) { ... }
export macro todo($msg:lit) { ... }
export macro unreachable() { ... }

export macro dbg($e:expr) {
    quote {
        {
            let __v = $e;
            print(f"[{$e:file}:{$e:line}] {$e:src} = {__v}");
            __v
        }
    }
}

export attr macro test($item:item) { ... }   // 注册到测试 runner
export attr macro derive($item:item, $($t:ident),+) { ... }
```

教程路径：

1. 会用 `assert!` / `dbg!`  
2. 会写两行 `macro`  
3. 会 `quote` + `$`  
4. 会 `@derive`  
5. 才会 `comptime` + `Ast`  

---

## 12. 完整示例

### 12.1 入门：日志宏

```bolide
macro log($level:lit, $msg:expr) {
    quote {
        print(f"[{$level}] {$msg}");
    }
}

log!("info", "server up");
```

### 12.2 中级：简单 HTML DSL

```bolide
macro h {
    ($tag:ident, $body:expr) => quote {
        "<" + stringify!($tag) + ">" + $body + "</" + stringify!($tag) + ">"
    },
    ($tag:ident) => quote {
        "<" + stringify!($tag) + " />"
    },
}

let page = h!(div, h!(h1, "Hi") + h!(p, "Bolide"));
```

### 12.3 高级：字段访问器生成

```bolide
attr macro getters($item:item) {
    comptime {
        // $item 为 class 定义 AST
        let name = $item.class_name();
        var methods: list<Ast> = [];
        for f in $item.fields() {
            let fname = Ast.ident(f.name);
            let fty = f.ty_ast();
            methods.push(quote {
                fn $fname(self) -> $fty {
                    return self.$fname;
                }
            });
        }
        return $item.with_extra_methods(methods);
    }
}

@getters
class User {
    id: int;
    name: str;
}
// 自动 freestanding 或 class 内方法，依协议定
```

### 12.4 与 Result / `?` 协作

```bolide
macro try_or($expr:expr, $fallback:expr) {
    quote {
        match $expr {
            Result.Ok(v) => v,
            Result.Err(_) => $fallback,
        }
    }
}

let n = try_or!(parse_int(s), 0);
```

---

## 13. 学习曲线与“强大”的边界

| 级别 | 你会什么 | 时长 |
|------|----------|------|
| 使用 | `assert!` `dbg!` `@test` `@derive` | 5 分钟 |
| 编写 L0 | `macro` + 模式 + `quote` + `$x:expr` | 30 分钟 |
| 编写 L1 | 多模式、重复 `$(...)*`、`:src`/`:line` | 2 小时 |
| 编写 L2 | `comptime` + `Ast` API + 属性宏 | 半天 |
| 专家 | 卫生细节、递归宏、derive 协议、展开顺序 | 按需 |

**刻意不把专家特性放进入门文档。**

---

## 14. 分阶段落地（建议）

### Phase M0 — 骨架（可合并一个版本）

- 语法：`macro name { arm => quote {..} }`、`name!(...)`  
- 片段：`ident` `expr` `lit` `block`  
- 卫生：基础  
- 内置：`assert!` `dbg!`  
- `bolide expand`  
- 诊断：调用点 + 宏名  

### Phase M1 — 实用

- 重复 `$(...)*` / `$(...),*`  
- `:src` `:line` `:file`  
- `export macro` + import  
- `@test` 属性  
- 多 arm 模式  

### Phase M2 — 结构化

- `@derive(Debug, Eq)`  
- `attr macro`  
- `TypeInfo` 只读反射（class/enum/value 字段）  

### Phase M3 — 完整 comptime

- `comptime fn` / `comptime { }` 常量  
- 完整 `Ast` 构造/查询 API  
- 展开深度与性能预算  
- （可选）`comptime` 读本地文件生成代码  

每阶段都保持：**旧阶段宏源码向前兼容。**

---

## 15. 语法草案（pest 级，示意）

```pest
// 仅示意，非最终
macro_def = {
    export_keyword? ~ "macro" ~ ident ~ (
        macro_single_body
      | "{" ~ macro_arm+ ~ "}"
    )
}
macro_single_body = { "(" ~ macro_pat_list? ~ ")" ~ block_or_quote }
macro_arm = { "(" ~ macro_pat_list? ~ ")" ~ "=>" ~ macro_body ~ ","? }
macro_body = { quote_expr | comptime_block | block }

quote_expr = { "quote" ~ (block | "(" ~ expr ~ ")") }
macro_call = { ident ~ "!" ~ "(" ~ tt_list? ~ ")" }

attr = { "@" ~ ident ~ ("(" ~ tt_list? ~ ")")? }
// class_def / func_def 前可挂 attr*

comptime_fn = { "comptime" ~ "fn" ~ ... }
comptime_block = { "comptime" ~ block }
```

`tt` = 平衡 token 树，供宏参数原始切片；模式匹配时再按 `expr`/`ident` 解析。

---

## 16. 与语言其他特性的边界

| 特性 | 关系 |
|------|------|
| 泛型 / monomorph | 宏在单态前展开；可生成泛型函数 |
| 闭包 / 一等函数 | 宏生成的代码可含闭包；宏本身不是运行时值 |
| `inline fn` | 优化提示；不替代宏 |
| f-string | 宏可用 f-string；`:src` 是宏层元数据 |
| FFI `extern` | 可用宏减少重复声明；不生成链接魔法 |
| 包管理器 | 宏随模块分发，无需特殊 crate 类型 |

---

## 17. 实现偏好（非语法，怎么顺怎么来）

| 项 | 选择 |
|----|------|
| M0–M1 comptime | 浅层树遍历解释器（只跑宏体需要的子集） |
| M3 comptime | 需要时再上沙箱 JIT；不阻塞前两阶段 |
| 宏参数解析 | 先收 `tt`，再按模式里的 `expr`/`ident` 二次解析（错误可定位） |
| 与现有 `inline fn` | 不合并；文档对照：“`inline` 优化，`macro!` 生成” |

语法层决议见 **§0 已拍板**，不再列为开放问题。

---

## 18. 设计验收标准

做完后应能回答“是”：

1. 新用户 10 分钟内会用 `assert!` / `dbg!`。  
2. **`assert(x)` 绝不展宏**；只有 `assert!(x)` 会。  
3. 中级用户不查 AST 文档也能写日志/重试/简单 DSL 宏。  
4. 高级用户能用 `comptime`+`Ast` 实现 `@derive(Debug)`（方法形式）。  
5. 任何宏错误都能指回**调用点**。  
6. `bolide expand` 输出人类可读。  
7. AOT 与 JIT 行为一致（宏仅影响前端）。  
8. 不引入第二套与 Bolide 无关的元语言。

---

## 19. 结语

Bolide 宏的取胜点不是“比 Rust 更底层”，而是：

> **用写 Bolide 的方式写编译期代码；用强制 `!` 和 `@` 标出元层面；用卫生 `quote` 保证不魔法泄漏；用分层让 90% 的人永远不必碰 `Ast`。**

这与语言一贯的目标一致：**脚本一样好写，原生一样能发。**

---

*文档状态：设计已拍板（强制 `!` + 好用优先）。*

**已实现（强能力路径）：**
- 声明式宏 / `quote` / 强制 `!` / 卫生 / 模式与**模板体** `$(...)*`/`+`
- `$n:lit` 次数重复；字段循环 `$field` / `self.$field` / `fn $field`
- 内置宏；`export macro` + import；`attr macro`（函数 prologue + 类方法生成）
- `@derive(Debug, Eq, Clone, Default)`、`@getters`、`@test`
- `comptime { }` + `comptime fn`（递归、if、len/str）
- `bolide expand`

**可选后续：** 完整反射式 `Ast` 值类型 API、任意 TokenStream 级过程宏。*
