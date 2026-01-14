# Bolide VS Code Extension

VS Code extension for the Bolide programming language with syntax highlighting and one-click run support.

## Features

- **Syntax Highlighting**: Full syntax highlighting for `.bl` files including:
  - Keywords (`fn`, `let`, `class`, `if`, `while`, `for`, `async`, `await`, `spawn`, etc.)
  - Types (`int`, `float`, `str`, `bigint`, `decimal`, `list`, `dict`, `channel`, `future`)
  - Literals (integers, floats, bigint `123B`, decimal `3.14d`, strings, booleans)
  - Comments (line `//` and block `/* */`)
  - Operators and channel operations (`<-`, `=>`, `->`)

- **Run Command**: Run the current Bolide file with `Ctrl+Shift+R` (or `Cmd+Shift+R` on macOS)

## Build Bolide Compiler

Before using the extension, you need to build the Bolide compiler:

```bash
# Navigate to the Bolide project root
cd D:\Project\bolide_new

# Build in release mode
cargo build --release

# The executable will be at: target/release/bolide.exe
```

## Installation

### Method 1: Install from VSIX (Recommended)

1. Build the VSIX package:
   ```bash
   cd vscode-bolide
   npm install
   npm install -g @vscode/vsce
   vsce package
   ```

2. Install in VS Code:
   - Open VS Code
   - Press `Ctrl+Shift+P` and type "Install from VSIX"
   - Select the generated `bolide-x.x.x.vsix` file

### Method 2: Copy to Extensions Folder

Copy the `vscode-bolide` folder directly to VS Code extensions directory:

- **Windows**: `%USERPROFILE%\.vscode\extensions\`
- **macOS**: `~/.vscode/extensions/`
- **Linux**: `~/.vscode/extensions/`

Then restart VS Code.

### Method 3: Development Mode

1. Open the `vscode-bolide` folder in VS Code
2. Run `npm install` to install dependencies
3. Press `F5` to launch Extension Development Host

## Configuration

Configure the Bolide executable path in VS Code settings:

1. Open Settings (`Ctrl+,`)
2. Search for "Bolide"
3. Set `bolide.executablePath` to the path of your Bolide executable

Or add to your `settings.json`:

```json
{
  "bolide.executablePath": "D:\\Project\\bolide_new\\target\\release\\bolide.exe"
}
```

## Usage

1. Open a `.bl` file
2. Configure the Bolide executable path (prompted on first run)
3. Press `Ctrl+Shift+R` to run the file, or use the play button in the editor title bar

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+Shift+R` | Run current Bolide file |

## License

MIT
