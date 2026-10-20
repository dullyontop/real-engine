#!/usr/bin/env bash
# Installs wasi-sdk 34 (Linux x86_64) and sets the variables that build Luau and Jolt for
# wasm32-unknown-unknown (ADR-0049 §1). In GitHub Actions the variables go to $GITHUB_ENV;
# elsewhere, `eval "$(tools/ci/wasi-sdk.sh)"` sets them in the calling shell.
#
# WASI_SDK_PATH, when already set to an installed wasi-sdk 34, is used as is.
set -euo pipefail

version=34
url="https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${version}/wasi-sdk-${version}.0-x86_64-linux.tar.gz"
# wasi-sdk publishes no checksum file; this is the archive's sha256, taken on 2026-10-20.
sha256="b761e3a0721dbae9c09a0059e5fdb2bf917d1b4a8a7b430fb3b5aafb0984b2c4"

sdk="${WASI_SDK_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/real-engine/wasi-sdk-${version}.0-x86_64-linux}"
if [ ! -x "$sdk/bin/clang++" ]; then
  mkdir -p "$(dirname "$sdk")"
  archive="$(mktemp)"
  curl -fsSL --retry 3 -o "$archive" "$url"
  echo "$sha256  $archive" | sha256sum -c - >&2
  tar -xzf "$archive" -C "$(dirname "$sdk")"
  rm -f "$archive"
fi

sysroot="$sdk/share/wasi-sysroot"
lib="$sysroot/lib/wasm32-wasip1"
vars=(
  "WASI_SDK_PATH=$sdk"
  "CC_wasm32_unknown_unknown=$sdk/bin/clang"
  "CXX_wasm32_unknown_unknown=$sdk/bin/clang++"
  "AR_wasm32_unknown_unknown=$sdk/bin/llvm-ar"
  "CXXSTDLIB_wasm32_unknown_unknown=c++"
  # Luau throws C++ exceptions (cc reads these flags for this target only); Chromium refuses modules that mix the legacy and the new
  # (exnref) encodings, so only the new one is emitted.
  "CXXFLAGS_wasm32_unknown_unknown=--target=wasm32-wasip1 --sysroot=$sysroot -fwasm-exceptions -mllvm -wasm-use-legacy-eh=false"
  # The C++ runtime with exceptions, wasi-libc, and the process clocks Luau's os.clock uses.
  "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS=-L $lib/eh -L $lib -l static=c++abi -l static=unwind -l static=c -l static=wasi-emulated-process-clocks"
)

if [ -n "${GITHUB_ENV:-}" ]; then
  printf '%s\n' "${vars[@]}" >> "$GITHUB_ENV"
else
  for v in "${vars[@]}"; do
    printf 'export %s=%q\n' "${v%%=*}" "${v#*=}"
  done
fi
