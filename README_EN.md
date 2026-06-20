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
    <img src="https://img.shields.io/badge/version-0.13.3-blue.svg" alt="Version">
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
- **First-Class Functions** - Functions as values: pass, store in lists, return; higher-order `map`/`filter`
- **Async Coroutines** - First-class async/await support
- **Bidirectional FFI** - Call C libraries (with callbacks); also compile Bolide to a static library callable from C (`export fn` + `.h` generation)
- **Module System** - Namespace-isolated module imports
- **Source Diagnostics** - `run`, `compile`, and the REPL report filenames, line/column numbers, source snippets, carets, and targeted help
- **Rich Types** - BigInt, Decimal, Dynamic, and more
- **Concurrency** - Threads, channels, thread pools
- **Memory Management** - ARC (atomic refcounts) + checked lifetime annotations + liveness-checked weak/unowned references

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

### Source Diagnostics

`bolide run`, `bolide compile`, and the REPL print source-aware diagnostics. Syntax errors use exact parser locations; common semantic errors such as undefined variables/functions/channels, unknown methods, missing required arguments, and import failures are mapped back to the most relevant source token with a short help message.

Example:

```bolide
let x = missing_name + 1;
print(x);
```

The CLI reports:

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

The same diagnostic layer is used by JIT runs, AOT builds, and static-library builds. In the REPL, diagnostics are shown with `<repl>:line:column`, the source line, and a caret marker.

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

### First-Class Functions

Functions are first-class values: assign to variables, pass as arguments, return from functions, store in lists. Type annotations are optional — the compiler infers function signatures.

```bolide
fn add1(x: int) -> int { return x + 1; }
fn double(x: int) -> int { return x * 2; }

// Assign a function to a variable (no annotation needed), then call
let f = add1;
print(f(10));            // 11

// Explicit function type annotation (optional)
let g: func(int) -> int = double;
print(g(10));            // 20

// Function as parameter: user-defined higher-order function
fn apply(callback: func(int) -> int, x: int) -> int {
    return callback(x);
}
print(apply(double, 21));  // 42

// Returning a function
fn pick(which: int) -> func(int) -> int {
    if which == 0 { return add1; }
    return double;
}
print(pick(0)(7));       // 8
print(pick(1)(7));       // 14

// Store functions in a list, call by index
let fns: list<func(int) -> int> = [add1, double];
print(fns[0](5));        // 6
print(fns[1](5));        // 10
```

### Higher-Order List Methods (map / filter)

`map` applies a callback to each element (may change element type); `filter` keeps elements where the callback returns true. Callbacks can be any named function.

```bolide
fn double(x: int) -> int { return x * 2; }
fn is_even(x: int) -> bool { return x % 2 == 0; }
fn label(n: int) -> str { return "n=" + str(n); }

let nums: list<int> = [1, 2, 3, 4];

print(nums.map(double));     // [2, 4, 6, 8]
print(nums.filter(is_even)); // [2, 4]
print(nums.map(label));      // ["n=1", "n=2", "n=3", "n=4"]  (cross-type map)

// float callbacks supported too
fn scale(x: float) -> float { return x * 2.0; }
let fs: list<float> = [1.5, 2.5, 3.5];
print(fs.map(scale));        // [3, 5, 7]
```

> Note: `map`/`filter` type inference is fully available **inside functions**; at top level (global scope), annotate the result type explicitly.

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

Bolide supports **bidirectional** C interop: it can call C libraries, and it can be called from C programs.

#### Bolide calls C

```bolide
extern "dyn:c" {
    fn abs(x: c_int) -> c_int;
}

extern "dyn:m" {
    fn sqrt(x: c_double) -> c_double;
}

let a: int = abs(-42);      // 42
let b: float = sqrt(16.0);  // 4.0

// Callbacks are supported too
fn my_callback(a: int, b: int) -> int { return a + b; }
let r: int = test_callback(my_callback, 10, 20);
```

#### External library specs

Use logical library names in `extern "..."` instead of hard-coding
platform filenames:

