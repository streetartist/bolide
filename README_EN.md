<p align="center">
  <img src="./bolide_logo.png" alt="Bolide Logo" width="200">
  <br>
  <b style="font-size: 32px;">Bolide</b>
  <br>
  <i>Modern JIT/AOT Compiled Programming Language</i>
  <br>
</p>

<p align="center">
  <a href="https://opensource.org/licenses/MIT">
    <img src="https://img.shields.io/badge/License-MIT-brightgreen.svg" alt="License: MIT">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/version-0.7.1-blue.svg" alt="Version">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/platform-windows%20%7C%20linux-lightgrey.svg" alt="Platform">
  </a>
</p>

---

**Bolide** is a modern programming language with JIT/AOT compilation based on **Cranelift**, combining clean syntax with native performance.

## Features

- **JIT Compilation** - Native performance via Cranelift, fast startup
- **AOT Compilation** - Compile to native executables, no runtime needed
- **Async Coroutines** - First-class async/await support
- **FFI** - Seamless C library interop with callback support
- **Module System** - Namespace-isolated module imports
- **Rich Types** - BigInt, Decimal, Dynamic, and more
- **Concurrency** - Threads, channels, thread pools
- **Memory Management** - ARC + lifetime annotations + weak/unowned references

## Quick Start

### Build from Source

```bash
# Clone repository
git clone https://github.com/your-repo/bolide.git
cd bolide

# Build
cargo build --release

# Run program
cargo run --release -- run examples/hello.bl
```

### Using Release Version

After downloading the Release package for your platform:

```bash
# Windows
bolide.exe run your_program.bl

# Linux / macOS
./bolide run your_program.bl
```

### AOT Compilation

Compile Bolide programs to standalone native executables:

```bash
# Compile to executable
bolide compile your_program.bl -o your_program

# Windows generates your_program.exe
# Linux/macOS generates your_program

# Run the compiled program directly
./your_program
```

AOT compilation advantages:
- **No runtime needed** - Generated executables run independently
- **Faster startup** - Skip JIT compilation phase
- **Easy distribution** - Single file deployment, no dependencies

## Syntax Examples

### Variables and Types

```bolide
let x: int = 42;
let pi: float = 3.14159;
let name: str = "Bolide";
let flag: bool = true;
let big: bigint = 123456789012345678901234567890b;
let precise: decimal = 3.14159265358979d;
```

### User Input

Use `input()` function to read user input from stdin (Python-like):

```bolide
// Input with prompt
let name: str = input("Enter your name: ");
print(name);

// Input without prompt
let content: str = input();
```

### Type Conversion

Bolide provides complete type conversion functions:

```bolide
// int() - convert to integer
let a: int = int(3.7);       // float -> int (truncate) = 3
let b: int = int("123");     // str -> int = 123

// float() - convert to float
let e: float = float(100);       // int -> float = 100.0
let f: float = float("2.718");   // str -> float = 2.718

// str() - convert to string
let h: str = str(12345);         // int -> str = "12345"
let i: str = str(3.14159);       // float -> str = "3.14159"
let j: str = str(true);          // bool -> str = "true"
```

### Functions

```bolide
fn add(a: int, b: int) -> int {
    return a + b;
}

fn greet(name: str) {
    print(name);
}
```

### Control Flow

```bolide
// if-elif-else
if x > 0 {
    print("positive");
} elif x < 0 {
    print("negative");
} else {
    print("zero");
}

// for loop - Python-style range
for i in range(5) { print(i); }           // 0, 1, 2, 3, 4
for i in range(3, 7) { print(i); }        // 3, 4, 5, 6

// for loop - list iteration
let nums: list<int> = [10, 20, 30];
for n in nums {
    print(n);
}

// while loop
while x > 0 {
    x = x - 1;
}
```

### List Operations

```bolide
let nums: list<int> = [3, 1, 4, 1, 5, 9];

// Basic operations
nums.push(10);           // append element
let x: int = nums.pop(); // pop last element
print(nums.len());       // get length

// Index access
print(nums[0]);          // get element
nums[0] = 100;           // set element

// Search
print(nums.contains(4)); // contains value (returns 0 or 1)
print(nums.index_of(4)); // find index (-1 if not found)

// Modification
nums.reverse();          // reverse in place
nums.sort();             // sort in place
```

