# Bolide `from` 生命周期与借用视图设计

> **状态**：设计草案（Draft）  
> **基准版本**：Bolide 0.14.1  
> **相关实现**：`lifetime_deps`（parser/AST）、JIT/AOT 借用检查与返回路径跳过 ARC  
> **相关文档**：`README.md` 内存管理、`docs/syntax-design-notes.md`、`docs/book/bolide-from-zero-to-mastery.md` §6.3–6.4 / 第 19 章  
> **目标一句话**：在 **不引入完整 Rust 式 `'a` 系统** 的前提下，把 `from` 从「小众省 ARC 开关」升级为 **可组合的借用视图体系**，使零拷贝 getter / 字典查找 / 方法访问成为日常可用 API。

---

## 目录

1. [动机与问题](#1-动机与问题)
2. [现状盘点（0.14.x）](#2-现状盘点014x)
3. [与 Rust 生命周期对照](#3-与-rust-生命周期对照)
4. [设计原则与非目标](#4-设计原则与非目标)
5. [核心概念模型](#5-核心概念模型)
6. [目标语法](#6-目标语法)
7. [语义规则（规范性）](#7-语义规则规范性)
8. [Elision（省略规则）](#8-elision省略规则)
9. [分阶段路线图](#9-分阶段路线图)
10. [标准库迁移方向](#10-标准库迁移方向)
11. [与 `ref` / `owned` / ARC / `weak` 的关系](#11-与-ref--owned--arc--weak-的关系)
12. [实现要点（编译器）](#12-实现要点编译器)
13. [诊断与错误信息](#13-诊断与错误信息)
14. [测试计划](#14-测试计划)
15. [兼容性与迁移](#15-兼容性与迁移)
16. [开放问题](#16-开放问题)
17. [决策记录（ADR 摘要）](#17-决策记录adr-摘要)
18. [附录：示例程序集](#18-附录示例程序集)

---

## 1. 动机与问题

### 1.1 用户能感知到的痛点

Bolide 默认用 **ARC** 管理堆对象，正确且省心，但在以下模式里会产生 **多余的 retain/release**：

```bolide
fn get_name(u: User) -> str {
    return u.name;   // 往往要把字段「变成调用方可独立持有的值」→ retain/clone 语义
}
```

对 `bigint`、长 `str`、大对象图，热路径上反复「取字段 / 取元素 / 字典 get 再只读」时，RC 流量可能可见。

语言已经提供了 `from`：

```bolide
fn get_value(ref x: bigint) -> bigint from x {
    return x;   // 返回借用，跳过返回路径 ARC
}
```

但 **工程实用性偏低**：

| 现象 | 后果 |
|------|------|
| 几乎只出现在测试 / `lifetime_*.bl` | 学习者与业务代码学不到惯用法 |
| `std/` 零使用 | 没有可抄的 API 样板 |
| 借用在类型上不可见（返回仍像普通 `str`） | 签名读不出「能不能存」 |
| 借用难以再传入其他函数 | 无法组合 |
| 逃逸规则很严且缺少「正规升格」习惯用法 | 稍一 `push` 就编译失败，体感差 |
| 与 Rust 函数寿命的日常场景不对齐 | 方法 getter、`Option<&V>`、切片视图等写不出 |

### 1.2 要回答的设计问题

1. **能否**在保持 ARC 默认模型的前提下，让 `from` 的用处接近 Rust **函数级**生命周期的日常收益？  
2. **不必**做成完整 `'a` / 结构体字段寿命 / HRTB 时，最小完备集是什么？  
3. 如何让 **标准库敢于返回借用**，从而形成生态？

### 1.3 成功标准（验收）

当下列条件成立时，本设计视为达标：

1. 用户能为 class 写 `fn name(self) -> borrow str from self` 且 JIT/AOT 正确。  
2. 借用结果可传入 `fn f(s: borrow str)`，在合法作用域内组合。  
3. `dict.get` / `list` 只读访问可表达 `Option<borrow V>`（或阶段性等价物）。  
4. 需要拥有时有 **单一、好记** 的升格：`v.copy()` / `owned(v)`。  
5. `std` 至少 3 个模块提供借用视图 API，并有文档与测试。  
6. 未使用借用的旧代码 **零修改** 仍能编译。

---

## 2. 现状盘点（0.14.x）

### 2.1 语法与 AST

```bolide
fn name(params) -> RetType from x, y { ... }
```

- `from` 后跟 **参数名列表**（不是类型名，也不是 `'a`）。  
- AST：`FuncDef.lifetime_deps: Option<Vec<String>>`。  
- 参数模式独立：`ParamMode::{ Borrow, Owned, Ref }`（默认 / `owned` / `ref`）。

### 2.2 运行时 / 代码生成行为

声明了 `lifetime_deps` 的函数进入 **lifetime mode**：

| 行为 | 说明 |
|------|------|
| 返回路径 | **跳过** RC 类型的 retain 及部分 cleanup 策略（借用返回） |
| 调用方 | `let b = f(a)` 将 `b` 记为借用 `a`，**不**按 owner 跟踪 release |
| 返回值校验 | 返回表达式必须能追溯到来源参数（Ident / Member / Index 链） |
| 逃逸检查 | 借用禁止：容器、字段、通道、`spawn` 等 |
| 来源冻结 | 借用存活期间禁止对来源重赋值 / 重声明 |
| 悬空检测 | 借用不能比来源作用域更长 |

JIT / AOT 均实现（见 `uses_lifetime_mode`、`validate_lifetime_return`、`check_borrow_escape` 等）。

### 2.3 已支持的数据流

`check_lifetime_source` 可识别：

- 直接参数名  
- 从参数派生的局部绑定  
- `Member` / `Index` 链（`x.field`、`xs[i]`）

`get_lifetime_call_source` 在调用端主要从 **第一个 `ref` 实参** 推断借用来源（多参数 `from x, y` 的调用端传播仍偏保守）。

### 2.4 现实使用面

| 区域 | 使用情况 |
|------|----------|
| `tests/test_borrow_*.bl`、`examples/lifetime_*.bl` | 有，几乎全是 `bigint` |
| `std/**` | 无 |
| 业务示例（blog / web / gui） | 无 |

### 2.5 结论

> **机制正确、检查严格、生态空白。**  
> 问题不在「能不能省 ARC」，而在「类型不可见 + 不可组合 + 无 std + 无 elision」。

---

## 3. 与 Rust 生命周期对照

### 3.1 概念映射

| Rust | Bolide 目标映射 | 说明 |
|------|-----------------|------|
| `&'a T` | `borrow T`（寿命由上下文/`from` 约束） | 借用类型一等公民 |
| `fn f<'a>(x: &'a T) -> &'a U` | `fn f(x: T) -> borrow U from x` | 寿命绑参数名，不引入 `'a` |
| elision | 单输入 / `self` 省略 `from` | 降低样板 |
| `.to_owned()` / `.clone()` | `.copy()` / `owned()` | 逃逸出口 |
| 不能跨线程送 `&T` | 禁止 `borrow` → `spawn` | 保持简单 |
| `struct Foo<'a> { x: &'a T }` | **非目标（首期）** | 与 ARC 对象图冲突大 |
| HRTB / 复杂 variance | **非目标** | 性价比低 |

### 3.2 Rust 好用的真正原因（本设计要对齐的）

1. **签名自解释**：见 `-> &str` 就知是视图。  
2. **可组合**：`len(first(xs))` 自然。  
3. **std 全是借用**：学会一种，处处复用。  
4. **elision**：80% getter 不用写寿命。  

**不是**因为用户爱写 `'a`。

### 3.3 Bolide 必须不同的地方

- 默认仍是 **ARC 拥有**；借用是 **可选加速与 API 精度**，不是唯一内存模型。  
- 不要求用户理解区域借用检查器的全部细节；规则应用 **参数名 + 作用域** 表达。  
- 与 `ref`（可写绑定）并存：`ref` 改的是**变量槽**，`borrow` 是**对象视图**。

---

## 4. 设计原则与非目标

### 4.1 原则

1. **ARC 仍是默认真理**；借用是显式、可关闭的视图层。  
2. **寿命绑在值/参数上**（`from xs`），不引入第二套 `'a` 字母系统。  
3. **类型可见**：`borrow T` 出现在类型位置，避免「看起来像 `str` 其实是借的」。  
4. **可组合优先于更多检查花样**。  
5. **逃逸要么禁止要么显式升格**，不提供静默变拥有。  
6. **分阶段落地**；每阶段单独可发布、可测试。  
7. **标准库先行**，否则语言特性仍会沉睡。

### 4.2 非目标（明确不做或长期不做）

| 非目标 | 原因 |
|--------|------|
| 完整命名寿命 `'a, 'b` 与子类型层级 | 学习成本与实现成本过高 |
| class 字段长期持有 `borrow T` | 与 ARC 所有权、对象图、异步边界纠缠 |
| 用借用取代 ARC | 违背语言定位 |
| 跨 `spawn` / 通道传递借用 | 安全与实现复杂度 |
| 与 C FFI 直接暴露 borrow 指针无规则 | 另案；首期仅 Bolide 侧 |

### 4.3 语法风格立场

`docs/syntax-design-notes.md` 指出 `owned`/`ref` 在参数前、`from` 在签名末，位置不统一。本设计：

- **保留** `from` 在签名末（与现有实现、文档兼容）。  
- **新增** 类型侧 `borrow T`（或 `view T`，见开放问题）。  
- 不强制立刻把 `ref`/`owned` 改成尖括号；可在附录记为长期统一项。

---

## 5. 核心概念模型

### 5.1 三种「值的身份」

| 身份 | 含义 | RC | 典型来源 |
|------|------|----|----------|
| **Owned** | 强引用拥有者之一 | 参与 retain/release | 字面量、`copy`、普通返回、`owned` 参数 |
| **Borrow view** | 不拥有，依附来源 | 不因视图创建 retain | `from` 返回、`borrow` 参数 |
| **Ref binding** | 可写调用方变量槽 | 视写入的新值而定 | `ref` 参数 |

```text
                    copy/owned()
         Owned  <----------------  borrow T
           |                         ^
           | 默认传参/返回            | from 返回
           v                         |
         （函数边界 ARC 策略）      来源参数/self
```

### 5.2 寿命（lifetime）在本设计中的定义

一个借用值 `v` 的寿命 **不超过** 其 **来源集合** `sources(v)` 中任一来源的存活范围。

- 来源是 **变量 / 参数 / self**（运行时身份），不是抽象区域名。  
- `from x, y` 表示 `sources(ret) ⊆ {x, y}`，实际取 **交集约束**（返回值不得长于任一来源）。  
- 编译器在赋值、返回、调用时维护 `var → sources` 数据流（扩展现有 `var_lifetime_source` / `borrowed_vars`）。

### 5.3 「跳过 ARC」的精确定义

对 **返回类型为借用** 或 **函数处于 lifetime mode** 的返回路径：

1. **不**对返回值执行「交给调用方独立拥有」所需的 retain。  
2. 调用方绑定 **不** 登记为 RC owner cleanup 目标。  
3. 调用方登记 borrow 边：`borrower → source`。  
4. 若调用方将借用 **升格为 owned**，此时执行一次 retain/clone（按类型）。

对 **owned 返回**（无 `from` / 非 `borrow T`）：保持 0.14 现有 ARC 行为。

---

## 6. 目标语法

### 6.1 借用类型

```bolide
borrow T          // 推荐主关键字
// 别名候选：view T（开放问题）
```

出现位置：

- 函数参数：`fn f(s: borrow str)`  
- 返回类型：`-> borrow str`  
- 局部注解：`let s: borrow str = ...`  
- 泛型：`fn id<T>(x: borrow T) -> borrow T from ...`（阶段 B+）  
- 容器包装：`Option<borrow T>`、`Result<borrow T, E>`（阶段 B）

**不**出现于（首期）：

- class 字段  
- `list<borrow T>` 作为长期存储（禁止或仅允许临时，见规则）  
- `channel<borrow T>`

### 6.2 函数签名

```bolide
// 显式 from
fn first(xs: list<str>) -> borrow str from xs {
    return xs[0];
}

// 多来源
fn pick(a: str, b: str, flag: bool) -> borrow str from a, b {
    if flag { return a; }
    return b;
}

// 方法
class User {
    name: str;
    fn name(self) -> borrow str from self {
        return self.name;
    }
}

// 借用参数（再借用）
fn print_s(s: borrow str) {
    print(s);
}

// 与 ref / owned 组合
fn take_name(ref u: User) -> borrow str from u {
    return u.name;
}

fn consume(owned u: User) {
    // 不可 from u 再返回 borrow 给调用方（来源已 move 进函数）
}
```

### 6.3 与旧语法兼容

```bolide
// 0.14 旧写法：返回类型不写 borrow，仅 from
fn get_value(ref x: bigint) -> bigint from x {
    return x;
}
```

语义：视为 **返回 `borrow bigint`** 的语法糖（或实现上等价 lifetime mode）。  
新代码 **推荐** 显式 `-> borrow T from ...`。

### 6.4 升格（escape hatch）

```bolide
let v: borrow str = first(xs);
let o1: str = v.copy();     // 推荐方法风格
let o2: str = owned(v);     // 或内置函数风格（二选一，见开放问题）
```

规则：

- `copy`/`owned` 得到 **Owned `T`**，可逃逸、可存容器、可 `spawn`（受既有 spawn 规则约束）。  
- 对不可深拷贝类型：编译错误或仅 retain 共享对象（**对象共享 vs 深拷贝** 见开放问题）；`str`/`bigint`/`list` 元素等需有明确定义。

### 6.5 禁止的直观例子

```bolide
fn bad1(xs: list<str>) -> borrow str from xs {
    let tmp: str = "x";
    return tmp;   // 错误：不是来自 xs
}

fn bad2(xs: list<str>) -> list<str> {
    let v: borrow str = first(xs);
    return [v];   // 错误：借用逃逸；应 v.copy()
}

fn bad3(xs: list<str>) {
    let v: borrow str = first(xs);
    spawn work(v); // 错误：借用不可跨线程
}
```

---

## 7. 语义规则（规范性）

### 7.1 类型一致性

1. `borrow T` 与 `T` **不同型**；不可隐式互相赋值。  
2. **例外（协变只读场景，可选阶段）**：`borrow T` 可自动适配仅接受 `borrow T` 的参数；接受 `T`（owned）的参数 **不** 自动接收 `borrow T`（必须 `copy`）。  
3. `ref x: T` 的 `x` 在函数内按 **可写绑定** 使用；若返回 `borrow` 来自 `x`，来源是调用方变量。

### 7.2 返回值来源

函数若：

- 返回类型含 `borrow`，或  
- 声明了 `from ...`，  

则每个 `return expr`（及作为返回值的尾表达式，若未来支持）必须满足：

```text
sources(expr) ⊆ declared_from_set
sources(expr) 非空
```

`sources` 计算：

| 表达式 | sources |
|--------|---------|
| 参数 `p` 且 `p` 在 from 集或本身为 borrow 入参 | `{p}` 或入参的 sources |
| 局部 `let y = e` | `sources(e)` |
| `e.field` / `e[i]` | `sources(e)` |
| `f(...)` 若 `f` 返回 borrow | 实参中与 `f` 的 from 集对应的来源并集/按签名映射 |
| 字面量、新建对象、`a+b` 等 | `∅`（不可直接作 borrow 返回） |
| `copy(e)` / `owned(e)` | `∅` 且类型为 owned（不可再当 borrow 返回除非再借） |

### 7.3 借用绑定的约束

对 `let b: borrow T = e` 或推断为 borrow 的绑定：

1. **来源冻结**：任一 `s ∈ sources(b)` 在 `b` 存活期间不可重赋值、不可 `owned` move 走、不可释放。  
2. **作用域**：`b` 的作用域不得长于任一来源。  
3. **逃逸禁止**：`b` 不可：
   - 传入 `owned` 上下文或要求 owned 的参数（无隐式 copy）  
   - 存入 `list`/`dict`/元组/对象字段  
   - 经 channel 发送  
   - 作为 `spawn`/`spawn thread` 参数  
   - 从 **未** 声明对应 `from`/`borrow` 返回的函数返回  
4. **允许**：只读方法调用（不延长寿命）、传入 `borrow T` 参数、`copy` 升格、`print` 类观察。

### 7.4 可变借用（首期可选）

首期 **仅只读借用** `borrow T`。

未来候选：`borrow mut T` / `ref borrow`——需排他规则（同一来源不可同时存在两个 mut borrow）。  
**阶段 A/B 不做 mut borrow**，避免与 `ref` 参数语义重叠混淆。

### 7.5 与默认参数模式（Borrow）的区别

| | 默认 `x: T`（ParamMode::Borrow） | `x: borrow T` |
|--|----------------------------------|---------------|
| 类型表面 | `T` | `borrow T` |
| 调用方 | 传 owned 或可兼容值 | 传 borrow 或「临时只读视图」 |
| 函数内 | 只读约定（实现上传指针） | 类型强制只读视图 |
| 返回 | 默认要 retain 才能当 owned 返回 | 可继续以 borrow 返回并 `from` |

长期可将「默认参数」在 ARC 类型上解释为「内部借用传递」，但 **类型层仍显示为 `T`**，以保持兼容。

### 7.6 `from` 与 `owned` 参数

```bolide
fn f(owned x: str) -> borrow str from x { return x; }  // 禁止
```

原因：`owned` 表示调用方已放弃；返回 borrow 给调用方会悬空。  
编译错误：`cannot borrow from owned parameter 'x'`.

### 7.7 多来源

```bolide
fn pick(a: str, b: str, c: bool) -> borrow str from a, b
```

- 返回 `a` 或 `b` 均可。  
- 调用方借用的 sources 为 **运行时实际来源**；静态上取 **上界** `from` 集做逃逸与作用域检查（保守：按 `a,b` 均可能冻结）。  
- 优化可选：用 dataflow 收窄（阶段 C）。

---

## 8. Elision（省略规则）

目标：对齐 Rust「大多数 getter 不写寿命」的体验。

### 8.1 规则 E1 — 单一输入来源

若函数：

- 返回 `borrow T`，且  
- 仅有一个可能作为来源的参数（owned/`ref`/默认的 `U`，或 `self`），且  
- 没有其它 `borrow` 入参，  

则 **可省略** `from that_param`，等价于显式 `from`。

```bolide
fn first(xs: list<str>) -> borrow str {   // elision: from xs
    return xs[0];
}
```

### 8.2 规则 E2 — 方法 self

若存在 `self`（或 `ref self`，若未来支持）且返回 `borrow T`，无其它候选来源参数，则默认 `from self`。

```bolide
fn name(self) -> borrow str {
    return self.name;
}
```

### 8.3 规则 E3 — 借用入参转发

若唯一参数为 `borrow T` 且返回 `borrow U`（`U` 来自该参数字段/自身），默认 `from` 该参数。

```bolide
fn as_str(s: borrow str) -> borrow str {
    return s;
}
```

### 8.4 必须显式 `from` 的情况

- 多个可能来源（`a, b`）  
- 返回来源不是「明显的那一个」  
- 返回类型写的是普通 `T` 却想走 lifetime mode（旧语法）且多参数  

### 8.5 旧语法 elision

```bolide
fn get_value(ref x: bigint) -> bigint from x
```

保持显式 `from` 推荐；是否对 `-> T from` 做「返回类型糖化为 borrow」见 §6.3。

---

## 9. 分阶段路线图

### 阶段 0 — 文档与现状巩固（已部分完成）

- [x] 教程中说明 `ref` / `owned` / `from` 与「跳过 ARC」  
- [ ] 本设计文档评审通过  
- [ ] README 链到本文，标明 `from` 为「进阶 / 演进中」

**交付**：认知一致，无语言变更。

---

### 阶段 A — 最小可用增强（建议首个语言版本）

**目标**：让「方法 getter + 显式 borrow 返回」可用，生态可开始试写。

| 项 | 内容 |
|----|------|
| A1 | 类型 `borrow T` 解析与类型检查 |
| A2 | `-> borrow T from params` 与旧 `-> T from params` 等价处理 |
| A3 | `from self` / 方法 lifetime |
| A4 | elision E1/E2 |
| A5 | `copy()` 或 `owned()` 升格（至少 `str`、`bigint`、class 对象 retain） |
| A6 | 诊断信息升级（见 §13） |
| A7 | 回归：扩展 `lifetime_*.bl` + 方法用例 |

**非目标**：`Option<borrow T>`、std 大规模迁移。

**验收示例**：

```bolide
class User {
    name: str;
    fn name(self) -> borrow str from self {
        return self.name;
    }
}

fn show(s: borrow str) {
    print(s);
}

let u: User = User("Ada");
show(u.name());
let owned_name: str = u.name().copy();
```

---

### 阶段 B — 可组合与容器 API

| 项 | 内容 |
|----|------|
| B1 | 参数 `borrow T` 与借用转发数据流 |
| B2 | `Option<borrow T>`、`Result<borrow T, E>` |
| B3 | 调用端多参数 `from` 来源映射修正 |
| B4 | elision E3 |
| B5 | `std`：`dict` get 视图、`list` get 视图、可选 `str` 切片视图 |
| B6 | 教程章节升级为「借用视图惯用法」 |

**验收示例**：

```bolide
fn get(m: dict<str, str>, k: str) -> Option<borrow str> from m {
    // ...
}

match get(map, "name") {
    Option.Some(s) => { print(s); },
    Option.None() => {},
}
```

---

### 阶段 C — 体验打磨（可选）

| 项 | 内容 |
|----|------|
| C1 | dataflow 收窄多来源冻结集 |
| C2 | 借用与 `match`/`if let` 深度交互 |
| C3 | 受限「栈上借用结构体」（仅局部，不可进 class 字段） |
| C4 | 性能对比 bench（getter 密集路径） |
| C5 | mut borrow 提案（独立 RFC） |

---

### 阶段 D — 明确拒绝或另案

- 完整 `'a` 语法  
- class 字段 `borrow T`  
- 跨线程借用  

---

## 10. 标准库迁移方向

### 10.1 原则

1. **旧 API 保留 owned 语义**，新增视图 API 或重载，避免 silent 行为变化。  
2. 命名建议：
   - `get` → owned 或 copy  
   - `get_ref` / `get_view` / `as_str` → `borrow`  
3. 文档写明：**视图仅在来源存活期内有效**。

### 10.2 优先模块

| 模块 | 候选 API | 阶段 |
|------|----------|------|
| 内置 `list` | `get_view(i) -> Option<borrow T>` | B |
| 内置 `dict` | `get_view(k) -> Option<borrow V>` | B |
| 内置 `str` | 切片 `slice_view(s,e) -> borrow str`（若表示允许） | B/C |
| `std/text` | 只读拆分视图 | C |
| 用户 class | 教程示范 getter | A |

### 10.3 不优先

- GUI 即时模式回调（寿命缠在帧上，易误用）  
- async 跨 await 持有 borrow（默认禁止跨 await，另案）

---

## 11. 与 `ref` / `owned` / ARC / `weak` 的关系

### 11.1 总表

| 机制 | 层 | 目的 | 日常优先级 |
|------|----|------|------------|
| 默认 ARC owned | 内存模型默认 | 安全省心 | 最高 |
| 默认/`Borrow` 传参 | 调用约定 | 只读传指针少 RC | 高 |
| `ref` | 绑定 | 改调用方变量 | 高 |
| `owned` 参数 | 绑定 | 移动所有权 | 中 |
| `borrow T` + `from` | 类型 + 寿命 | 零拷贝视图 | 中（增强后） |
| `weak`/`unowned` | 类型 | 破环 / 不延长寿命 | 中（图结构） |
| `value` | 类型 | 按值聚合无堆 RC | 高（数值） |

### 11.2 口诀（给文档/书用）

```text
只读看一眼     → 默认传参或 borrow 视图
改调用方变量   → ref
移交所有权     → owned
返回内部视图   → borrow T + from（或 elision）
要存/要并发    → copy 成 owned
破引用环       → weak / unowned
数值热路径     → value + inline
```

### 11.3 不要混用的反模式

```bolide
// 反模式：用 ref 假装 borrow 返回
fn name(ref u: User) -> str { return u.name; }  // owned 返回，可能 retain

// 正模式
fn name(self) -> borrow str from self { return self.name; }
```

---

## 12. 实现要点（编译器）

### 12.1 管线位置

```text
parse → AST(lifetime_deps, Type::Borrow(T))
      → expand macros
      → monomorph
      → type check（borrow 流、elision 降糖）
      → JIT/AOT codegen（RC 策略、借用边）
```

Elision 建议在 **类型检查前或早期** 降糖为显式 `lifetime_deps`，后端只认显式集。

### 12.2 AST / 类型扩展

```text
Type::Borrow(Box<Type>)
FuncDef.lifetime_deps: Option<Vec<String>>  // 保持
Param 可带 Type::Borrow
```

旧代码：`lifetime_deps = Some` 且返回非 Borrow → 内部视为返回 Borrow 或保持「lifetime mode 标志」双轨，直至弃用期结束。

### 12.3 数据流结构（扩展现有）

| 表 | 用途 |
|----|------|
| `lifetime_deps` | 当前函数声明的 from 集 |
| `var_lifetime_source` / `sources` | 变量 → 来源参数集合 |
| `borrowed_vars` | 调用方借用边 + 来源深度 |
| `lifetime_funcs` | 哪些函数返回借用 |
| 签名表 | 形参 index → from 映射，供调用端传播 |

### 12.4 调用端来源传播（修正点）

现状偏向「第一个 ref 实参」。阶段 B 应改为：

1. 读取被调函数 `lifetime_deps` 参数名列表。  
2. 映射到实参表达式的 sources。  
3. 多来源时调用方 `sources(ret) = union(mapped)`，作用域检查走保守交集。

### 12.5 Codegen

- `uses_lifetime_mode()` 真条件扩展为：`lifetime_deps.is_some() || returns_borrow_type`。  
- 返回 borrow：跳过 retain（已有）。  
- `copy`：生成 `emit_retain` 或类型专用 clone。  
- 不要对 borrow 临时值做 owner cleanup。

### 12.6 与 inline / 泛型

- `inline fn`：若含 lifetime，inline 后借用边接到调用方（或禁止 inline 含 borrow 的函数，阶段 A 可简单禁止）。  
- 泛型 monomorph：`borrow T` 一并实例化。

### 12.7 LLVM 后端

与 Cranelift 路径共享借用检查（检查宜在统一前端/中端完成）；后端只消费「是否 retain」标志。

---

## 13. 诊断与错误信息

### 13.1 必须友好的几类

| 错误码（建议） | 场景 | 帮助文案要点 |
|----------------|------|----------------|
| `borrow_escape_container` | push/字段存储 | 提示 `.copy()` |
| `borrow_escape_spawn` | spawn 参数 | 提示先 copy 或改传 owned |
| `borrow_source_assign` | 来源重赋值 | 说明借用仍存活 |
| `borrow_outlives_source` | 作用域过长 | 缩小作用域或 copy |
| `borrow_return_bad_source` | return 非 from 集 | 指出应 from 谁 |
| `borrow_from_owned_param` | from owned 参数 | 语义非法 |
| `borrow_type_mismatch` | owned 与 borrow 混用 | 提示 copy 或改签名 |
| `borrow_elision_ambiguous` | 多来源未写 from | 要求显式 `from a, b` |

### 13.2 示例诊断

```text
error: borrowed value escapes via list.push
  --> main.bl:12:5
 12 |     xs.push(v);
    |     ^^^^^^^^^^
  note: 'v' borrows from 'names'
  help: copy first: xs.push(v.copy());
```

---

## 14. 测试计划

### 14.1 单元 / 回归（`tests/`）

| 组 | 内容 |
|----|------|
| 兼容 | 旧 `-> bigint from x` 全绿 |
| 类型 | `borrow` 与 `T` 不可混赋 |
| 方法 | `from self`、elision E2 |
| 逃逸 | 容器/字段/通道/spawn 负例 |
| 升格 | copy 后可 push |
| 组合 | borrow 参数转发 |
| 多来源 | `from a,b` 正负例 |
| Option | `Option<borrow T>` match |
| AOT | 同上样例 `bolide compile` |

### 14.2 性能（`bench/` 可选）

- 密集 getter 路径：owned 返回 vs borrow 返回，RC 次数或耗时对照。  
- 仅作趋势参考，不设硬门槛。

### 14.3 文档测试

- 书 §6.4 / 第 19 章与 README 示例可运行。  
- 本文附录示例进 `examples/borrow_*.bl`。

---

## 15. 兼容性与迁移

### 15.1 兼容保证

| 代码 | 行为 |
|------|------|
| 从不使用 `from` / `borrow` | **无变化** |
| `-> T from x` 旧写法 | 继续合法 ≥ 2 个次版本 |
| 新推荐 | `-> borrow T from x` 或 elision |

### 15.2 弃用策略（若统一语法）

1. 版本 N：双轨 + 文档推荐 borrow。  
2. 版本 N+1：对「`from` 但返回类型非 borrow」给 lint warning。  
3. 版本 N+2+：可选改为硬错误（需社区评估）。

### 15.3 对包生态

- 包若发布「返回 borrow 的 API」，semver 视为 **breaking**（调用方不能再当 owned 用）。  
- 建议新包直接用视图命名，避免改旧函数语义。

---

## 16. 开放问题

| # | 问题 | 候选 | 倾向 |
|---|------|------|------|
| Q1 | 关键字 `borrow` vs `view` | borrow 更贴 Rust；view 更中性 | **borrow** |
| Q2 | 升格方法名 | `copy` / `owned` / `to_owned` | **`copy()`**，与「非深拷贝对象 retain」文档说明绑定 |
| Q3 | class 对象 `copy` 语义 | 浅 retain vs 深克隆 | 默认 **retain（共享对象）**；深克隆另 API |
| Q4 | `str` 切片是否可 `borrow str` | 需底层表示支持子切片 | 阶段 B 调研 |
| Q5 | `list[i]` 对 `list<int>` 等标量 | 返回 owned 标量即可，不必 borrow | 标量不强制 borrow |
| Q6 | 是否允许 `let s: borrow str = u.name` 字段直接借 | 需要字段借用语法 | 阶段 A 仅通过方法/函数返回 |
| Q7 | async：borrow 跨 await | 默认禁止 | **禁止** |
| Q8 | `ref self` vs `self` | 方法借用是否要写 ref | 返回 borrow 时 `self` 默认只读借用足够 |
| Q9 | 语法位置统一（syntax-design-notes） | 是否把 from 改前缀 | **暂不动**，避免大破坏 |
| Q10 | LLVM 与 Cranelift 检查是否必须完全一致 | 应一致 | 中端统一检查 |

---

## 17. 决策记录（ADR 摘要）

### ADR-1：不引入 `'a` 命名寿命

- **决定**：寿命用 `from 参数名` + 作用域数据流。  
- **原因**：贴合现有实现；降低学习成本；足够表达函数级视图。  
- **后果**：结构体字段借用、复杂寿命关系受限。

### ADR-2：借用进入类型系统（`borrow T`）

- **决定**：阶段 A 引入。  
- **原因**：无类型可见性则 std 与组合无法成立。  
- **后果**：类型检查变复杂；需处理好与旧 `from` 双轨。

### ADR-3：只读借用优先，可变借用延后

- **决定**：阶段 A/B 仅 `borrow T`。  
- **原因**：与 `ref` 重叠；排他规则重。  
- **后果**：可变视图仍用 `ref` 参数。

### ADR-4：禁止 class 字段存 borrow（首期）

- **决定**：阶段 A–B 禁止。  
- **原因**：对象图 + ARC + 异步边界风险。  
- **后果**：不能实现完整「借用结构体」；用局部变量与函数组合替代。

### ADR-5：标准库采用「新增视图 API」而非静默改语义

- **决定**：不直接把 `get` 改成返回 borrow。  
- **原因**：保护现有代码与心智模型。  

### ADR-6：跨线程永不借

- **决定**：硬禁止 borrow 进入 spawn/channel。  
- **原因**：实现简单、安全默认。  

---

## 18. 附录：示例程序集

### 18.1 旧语法（保持合法）

```bolide
fn get_value(ref x: bigint) -> bigint from x {
    return x;
}

let a: bigint = 100B;
let b: bigint = get_value(a);
print(b);
```

### 18.2 目标：方法 getter + 组合

```bolide
class User {
    name: str;
    age: int;

    fn name(self) -> borrow str from self {
        return self.name;
    }
}

fn greet(prefix: borrow str, name: borrow str) {
    print(prefix);
    print(name);
}

fn main() {
    let u: User = User("Ada", 36);
    greet("hello", u.name());
    let stored: str = u.name().copy();
    print(stored);
}
```

### 18.3 目标：字典视图（阶段 B）

```bolide
fn show_name(m: dict<str, str>) {
    match m.get_view("name") {
        Option.Some(n) => { print(n); },
        Option.None() => { print("missing"); },
    }
}
```

### 18.4 目标：多来源

```bolide
fn longer<'not_rust>(a: str, b: str) -> borrow str from a, b {
    if a.len() >= b.len() {
        return a;
    }
    return b;
}
```

（注：不要引入 `'not_rust`；此处仅为强调「无需命名寿命」。正确签名如下。）

```bolide
fn longer(a: str, b: str) -> borrow str from a, b {
    if a.len() >= b.len() { return a; }
    return b;
}
```

### 18.5 负例：逃逸

```bolide
fn first(xs: list<str>) -> borrow str from xs {
    return xs[0];
}

fn bad(xs: list<str>) -> list<str> {
    let v = first(xs);
    // return [v];           // error
    return [v.copy()];       // ok
}
```

### 18.6 与 `ref` 配合

```bolide
fn rename(ref u: User, new_name: str) {
    u.name = new_name;
}

fn peek(ref u: User) -> borrow str from u {
    return u.name;
}
```

注意：`peek` 借用存活时，对 `u` 的字段赋值应触发 **来源冻结/变异** 规则（只读 borrow 下写来源应报错）。细则在实现阶段写入「borrow 存活时来源不可变」（比单纯禁止重绑定变量更严，需在类型检查中处理 field store）。

---

## 19. 文档与发布清单

当阶段 A 合入时：

1. 更新 `README.md` 内存管理 / 函数章节。  
2. 更新 `docs/book/bolide-from-zero-to-mastery.md` §6.4、第 19 章。  
3. 更新 `CHANGELOG.md`。  
4. `std/README.md` 增加「借用视图」小节（若有 API）。  
5. 本文状态改为 **Accepted** 或拆出 `from-lifetime-impl-checklist.md`。

---

## 20. 总结

| 问题 | 答案 |
|------|------|
| 现在的 `from` 有用吗？ | 机制有用，工程面窄。 |
| 能否更像 Rust 函数寿命？ | **能**，通过 `borrow T` + 可组合 + elision + std，而不是 `'a`。 |
| 最大杠杆？ | 类型可见、可组合、std 敢返回视图、显式 copy 逃逸。 |
| 首步？ | **阶段 A**：`borrow T`、`from self`、elision、copy 升格。 |

---

## 修订历史

| 日期 | 版本 | 说明 |
|------|------|------|
| 2026-08-09 | 0.1 | 初稿：现状、原则、语法、语义、阶段路线、ADR、测试与开放问题 |
