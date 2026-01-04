<p align="center">
  <img src="./bolide_logo.png" alt="Bolide Logo" width="200">
  <br>
  <b style="font-size: 32px;">Bolide</b>
  <br>
  <i>A Modern JIT-Compiled Programming Language</i>
  <br>
</p>

<p align="center">
  <a href="https://opensource.org/licenses/MIT">
    <img src="https://img.shields.io/badge/License-MIT-brightgreen.svg" alt="License: MIT">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/version-0.1.0-blue.svg" alt="Version">
  </a>
  <a href="#">
    <img src="https://img.shields.io/badge/platform-windows%20%7C%20linux-lightgrey.svg" alt="Platform">
  </a>
  <a href="./README.md">
    <img src="https://img.shields.io/badge/lang-中文-blue.svg" alt="中文">
  </a>
</p>

---

**Bolide** is a modern programming language with JIT compilation powered by **Cranelift**. It combines simple syntax with native performance.

## Features

- **JIT Compilation** - Native performance via Cranelift
- **Async/Await** - First-class coroutine support
- **FFI** - Seamless C library integration with callbacks
- **Module System** - Namespace-isolated imports
- **Rich Types** - BigInt, Decimal, Dynamic types
- **Concurrency** - Threads, channels, thread pools

## Quick Start

### Build from Source

```bash
git clone https://github.com/your-repo/bolide.git
cd bolide
cargo build --release
cargo run --release -- run examples/hello.bl
```

### Using Release Binary

```bash
# Windows
bolide.exe run your_program.bl

# Linux / macOS
./bolide run your_program.bl
```

## Syntax

### Variables and Types

```bolide
let x: int = 42;
let pi: float = 3.14159;
let name: str = "Bolide";
let big: bigint = 123456789012345678901234567890b;
```

### Functions

```bolide
fn add(a: int, b: int) -> int {
    return a + b;
}
```

### Module System

```bolide
import "math_utils.bl";
let result: int = math_utils.add(10, 20);
```

### FFI

```bolide
extern "msvcrt.dll" {
    fn abs(x: c_int) -> c_int;
}
let a: int = abs(-42);  // 42
```

## License

MIT License
