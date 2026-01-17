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
    <img src="https://img.shields.io/badge/version-0.8.1-blue.svg" alt="Version">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/platform-windows%20%7C%20linux-lightgrey.svg" alt="Platform">
  </a>
</p>

---

**Bolide** 是一门现代化编程语言，基于 **Cranelift** 实现 JIT/AOT 编译，兼具简洁语法与原生性能。

## 特性

- **JIT 编译** - 基于 Cranelift 的即时编译，快速启动
- **AOT 编译** - 提前编译为原生可执行文件，无需运行时
- **异步协程** - 一等公民的 async/await 支持
- **FFI** - 无缝调用 C 库，支持回调函数
- **模块系统** - 命名空间隔离的模块导入
- **丰富类型** - BigInt、Decimal、Dynamic 等
- **并发支持** - 线程、通道、线程池
- **内存管理** - ARC 引用计数 + 生命周期注解 + weak/unowned 引用

## 快速开始

### 从源码构建

```bash
# 克隆仓库
git clone https://github.com/your-repo/bolide.git
cd bolide

# 构建
cargo build --release

# 运行程序
cargo run --release -- run examples/hello.bl
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

## 语法示例

### 变量与类型

```bolide
let x: int = 42;
let pi: float = 3.14159;
let name: str = "Bolide";
let flag: bool = true;
let big: bigint = 123456789012345678901234567890b;
let precise: decimal = 3.14159265358979d;
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

### 函数

```bolide
fn add(a: int, b: int) -> int {
    return a + b;
}

fn greet(name: str) {
    print(name);
}
```

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
while x > 0 {
    x = x - 1;
}

// for 循环 - 字典遍历 (Python 风格)
let scores = {"Alice": 100, "Bob": 85};
for k, v in scores {
    print(k);  // 键
    print(v);  // 值
}
```

### 列表操作

Bolide 提供了丰富的 Python 风格列表操作：

```bolide
let nums: list<int> = [3, 1, 4, 1, 5, 9];

// 基本操作
nums.push(10);           // 追加元素
let x: int = nums.pop(); // 弹出最后一个元素
print(nums.len());       // 获取长度

// 索引访问
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
let sliced: list<int> = nums.slice(1, 4);  // 切片 [1:4)
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
let scores: dict<str, int> = {"Alice": 100, "Bob": 90};
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

// 启动协程
let f1: future = fetch_data(1);
let f2: future = fetch_data(2);

// 等待结果
let r1: int = await f1;
let r2: int = await f2;
```

### 高级并发特性

#### Await All (并发等待)

```bolide
async fn fetch_a() -> int { return 100; }
async fn fetch_b() -> int { return 200; }

// 并发执行所有任务并等待结果
let (a, b) = await all {
    fetch_a(),
    fetch_b()
};
```

#### Async Select (竞态等待)

```bolide
// 等待第一个完成的任务
async select {
    res1 = task_fast() => {
        print("fast finished");
    }
    res2 = task_slow() => {
        print("slow finished");
    }
}
```

### 多线程与并行

#### Spawn & Join

使用 `spawn`关键字在新的系统线程中启动任务：

```bolide
fn heavy_work(id: int) -> int {
    // 耗时计算...
    return id * id;
}

// 启动新线程
let t: future = spawn heavy_work(10);

// 等待线程结束并获取结果
let result: int = join(t);
```

#### 线程池 (Thread Pool)

使用 `pool` 块将任务分发到指定大小的线程池中执行：

```bolide
pool(4) {
    // 这些任务将在4个工作线程中并发执行
    spawn task(1);
    spawn task(2);
    spawn task(3);
}
// pool 块结束时会自动等待所有任务完成
```

#### 通道 (Channels)

线程间安全的通信机制：

```bolide
// 创建通道
let ch: channel<int> = channel();

// 定义发送函数
fn sender(c: channel<int>) {
    c <- 42;
}

// 启动发送线程
spawn sender(ch);

let val: int = <- ch;  // 接收数据
```

#### Channel Select (多路复用)

使用 `select` 语句处理多个通道操作，支持超时和默认分支：

```bolide
select {
    val1 <- ch1 => {
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

```bolide
// math_utils.bl
fn add(a: int, b: int) -> int {
    return a + b;
}

// main.bl
import "math_utils.bl";

let result: int = math_utils.add(10, 20);
print(result);  // 30
```

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

### FFI (C 语言互操作)

```bolide
// 声明 C 函数
extern "msvcrt.dll" {
    fn abs(x: c_int) -> c_int;
    fn sqrt(x: c_double) -> c_double;
}

let a: int = abs(-42);      // 42
let b: float = sqrt(16.0);  // 4.0

// 支持回调函数
fn my_callback(a: int, b: int) -> int {
    return a + b;
}
let r: int = test_callback(my_callback, 10, 20);
```

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
| `future` | 协程 Future | `let f: future = async_fn();` |


## 内存管理

Bolide 使用 **ARC (自动引用计数)** 作为默认内存管理方式，同时提供生命周期注解和弱引用来处理特殊场景。

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

### weak 引用

`weak` 引用不增加引用计数，当原对象被释放时自动变为 nil：

```bolide
class Node {
    value: int;
}

let obj: Node = Node(42);  // 直接在构造时初始化字段

let w: weak Node = obj;  // weak 引用，不增加引用计数
// 访问 weak 引用时会自动检查是否为 nil
```

### unowned 引用

`unowned` 引用不增加引用计数，假设对象始终存在（不进行 nil 检查）：

```bolide
let obj: Node = Node(42);
let u: unowned Node = obj;  // unowned 引用
print(u.value);  // 直接访问，无 nil 检查
```


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

## 许可证

MIT License
