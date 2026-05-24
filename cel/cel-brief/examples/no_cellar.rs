//! Minimal `cel-brief` example using zero other Cellar crates.
//!
//! Phase 1: wires two trivial [`cel_brief::Source`] implementations against
//! the core types and prints their contributions. The full
//! [`cel_brief::builder::BriefBuilder`] flow (fan-out, tokenize, prune,
//! governance, receipt) lands in Phase 2; this example hand-assembles a
//! [`cel_brief::Brief`] with an empty receipt to exercise the end-to-end
//! type surface without leaking any Cellar-specific dependencies.
//!
//! Run with: `cargo run -p cel-brief --example no_cellar`

use async_trait::async_trait;

use cel_brief::{
    Brief, BriefContext, BriefMessage, BriefReceipt, Contribution, ContributionContent, Priority,
    Role, Source, SourceError, SourceId, TokenBudget,
};

/// Static system prompt — Critical priority, never redactable.
struct StaticSystemPrompt {
    text: &'static str,
}

#[async_trait]
impl Source for StaticSystemPrompt {
    fn id(&self) -> SourceId {
        SourceId::new("system_prompt")
    }

    fn priority(&self) -> Priority {
        Priority::Critical
    }

    async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        Ok(vec![Contribution::system(
            self.text,
            // ~4 chars/token rule of thumb.
            self.text.len().div_ceil(4),
        )])
    }
}

/// Echoes `ctx.user_message` back as a `User` text contribution. Returns
/// [`SourceError::Skipped`] when the context has no user message — that
/// makes it a clear no-op rather than a hard failure.
struct UserMessageEcho;

#[async_trait]
impl Source for UserMessageEcho {
    fn id(&self) -> SourceId {
        SourceId::new("user_message")
    }

    fn priority(&self) -> Priority {
        Priority::Critical
    }

    async fn contribute(&self, ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        let Some(msg) = ctx.user_message.as_deref() else {
            return Err(SourceError::Skipped("no user message on context".into()));
        };
        let est = msg.len().div_ceil(4);
        Ok(vec![Contribution::text(Role::User, msg.to_owned(), est)])
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let ctx = BriefContext::new(TokenBudget::default())
        .with_turn(1)
        .with_goal("ship cel-brief phase 1")
        .with_user_message("Hello cel-brief!");

    let sources: Vec<Box<dyn Source>> = vec![
        Box::new(StaticSystemPrompt {
            text: "You are a helpful assistant grounded in the Cellar device.",
        }),
        Box::new(UserMessageEcho),
    ];

    // Phase 1: fan out by hand. Phase 2's BriefBuilder will replace this with
    // a parallel fan-out, tokenizer-driven budget pruning, governance, and
    // a populated receipt.
    let mut system_text = String::new();
    let mut messages = Vec::<BriefMessage>::new();

    for source in &sources {
        let id = source.id();
        match source.contribute(&ctx).await {
            Ok(contributions) => {
                println!(
                    "source `{id}` (priority={:?}) returned {} contribution(s)",
                    source.priority(),
                    contributions.len()
                );
                for c in contributions {
                    match c.content {
                        ContributionContent::System { text } => {
                            if !system_text.is_empty() {
                                system_text.push_str("\n\n");
                            }
                            system_text.push_str(&text);
                        }
                        ContributionContent::Text { role, content } => {
                            messages.push(BriefMessage::Text {
                                role,
                                content,
                                source: id.clone(),
                            });
                        }
                        ContributionContent::Image { role, data, alt } => {
                            messages.push(BriefMessage::Image {
                                role,
                                data,
                                alt,
                                source: id.clone(),
                            });
                        }
                        ContributionContent::ToolCall {
                            id: call_id,
                            name,
                            args,
                        } => {
                            messages.push(BriefMessage::ToolCall {
                                id: call_id,
                                name,
                                args,
                                source: id.clone(),
                            });
                        }
                        ContributionContent::ToolResult {
                            id: call_id,
                            content,
                        } => {
                            messages.push(BriefMessage::ToolResult {
                                id: call_id,
                                content,
                                source: id.clone(),
                            });
                        }
                        ContributionContent::Tool { schema: _ } => {
                            // Tool catalog assembly lands in Phase 2/3.
                        }
                    }
                }
            }
            Err(SourceError::Skipped(reason)) => {
                println!("source `{id}` skipped: {reason}");
            }
            Err(err) => {
                eprintln!("source `{id}` failed: {err}");
            }
        }
    }

    let brief = Brief {
        system: if system_text.is_empty() {
            None
        } else {
            Some(system_text)
        },
        messages,
        tools: Vec::new(),
        receipt: BriefReceipt::empty(),
    };

    println!();
    println!("--- assembled Brief (Phase 1, no builder) ---");
    if let Some(system) = &brief.system {
        println!("system: {system}");
    }
    for (idx, msg) in brief.messages.iter().enumerate() {
        match msg {
            BriefMessage::Text {
                role,
                content,
                source,
            } => {
                println!("[{idx}] text role={role:?} src={source}: {content}");
            }
            BriefMessage::Image { role, source, .. } => {
                println!("[{idx}] image role={role:?} src={source}");
            }
            BriefMessage::ToolCall {
                id, name, source, ..
            } => {
                println!("[{idx}] tool_call id={id} name={name} src={source}");
            }
            BriefMessage::ToolResult { id, source, .. } => {
                println!("[{idx}] tool_result id={id} src={source}");
            }
        }
    }
    println!(
        "receipt: total_tokens={} dropped={} redactions={}",
        brief.receipt.total_tokens,
        brief.receipt.dropped.len(),
        brief.receipt.redactions.len()
    );
}
