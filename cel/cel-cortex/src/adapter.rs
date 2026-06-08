//! Adapter system — re-exported from the `cel-adapter-sdk` crate.
//!
//! The adapter contract (the [`AdapterDriver`] trait, manifest types,
//! [`ActionResult`]/[`AdapterError`], and the discovery/registration helpers
//! such as [`RegisteredAdapter`], [`discover_adapters`], and
//! [`adapter_actions_from_manifests`]) now lives in the standalone
//! `cel-adapter-sdk` crate. This decouples adapter authors from the full
//! perception engine: adapters depend on the thin SDK, not on `cel-cortex`.
//!
//! This module re-exports the entire SDK surface unchanged, so every existing
//! `cel_cortex::adapter::*` and `cel_cortex::*` path keeps resolving exactly
//! as before. See `cel-adapter-sdk` for the definitions and tests.

pub use cel_adapter_sdk::*;
