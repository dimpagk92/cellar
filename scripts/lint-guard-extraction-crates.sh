#!/usr/bin/env bash
# lint-guard-extraction-crates.sh
#
# Asserts that each OSS-extraction-candidate crate's non-dev `dependencies`
# set is a subset of an explicit allowlist. This is the "no reverse deps
# back into Cellar" guarantee — without it, a single careless `use cel_…`
# import lands in src/ and the extraction stops being a mechanical move.
#
# It also encodes the cel-cortex / cel-memory / cel-memory-sqlite / cel-brief
# ownership split directly (see the "ownership invariants" block below):
#   - no candidate crate may depend on cel-cortex or cel-brief
#   - cel-brief may depend on cel-memory ONLY as an optional (feature-gated) dep
#
# Allowlist matrix:
#
#   cel-memory:         async-trait, chrono, serde, serde_json, thiserror,
#                       tokio, uuid, tracing
#
#   cel-memory-sqlite:  cel-memory (the only `cel-*` dep allowed),
#                       async-trait, chrono, rusqlite, serde, serde_json,
#                       sqlite-vec, thiserror, tokio, uuid, zerocopy,
#                       fastembed (optional), tracing
#
#   cel-brief:          cel-memory (the only `cel-*` dep allowed, and only
#                       behind the `memory` feature),
#                       async-trait, futures-util, serde, serde_json,
#                       thiserror, tokio, tracing, tiktoken-rs
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
allowed_for() {
  case "$1" in
    cel-memory)
      echo "async-trait chrono serde serde_json thiserror tokio uuid tracing"
      ;;
    cel-memory-sqlite)
      # The summarizer is injected via the `cel_memory::Summarizer` trait, so
      # the concrete LLM-client impl lives in a downstream crate — this crate
      # no longer pulls a router or reqwest. The only `cel-*` dep is cel-memory.
      echo "cel-memory async-trait chrono rusqlite serde serde_json sqlite-vec thiserror tokio uuid zerocopy fastembed tracing"
      ;;
    cel-brief)
      echo "cel-memory async-trait futures-util serde serde_json thiserror tokio tracing tiktoken-rs"
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

# ─── Ownership invariants (explicit, beyond the allowlist) ──────────────────
# Encode the cel-cortex / cel-memory / cel-memory-sqlite / cel-brief ownership
# split directly so a future allowlist edit can't silently re-admit a
# live-perception or brief-assembly dependency:
#   - no candidate may depend on cel-cortex (live world/device context) or
#     cel-brief (per-turn LLM brief assembly)
#   - cel-brief may depend on cel-memory ONLY as an optional (feature-gated) dep
echo "==> ownership invariants"
for crate in "${CRATES[@]}"; do
  meta=$(cargo metadata --format-version 1 --no-deps \
    --manifest-path "cel/$crate/Cargo.toml")
  for forbidden in cel-cortex cel-brief; do
    [ "$crate" = "$forbidden" ] && continue
    if echo "$meta" \
      | jq -e --arg n "$crate" --arg f "$forbidden" '
          .packages[]
          | select(.name == $n)
          | .dependencies[]
          | select(.kind != "dev")
          | select(.name == $f)
        ' >/dev/null; then
      echo "  VIOLATION: $crate depends on $forbidden (ownership boundary)" >&2
      VIOLATIONS=$((VIOLATIONS + 1))
    fi
  done
done

# cel-brief's cel-memory dep must be optional (pulled only by the `memory`
# feature). An unconditional dep would make cel-memory part of cel-brief's
# default graph — a boundary regression.
brief_mem_optional=$(
  cargo metadata --format-version 1 --no-deps \
    --manifest-path "cel/cel-brief/Cargo.toml" \
  | jq -r '
      .packages[]
      | select(.name == "cel-brief")
      | .dependencies[]
      | select(.name == "cel-memory")
      | .optional
    '
)
if [ "$brief_mem_optional" = "true" ]; then
  echo "  ok  cel-brief -> cel-memory is optional (feature-gated)"
else
  echo "  VIOLATION: cel-brief's cel-memory dep must be optional (feature-gated)," >&2
  echo "             got optional=${brief_mem_optional:-<absent>}" >&2
  VIOLATIONS=$((VIOLATIONS + 1))
fi

if [ "$VIOLATIONS" -gt 0 ]; then
  echo >&2
  echo "lint-guard: $VIOLATIONS violation(s). Update the allowlist / ownership" >&2
  echo "  invariants in scripts/lint-guard-extraction-crates.sh" >&2
  echo "OR remove the offending dep from the crate." >&2
  exit 1
fi

echo
echo "lint-guard: all candidate crates respect the allowlist."
