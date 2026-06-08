//! Embedded agent runtime.
//!
//! Holds the LLM provider + model handle (from `cellar-llm-router`'s
//! `agent` subsystem) and drives one **agentic turn** per `agent.message`
//! IPC call.  A turn is a loop:
//!
//! ```text
//! user message
//!   └─► write user chunk
//!         └─► retrieve context
//!               └─► LLM complete
//!                     ├─[EndTurn] write assistant chunk → publish frames → done
//!                     └─[ToolUse] for each tool_use block:
//!                                   publish ToolCallAttempt
//!                                   gateway.intercept_tool_call(...)
//!                                   publish ToolCallResult
//!                                 add assistant + tool-result messages to history
//!                                 loop ↑
//! ```
//!
//! Every turn ends with `MessageComplete` + `RequestDone` on the chat bus.
//! Sessions and chat history live in the locked [`cel_memory::MemoryProvider`].
//!
//! **Gateway coupling:** if the agent was built with `with_gateway()`, the
//! `cel_act` tool is advertised to the LLM and tool-use blocks are dispatched
//! through the governance gateway.  Without a gateway, tool definitions are
//! omitted and the system prompt tells the model actuation isn't available.
//!
//! Memory writes per turn:
//!
//! - One `ChunkKind::Chat` with `metadata.role = "user"`.
//! - One `ChunkKind::Chat` with `metadata.role = "assistant"` (final text).
//! - One `ChunkKind::Chat` with `metadata.role = "tool_result"` per resolved
//!   tool call (content = JSON stringified outcome for audit trail).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use cel_act_gateway::{ActionOutcome, AgentGateway, ProposedAction};
use cel_brief::{
    BriefBuilder, BriefContext, BriefMessage, HistoryEntry, HistorySource, MemorySource,
    Role as BriefRole, SystemPromptSource, TokenBudget, UserMessageSource,
};
use cel_memory::{
    CallerScope, ChunkKind, ChunkSource, MemoryProvider, MemoryQuery, NewMemoryChunk,
    RetrievalProfile,
};
use cellar_ipc::subscription::StreamPayload;
use cellar_llm_router::{
    CompletionRequest, CompletionResponse, ContentBlock, LlmProvider, Message, Role, StopReason,
    ToolDefinition,
};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::chat_bus::{ChatBroadcast, ChatBus};

