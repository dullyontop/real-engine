#!/usr/bin/env bash
# Prints the cargo caches' prefix key: `v0-rust-` and a hash of the root
# Cargo.toml's [profile.*] tables, without their comments (ADR-0023's amendment
# for WP31 PR "one build"). Swatinem/rust-cache hashes the members' manifests,
# Cargo.lock, rust-toolchain.toml and .cargo/config.toml, but not a virtual
# workspace's own Cargo.toml, where the profiles live. Without this, a profile
# change would keep every key: runs would restore dependencies built with the
# old profile and rebuild them, and main, finding its key already saved, would
# never save the new build.
#
# Runs on Linux, Windows (Git Bash) and macOS.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# Each [profile...] table's lines, from its header to the next table's, without
# comments and blank lines.
profiles="$(awk '
  /^[[:space:]]*\[/ { inside = ($0 ~ /^[[:space:]]*\[profile[].]/) }
  inside {
    sub(/[[:space:]]+#.*$/, "")
    if ($0 !~ /^[[:space:]]*(#|$)/) print
  }
' "$root/Cargo.toml")"
if command -v sha256sum > /dev/null; then
  hash="$(printf '%s\n' "$profiles" | sha256sum | cut -c1-12)"
else
  hash="$(printf '%s\n' "$profiles" | shasum -a 256 | cut -c1-12)"
fi
echo "v0-rust-$hash"
