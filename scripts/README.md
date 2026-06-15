# Release Packaging

Use `scripts/package_release.ps1` from the repository root to build release artifacts.

```powershell
.\scripts\package_release.ps1
```

Artifacts are written to `dist/`:

- `bolide-<version>-windows-x86_64.zip`: portable Windows package.
- `bolide-<version>-windows-x86_64.msi`: Windows MSI installer.
- `bolide-<version>-windows-x86_64-installer.zip`: Windows installer package containing `install.ps1` and payload.
- `bolide-<version>-linux-x86_64-install.sh`: self-contained Linux installer script.

Each package includes:

- `bolide` / `bolide.exe`
- `libbolide_runtime.a` / `bolide_runtime.lib`
- `std/`

## Installers

Windows user install:

```powershell
msiexec /i .\bolide-0.12.1-windows-x86_64.msi
```

Alternative PowerShell installer:

```powershell
Expand-Archive .\bolide-0.12.1-windows-x86_64-installer.zip
.\bolide-0.12.1-windows-x86_64-installer\install.ps1
```

Windows machine install:

```powershell
.\bolide-0.12.1-windows-x86_64-installer\install.ps1 -Machine
```

Linux install:

```sh
chmod +x bolide-0.12.1-linux-x86_64-install.sh
sudo ./bolide-0.12.1-linux-x86_64-install.sh
```

Linux custom prefix:

```sh
PREFIX="$HOME/.local" ./bolide-0.12.1-linux-x86_64-install.sh
```

Both installers overwrite the previous Bolide files in the same install directory, so upgrades use the same command as first install.

## Builder Dependencies

Windows packaging host:

- Rust toolchain with Cargo.
- MSVC build tools or compatible Windows SDK libraries needed by the Rust `windows` crates.
- `lld-link` available on `PATH` for Bolide AOT output linking.
- PowerShell 7 or Windows PowerShell 5.1.
- Zig on `PATH`.
- `cargo-zigbuild`: `cargo install cargo-zigbuild`.
- Rust Linux target: the script runs `rustup target add x86_64-unknown-linux-gnu`.
- `tar` available on `PATH`.
- .NET SDK 8 or newer. The script installs the WiX CLI into `.tools/wix` if `wix` is not already on `PATH`.

Linux user dependencies:

- `sh`, `tar`, `base64`, `ln`, `mkdir`, `chmod`.
- For AOT compilation on Linux after installation: a C compiler/linker such as `gcc` or `clang`, plus glibc development files for the target distro.

Windows user dependencies:

- For `bolide run`: no extra runtime files beyond the installed package.
- For `bolide compile`: Windows C/C++ build tools with `lld-link` or a compatible linker and Windows SDK libraries on `PATH`.