| Spec | Purpose | AOT | JIT | Notes |
|------|---------|-----|-----|-------|
| `bolide` | Bolide runtime built-ins | Direct link | Direct link | Intended for standard-library wrappers |
| `lib:name` | Native static/import-library link | Supported | Not supported | Windows maps to `name.lib`; Unix maps to `-lname` |
| `dyn:name` | Runtime dynamic loading | Supported | Supported | Windows maps to `name.dll`; Linux to `libname.so`; macOS to `libname.dylib` |
| `auto:name` | JIT dynamic loading, AOT native link | Supported | Supported | JIT behaves like `dyn:name`; AOT behaves like `lib:name` |

Common aliases:
- `dyn:c` / `dyn:m`: dynamically load the C/math runtime for the host platform.
- `lib:c` / `lib:m`: AOT-link the C/math runtime.
- `auto:c` / `auto:m`: use dynamic loading in JIT and native linking in AOT.

Do not write `extern "foo.dll"`, `extern "libfoo.so"`, or
`extern "foo.dylib"` in portable Bolide source. Use `auto:name` when you want
one binding that runs under JIT with a shared library and compiles under AOT
against a static/import library. AOT becomes single-file only if the linked
library is a true static library; an import library still requires its DLL at
runtime.

#### C calls Bolide

Mark functions with `export fn` to expose them to C (bare symbol name, no
name mangling), then compile to a static library with `--lib` and generate a
C header with `--header`:

```bolide
// mathlib.bl — exported functions use bare names for C linkage
export fn add(a: int, b: int) -> int { return a + b; }
export fn scale(x: float, k: float) -> float { return x * k; }

fn internal_helper() -> int { return 1; }  // no export -> not exposed
```

```bash
# Compile to a static library and emit the header
bolide compile mathlib.bl --lib --header
# Products: mathlib.lib (Windows) / libmathlib.a (Linux), mathlib.h
```

The C side links the Bolide library + the runtime library:

```c
#include "mathlib.h"
#include <stdio.h>

int main(void) {
    printf("add(3,4) = %lld\n", add(3, 4));            // 7
    printf("scale(2.5,4.0) = %f\n", scale(2.5, 4.0));  // 10.0
    return 0;
}
```

```bash
cl main.c mathlib.lib bolide_runtime.lib   # Windows (MSVC)
cc main.c libmathlib.a libbolide_runtime.a # Linux
```

> **C interop ABI contract**: Only **numeric and pointer signatures** are
> stable across the C boundary — `int`/`bool` map to `long long`, `float`
> maps to `double`, and other composite types (`str`/`list`/objects) are
> passed as opaque runtime pointers (`void*`) that C cannot safely construct.
> Use plain numeric/pointer signatures for C-friendly exports.

### Error Handling (try/catch/throw)

Bolide provides lightweight exception handling: `throw` raises an exception,
`try/catch` catches it, with an optional `finally` cleanup block.

```bolide
try {
    print("in try body");
    throw 42;
    print("after throw (will not print)");
} catch (e: int) {
    print("caught:");
    print(e);  // 42
} finally {
    print("cleanup runs either way");
}
```

- **`throw`** can raise a value of any type (`int`, `str`, objects, etc.)
- **`catch (e: T)`** matches exceptions by type tag, including subclass matching
- **`finally`** always runs whether or not an exception was thrown

#### Built-in `Error` class

A built-in `Error` class (single field `message: str`) is always available — no
import needed. User classes can inherit from it, and a base-class `catch` also
catches subclasses:

```bolide
// throw a built-in Error
try {
    throw Error("something broke");
} catch (e: Error) {
    print(e.message);  // something broke
}

// custom error subclass
class MyError: Error {}

try {
    throw MyError("custom failure");
} catch (e: Error) {           // base-class catch also catches subclasses
    print(e.message);          // custom failure
}
```

If you define your own class named `Error`, it overrides the built-in one.

> **Current limitation**: try/catch works within a single function (catch and
> throw must be in the same compiled function). Cross-function stack unwinding
> is planned for a future version.

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
| `func(T...) -> R` | Function value | `let f: func(int) -> int = add1;` |

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
