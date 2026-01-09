<p align="center">
  <img src="./bolide_logo.png" alt="Bolide Logo" width="200">
  <br>
  <b style="font-size: 32px;">Bolide</b>
  <br>
  <i>现代化 JIT 编译型编程语言</i>
  <br>
</p>

<p align="center">
  <a href="https://opensource.org/licenses/MIT">
    <img src="https://img.shields.io/badge/License-MIT-brightgreen.svg" alt="License: MIT">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/version-0.6.3-blue.svg" alt="Version">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/platform-windows%20%7C%20linux-lightgrey.svg" alt="Platform">
  </a>
</p>

---

**Bolide** 是一门现代化编程语言，基于 **Cranelift** 实现 JIT 编译，兼具简洁语法与原生性能。

## 特性

- **JIT 编译** - 基于 Cranelift 的原生性能
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
| `tuple` | 元组 | `let t: tuple = (1, 2, 3);` |
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

### 已知限制

> **注意**: v0.6.2 版本中 weak/unowned 引用存在以下限制：
> - weak 引用通过成员访问时可能返回不正确的值
> - 建议在生产环境中谨慎使用，后续版本将修复

## 项目结构

```
bolide/
├── crates/
│   ├── bolide-cli/       # 命令行入口
│   ├── bolide-compiler/  # JIT 编译器 (Cranelift)
│   ├── bolide-parser/    # 词法/语法分析器 (PEG)
│   └── bolide-runtime/   # 运行时库
├── examples/             # 示例程序
└── README.md
```

## 许可证

MIT License
