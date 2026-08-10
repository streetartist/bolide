param(
    [Parameter(Mandatory = $true)][string]$OutputDir,
    [string]$Abis = "arm64-v8a,x86_64"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$sdkRoot = if ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT } elseif ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { throw "ANDROID_HOME or ANDROID_SDK_ROOT is required" }
$ndkRoot = if ($env:ANDROID_NDK_HOME) {
    $env:ANDROID_NDK_HOME
} else {
    (Get-ChildItem (Join-Path $sdkRoot "ndk") -Directory | Sort-Object Name -Descending | Select-Object -First 1).FullName
}
$toolBin = Join-Path $ndkRoot "toolchains\llvm\prebuilt\windows-x86_64\bin"
$clang = Join-Path $toolBin "clang.exe"
$llvmAr = Join-Path $toolBin "llvm-ar.exe"
$api = "26"

# OpenSSL's Android configuration requires Unix path semantics. Keep Perl,
# shell and make in one MSYS2 environment so paths and quoting stay consistent.
$msysBin = "C:\msys64\usr\bin"
$unixPerl = Join-Path $msysBin "perl.exe"
$unixMake = Join-Path $msysBin "make.exe"
if (-not (Test-Path -LiteralPath $unixPerl)) { throw "MSYS2 Perl is required to build OpenSSL for Android" }
if (-not (Test-Path -LiteralPath $unixMake)) { throw "MSYS2 make is required to build OpenSSL for Android" }
$env:Path = "$msysBin;$env:Path"
$env:OPENSSL_SRC_PERL = $unixPerl
$env:PERL = $unixPerl.Replace("\", "/")
# OpenSSL writes the MSYS spelling `/usr/bin/perl` into its Makefile. Let the
# absolute PERL environment value override that assignment. OpenSSL's build is
# otherwise single-threaded, so give GNU make a bounded parallel job count.
$buildJobs = if ($env:BOLIDE_BUILD_JOBS) {
    [int]$env:BOLIDE_BUILD_JOBS
} else {
    [Math]::Min([Environment]::ProcessorCount, 8)
}
if ($buildJobs -lt 1) { throw "BOLIDE_BUILD_JOBS must be a positive integer" }
$env:MAKEFLAGS = "-e -j$buildJobs"
Write-Host "Building Android Rust libraries with up to $buildJobs parallel jobs"

$targets = @{
    "arm64-v8a" = @{ Rust = "aarch64-linux-android"; Clang = "aarch64-linux-android"; Env = "AARCH64_LINUX_ANDROID" }
    "armeabi-v7a" = @{ Rust = "armv7-linux-androideabi"; Clang = "armv7a-linux-androideabi"; Env = "ARMV7_LINUX_ANDROIDEABI" }
    "x86" = @{ Rust = "i686-linux-android"; Clang = "i686-linux-android"; Env = "I686_LINUX_ANDROID" }
    "x86_64" = @{ Rust = "x86_64-linux-android"; Clang = "x86_64-linux-android"; Env = "X86_64_LINUX_ANDROID" }
}

foreach ($abi in $Abis.Split(",", [System.StringSplitOptions]::RemoveEmptyEntries)) {
    $abi = $abi.Trim()
    if (-not $targets.ContainsKey($abi)) { throw "Unsupported ABI: $abi" }
    $config = $targets[$abi]
    $linker = Join-Path $toolBin "$($config.Clang)$api-clang.cmd"
    if (-not (Test-Path -LiteralPath $linker)) { throw "NDK linker not found: $linker" }

    Set-Item -Path "Env:CARGO_TARGET_$($config.Env)_LINKER" -Value $linker
    # Do not give `cc` the NDK .cmd wrapper here. It resolves that wrapper to
    # a path containing a Windows backslash; OpenSSL then runs the path through
    # an MSYS shell, where the backslash is treated as an escape. The native
    # clang driver accepts the same Android target explicitly.
    Set-Item -Path "Env:CC_$($config.Rust.Replace('-', '_'))" -Value $clang.Replace("\", "/")
    Set-Item -Path "Env:CFLAGS_$($config.Rust.Replace('-', '_'))" -Value "--target=$($config.Clang)$api"
    Set-Item -Path "Env:AR_$($config.Rust.Replace('-', '_'))" -Value $llvmAr.Replace("\", "/")
    # OpenSSL's MSYS shell treats backslashes as escapes, so expose the NDK
    # root using portable forward slashes while keeping the linker executable
    # itself as a normal Windows path for Cargo.
    $portableNdkRoot = $ndkRoot.Replace("\", "/")
    $env:ANDROID_NDK_HOME = $portableNdkRoot
    $env:ANDROID_NDK_ROOT = $portableNdkRoot

    & cargo build --manifest-path (Join-Path $repoRoot "Cargo.toml") -p bolide-android --target $config.Rust --release
    if ($LASTEXITCODE -ne 0) { throw "Rust build failed for $abi" }

    $abiOut = Join-Path $OutputDir $abi
    New-Item -ItemType Directory -Force -Path $abiOut | Out-Null
    Copy-Item -Force -LiteralPath (Join-Path $repoRoot "target\$($config.Rust)\release\libbolide_android.so") -Destination (Join-Path $abiOut "libbolide_android.so")
}
