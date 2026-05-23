//! Cellar IPC — JSON-RPC 2.0 over Unix domain socket.
//!
//! Implements the wire-level protocol specified in
//! `/Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md`. The
//! daemon hosts the server; clients (Tauri app, `cellar` CLI, integration
//! tests, future remote tools) connect over `~/.cellar/daemon.sock`.
//!
//! Key design commitments (from the RFC):
//!
//! - **One socket, one protocol.** JSON-RPC 2.0 with notifications used as
//!   subscription frames. No HTTP, no SSE, no websockets.
//! - **Line-delimited JSON framing.** One JSON-RPC message per `\n`.
//! - **File-permission auth only.** Socket is `0600` for the owning user.
//! - **Type-safe.** Every request, response, and stream frame is a typed
//!   Rust shape in this crate; consumers compile against these types.
//! - **Locked contract.** Adding a method or frame variant is a backward-
//!   compatible extension; renaming or removing one is a breaking change
//!   and requires a protocol-version bump.
//!
//! v1 Phase 1 ships:
//! - The complete type surface (every method's `Params` + `Result`, every
//!   stream's frame variants, every error code from the RFC).
//! - The line-delimited JSON-RPC 2.0 envelope codec.
//! - A [`Server`] that accepts UDS connections and dispatches to a
//!   [`Handler`].
//! - A [`Client`] that connects, calls RPCs, and subscribes to streams.
//! - [`StubHandler`] returning [`IpcError::NotImplemented`] for every
//!   method whose backing subsystem (rules storage, agent runtime, etc.)
//!   isn't wired yet — `system.*` and `daemon.status` have real bodies.

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod client;
pub mod codec;
pub mod envelope;
pub mod error;
pub mod handler;
pub mod params;
pub mod results;
pub mod server;
pub mod stub;
pub mod subscription;

// Convenient re-exports — the symbols every caller will name.
pub use client::Client;
pub use codec::{read_message, write_message, Message};
pub use envelope::{ErrorObject, JsonRpcError, JsonRpcRequest, JsonRpcResponse, RequestId};
pub use error::{IpcError, IpcResult};
pub use handler::Handler;
pub use server::{serve_connection, Server};
pub use stub::StubHandler;
pub use subscription::{StreamFrame, StreamName, SubscriptionId};
