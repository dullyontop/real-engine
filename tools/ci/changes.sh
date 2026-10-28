#!/usr/bin/env bash
# Decides which of ci.yml's checks a run needs (ADR-0023's amendment of
# 2026-10-28: a private repository's CI minutes are billed). Prints five lines,
# `fmt=`, `code=`, `full=`, `msrv=` and `deny=`, each `true` or `false`, and
# appends them to $GITHUB_OUTPUT when that is set.
#
# - fmt: every run but the scheduled ones.
# - code (clippy, test, xtask): the change touches anything but documentation,
#   which is the files under docs/ and the Markdown files at the root.
# - full (the jobs that build release `real`: verify-*, xvfb-smoke,
#   build-package, build-web): a pull request that touches anything but
#   documentation, or a push to main that changes a build input.
# - msrv and deny: the change touches a build input (Cargo.lock, a Cargo.toml,
#   rust-toolchain.toml, .cargo/, deny.toml, .github/ or tools/ci/). deny also
#   runs on every scheduled run, msrv on the weekly one.
# A run whose change cannot be told (a manual run, a push whose previous head is
# unknown) runs everything.
#
# Environment: EVENT (github.event_name), BEFORE (github.event.before, for a
# push) and SCHEDULE (github.event.schedule, for a scheduled run). A pull
# request is diffed as HEAD^1..HEAD, since GitHub checks out the PR merged into
# its base; a push as BEFORE..HEAD.
set -euo pipefail

# ci.yml's weekly cron line; its daily one runs deny alone.
weekly="17 5 * * 1"

docs_only=true
inputs=false
known=false
case "${EVENT:-}" in
  pull_request)
    range=(HEAD^1 HEAD)
    known=true
    ;;
  push)
    if [ -n "${BEFORE:-}" ] && git cat-file -e "${BEFORE}^{commit}" 2>/dev/null; then
      range=("$BEFORE" HEAD)
      known=true
    fi
    ;;
esac

if $known; then
  while IFS= read -r file; do
    [ -n "$file" ] || continue
    case "$file" in
      docs/*) ;;
      */*) docs_only=false ;;
      *.md) ;;
      *) docs_only=false ;;
    esac
    case "$file" in
      Cargo.lock | Cargo.toml | */Cargo.toml | rust-toolchain.toml | .cargo/* | deny.toml | .github/* | tools/ci/*)
        inputs=true
        ;;
    esac
  done < <(git diff --name-only "${range[@]}")
fi

fmt=true code=true full=true msrv=true deny=true
case "${EVENT:-}" in
  schedule)
    fmt=false code=false full=false deny=true
    if [ "${SCHEDULE:-}" = "$weekly" ]; then msrv=true; else msrv=false; fi
    ;;
  pull_request | push)
    if $known; then
      if $docs_only; then code=false; fi
      if [ "$EVENT" = pull_request ]; then full=$code; else full=$inputs; fi
      msrv=$inputs
      deny=$inputs
    fi
    ;;
esac

for line in "fmt=$fmt" "code=$code" "full=$full" "msrv=$msrv" "deny=$deny"; do
  echo "$line"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "$line" >> "$GITHUB_OUTPUT"; fi
done
