//! Shared schemas and matcher for Cellar daemon and clients.
//!
//! This crate is the source of truth for:
//! - The event envelope flowing through the daemon's bus (`event`)
//! - The rule schema and the three rule kinds (`rule`)
//! - The expression language used in rule `match` clauses (`expression`)
//! - The matcher itself — pure function over events and rules (`matcher`)
//! - Watchlists, webhooks, confirmations, agent types, IPC shapes
//!
//! The matcher is pure logic: no async, no I/O, no allocations beyond the result
//! vector. All evaluation is synchronous and side-effect-free. The daemon wires
//! it to SQLite (for watchlists) and the event bus (for events) via the
//! `WatchlistLookup` trait.

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod agent;
pub mod confirmation;
pub mod event;
pub mod expression;
pub mod ipc;
pub mod matcher;
pub mod rule;
pub mod watchlist;
pub mod webhook;

// Convenient re-exports
pub use event::{Event, EventKind, EventSource};
pub use expression::{Expression, Operator};
pub use matcher::{MatchResult, Matcher, WatchlistLookup};
pub use rule::{Action, ActionType, Rule, RuleKind};
pub use watchlist::{InMemoryWatchlists, Watchlist};
pub use webhook::WebhookConfig;
