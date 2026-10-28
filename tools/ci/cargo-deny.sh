#!/usr/bin/env bash
# Installs cargo-deny 0.20.2 (Linux x86_64, the release's static musl build), so
# the `deny` job no longer compiles it on every run (ADR-0023's amendment of
# 2026-10-28). In GitHub Actions its folder goes to $GITHUB_PATH; elsewhere,
# `eval "$(tools/ci/cargo-deny.sh)"` puts it on the calling shell's PATH.
#
# CARGO_DENY_PATH, when already set to an installed cargo-deny 0.20.2, is used as is.
set -euo pipefail

version=0.20.2
name="cargo-deny-${version}-x86_64-unknown-linux-musl"
url="https://github.com/EmbarkStudios/cargo-deny/releases/download/${version}/${name}.tar.gz"
# The release's own .sha256 file, checked on 2026-10-28.
sha256="9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"

dir="${CARGO_DENY_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/real-engine/${name}}"
if [ ! -x "$dir/cargo-deny" ]; then
  # Unpacks into a scratch folder and moves it into place, so an interrupted run never
  # leaves a partial install that the next run would take as installed.
  mkdir -p "$(dirname "$dir")"
  tmp="$(mktemp -d "$(dirname "$dir")/.cargo-deny.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL --retry 3 -o "$tmp/cargo-deny.tar.gz" "$url"
  echo "$sha256  $tmp/cargo-deny.tar.gz" | sha256sum -c - >&2
  tar -xzf "$tmp/cargo-deny.tar.gz" -C "$tmp"
  rm -rf "$dir"
  mv "$tmp/$name" "$dir"
fi
if [ "$("$dir/cargo-deny" --version)" != "cargo-deny $version" ]; then
  echo "cargo-deny.sh: $dir/cargo-deny is not cargo-deny $version" >&2
  exit 1
fi

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$dir" >> "$GITHUB_PATH"
else
  printf 'export PATH=%q:"$PATH"\n' "$dir"
fi
