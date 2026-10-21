#!/usr/bin/env bash
# Installs binaryen 133 (Linux x86_64) for `wasm-opt`, which shrinks the browser build
# (ADR-0049). In GitHub Actions its bin/ goes to $GITHUB_PATH; elsewhere,
# `eval "$(tools/ci/binaryen.sh)"` puts it on the calling shell's PATH.
#
# BINARYEN_PATH, when already set to an installed binaryen 133, is used as is.
set -euo pipefail

version=133
url="https://github.com/WebAssembly/binaryen/releases/download/version_${version}/binaryen-version_${version}-x86_64-linux.tar.gz"
# The release's own .sha256 file, checked on 2026-10-21.
sha256="2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489"

dir="${BINARYEN_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/real-engine/binaryen-version_${version}}"
if [ ! -x "$dir/bin/wasm-opt" ]; then
  # Unpacks into a scratch folder and moves it into place, so an interrupted run never
  # leaves a partial install that the next run would take as installed.
  mkdir -p "$(dirname "$dir")"
  tmp="$(mktemp -d "$(dirname "$dir")/.binaryen.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL --retry 3 -o "$tmp/binaryen.tar.gz" "$url"
  echo "$sha256  $tmp/binaryen.tar.gz" | sha256sum -c - >&2
  tar -xzf "$tmp/binaryen.tar.gz" -C "$tmp"
  rm -rf "$dir"
  mv "$tmp/binaryen-version_${version}" "$dir"
fi
if [ ! -x "$dir/bin/wasm-opt" ]; then
  echo "binaryen.sh: no wasm-opt in $dir" >&2
  exit 1
fi

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$dir/bin" >> "$GITHUB_PATH"
else
  printf 'export PATH=%q:"$PATH"\n' "$dir/bin"
fi
