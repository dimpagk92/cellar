//! Core types: [`Brief`], [`BriefMessage`], [`Role`], [`BriefContext`],
//! [`TokenBudget`], [`Priority`], [`ToolSchema`], [`ImageData`], [`SourceId`].
//!
//! Implements plan §4. See
//! [cellar-cel-brief.md](file:///Users/dimitriospagkratis/.claude/plans/cellar-cel-brief.md).

use std::collections::HashMap;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::receipt::BriefReceipt;

/// Stable identifier for a [`crate::source::Source`].
///
/// Used to tag every [`BriefMessage`] / [`ToolSchema`] with its origin, and to
/// key the per-source statistics in [`BriefReceipt`]. Sources own their IDs and
/// must keep them stable across turns for receipts and debugging to make sense.
///
/// IDs should be short, snake_case, and unique within a single
/// [`crate::builder::BriefBuilder`]. A `BriefBuilder` rejects two sources with
/// the same ID at registration time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub String);

impl SourceId {
    /// Construct a new [`SourceId`] from anything string-like.
    pub fn new(id: impl Into<String>) -> Self {
        SourceId(id.into())
    }

    /// The underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SourceId {
    fn from(s: &str) -> Self {
        SourceId(s.to_owned())
    }
}

impl From<String> for SourceId {
    fn from(s: String) -> Self {
        SourceId(s)
    }
}

/// Conversation role for a [`BriefMessage`].
///
/// Mirrors the four-way role split shared by OpenAI's and Anthropic's chat
/// APIs. Providers that collapse `Tool` into a user-style message do that
/// mapping in the renderer, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The system prompt role.
    System,
    /// A user message — humans or upstream agents.
    User,
    /// An assistant message — the model's prior turn.
    Assistant,
    /// A tool result — outcome of a previously issued tool call.
    Tool,
}

/// Raw image bytes plus enough metadata for a renderer to encode them.
///
/// `cel-brief` is provider-agnostic, so we never pre-encode for OpenAI /
/// Anthropic / local wire formats here. The renderer reads `media_type` and
/// the bytes and emits whatever the target API wants (base64 data URL,
/// multi-part upload, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageData {
    /// IANA media type, e.g. `image/png`, `image/jpeg`, `image/webp`.
    pub media_type: String,
    /// Raw image bytes.
    pub bytes: Vec<u8>,
    /// Optional pixel width — populated by sources that know it.
    #[serde(default)]
    pub width: Option<u32>,
    /// Optional pixel height — populated by sources that know it.
    #[serde(default)]
    pub height: Option<u32>,
}

/// One message in a [`Brief`]'s conversation transcript.
///
/// Tagged with the [`SourceId`] of the contributing source so the
/// [`BriefReceipt`] can attribute every visible byte. Variants mirror the
/// `Text | Image | ToolCall | ToolResult` matrix from plan §4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BriefMessage {
    /// Plain text content under a role.
    Text {
        /// Role this message is attributed to.
        role: Role,
        /// Message body.
        content: String,
        /// Source that contributed the message.
        source: SourceId,
    },
    /// Image content under a role (typically `User` for vision turns).
    Image {
        /// Role this image is attributed to.
        role: Role,
        /// The image payload.
        data: ImageData,
        /// Optional alt / caption text for accessibility and fall-back.
        #[serde(default)]
        alt: Option<String>,
        /// Source that contributed the image.
        source: SourceId,
    },
    /// A tool invocation the model previously emitted. Carries the original
    /// tool call ID so a [`BriefMessage::ToolResult`] can be matched to it.
    ToolCall {
        /// Provider-issued tool-call ID.
        id: String,
        /// Tool name (matches a [`ToolSchema::name`]).
        name: String,
        /// JSON arguments the model passed.
        args: Value,
        /// Source that contributed the call (typically a history source).
        source: SourceId,
    },
    /// The result returned for a prior tool call.
    ToolResult {
        /// Tool-call ID this result responds to.
        id: String,
        /// Serialised result content.
        content: String,
        /// Source that contributed the result.
        source: SourceId,
    },
}

impl BriefMessage {
    /// The [`SourceId`] that produced this message.
    pub fn source(&self) -> &SourceId {
        match self {
            BriefMessage::Text { source, .. }
            | BriefMessage::Image { source, .. }
            | BriefMessage::ToolCall { source, .. }
            | BriefMessage::ToolResult { source, .. } => source,
        }
    }
}

