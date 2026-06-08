//! Request parameter types — one struct per RPC method.
//!
//! Submodules mirror the RFC's RPC group structure
//! (`system`, `daemon`, `rules`, `watchlists`, `webhooks`, `events`,
//! `fires`, `agent_actions`, `confirmation`, `agent`, `settings`). Empty
//! params (`daemon.status`, `rules.list` without filter, etc.) are
//! represented as unit structs so the handler signature stays uniform.

pub mod agent;
pub mod agent_actions;
pub mod confirmation;
pub mod daemon;
pub mod events;
pub mod fires;
pub mod gateway;
pub mod memory;
pub mod rules;
pub mod settings;
pub mod stream_filter;
pub mod system;
pub mod watchlists;
pub mod webhooks;