/// Errors from the agent runtime.
#[derive(Debug, Error)]
pub enum AgentRuntimeError {
    /// LLM provider call failed.
    #[error("llm provider error: {0}")]
    Provider(#[from] cellar_llm_router::LlmError),
    /// Memory provider call failed.
    #[error("memory error: {0}")]
    Memory(#[from] cel_memory::MemoryError),
    /// Tool dispatch via gateway failed.
    #[error("gateway error: {0}")]
    Gateway(#[from] cel_act_gateway::GatewayError),
    /// Per-turn brief assembly (cel-brief) failed.
    #[error("brief assembly error: {0}")]
    Brief(#[from] cel_brief::BriefError),
}

/// Per-call result returned by `run_turn`.
#[derive(Debug, Clone)]
pub struct TurnResult {
    /// `request_id` returned by `agent.message`; correlates frames on the bus.
    pub request_id: String,
    /// Memory chunk id of the user message (= IPC `message_id`).
    pub user_message_id: String,
    /// Memory chunk id of the assistant message.
    pub assistant_message_id: String,
    /// The final assistant text (concatenated from all text blocks after the
    /// last `ToolUse` loop, or "(no response)" if the LLM produced none).
    pub assistant_text: String,
    /// Number of tool calls dispatched during the turn.
    pub tool_calls_dispatched: usize,
}

/// Embedded agent runtime. One per daemon. All fields are `Arc`-shared.
pub struct AgentRuntime {
    memory: Arc<dyn MemoryProvider>,
    llm: Arc<dyn LlmProvider>,
    model: String,
    chat_bus: ChatBus,
    max_tokens: u32,
    system_prompt: String,
    /// Optional gateway for `cel_act` tool dispatch. When set, the runtime
    /// advertises the `cel_act` tool to the LLM and dispatches tool-use blocks
    /// through the governance gateway. When absent, tool dispatch is disabled
    /// and the system prompt informs the model accordingly.
    gateway: Option<Arc<dyn AgentGateway>>,
    /// Maximum number of tool-call iterations per turn. Prevents infinite loops
    /// if the LLM keeps emitting tool calls. Default: 10.
    max_tool_iterations: usize,
    /// Set of session IDs for which an in-flight `run_turn` should abort on
    /// the next tool-loop iteration. Cleared when the interrupt is consumed.
    /// Uses `std::sync::Mutex` (not tokio) because it's held only for the
    /// duration of a `HashSet` insert or remove — never across an `.await`.
    interrupted: Arc<Mutex<HashSet<String>>>,
    /// Prompt-token ceiling handed to the per-turn `cel-brief` `BriefBuilder`.
    /// The brief prunes lowest-importance history first once the assembled
    /// prompt would exceed this; system prompt and the user's message are
    /// `Critical` and never pruned. Defaults high
    /// ([`DEFAULT_BRIEF_PROMPT_BUDGET_TOKENS`]) so chat sessions behave like
    /// the pre-brief "send everything" path until a caller tunes it down to a
    /// real model context window.
    brief_prompt_budget_tokens: usize,
    /// Top-K durable memories the brief's `MemorySource` recalls each turn
    /// (cross-session `JobSummary` / `Rollup` / `Correction` / `Context`
    /// chunks — deliberately NOT `Chat`, which `HistorySource` already replays
    /// for the current session, so there's no conversation duplication and the
    /// just-written user turn can't be recalled as a "memory"). `0` disables
    /// the source entirely. Default [`DEFAULT_MEMORY_RECALL_K`].
    memory_recall_k: usize,
}

const DEFAULT_MAX_TOKENS: u32 = 2048;
const DEFAULT_MAX_TOOL_ITERATIONS: usize = 10;
/// Default prompt-token budget for the per-turn brief. Deliberately large so
/// the brief's pruning is effectively a no-op for ordinary chat — the budget
/// machinery records token usage in the receipt without changing which
/// messages reach the model. Tune down via
/// [`AgentRuntime::with_brief_prompt_budget`] to enforce a real context window.
const DEFAULT_BRIEF_PROMPT_BUDGET_TOKENS: usize = 96_000;
/// Default top-K for the brief's cross-session `MemorySource` recall. Modest
/// so it adds a few high-signal durable memories without crowding the prompt;
/// they're Normal priority and redactable, so the budget prunes them before
/// the system prompt or the user's message. Override via
/// [`AgentRuntime::with_memory_recall`]; `0` disables recall.
const DEFAULT_MEMORY_RECALL_K: usize = 6;

const DEFAULT_SYSTEM_PROMPT_NO_TOOLS: &str =
    "You are Cellar, the embedded agent in the user's Cellar daemon. \
     Answer concisely. The cel_act tool dispatch is not yet enabled in \
     this build — if the user asks you to perform an action on their \
     desktop, explain that the action surface is coming in a near-term \
     update and offer the analysis you can provide without execution.";

const DEFAULT_SYSTEM_PROMPT_WITH_TOOLS: &str =
    "You are Cellar, the embedded agent in the user's Cellar daemon. \
     You have access to the `cel_act` tool, which executes actions on the user's \
     macOS desktop through the Cellar governance gateway. Every `cel_act` call is \
     intercepted by the rules engine before execution — the user may need to approve \
     it. Use `cel_act` to help the user with desktop tasks. Be concise and precise \
     about the actions you take. Always confirm what you did after a tool call completes.";

/// The `cel_act` tool definition advertised to the LLM when a gateway is
/// wired in.  The LLM uses this to structure its tool-use requests.
fn cel_act_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "cel_act".into(),
        description: "Execute an action on the user's macOS desktop via the Cellar governance \
                       gateway. Every call is intercepted by the rules engine — the user may \
                       be asked to approve sensitive actions."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action_type": {
                    "type": "string",
                    "description": "The action to perform. Common values: 'ax.click', \
                                    'ax.set_value', 'ax.action', 'fs.read', 'fs.write', \
                                    'fs.copy', 'fs.move', 'shell.run', 'navigate', \
                                    'browser.click', 'browser.type', 'browser.navigate'."
                },
                "action_args": {
                    "type": "object",
                    "description": "Action-specific arguments. Structure depends on action_type."
                }
            },
            "required": ["action_type"]
        }),
    }
}

/// Caller-id stamped on every chunk the agent writes.
pub const AGENT_CALLER_ID: &str = "embedded";

impl AgentRuntime {
    /// Construct a new runtime without a gateway (chat-only mode).
    pub fn new(
        memory: Arc<dyn MemoryProvider>,
        llm: Arc<dyn LlmProvider>,
        model: impl Into<String>,
        chat_bus: ChatBus,
    ) -> Self {
        Self {
            memory,
            llm,
            model: model.into(),
            chat_bus,
            max_tokens: DEFAULT_MAX_TOKENS,
            system_prompt: DEFAULT_SYSTEM_PROMPT_NO_TOOLS.to_string(),
            gateway: None,
            max_tool_iterations: DEFAULT_MAX_TOOL_ITERATIONS,
            interrupted: Arc::new(Mutex::new(HashSet::new())),
            brief_prompt_budget_tokens: DEFAULT_BRIEF_PROMPT_BUDGET_TOKENS,
            memory_recall_k: DEFAULT_MEMORY_RECALL_K,
        }
    }

    /// Signal that the in-flight turn for `session_id` (if any) should abort
    /// on its next loop iteration. Safe to call even when no turn is running —
    /// the flag is cleared the next time `run_turn` checks it.
    pub fn interrupt(&self, session_id: &str) {
        if let Ok(mut set) = self.interrupted.lock() {
            set.insert(session_id.to_string());
        }
    }

    /// Check (and clear) the interrupt flag for `session_id`.
    fn consume_interrupt(&self, session_id: &str) -> bool {
        self.interrupted
            .lock()
            .map(|mut set| set.remove(session_id))
            .unwrap_or(false)
    }

    /// Builder: attach a gateway for `cel_act` tool dispatch.
    ///
    /// Also switches the default system prompt to the one that tells the model
    /// tool dispatch is available.  A custom `with_system_prompt()` call after
    /// this will override that auto-switch.
    pub fn with_gateway(mut self, gateway: Arc<dyn AgentGateway>) -> Self {
        // Only override system prompt if still at the no-tools default.
        if self.system_prompt == DEFAULT_SYSTEM_PROMPT_NO_TOOLS {
            self.system_prompt = DEFAULT_SYSTEM_PROMPT_WITH_TOOLS.to_string();
        }
        self.gateway = Some(gateway);
        self
    }

