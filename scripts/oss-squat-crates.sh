#!/usr/bin/env bash
# oss-squat-crates.sh
#
# "Squat" the OSS-extraction-candidate crate names on crates.io with a
# placeholder publish so we don't lose the name before the real extraction
# or first real release. Without a squat publish, any other crate author can
# take `cel-memory` / `cel-memory-sqlite` / `cel-brief` and we'd be stuck
# renaming our public surface forever.
#
# This script is INTENTIONALLY dry-run by default. Running it as-is hits the
# crates.io API without actually publishing — it validates that the package
# would publish (manifest is valid, all files are present, deps resolve to
# published versions), and prints what would be uploaded.
#
# To ACTUALLY squat the names, uncomment the four `cargo publish` lines
# below (one per crate plus the workspace lock-down line) AFTER:
#
#   1. Logging in: `cargo login` with a token from https://crates.io/me
#   2. Confirming the version bumped to `0.0.0` in each Cargo.toml (or in
#      the workspace `[workspace.package].version` if you want to publish
#      the whole workspace at the placeholder version).
#   3. Reading the dry-run output below and confirming the file list is
#      what you expect (no `.env`, no large binaries, no leaked secrets).
#
# Order matters because of the in-workspace dep graph:
#
#   cel-memory          (no deps on the other candidates)
#       │
#       ├── cel-memory-sqlite (path dep on cel-memory)
#       │
#       └── cel-brief         (optional path dep on cel-memory via `memory` feature)
#
# Both `cel-memory-sqlite` and `cel-brief` reference `cel-memory` via path +
# version dependencies. Order still matters: publish `cel-memory` first, wait
# for crates.io to index it, then publish the two consumers. If publishing
# placeholder `0.0.0` crates, make the workspace package version and both
# consumer dependency versions agree before running the real publish lines.
#
# Usage:
#   ./scripts/oss-squat-crates.sh           # dry run — safe, no upload
#   (after uncommenting)                    # real publish — irreversible
#
# Reference: https://doc.rust-lang.org/cargo/commands/cargo-publish.html

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "==> Dry-running cel-memory (no `cel-*` deps — safest to publish first)"
cargo publish --dry-run -p cel-memory

echo
echo "==> Dry-running cel-memory-sqlite (path dep on cel-memory)"
cargo publish --dry-run -p cel-memory-sqlite

echo
echo "==> Dry-running cel-brief (optional path dep on cel-memory via `memory` feature)"
cargo publish --dry-run -p cel-brief

echo
echo "Dry runs complete. To actually squat the names on crates.io, uncomment"
echo "the four lines below and re-run this script. Order must stay cel-memory"
echo "first, then the two consumers. Each `cargo publish` is irreversible —"
echo "crates.io does not allow re-publishing the same version."
echo
# === REAL PUBLISH — DO NOT UNCOMMENT WITHOUT READING THE HEADER ABOVE ===
# cargo publish -p cel-memory
# # Wait ~30s for crates.io to index cel-memory before publishing dependents:
# sleep 30
# cargo publish -p cel-memory-sqlite
# cargo publish -p cel-brief
# === END REAL PUBLISH ===
