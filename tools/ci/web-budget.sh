#!/usr/bin/env bash
# The web runtime's size against its budget (ADR-0049's amendment for WP31 PR
# "web budget"), read from the line `cargo xtask web-test` prints:
#
#   real_bg.wasm: <bytes> bytes gzipped (budget <bytes>)
#
# Usage: tools/ci/web-budget.sh LOG, where LOG holds web-test's output. Prints a
# Markdown table of the size, the budget and the margin, and appends it to
# $GITHUB_STEP_SUMMARY when that is set, so it shows on the run's page. When the
# margin is under 5% of the budget it also prints a `::warning::` line, which
# GitHub shows on the PR's checks. The size never fails this script: web-test
# itself fails above the budget, and the table is written then too.
set -euo pipefail

log="${1:?usage: tools/ci/web-budget.sh LOG}"

# 1234567 -> 1,234,567 (and -1234 -> -1,234).
commas() {
  local n="$1" sign="" out=""
  if [ "$n" -lt 0 ]; then
    sign="-"
    n=$((-n))
  fi
  while [ "${#n}" -gt 3 ]; do
    out=",${n: -3}$out"
    n="${n:0:${#n}-3}"
  done
  echo "$sign$n$out"
}

line=""
if [ -f "$log" ]; then
  line="$(grep -Eo 'real_bg\.wasm: [0-9]+ bytes gzipped \(budget [0-9]+\)' "$log" | tail -n 1 || true)"
fi
if [ -z "$line" ]; then
  report="### Web runtime size

Not measured: \`cargo xtask web-test\` did not reach its size check."
else
  size="$(sed -E 's/^real_bg\.wasm: ([0-9]+) .*/\1/' <<< "$line")"
  budget="$(sed -E 's/.*\(budget ([0-9]+)\)$/\1/' <<< "$line")"
  margin=$((budget - size))
  percent="$(awk -v m="$margin" -v b="$budget" 'BEGIN { printf "%.2f", 100 * m / b }')"
  mb() { awk -v n="$1" 'BEGIN { printf "%.2f MB", n / 1000000 }'; }
  if [ "$margin" -ge 0 ]; then
    last="| Margin | $(commas "$margin") | $percent% of the budget |"
  else
    last="| Over the budget by | $(commas $((-margin))) | ${percent#-}% of the budget |"
  fi
  report="### Web runtime size

| | Bytes | |
|---|---:|---|
| \`real_bg.wasm\`, gzipped | $(commas "$size") | $(mb "$size") |
| Budget (ADR-0049) | $(commas "$budget") | $(mb "$budget") |
$last"
  # Under 5% of the budget left: 500,000 bytes of 10 MB.
  if [ "$margin" -lt 0 ]; then
    echo "::warning title=Web runtime size::real_bg.wasm is $(commas "$size") bytes gzipped, $(commas $((-margin))) over its $(commas "$budget")-byte budget (ADR-0049)."
  elif [ $((margin * 20)) -lt "$budget" ]; then
    echo "::warning title=Web runtime size::real_bg.wasm is $(commas "$size") bytes gzipped, $(commas "$margin") ($percent%) under its $(commas "$budget")-byte budget (ADR-0049)."
  fi
fi

echo "$report"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  echo "$report" >> "$GITHUB_STEP_SUMMARY"
fi
