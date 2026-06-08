//! `agent.*` request parameters.

use serde::{Deserialize, Serialize};

/// Params for `agent.sessions.list`. Currently empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSessionsListParams {}

/// Params for `agent.sessions.create`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSessionsCreateParams {
    /// Optional initial title for the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Params for `agent.sessions.get` and `agent.sessions.delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionIdParams {
    /// Session ID.
    pub session_id: String,
}

/// Params for `agent.sessions.rename`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSessionsRenameParams {
    /// Session ID.
    pub session_id: String,
    /// New title.
    pub title: String,
}

/// Params for `agent.message`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMessageParams {
    /// Session ID.
    pub session_id: String,
    /// User message content.
    pub content: String,
}

/// Params for `agent.run` — a one-shot goal execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunParams {
    /// The natural-language goal for the agent to carry out.
    pub goal: String,
    /// Plan only — instruct the agent to describe what it would do without
    /// dispatching tools. Defaults to false.
    #[serde(default)]
    pub dry_run: bool,
}

/// Params for `agent.chat.subscribe`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentChatSubscribeParams {
    /// Session ID to subscribe to.
    pub session_id: String,
}

/// Params for `agent.interrupt`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInterruptParams {
    /// Session ID to interrupt.
    pub session_id: String,
}
