//! Natural-language → compiled `Rule` compiler.
//!
//! Runs once per rule, at authoring time. **Never** runs during rule
//! evaluation. The matcher is deterministic and offline; this crate is the
//! only place an LLM touches the rules pipeline.
//!
//! Flow:
//! 1. User types `"alert me when a file >1GB is deleted from Documents"`.
//! 2. [`Compiler::compile`] sends a prompt to the configured LLM provider
//!    (resolved by the daemon via `cellar-llm-router` for the
//!    `nl_compiler` subsystem).
//! 3. The LLM returns JSON matching `cellar-types::rule::Rule`.
//! 4. The compiler extracts and validates the JSON. On validation failure,
//!    it retries once with the validator's error fed back into the prompt.
//! 5. On success, [`CompileResult`] is returned with the draft rule and a
//!    human-readable summary the UI shows to the user for confirmation.
//!
//! The compiler never persists the rule. Save is a separate daemon RPC.

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod compiler;
pub mod error;
pub mod parse;
pub mod prompt;
pub mod summary;
pub mod validate;

pub use compiler::{CompileRequest, CompileResult, Compiler};
pub use error::CompileError;
pub use summary::summarize_rule;
