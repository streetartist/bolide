#!/usr/bin/env bash
set -euo pipefail

output_dir="$1"
abis="${2:-arm64-v8a,x86_64}"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
sdk_root="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"
if [[ -z "$sdk_root" ]]; then
  echo "ANDROID_HOME or ANDROID_SDK_ROOT is required" >&2
  exit 1
fi
ndk_root="${ANDROID_NDK_HOME:-$(find "$sdk_root/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -1)}"
host_tag="linux-x86_64"
[[ "$(uname -s)" == "Darwin" ]] && host_tag="darwin-x86_64"
tool_bin="$ndk_root/toolchains/llvm/prebuilt/$host_tag/bin"
command -v perl >/dev/null || { echo "Perl is required to build OpenSSL for Android" >&2; exit 1; }

IFS=',' read -ra abi_list <<< "$abis"
for abi in "${abi_list[@]}"; do
  case "$abi" in
    arm64-v8a) rust_target=aarch64-linux-android; clang_target=aarch64-linux-android ;;
    armeabi-v7a) rust_target=armv7-linux-androideabi; clang_target=armv7a-linux-androideabi ;;
    x86) rust_target=i686-linux-android; clang_target=i686-linux-android ;;
    x86_64) rust_target=x86_64-linux-android; clang_target=x86_64-linux-android ;;
    *) echo "Unsupported ABI: $abi" >&2; exit 1 ;;
  esac
  env_key="$(echo "$rust_target" | tr '[:lower:]-' '[:upper:]_')"
  linker="$tool_bin/${clang_target}26-clang"
  export "CARGO_TARGET_${env_key}_LINKER=$linker"
  export "CC_$(echo "$rust_target" | tr '-' '_')=$linker"
  export "AR_$(echo "$rust_target" | tr '-' '_')=$tool_bin/llvm-ar"
  export ANDROID_NDK_HOME="$ndk_root"
  cargo build --manifest-path "$repo_root/Cargo.toml" -p bolide-android --target "$rust_target" --release
  mkdir -p "$output_dir/$abi"
  cp "$repo_root/target/$rust_target/release/libbolide_android.so" "$output_dir/$abi/"
done