    /// Builder: override the system prompt.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Builder: override the max-tokens cap per LLM call.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Builder: override the max tool-call iterations per turn (default 10).
    pub fn with_max_tool_iterations(mut self, n: usize) -> Self {
        self.max_tool_iterations = n;
        self
    }

    /// Builder: override the per-turn brief prompt-token budget (default
    /// [`DEFAULT_BRIEF_PROMPT_BUDGET_TOKENS`]). Set this to the model's real
    /// context window (minus the response reserve) to make the brief prune
    /// stale history instead of overflowing the provider.
    pub fn with_brief_prompt_budget(mut self, tokens: usize) -> Self {
        self.brief_prompt_budget_tokens = tokens;
        self
    }

    /// Builder: override how many durable cross-session memories the brief
    /// recalls each turn (default [`DEFAULT_MEMORY_RECALL_K`]). Pass `0` to
    /// disable the `MemorySource` — the brief then assembles from system
    /// prompt + this-session history + the user message only.
    pub fn with_memory_recall(mut self, k: usize) -> Self {
        self.memory_recall_k = k;
        self
    }

    /// Cheap accessor for the chat bus.
    pub fn chat_bus(&self) -> &ChatBus {
        &self.chat_bus
    }

    /// Drive one agentic turn end-to-end.
    ///
    /// Steps:
    /// 1. Write user chunk.
    /// 2. Retrieve recent context.
    /// 3. LLM loop (tool dispatch when `stop_reason == ToolUse`).
    /// 4. Write final assistant text chunk.
    /// 5. Publish `MessageComplete` + `RequestDone` frames.
    pub async fn run_turn(
        &self,
        session_id: &str,
        user_content: &str,
    ) -> Result<TurnResult, AgentRuntimeError> {
        let request_id = format!("req_{}", Uuid::now_v7());

        // ── 1. Write user chunk ──
        let user_chunk = self
            .memory
            .write(NewMemoryChunk {
                kind: ChunkKind::Chat,
                source: ChunkSource::Embedded,
                session_id: Some(session_id.into()),
                project_root: None,
                caller_id: AGENT_CALLER_ID.into(),
                content: user_content.into(),
                metadata: json!({ "role": "user" }),
                importance: None,
                shareable: false,
                pinned: false,
            })
            .await?;

        // ── 2. Retrieve recent context ──
        let context = self
            .memory
            .retrieve(MemoryQuery {
                text: user_content.into(),
                kinds: Some(vec![ChunkKind::Chat]),
                since: None,
                until: None,
                session_id: Some(session_id.into()),
                caller_scope: CallerScope::Own,
                project_root_prefix: None,
                k: 20,
                include_rollups: true,
                min_importance: None,
                profile: RetrievalProfile::AgentChatTurn,
                caller_id: AGENT_CALLER_ID.into(),
            })
            .await?;

        // ── 3. Assemble the initial prompt via cel-brief ──
        //
        // The per-turn assembly (system prompt + replayed history + recalled
        // memories + the user's message) flows through a `cel_brief::BriefBuilder`
        // instead of the hand-rolled `Vec<Message>` build it replaced. Sources,
        // in registration → render order:
        //   - SystemPromptSource (Critical) → `brief.system`
        //   - HistorySource      (Normal)   → this session's prior turns,
        //                                      replayed in order, pruned
        //                                      lowest-importance-first if over budget
        //   - MemorySource       (Normal)   → cross-session durable recall
        //                                      (JobSummary/Rollup/Correction/Context
        //                                      only — never Chat, so it can't
        //                                      duplicate HistorySource or recall
        //                                      the just-written user turn). Placed
        //                                      right before the user message so the
        //                                      model attends to it with the question.
        //                                      Disabled when `memory_recall_k == 0`.
        //   - UserMessageSource  (Critical) → the current user turn (always last)
        // `PerceptionSource` (live Cortex snapshot) remains a follow-up — see
        // cel-brief plan §10.1.
        let mut ordered: Vec<_> = context.into_iter().collect();
        ordered.sort_by_key(|c| c.created_at);
        let history: Vec<HistoryEntry> = ordered
            .iter()
            .filter(|chunk| chunk.id != user_chunk.id) // current turn added by UserMessageSource
            .map(|chunk| {
                let role = match chunk.metadata.get("role").and_then(|v| v.as_str()) {
                    Some("assistant") => BriefRole::Assistant,
                    _ => BriefRole::User,
                };
                HistoryEntry::Text {
                    role,
                    content: chunk.content.clone(),
                }
            })
            .collect();
        let history_window = history.len().max(1);

        let mut builder = BriefBuilder::new()
            .budget(TokenBudget::new(self.brief_prompt_budget_tokens, 0))
            .source(Arc::new(SystemPromptSource::new(
                self.system_prompt.clone(),
            )))
            .source(Arc::new(HistorySource::new(history, history_window)));
        if self.memory_recall_k > 0 {
            builder = builder.source(Arc::new(
                MemorySource::new(self.memory.clone(), AGENT_CALLER_ID, self.memory_recall_k)
                    .with_caller_scope(CallerScope::Own)
                    .with_kinds(Some(vec![
                        ChunkKind::JobSummary,
                        ChunkKind::Rollup,
                        ChunkKind::Correction,
                        ChunkKind::Context,
                    ])),
            ));
        }
        // UserMessageSource is registered last so the user's current turn is the
        // final message — `brief_message_to_llm` and the loop below depend on it.
        let builder = builder.source(Arc::new(UserMessageSource::new()));

        let brief_ctx = BriefContext::new(TokenBudget::new(self.brief_prompt_budget_tokens, 0))
            .with_user_message(user_content)
            .with_turn(ordered.len() as u64);

        let brief = match builder.build(&brief_ctx).await {
            Ok(b) => b,
            Err(e) => {
                self.publish_error(session_id, &request_id, &e.to_string(), true);
                return Err(AgentRuntimeError::Brief(e));
            }
        };

        // Compact receipt summary, stamped onto the assistant chunk's metadata
        // below so the assembly is auditable from the activity/memory trail.
        let brief_receipt_summary = summarize_brief_receipt(&brief.receipt);
        tracing::debug!(
            total_tokens = brief.receipt.total_tokens,
            dropped = brief.receipt.dropped.len(),
            "agent turn: assembled brief"
        );

        // System prompt now travels via the brief (SystemPromptSource); the
        // historical + user messages become the initial LLM message list.
        let brief_system = brief.system.clone();
        let mut messages: Vec<Message> = brief
            .messages
            .into_iter()
            .filter_map(brief_message_to_llm)
            .collect();
        // Defensive: UserMessageSource always contributes the user turn, but if
        // a future budget/governance change ever dropped it, keep the turn
        // well-formed by re-appending the user's literal message.
        if !messages
            .last()
            .is_some_and(|m| matches!(m.role, Role::User))
        {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: user_content.into(),
                }],
            });
        }

        // ── 4. Agentic loop ──
        let tools: Vec<ToolDefinition> = if self.gateway.is_some() {
            vec![cel_act_tool_definition()]
        } else {
            vec![]
        };

        let mut total_tokens: u64 = 0;
        let mut tool_calls_dispatched: usize = 0;
        let mut final_assistant_text = String::new();

        for _iteration in 0..self.max_tool_iterations {
            // Check for an interrupt signal before each LLM call.
            if self.consume_interrupt(session_id) {
                self.publish_error(
                    session_id,
                    &request_id,
                    "turn interrupted by agent.interrupt",
                    true,
                );
                // Still publish RequestDone so the client knows the turn ended.
                self.chat_bus.publish(ChatBroadcast {
                    session_id: session_id.into(),
                    payload: StreamPayload::RequestDone {
                        request_id: request_id.clone(),
                        tokens_used: total_tokens,
                    },
                });
                return Ok(TurnResult {
                    request_id,
                    user_message_id: user_chunk.id,
                    assistant_message_id: String::new(),
                    assistant_text: "(interrupted)".into(),
                    tool_calls_dispatched,
                });
            }

            let req = CompletionRequest {
                model: self.model.clone(),
                system: brief_system.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
                temperature: None,
                max_tokens: Some(self.max_tokens),
                stop_sequences: vec![],
            };

            let response: CompletionResponse = match self.llm.complete(req).await {
                Ok(r) => r,
                Err(e) => {
                    self.publish_error(session_id, &request_id, &e.to_string(), true);
                    return Err(AgentRuntimeError::Provider(e));
                }
            };

            total_tokens = total_tokens.saturating_add(u64::from(
                response
                    .usage
                    .input_tokens
                    .saturating_add(response.usage.output_tokens),
            ));

            // Collect text blocks.
            let mut text_this_step = String::new();
            for block in &response.content {
                if let ContentBlock::Text { text } = block {
                    text_this_step.push_str(text);
                }
            }
            if !text_this_step.is_empty() {
                final_assistant_text = text_this_step;
            }

            // Collect tool-use blocks.
            let tool_uses: Vec<_> = response
                .content
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolUse { id, name, input } = b {
                        Some((id.clone(), name.clone(), input.clone()))
                    } else {
                        None
                    }
                })
                .collect();

            // If no tools requested, or stop_reason says done → exit loop.
            if tool_uses.is_empty() || response.stop_reason != StopReason::ToolUse {
                break;
            }

            // Record assistant message (tool-use blocks) in conversation.
            messages.push(Message {
                role: Role::Assistant,
                content: response.content.clone(),
            });

            // Dispatch each tool use.
            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for (tool_call_id, tool_name, tool_args) in tool_uses {
                // Publish attempt frame.
                self.chat_bus.publish(ChatBroadcast {
                    session_id: session_id.into(),
                    payload: StreamPayload::ToolCallAttempt {
                        request_id: request_id.clone(),
                        tool_name: tool_name.clone(),
                        args: tool_args.clone(),
                        tool_call_id: tool_call_id.clone(),
                    },
                });

                let (outcome_str, result_value, is_error) =
                    match self.dispatch_tool(session_id, &tool_name, tool_args).await {
                        Ok(outcome) => {
                            tool_calls_dispatched += 1;
                            let (outcome_str, result_value) = outcome_to_wire(&outcome);
                            (outcome_str, result_value, false)
                        }
                        Err(e) => {
                            tracing::error!(
                                tool = tool_name,
                                error = %e,
                                "agent tool dispatch error"
                            );
                            ("error".into(), json!({ "error": e.to_string() }), true)
                        }
                    };

                // Publish result frame.
                self.chat_bus.publish(ChatBroadcast {
                    session_id: session_id.into(),
                    payload: StreamPayload::ToolCallResult {
                        request_id: request_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                        outcome: outcome_str.clone(),
                        result: Some(result_value.clone()),
                    },
                });

                // Write tool-result memory chunk for audit trail.
                let _ = self
                    .memory
                    .write(NewMemoryChunk {
                        kind: ChunkKind::Chat,
                        source: ChunkSource::Embedded,
                        session_id: Some(session_id.into()),
                        project_root: None,
                        caller_id: AGENT_CALLER_ID.into(),
                        content: serde_json::to_string(&result_value)
                            .unwrap_or_else(|_| outcome_str.clone()),
                        metadata: json!({
                            "role": "tool_result",
                            "tool_call_id": tool_call_id,
                            "tool_name": tool_name,
                            "outcome": outcome_str,
                        }),
                        importance: None,
                        shareable: false,
                        pinned: false,
                    })
                    .await;

                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tool_call_id,
                    content: result_value,
                    is_error,
                });
            }

            // Feed tool results back into the conversation.
            messages.push(Message {
                role: Role::User,
                content: tool_results,
            });
        }

        if final_assistant_text.is_empty() {
            final_assistant_text.push_str("(no response)");
        }

        // ── 5. Write final assistant chunk ──
        let asst_chunk = match self
            .memory
            .write(NewMemoryChunk {
                kind: ChunkKind::Chat,
                source: ChunkSource::Embedded,
                session_id: Some(session_id.into()),
                project_root: None,
                caller_id: AGENT_CALLER_ID.into(),
                content: final_assistant_text.clone(),
                metadata: json!({
                    "role": "assistant",
                    "request_id": request_id,
                    "model": self.model,
                    "tool_calls_dispatched": tool_calls_dispatched,
                    "brief_receipt": brief_receipt_summary,
                }),
                importance: None,
                shareable: false,
                pinned: false,
            })
            .await
        {
            Ok(c) => c,
            Err(e) => {
                self.publish_error(session_id, &request_id, &e.to_string(), true);
                return Err(AgentRuntimeError::Memory(e));
            }
        };

        // ── 6. Publish completion frames ──
        self.chat_bus.publish(ChatBroadcast {
            session_id: session_id.into(),
            payload: StreamPayload::MessageComplete {
                request_id: request_id.clone(),
                message_id: asst_chunk.id.clone(),
            },
        });
        self.chat_bus.publish(ChatBroadcast {
            session_id: session_id.into(),
            payload: StreamPayload::RequestDone {
                request_id: request_id.clone(),
                tokens_used: total_tokens,
            },
        });

        Ok(TurnResult {
            request_id,
            user_message_id: user_chunk.id,
            assistant_message_id: asst_chunk.id,
            assistant_text: final_assistant_text,
            tool_calls_dispatched,
        })
    }

    /// Dispatch one tool call through the gateway (if wired) or return an
    /// error if the gateway is absent.
    async fn dispatch_tool(
        &self,
        session_id: &str,
        tool_name: &str,
        tool_args: Value,
    ) -> Result<ActionOutcome, AgentRuntimeError> {
        match &self.gateway {
            None => Err(AgentRuntimeError::Gateway(
                cel_act_gateway::GatewayError::Actuator(format!(
                    "no gateway wired; cannot dispatch tool '{tool_name}'"
                )),
            )),
            Some(gw) => Ok(gw
                .intercept_tool_call(ProposedAction {
                    caller: AGENT_CALLER_ID.into(),
                    action_type: tool_name.into(),
                    action_args: tool_args,
                    agent_session_id: Some(session_id.into()),
                    project_root: None,
                })
                .await?),
        }
    }

    fn publish_error(&self, session_id: &str, request_id: &str, message: &str, recoverable: bool) {
        self.chat_bus.publish(ChatBroadcast {
            session_id: session_id.into(),
            payload: StreamPayload::Error {
                request_id: request_id.into(),
                message: message.into(),
                recoverable,
            },
        });
    }
}