/// Provider-agnostic description of a tool the model can call.
///
/// `input_schema` is JSON Schema. Renderers translate to the
/// provider-specific shape (OpenAI `tools[]`, Anthropic `tools[]`, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Stable tool name. Must match the `name` on any
    /// [`BriefMessage::ToolCall`] that targets this tool.
    pub name: String,
    /// Human-readable description used by the model to decide when to call.
    pub description: String,
    /// JSON Schema describing the tool's input arguments.
    pub input_schema: Value,
    /// Source that contributed this tool.
    pub source: SourceId,
}

/// Priority bucket for a [`crate::source::Source`] / [`crate::source::Contribution`].
///
/// Drives both ordering and the per-priority floor in [`TokenBudget`].
/// Sources should pick the lowest priority that still preserves correctness:
/// over-claiming `Critical` defeats the budget's purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Lowest priority. Pruned first when over budget.
    Low,
    /// Default priority for most sources.
    Normal,
    /// High-value contributions (tools, fresh perception).
    High,
    /// Must-keep contributions (system prompt, the user's message).
    Critical,
}

impl Priority {
    /// All priority levels in ascending order. Useful for budget pruning
    /// loops that sweep from low to high.
    pub const ALL: [Priority; 4] = [
        Priority::Low,
        Priority::Normal,
        Priority::High,
        Priority::Critical,
    ];
}

/// Token budget for one [`Brief`].
///
/// `total` is the inclusive ceiling for the assembled brief; the builder
/// reserves `reserve_for_response` tokens for the model's reply and prunes
/// contributions until the visible total fits within
/// `total - reserve_for_response`. `floor_per_priority` lets callers
/// guarantee a minimum allocation per [`Priority`] so a flood of
/// [`Priority::Low`] contributions can't squeeze out [`Priority::Critical`]
/// content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Inclusive ceiling for prompt + reserved response, in tokens.
    pub total: usize,
    /// Tokens held back from the prompt so the model has room to reply.
    pub reserve_for_response: usize,
    /// Minimum tokens guaranteed per priority bucket (best-effort: a higher
    /// priority can still borrow from a lower floor when over budget).
    pub floor_per_priority: HashMap<Priority, usize>,
}

impl TokenBudget {
    /// Build a budget with `total` tokens, `reserve_for_response` held back,
    /// and no per-priority floors. Floors can be added with
    /// [`TokenBudget::with_floor`].
    pub fn new(total: usize, reserve_for_response: usize) -> Self {
        TokenBudget {
            total,
            reserve_for_response,
            floor_per_priority: HashMap::new(),
        }
    }

    /// Set the minimum tokens guaranteed for `priority`. Returns `self` for
    /// chaining.
    pub fn with_floor(mut self, priority: Priority, floor: usize) -> Self {
        self.floor_per_priority.insert(priority, floor);
        self
    }

    /// Tokens available to the prompt (i.e. `total - reserve_for_response`),
    /// saturating at zero if the reserve exceeds the total.
    pub fn prompt_budget(&self) -> usize {
        self.total.saturating_sub(self.reserve_for_response)
    }
}

impl Default for TokenBudget {
    /// A reasonable default for an 8k-class model: 8000 total, 1024 reserved
    /// for the response, no per-priority floors.
    fn default() -> Self {
        TokenBudget::new(8000, 1024)
    }
}

/// Per-turn input handed to every [`crate::source::Source`].
///
/// Sources read from `ctx` to decide what to contribute. `turn`, `goal`, and
/// `user_message` are caller-supplied; `budget` is informational here
/// (enforcement is the builder's job); `now` lets sources timestamp without
/// reaching for the clock directly, which keeps tests deterministic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefContext {
    /// Turn number within the current session. Monotonically increasing.
    pub turn: u64,
    /// Optional running goal carried across turns.
    #[serde(default)]
    pub goal: Option<String>,
    /// The user's latest message, if any.
    #[serde(default)]
    pub user_message: Option<String>,
    /// Token budget for this turn.
    pub budget: TokenBudget,
    /// Wall-clock time the builder considers "now". Sources should prefer
    /// this over `SystemTime::now()` so tests are reproducible.
    pub now: SystemTime,
}

impl BriefContext {
    /// Construct a [`BriefContext`] with the supplied budget and
    /// `now = SystemTime::now()`. The remaining optional fields default to
    /// `None` / `0`.
    pub fn new(budget: TokenBudget) -> Self {
        BriefContext {
            turn: 0,
            goal: None,
            user_message: None,
            budget,
            now: SystemTime::now(),
        }
    }

