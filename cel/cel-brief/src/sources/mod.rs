//! Built-in [`crate::source::Source`] implementations.
//!
//! Each submodule ships
//! one or two related impls, gated by the relevant cargo feature where
//! applicable:
//!
//! | Module | Source | Feature |
//! |---|---|---|
//! | [`system_prompt`] | [`SystemPromptSource`] | (default) |
//! | [`user_message`]  | [`UserMessageSource`]  | (default) |
//! | [`tool_catalog`]  | [`ToolCatalogSource`]  | (default) |
//! | [`history`]       | [`HistorySource`], [`HistoryStore`], [`HistoryEntry`] | (default) |
//! | `memory` (feature) | `MemorySource`, `MemoryQueryStrategy` | `memory` |
//! | `perception` (feature) | `PerceptionSource`, `PerceptionSnapshot`, `PerceptionMode`, `PerceptionError` | `perception` |
//!
//! Phase 4 (governance + polish) keeps `Governance` and `RedactionRecord` in
//! [`crate::governance`] / [`crate::receipt`]; this module is sources only.

pub mod history;
pub mod receipt;
pub mod system_prompt;
pub mod tool_catalog;
pub mod user_message;

#[cfg(feature = "memory")]
pub mod memory;

#[cfg(feature = "perception")]
pub mod perception;

pub use history::{HistoryEntry, HistorySource, HistoryStore};
pub use receipt::ReceiptSource;
pub use system_prompt::SystemPromptSource;
pub use tool_catalog::ToolCatalogSource;
pub use user_message::UserMessageSource;

#[cfg(feature = "memory")]
pub use memory::{MemoryQueryStrategy, MemorySource, QueryTextExtractor};

#[cfg(feature = "perception")]
pub use perception::{PerceptionError, PerceptionMode, PerceptionSnapshot, PerceptionSource};