### Dictionaries

```bolide
// Strongly typed dictionary
let scores: dict<str, int> = {"Alice": 100, "Bob": 90};
print(scores["Alice"]);  // 100

// Mixed type dictionary (auto-inferred as dict<dynamic, dynamic>)
let profile = {"name": "Bolide", 1: "Version", "active": true};

// Common operations
scores["Charlie"] = 95;     // insert/update
scores.remove("Bob");       // delete
print(scores.len());        // get length
```

### Async/Await

```bolide
async fn fetch_data(id: int) -> int {
    return id * 10;
}

// Start coroutines
let f1: future = fetch_data(1);
let f2: future = fetch_data(2);

// Wait for results
let r1: int = await f1;
let r2: int = await f2;
```

### Multithreading

#### Spawn & Join

```bolide
fn heavy_work(id: int) -> int {
    return id * id;
}

// Start new thread
let t: future = spawn heavy_work(10);

// Wait for thread and get result
let result: int = join(t);
```

#### Thread Pool

```bolide
pool(4) {
    // Tasks run concurrently in 4 worker threads
    spawn task(1);
    spawn task(2);
    spawn task(3);
}
// Pool block auto-waits for all tasks
```

#### Channels

```bolide
// Create channel
let ch: channel<int> = channel();

fn sender(c: channel<int>) {
    c <- 42;
}

spawn sender(ch);
let val: int = <- ch;  // receive data
```

### Module System

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

### Classes and OOP

```bolide
class Point {
    x: int;
    y: int;

    fn distance() -> int {
        return self.x * self.x + self.y * self.y;
    }
}

let p: Point = Point(3, 4);
print(p.distance());  // 25
```

### FFI (C Interop)

```bolide
extern "msvcrt.dll" {
    fn abs(x: c_int) -> c_int;
    fn sqrt(x: c_double) -> c_double;
}

let a: int = abs(-42);      // 42
let b: float = sqrt(16.0);  // 4.0
```

## Type System

| Type | Description | Example |
|------|-------------|---------|
| `int` | 64-bit integer | `let x: int = 42;` |
| `float` | 64-bit float | `let pi: float = 3.14;` |
| `bool` | Boolean | `let flag: bool = true;` |
| `str` | String | `let s: str = "hello";` |
| `bigint` | Arbitrary precision integer | `let b: bigint = 999b;` |
| `decimal` | High precision decimal | `let d: decimal = 3.14d;` |
| `list<T>` | Generic list | `let l: list<int> = [1, 2, 3];` |
| `dict<K, V>` | Dictionary | `let d: dict<str, int> = {"a": 1};` |
| `channel<T>` | Channel | `let ch: channel<int> = channel();` |
| `future` | Coroutine Future | `let f: future = async_fn();` |

## Project Structure

```
bolide/
├── crates/
│   ├── bolide-cli/       # CLI entry point
│   ├── bolide-compiler/  # JIT compiler (Cranelift)
│   ├── bolide-parser/    # Lexer/Parser (PEG)
│   └── bolide-runtime/   # Runtime library
├── vscode-bolide/        # VS Code extension
├── examples/             # Example programs
└── README.md
```

## VS Code Extension

Bolide provides a VS Code extension with syntax highlighting and one-click run support.

### Installation

#### Method 1: Copy to Extensions Folder (Recommended)

Copy the `vscode-bolide` folder to VS Code extensions directory:

- **Windows**: `%USERPROFILE%\.vscode\extensions\`
- **macOS**: `~/.vscode/extensions/`
- **Linux**: `~/.vscode/extensions/`

Then restart VS Code.

#### Method 2: Package as VSIX

```bash
cd vscode-bolide
npm install
npm install -g @vscode/vsce
vsce package
```

Then in VS Code, press `Ctrl+Shift+P`, type "Install from VSIX", and select the generated `.vsix` file.

### Configuration

Configure the Bolide executable path in VS Code settings:

```json
{
  "bolide.executablePath": "D:\\Project\\bolide_new\\target\\release\\bolide.exe"
}
```

### Usage

1. Open a `.bl` file
2. Press `Ctrl+Shift+R` to run the current file

## License

MIT License
