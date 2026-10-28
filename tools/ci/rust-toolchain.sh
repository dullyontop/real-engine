#!/usr/bin/env bash
# Installs the Rust toolchain rust-toolchain.toml pins, with its profile and
# components, makes it the default, and uninstalls every other toolchain, such
# as the runner image's own stable (ADR-0023's amendment for WP31 PR
# "toolchain"). Swatinem/rust-cache hashes the version of every installed
# toolchain into its keys, so with the image's stable left in place the image's
# next Rust update would still change every key, which is what the pin is for.
#
# Runs on Linux, Windows (Git Bash) and macOS, before the cargo cache. A job
# that also needs another release (msrv's 1.95.0) installs it afterwards.
set -euo pipefail

rustup toolchain install
pinned="$(rustup show active-toolchain | cut -d' ' -f1)"
rustup default "$pinned"
for toolchain in $(rustup toolchain list --quiet); do
  if [ "$toolchain" != "$pinned" ]; then
    rustup toolchain uninstall "$toolchain"
  fi
done
rustup toolchain list
