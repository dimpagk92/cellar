//! Response types — one per RPC method.

pub mod agent;
pub mod confirmation;
pub mod daemon;
pub mod ok;
pub mod rules;
pub mod subscribe;
pub mod system;
pub mod watchlists;
pub mod webhooks;

pub use ok::OkResult;
pub use subscribe::SubscribeResult;