/// Map an [`ActionOutcome`] to the wire strings used in `ToolCallResult`.
fn outcome_to_wire(outcome: &ActionOutcome) -> (String, Value) {
    match outcome {
        ActionOutcome::Executed { result } => ("allowed".into(), result.clone()),
        ActionOutcome::Vetoed {
            rule_id, rule_name, ..
        } => (
            "vetoed".into(),
            json!({ "rule_id": rule_id, "rule_name": rule_name }),
        ),
        ActionOutcome::ConfirmationDenied { rule_id, rule_name } => (
            "denied".into(),
            json!({ "rule_id": rule_id, "rule_name": rule_name }),
        ),
        ActionOutcome::ConfirmationTimedOut {
            rule_id,
            rule_name,
            timeout_s,
        } => (
            "timed_out".into(),
            json!({ "rule_id": rule_id, "rule_name": rule_name, "timeout_s": timeout_s }),
        ),
    }
}

/// Render a memory chunk's `metadata.role` to the IPC wire role string.
pub fn role_str_of_chunk(metadata: &Value) -> &str {
    metadata
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("user")
}

/// Map a `cel_brief::Role` onto the LLM-router `Role`.
///
/// `System` never reaches the message list — it travels via
/// `CompletionRequest.system` — but is mapped to `User` defensively in case a
/// source ever emits a `Role::System` *message* (rather than a system
/// contribution). `Tool` collapses to `User`, matching how the router models
/// tool-result turns.
fn brief_role_to_llm(role: BriefRole) -> Role {
    match role {
        BriefRole::Assistant => Role::Assistant,
        BriefRole::System | BriefRole::User | BriefRole::Tool => Role::User,
    }
}

