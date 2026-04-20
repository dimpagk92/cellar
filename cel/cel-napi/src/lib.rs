//! CEL Node.js Native Bindings
//!
//! Exposes the CEL unified context API to TypeScript via napi-rs.

// All public functions in this crate are consumed via FFI (napi-rs), not Rust calls.
// The test harness incorrectly reports them as unused.
#![allow(dead_code)]
// The ctor crate (used by napi-rs for module registration) triggers cfg warnings
// on newer Rust compilers. Safe to suppress until napi-rs updates ctor.
#![allow(unexpected_cfgs)]

use napi_derive::napi;

mod adapter_registry;
mod cdp;
mod goal_runner;
mod context;
mod cortex;
mod input;
mod llm;
mod planner;
mod store;
mod watchdog;

/// Shared Tokio runtime — created once, reused across all async operations.
static TOKIO_RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

pub(crate) fn rt_handle() -> napi::Result<tokio::runtime::Handle> {
    Ok(TOKIO_RT
        .get_or_init(|| {
            tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime")
        })
        .handle()
        .clone())
}

/// Get the CEL runtime version.
#[napi]
pub fn cel_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
