#!/usr/bin/env bash
# lint-guard-extraction-crates.sh
#
# Workstream B from plans/cellar-oss-extraction-prep.md §B.
#
# Asserts that each OSS-extraction-candidate crate's non-dev `dependencies`
# set is a subset of an explicit allowlist. This is the "no reverse deps
# back into Cellar" guarantee — without it, a single careless `use cel_…`
# import lands in src/ and the extraction stops being a mechanical move.
#
# Allowlist matrix (must match plan §B):
#
#   cel-memory:         async-trait, chrono, serde, serde_json, thiserror,
#                       tokio, uuid, tracing
#
#   cel-memory-sqlite:  cel-memory (the only `cel-*` dep allowed),
#                       async-trait, chrono, rusqlite, serde, serde_json,
#                       sqlite-vec, thiserror, tokio, uuid, zerocopy,
#                       fastembed (optional), tracing, reqwest
#
#   cel-brief:          cel-memory (the only `cel-*` dep allowed),
#                       async-trait, serde, serde_json, thiserror, tokio,
#                       tracing, tiktoken-rs
#
# Exits non-zero on the first violation, listing the offending dep + crate.
# This script reads `cargo metadata` JSON, so it catches both direct
# `Cargo.toml` deps and any deps brought in implicitly through workspace
# inheritance — `rg "^use cel_"` (the lighter check in
# .github/workflows/extraction-readiness.yml) does not.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Allowlist per crate (function-based for bash 3.2 compatibility — macOS
# ships bash 3.2 by default and `declare -A` requires bash 4+).
# Keep in sync with plans/cellar-oss-extraction-prep.md §B.
allowed_for() {
  case "$1" in
    cel-memory)
      echo "async-trait chrono serde serde_json thiserror tokio uuid tracing"
      ;;
    cel-memory-sqlite)
      # TODO(extraction): cellar-llm-router is a Cellar-internal crate added
      # by the Phase 3 summarizer (commits 56f1b7a → 8f3e68f → b03be21).
      # Before extracting cel-memory-sqlite, do ONE of:
      #   1. Rename cellar-llm-router → cel-llm-router and extract it as a
      #      4th candidate crate alongside.
      #   2. Split AnthropicSummarizer + OllamaSummarizer impls into a new
      #      `cel-memory-summarizers` crate that depends on cellar-llm-router;
      #      cel-memory-sqlite then drops back to extraction-clean.
      #   3. Have the impls call providers via reqwest directly without the
      #      router abstraction (loses retry/auth/model-selection plumbing).
      # See cellar-oss-extraction-prep.md §11 follow-ups.
      echo "cel-memory async-trait chrono rusqlite serde serde_json sqlite-vec thiserror tokio uuid zerocopy fastembed tracing reqwest cellar-llm-router"
      ;;
    cel-brief)
      echo "cel-memory async-trait serde serde_json thiserror tokio tracing tiktoken-rs"
      ;;
    *)
      echo ""
      ;;
  esac
}

# Deterministic iteration order.
CRATES=("cel-memory" "cel-memory-sqlite" "cel-brief")

if ! command -v jq >/dev/null 2>&1; then
  echo "lint-guard: this script requires jq. Install via 'brew install jq' or apt." >&2
  exit 2
fi

VIOLATIONS=0
for crate in "${CRATES[@]}"; do
  allowed="$(allowed_for "$crate")"
  echo "==> $crate"

  # `--no-deps` returns only the workspace member's own dependencies, without
  # transitively resolving the full graph. `jq` filters out anything where
  # the dependency kind is "dev" (dev-deps don't propagate to crates.io
  # consumers, so they don't gate extraction). `kind: null` means a normal
  # runtime dependency; `kind: "build"` is also a propagated dep so we keep
  # those too.
  deps=$(
    cargo metadata --format-version 1 --no-deps \
      --manifest-path "cel/$crate/Cargo.toml" \
    | jq -r --arg name "$crate" '
        .packages[]
        | select(.name == $name)
        | .dependencies[]
        | select(.kind != "dev")
        | .name
      ' \
    | sort -u
  )

  for dep in $deps; do
    if ! [[ " $allowed " == *" $dep "* ]]; then
      echo "  VIOLATION: '$dep' is not in the allowlist for $crate" >&2
      echo "             allowed = $allowed" >&2
      VIOLATIONS=$((VIOLATIONS + 1))
    else
      echo "  ok  $dep"
    fi
  done
done

if [ "$VIOLATIONS" -gt 0 ]; then
  echo >&2
  echo "lint-guard: $VIOLATIONS violation(s). Update the allowlist in" >&2
  echo "  scripts/lint-guard-extraction-crates.sh AND plans/cellar-oss-extraction-prep.md §B" >&2
  echo "OR remove the offending dep from the crate." >&2
  exit 1
fi

echo
echo "lint-guard: all candidate crates respect the allowlist."
