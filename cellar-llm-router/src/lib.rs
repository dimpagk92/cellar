//! LLM Router for Cellar.
//!
//! Provides a single trait (`LlmProvider`) and three native adapters:
//! - `AnthropicProvider` — native Anthropic Messages API. Default provider.
//! - `OpenAIProvider`    — OpenAI-compatible with configurable `BASE_URL`,
//!   covers OpenAI, OpenRouter, LiteLLM, vLLM, LM Studio, Together, Groq.
//! - `OllamaProvider`    — local models via Ollama.
//!
//! Each Cellar subsystem (agent, nl_compiler, memory, …) selects its provider
//! and model independently via env vars. See `config` and `router` for the
//! resolution rules. The Router is the only public entry point the daemon
//! needs — get a provider handle for a subsystem and call its trait methods.
//!
//! Design properties:
//! - **Pure transport.** No prompting, no agent loop, no tool dispatch. Just
//!   `complete()` and `stream()` over a uniform schema.
//! - **Object-safe trait.** `Arc<dyn LlmProvider>` is used everywhere so the
//!   daemon can swap providers and tests can substitute mocks.
//! - **No global state.** The Router is constructed from env vars at startup
//!   and passed by reference. No static singletons.
//! - **Wire-shape neutral.** Internal types follow Anthropic's content-block
//!   model; the OpenAI adapter translates flat tool_calls back and forth.

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod config;
pub mod error;
pub mod provider;
pub mod providers;
pub mod router;
pub mod types;

pub use config::{ProviderKind, SubsystemConfig};
pub use error::{LlmError, Result};
pub use provider::{LlmProvider, MockProvider};
pub use router::Router;
pub use types::{
    CompletionChunk, CompletionRequest, CompletionResponse, ContentBlock, Message, Role,
    StopReason, ToolDefinition, Usage,
};
