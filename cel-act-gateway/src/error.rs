//! Gateway errors.

use thiserror::Error;

/// Errors the gateway can return from `intercept`.
#[derive(Debug, Error)]
pub enum GatewayError {
    /// The underlying actuator failed.
    #[error("actuator error: {0}")]
    Actuator(String),

    /// The confirmation broker errored before the user could respond.
    #[error("confirmation broker error: {0}")]
    Broker(String),

    /// Writing to the memory subsystem failed. Note this is a hard error —
    /// audit-write failure is not silently ignored.
    #[error("memory error: {0}")]
    Memory(#[from] cel_memory::MemoryError),

    /// Caller passed an invalid argument.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}
