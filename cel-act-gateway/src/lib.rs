//! The `cel_act` gateway — the trust-and-execution wedge in code.
//!
//! Every actuation call from any source flows through this one chokepoint:
//! the embedded agent's tool dispatch (in-process inside the daemon),
//! external MCP clients (Cursor, Codex, Claude Desktop, etc.), and the CLI.
//! The gateway synthesises an [`Event`] of kind `agent_action_attempted`,
//! runs the rule matcher synchronously over it, and decides what to do.
//!
//! Five decisions (one per rule action variant plus the no-match default):
//!
//! | Matched rule action | Gateway decision | Outcome |
//! |---|---|---|
//! | (no match) / `log_only` | Allow | Action executes immediately. |
//! | `webhook` | Allow (with webhook intent recorded) | Action executes; webhook sender (Phase 1 follow-up) POSTs. |
//! | `require_confirmation` | Pause | Gateway awaits a [`ConfirmationDecision`] from the broker; on Allow the action executes, on Deny it returns [`ActionOutcome::ConfirmationDenied`]. |
//! | `veto` | Veto | Action returns [`ActionOutcome::Vetoed`] without executing. |
//! | `soft_block` | Veto-with-countermeasure | Action returns Vetoed; countermeasure dispatch is the daemon's responsibility (Phase 1.x). |
//!
//! Every intercepted call writes one or more memory chunks (`Action` kind for
//! the attempt + outcome; `Fire` kind for each matched rule). The locked
//! [`MemoryProvider`] surface is the source of truth — the gateway holds an
//! `Arc<dyn MemoryProvider>` and never knows what backs it.
//!
//! See `/Users/dimitriospagkratis/.claude/plans/cellar-app-v1.md` §6, §7.3, §10.1.
//!
//! [`Event`]: cellar_types::Event
//! [`MemoryProvider`]: cel_memory::MemoryProvider

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod action;
pub mod decision;
pub mod error;
pub mod gateway;
pub mod test_support;
pub mod traits;

pub use action::{ActionOutcome, ConfirmationDecision, ConfirmationRequest, ProposedAction};
pub use decision::{Decision, FiredRuleSnapshot};
pub use error::GatewayError;
pub use gateway::Gateway;
pub use traits::{Actuator, ConfirmationBroker, RuleSource};