    /// Set the turn number.
    pub fn with_turn(mut self, turn: u64) -> Self {
        self.turn = turn;
        self
    }

    /// Set the running goal.
    pub fn with_goal(mut self, goal: impl Into<String>) -> Self {
        self.goal = Some(goal.into());
        self
    }

    /// Set the user message.
    pub fn with_user_message(mut self, message: impl Into<String>) -> Self {
        self.user_message = Some(message.into());
        self
    }

    /// Override `now` (used in tests for deterministic time).
    pub fn with_now(mut self, now: SystemTime) -> Self {
        self.now = now;
        self
    }
}

/// The assembled, budgeted, governance-reviewed bundle handed to the LLM.
///
/// Produced by [`crate::builder::BriefBuilder::build`] (Phase 2). The
/// [`BriefReceipt`] makes the assembly process auditable — per-source token
/// counts, dropped items, redactions, and timings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    /// Optional system prompt assembled from contributing sources.
    #[serde(default)]
    pub system: Option<String>,
    /// Conversation messages, in render order.
    pub messages: Vec<BriefMessage>,
    /// Tool schemas exposed to the model this turn.
    pub tools: Vec<ToolSchema>,
    /// Receipt detailing how this brief was assembled.
    pub receipt: BriefReceipt,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_id_round_trips_through_string() {
        let id = SourceId::new("system_prompt");
        assert_eq!(id.as_str(), "system_prompt");
        assert_eq!(format!("{id}"), "system_prompt");

        let from_str: SourceId = "history".into();
        let from_string: SourceId = String::from("memory").into();
        assert_eq!(from_str, SourceId::new("history"));
        assert_eq!(from_string, SourceId::new("memory"));
    }

    #[test]
    fn priority_all_is_sorted_ascending() {
        let all = Priority::ALL;
        for window in all.windows(2) {
            assert!(
                window[0] < window[1],
                "{:?} should be < {:?}",
                window[0],
                window[1]
            );
        }
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn token_budget_prompt_budget_subtracts_reserve() {
        let budget = TokenBudget::new(1000, 200);
        assert_eq!(budget.prompt_budget(), 800);

        // Saturating: reserve > total → 0, not underflow.
        let zero = TokenBudget::new(100, 500);
        assert_eq!(zero.prompt_budget(), 0);
    }

    #[test]
    fn token_budget_with_floor_inserts_floor() {
        let budget = TokenBudget::new(1000, 100)
            .with_floor(Priority::Critical, 200)
            .with_floor(Priority::High, 100);
        assert_eq!(
            budget.floor_per_priority.get(&Priority::Critical),
            Some(&200)
        );
        assert_eq!(budget.floor_per_priority.get(&Priority::High), Some(&100));
        assert!(!budget.floor_per_priority.contains_key(&Priority::Low));
    }

    #[test]
    fn brief_message_reports_its_source() {
        let sid = SourceId::new("test");
        let msg = BriefMessage::Text {
            role: Role::User,
            content: "hi".into(),
            source: sid.clone(),
        };
        assert_eq!(msg.source(), &sid);

        let tool_call = BriefMessage::ToolCall {
            id: "call_1".into(),
            name: "search".into(),
            args: serde_json::json!({"q": "rust"}),
            source: sid.clone(),
        };
        assert_eq!(tool_call.source(), &sid);
    }

    #[test]
    fn brief_context_builder_helpers_set_fields() {
        let budget = TokenBudget::new(4000, 512);
        let ctx = BriefContext::new(budget.clone())
            .with_turn(7)
            .with_goal("ship phase 1")
            .with_user_message("hi");
        assert_eq!(ctx.turn, 7);
        assert_eq!(ctx.goal.as_deref(), Some("ship phase 1"));
        assert_eq!(ctx.user_message.as_deref(), Some("hi"));
        assert_eq!(ctx.budget, budget);
    }

    #[test]
    fn types_round_trip_through_serde_json() {
        let msg = BriefMessage::Text {
            role: Role::System,
            content: "be helpful".into(),
            source: SourceId::new("sys"),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: BriefMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, back);

        let tool = ToolSchema {
            name: "echo".into(),
            description: "echoes input".into(),
            input_schema: serde_json::json!({"type": "object"}),
            source: SourceId::new("tools"),
        };
        let json = serde_json::to_string(&tool).expect("serialize tool");
        let back: ToolSchema = serde_json::from_str(&json).expect("deserialize tool");
        assert_eq!(tool, back);
    }
}
