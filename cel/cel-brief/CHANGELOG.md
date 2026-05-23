# Changelog

All notable changes to `cel-brief` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-`0.1.0` versions develop in-workspace as part of [Cellar](https://github.com/dimpagk92/cellar);
the first published release on crates.io will be `0.1.0`. See
[`plans/cellar-oss-extraction-prep.md`](../../plans/cellar-oss-extraction-prep.md)
for the extraction roadmap and
[`plans/cellar-cel-brief.md`](../../plans/cellar-cel-brief.md) for the
implementation plan.

## [Unreleased]

## [0.1.0-pre] — 2026-05-23

### Added
- Phase 0 scaffolding: crate skeleton with module layout for `types`,
  `source`, `builder`, `governance`, `budget`, `tokenizer`, `error`,
  `receipt`. Bodies remain stubs — Phase 1+ implementation is tracked in
  [`plans/cellar-cel-brief.md`](../../plans/cellar-cel-brief.md).
- `BriefError` placeholder type with `Result` alias.
- `examples/no_cellar.rs` placeholder. Builds end-to-end once Phase 2
  (`BriefBuilder` + default `ImportanceFirst` pruning) lands.
- `memory` feature — opt-in dependency on `cel-memory` (the trait crate
  only; `cel-memory-sqlite` stays out of `cel-brief`'s graph).
- `perception` feature — placeholder for the upcoming `PerceptionSource`
  trait (Phase 4).

### Notes
- Imports only `cel-memory` (and only behind the `memory` feature) from
  the workspace — verified by
  `scripts/lint-guard-extraction-crates.sh` (added 2026-05-23).
- The trait surface (`Source`, `BriefBuilder`, `Governance`, `Tokenizer`)
  WILL break during the `0.1.0-pre` series. Extraction to crates.io is
  gated on the surface reaching v0.5+ internal stability.
