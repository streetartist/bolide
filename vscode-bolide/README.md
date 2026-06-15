# Bolide VS Code Extension

VS Code extension for the Bolide programming language.

## Features

- Syntax highlighting for current Bolide `.bl` syntax:
  - declarations: `fn`, `async fn`, `export fn`, `class`, `enum`, `union`, `extern`, `struct`, `type`
  - control flow: `if`/`elif`/`else`, `while`, `for in`, `match`, `try`/`catch`/`finally`, `throw`
  - concurrency: `async`, `await`, `await scope`, `spawn`, `spawn all`, `spawn select`, `pool`, channel `<-`, `select`
  - ownership and lifetimes: `owned`, `ref`, `from`, `weak`, `unowned`
  - types: `int`, `float`, `bool`, `str`, `bytes`, `bigint`, `decimal`, `dynamic`, `ptr`, `future`, `list<T>`, `dict<K,V>`, `channel<T>`, `func(...) -> T`
  - FFI C ABI types: `c_int`, `c_double`, `*char`, `*void`, `size_t`, fixed-width integer types, and `fn(...) -> ...` C function pointers
  - literals: strings, booleans, `none`, decimal/bigint suffixes, hex integers, and numeric separators
  - operators: `->`, `=>`, `<-`, compound assignment, bitwise operators, spread `*`/`**`
- Context-aware highlighting for `spawn all`: `all` is not treated as a global keyword, so calls like `database.all("posts")` remain normal method calls.
- Editor language configuration for comments, bracket matching, indentation, word selection, and block-comment continuation.
- Commands to run the current file with JIT or compile/run it with AOT.

## Commands

| Command | Default shortcut | Description |
| --- | --- | --- |
| `Bolide: Run Current File` | `Ctrl+Shift+R` / `Cmd+Shift+R` | Runs `bolide run current_file.bl` |
| `Bolide: Build Current File (AOT)` | `Ctrl+Shift+B` / `Cmd+Shift+B` | Compiles current file to a native executable, then runs it |

## Configuration

Set the Bolide executable path in VS Code settings:

```json
{
  "bolide.executablePath": "D:\\Project\\bolide\\target\\release\\bolide.exe"
}
```

If this setting is empty, the extension searches:

1. `bolide` / `bolide.exe` from `PATH`
2. `bolide` / `bolide.exe` in the workspace root
3. `target/release/bolide` / `target/release/bolide.exe` under the workspace root

## Development

```bash
cd vscode-bolide
npm install
npm run compile
```

Press `F5` in VS Code to launch an Extension Development Host.

## Packaging

```bash
cd vscode-bolide
npm install
npm install -g @vscode/vsce
vsce package
```

Install the generated `.vsix` with `Extensions: Install from VSIX...`.

## License

MIT