/// Convert one assembled [`BriefMessage`] into an LLM-router [`Message`].
///
/// Returns `None` for variants the chat path doesn't render yet (images).
/// `Text` is the only variant the current three-source wiring produces;
/// `ToolCall` / `ToolResult` are handled so a future `ToolCatalogSource` or
/// tool-replaying `HistorySource` doesn't silently drop content.
fn brief_message_to_llm(msg: BriefMessage) -> Option<Message> {
    match msg {
        BriefMessage::Text { role, content, .. } => Some(Message {
            role: brief_role_to_llm(role),
            content: vec![ContentBlock::Text { text: content }],
        }),
        BriefMessage::ToolCall { id, name, args, .. } => Some(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id,
                name,
                input: args,
            }],
        }),
        BriefMessage::ToolResult { id, content, .. } => Some(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id,
                content: Value::String(content),
                is_error: false,
            }],
        }),
        // Vision turns aren't wired through the embedded chat agent yet.
        BriefMessage::Image { .. } => None,
    }
}

/// Compact, audit-friendly summary of a [`cel_brief::BriefReceipt`] for the
/// assistant chunk's `metadata.brief_receipt`. Keeps the durable trail small
/// (per-source kept/dropped counts + totals) rather than serialising the full
/// receipt with its `Duration` timings.
fn summarize_brief_receipt(receipt: &cel_brief::BriefReceipt) -> Value {
    let mut by_source = serde_json::Map::new();
    for (sid, stats) in &receipt.by_source {
        by_source.insert(
            sid.to_string(),
            json!({
                "contributions": stats.contributions,
                "kept": stats.kept,
                "tokens": stats.tokens,
            }),
        );
    }
    json!({
        "total_tokens": receipt.total_tokens,
        "dropped": receipt.dropped.len(),
        "redactions": receipt.redactions.len(),
        "by_source": by_source,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cel_act_gateway::{ActionOutcome, AgentGateway, GatewayError, ProposedAction};
    use cel_memory::BasicMemoryProvider;
    use cellar_llm_router::provider::MockProvider;
    use cellar_llm_router::types::{CompletionResponse, StopReason, Usage};
    use serde_json::json;

    fn text_response(text: &str) -> CompletionResponse {
        CompletionResponse {
            content: vec![ContentBlock::Text { text: text.into() }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 12,
                output_tokens: 34,
            },
            model: None,
        }
    }

    fn tool_use_response(tool_id: &str, tool_name: &str, args: Value) -> CompletionResponse {
        CompletionResponse {
            content: vec![ContentBlock::ToolUse {
                id: tool_id.into(),
                name: tool_name.into(),
                input: args,
            }],
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
            },
            model: None,
        }
    }

    async fn fresh_session(memory: &Arc<dyn MemoryProvider>) -> String {
        memory
            .open_session(cel_memory::NewMemorySession {
                caller_id: AGENT_CALLER_ID.into(),
                title: Some("test".into()),
                metadata: Value::Null,
            })
            .await
            .unwrap()
            .id
    }

    // ── Mock gateway ──

    /// Always-allow mock gateway for testing tool dispatch.
    struct MockGateway {
        response: Value,
    }

    impl MockGateway {
        fn allow(response: Value) -> Arc<dyn AgentGateway> {
            Arc::new(Self { response })
        }
    }

    #[async_trait]
    impl AgentGateway for MockGateway {
        async fn intercept_tool_call(
            &self,
            _action: ProposedAction,
        ) -> Result<ActionOutcome, GatewayError> {
            Ok(ActionOutcome::Executed {
                result: self.response.clone(),
            })
        }
    }

    // ── Tests ──

    #[tokio::test]
    async fn single_turn_writes_user_and_assistant_and_emits_frames() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = MockProvider::new(vec![text_response("Hello back!")]);
        let bus = ChatBus::new();
        let mut rx = bus.subscribe();
        let agent = AgentRuntime::new(memory.clone(), provider, "mock-model", bus);

        let session_id = fresh_session(&memory).await;
        let result = agent.run_turn(&session_id, "Hi!").await.unwrap();
        assert_eq!(result.assistant_text, "Hello back!");
        assert_eq!(result.tool_calls_dispatched, 0);

        // Two frames: MessageComplete + RequestDone.
        let f1 = rx.recv().await.unwrap();
        assert!(matches!(f1.payload, StreamPayload::MessageComplete { .. }));
        let f2 = rx.recv().await.unwrap();
        assert!(matches!(f2.payload, StreamPayload::RequestDone { .. }));

        // 2 chat chunks (user + assistant).
        let stats = memory.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 2);
    }

    #[tokio::test]
    async fn empty_response_yields_no_response_fallback_text() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = MockProvider::new(vec![]);
        let bus = ChatBus::new();
        let agent = AgentRuntime::new(memory.clone(), provider, "mock-model", bus);
        let session_id = fresh_session(&memory).await;
        let result = agent.run_turn(&session_id, "hello?").await.unwrap();
        assert_eq!(result.assistant_text, "(no response)");
    }

    #[tokio::test]
    async fn multi_turn_carries_context() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = MockProvider::new(vec![
            text_response("First answer"),
            text_response("Second answer"),
        ]);
        let bus = ChatBus::new();
        let agent = AgentRuntime::new(memory.clone(), provider, "mock-model", bus);
        let session_id = fresh_session(&memory).await;

        agent.run_turn(&session_id, "first").await.unwrap();
        agent.run_turn(&session_id, "second").await.unwrap();

        // 4 chunks: user+assistant per turn.
        let stats = memory.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 4);
    }

    #[tokio::test]
    async fn tool_dispatch_publishes_attempt_and_result_frames() {
        // LLM: first call → tool_use, second call → text (end turn).
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = MockProvider::new(vec![
            tool_use_response("tc_1", "cel_act", json!({"action_type": "ax.click"})),
            text_response("Done! I clicked the button."),
        ]);
        let bus = ChatBus::new();
        let mut rx = bus.subscribe();
        let gateway = MockGateway::allow(json!({"clicked": true}));

        let agent =
            AgentRuntime::new(memory.clone(), provider, "mock-model", bus).with_gateway(gateway);

        let session_id = fresh_session(&memory).await;
        let result = agent
            .run_turn(&session_id, "Click the button")
            .await
            .unwrap();

        assert_eq!(result.tool_calls_dispatched, 1);
        assert_eq!(result.assistant_text, "Done! I clicked the button.");

        // Frames: ToolCallAttempt, ToolCallResult, MessageComplete, RequestDone.
        let f1 = rx.recv().await.unwrap();
        assert!(
            matches!(f1.payload, StreamPayload::ToolCallAttempt { ref tool_name, .. } if tool_name == "cel_act"),
            "expected ToolCallAttempt, got {:?}",
            f1.payload
        );
        let f2 = rx.recv().await.unwrap();
        assert!(
            matches!(f2.payload, StreamPayload::ToolCallResult { ref outcome, .. } if outcome == "allowed"),
            "expected ToolCallResult allowed, got {:?}",
            f2.payload
        );
        let f3 = rx.recv().await.unwrap();
        assert!(matches!(f3.payload, StreamPayload::MessageComplete { .. }));
        let f4 = rx.recv().await.unwrap();
        assert!(matches!(f4.payload, StreamPayload::RequestDone { .. }));

        // Chunks: user + tool_result + assistant = 3.
        let stats = memory.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 3);
    }

    #[tokio::test]
    async fn tool_dispatch_without_gateway_returns_error_result() {
        // Agent with no gateway; LLM emits tool_use → runtime should handle gracefully.
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = MockProvider::new(vec![
            // LLM returns tool_use but no gateway → dispatch error → next LLM call gets error result.
            tool_use_response("tc_1", "cel_act", json!({"action_type": "ax.click"})),
            text_response("I couldn't complete the action."),
        ]);
        let bus = ChatBus::new();
        let mut rx = bus.subscribe();
        let agent = AgentRuntime::new(memory.clone(), provider, "mock-model", bus);

        let session_id = fresh_session(&memory).await;
        let result = agent.run_turn(&session_id, "Do something").await.unwrap();

        // Turn still completes (error is fed back to LLM, which produces a text response).
        assert_eq!(result.assistant_text, "I couldn't complete the action.");

        // Frames: ToolCallAttempt (still published), ToolCallResult (error), MessageComplete, RequestDone.
        let f1 = rx.recv().await.unwrap();
        assert!(matches!(f1.payload, StreamPayload::ToolCallAttempt { .. }));
        let f2 = rx.recv().await.unwrap();
        assert!(
            matches!(f2.payload, StreamPayload::ToolCallResult { ref outcome, .. } if outcome == "error"),
            "expected error outcome, got {:?}",
            f2.payload
        );
        let f3 = rx.recv().await.unwrap();
        assert!(matches!(f3.payload, StreamPayload::MessageComplete { .. }));
        let _ = rx.recv().await.unwrap(); // RequestDone
    }

    #[tokio::test]
    async fn with_gateway_switches_system_prompt() {
        // with_gateway() should flip the system prompt to the "tools enabled" version.
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = MockProvider::new(vec![text_response("ok")]);
        let bus = ChatBus::new();
        let gateway = MockGateway::allow(json!({}));
        let agent =
            AgentRuntime::new(memory.clone(), provider, "mock-model", bus).with_gateway(gateway);
        // system_prompt should no longer contain the "not yet enabled" verbiage.
        assert!(
            !agent.system_prompt.contains("not yet enabled"),
            "system prompt should be updated when gateway is wired"
        );
    }

    #[tokio::test]
    async fn turn_stamps_brief_receipt_on_assistant_chunk() {
        // The per-turn assembly now runs through cel-brief; its receipt is
        // stamped onto the assistant chunk's metadata for the audit trail.
        // Assert the receipt is present, records tokens, and shows both
        // Critical sources (system prompt + user message) surviving.
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = MockProvider::new(vec![text_response("hi there")]);
        let bus = ChatBus::new();
        let agent = AgentRuntime::new(memory.clone(), provider, "mock-model", bus);
        let session_id = fresh_session(&memory).await;

        let result = agent.run_turn(&session_id, "hello").await.unwrap();

        let chunk = memory
            .get(&result.assistant_message_id)
            .await
            .unwrap()
            .expect("assistant chunk exists");
        let receipt = chunk
            .metadata
            .get("brief_receipt")
            .expect("brief_receipt present in assistant metadata");

        assert!(
            receipt
                .get("total_tokens")
                .and_then(|v| v.as_u64())
                .is_some_and(|t| t > 0),
            "receipt should record a positive total_tokens, got {receipt}"
        );
        let by_source = receipt
            .get("by_source")
            .and_then(|v| v.as_object())
            .expect("by_source map present");
        assert!(
            by_source.contains_key("system_prompt"),
            "system_prompt source should appear in the receipt"
        );
        let user_kept = by_source
            .get("user_message")
            .and_then(|v| v.get("kept"))
            .and_then(|v| v.as_u64())
            .expect("user_message kept count present");
        assert_eq!(
            user_kept, 1,
            "the user's message must survive into the brief"
        );
    }

    #[tokio::test]
    async fn brief_assembly_preserves_prior_turn_history() {
        // Two turns in one session: the second turn's brief should replay the
        // prior turn as history. `BasicMemoryProvider.retrieve` is a strict
        // case-insensitive *substring* match (it returns a chunk only when its
        // content contains the whole query text), so the two turns share an
        // identical user message — that guarantees the first turn's user chunk
        // is retrieved and flows into HistorySource on the second turn.
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = MockProvider::new(vec![
            text_response("first answer"),
            text_response("second answer"),
        ]);
        let bus = ChatBus::new();
        let agent = AgentRuntime::new(memory.clone(), provider, "mock-model", bus);
        let session_id = fresh_session(&memory).await;

        let prompt = "status report please";
        agent.run_turn(&session_id, prompt).await.unwrap();
        let second = agent.run_turn(&session_id, prompt).await.unwrap();
        assert_eq!(second.assistant_text, "second answer");

        let chunk = memory
            .get(&second.assistant_message_id)
            .await
            .unwrap()
            .expect("assistant chunk exists");
        let by_source = chunk.metadata["brief_receipt"]["by_source"]
            .as_object()
            .expect("by_source map present");
        let history_kept = by_source
            .get("history")
            .and_then(|v| v.get("kept"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert!(
            history_kept >= 1,
            "second turn should replay >=1 prior-turn history entry, got {history_kept}"
        );
    }

    #[tokio::test]
    async fn brief_recalls_durable_memory_across_sessions() {
        // Seed a durable JobSummary memory in a PRIOR session (authored by the
        // embedded agent so CallerScope::Own matches), then run a turn in a
        // fresh session whose user message is a substring of that memory's
        // content. MemorySource is scoped to durable kinds, so it recalls the
        // JobSummary cross-session — and because Chat is excluded, it neither
        // duplicates the conversation nor recalls the just-written user turn.
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let prior = memory
            .open_session(cel_memory::NewMemorySession {
                caller_id: AGENT_CALLER_ID.into(),
                title: Some("prior".into()),
                metadata: Value::Null,
            })
            .await
            .unwrap();
        let memo = "deploy checklist: run migrations before flipping the flag";
        memory
            .write(NewMemoryChunk {
                kind: ChunkKind::JobSummary,
                source: ChunkSource::Embedded,
                session_id: Some(prior.id.clone()),
                project_root: None,
                caller_id: AGENT_CALLER_ID.into(),
                content: memo.into(),
                metadata: json!({ "role": "assistant" }),
                importance: Some(0.8),
                shareable: false,
                pinned: false,
            })
            .await
            .unwrap();

        let provider = MockProvider::new(vec![text_response("ack")]);
        let bus = ChatBus::new();
        // Default memory_recall_k (> 0) ⇒ MemorySource is wired.
        let agent = AgentRuntime::new(memory.clone(), provider, "mock-model", bus);
        let chat_session = fresh_session(&memory).await;

        let result = agent.run_turn(&chat_session, memo).await.unwrap();

        let chunk = memory
            .get(&result.assistant_message_id)
            .await
            .unwrap()
            .expect("assistant chunk exists");
        let by_source = chunk.metadata["brief_receipt"]["by_source"]
            .as_object()
            .expect("by_source map present");
        let mem_kept = by_source
            .get("memory")
            .and_then(|v| v.get("kept"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert!(
            mem_kept >= 1,
            "MemorySource should recall the durable cross-session memory, got kept={mem_kept}"
        );
    }

    #[tokio::test]
    async fn memory_recall_zero_disables_memory_source() {
        // with_memory_recall(0) drops MemorySource entirely: the receipt has no
        // `memory` entry even when a matching durable memory exists.
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let memo = "the archived runbook mentions the staging password rotation";
        memory
            .write(NewMemoryChunk {
                kind: ChunkKind::JobSummary,
                source: ChunkSource::Embedded,
                session_id: None,
                project_root: None,
                caller_id: AGENT_CALLER_ID.into(),
                content: memo.into(),
                metadata: json!({ "role": "assistant" }),
                importance: Some(0.8),
                shareable: false,
                pinned: false,
            })
            .await
            .unwrap();

        let provider = MockProvider::new(vec![text_response("ack")]);
        let bus = ChatBus::new();
        let agent =
            AgentRuntime::new(memory.clone(), provider, "mock-model", bus).with_memory_recall(0);
        let chat_session = fresh_session(&memory).await;

        let result = agent.run_turn(&chat_session, memo).await.unwrap();

        let chunk = memory
            .get(&result.assistant_message_id)
            .await
            .unwrap()
            .expect("assistant chunk exists");
        let by_source = chunk.metadata["brief_receipt"]["by_source"]
            .as_object()
            .expect("by_source map present");
        assert!(
            !by_source.contains_key("memory"),
            "MemorySource must be absent when memory_recall_k == 0, got {by_source:?}"
        );
    }
}
