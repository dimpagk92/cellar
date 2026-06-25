//! Minimal `cel-brief` example using only generic in-file sources.
//!
//! Wires the real [`cel_brief::BriefBuilder`] against four pluggable sources,
//! applies a token budget tight enough to force pruning, and prints the
//! assembled [`cel_brief::Brief`] plus the full [`cel_brief::BriefReceipt`].
//!
//! This file exists as a pluggability proof: prompt assembly should work with
//! any source that implements the `Source` trait.
//!
//! Run with: `cargo run -p cel-brief --example standalone`.

use std::sync::Arc;

use async_trait::async_trait;

use cel_brief::{
    BriefBuilder, BriefContext, BriefMessage, Contribution, Priority, Role, Source, SourceError,
    SourceId, TokenBudget, ToolSchema,
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
/// [`SourceError::Skipped`] when the context has no user message.
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

/// Stand-in memory source — emits fake recollections with varying importance
/// scores. Demonstrates that the builder prunes lower-value inputs when the
/// budget is tight.
struct FakeMemory;

#[async_trait]
impl Source for FakeMemory {
    fn id(&self) -> SourceId {
        SourceId::new("memory")
    }

    fn priority(&self) -> Priority {
        Priority::Normal
    }

    async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        let entries = [
            ("User prefers concise answers.", 0.9_f32),
            (
                "Last week's incident summary: deploy stuck on migration 119.",
                0.7,
            ),
            ("Prefers deployment summaries before detailed logs.", 0.5),
            ("Owns a mechanical keyboard.", 0.2),
        ];
        Ok(entries
            .into_iter()
            .map(|(text, importance)| {
                let est = text.len().div_ceil(4);
                Contribution::text(Role::Assistant, text.to_owned(), est)
                    .with_importance(importance)
                    .with_tag("memory")
            })
            .collect())
    }
}

/// Two-tool catalog so the receipt's tool path is exercised.
struct ToolCatalog;

#[async_trait]
impl Source for ToolCatalog {
    fn id(&self) -> SourceId {
        SourceId::new("tools")
    }

    fn priority(&self) -> Priority {
        Priority::High
    }

    async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        let echo = ToolSchema {
            name: "echo".into(),
            description: "Echo a string back to the user.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
            }),
            source: SourceId::new("tools"),
        };
        let now = ToolSchema {
            name: "now".into(),
            description: "Return the current Unix timestamp.".into(),
            input_schema: serde_json::json!({"type": "object"}),
            source: SourceId::new("tools"),
        };
        Ok(vec![
            Contribution::tool(echo, 32),
            Contribution::tool(now, 16),
        ])
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let ctx = BriefContext::new(TokenBudget::default())
        .with_turn(1)
        .with_goal("prepare a deployment status reply")
        .with_user_message("What should I know before replying to the deploy thread?");

    let budget = TokenBudget::new(80, 16);

    let builder = BriefBuilder::new()
        .source(Arc::new(StaticSystemPrompt {
            text: "You are a helpful assistant grounded in provided sources.",
        }))
        .source(Arc::new(UserMessageEcho))
        .source(Arc::new(FakeMemory))
        .source(Arc::new(ToolCatalog))
        .budget(budget);

    let brief = match builder.build(&ctx).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("build failed: {e}");
            std::process::exit(1);
        }
    };

    println!("--- assembled Brief ---");
    if let Some(system) = &brief.system {
        println!("system: {system}");
    } else {
        println!("system: <none>");
    }

    println!("\nmessages ({}):", brief.messages.len());
    for (idx, msg) in brief.messages.iter().enumerate() {
        match msg {
            BriefMessage::Text {
                role,
                content,
                source,
            } => println!("  [{idx}] text role={role:?} src={source}: {content}"),
            BriefMessage::Image { role, source, .. } => {
                println!("  [{idx}] image role={role:?} src={source}");
            }
            BriefMessage::ToolCall {
                id, name, source, ..
            } => println!("  [{idx}] tool_call id={id} name={name} src={source}"),
            BriefMessage::ToolResult { id, source, .. } => {
                println!("  [{idx}] tool_result id={id} src={source}");
            }
        }
    }

    println!("\ntools ({}):", brief.tools.len());
    for (idx, t) in brief.tools.iter().enumerate() {
        println!("  [{idx}] {} (src={}): {}", t.name, t.source, t.description);
    }

    println!("\n--- BriefReceipt ---");
    println!("total_tokens : {}", brief.receipt.total_tokens);
    println!("dropped      : {}", brief.receipt.dropped.len());
    for d in &brief.receipt.dropped {
        println!("  - {} ({} tokens, {:?})", d.source, d.tokens, d.reason);
    }
    println!("redactions   : {}", brief.receipt.redactions.len());

    println!("\nby_source:");
    let mut by_source: Vec<_> = brief.receipt.by_source.iter().collect();
    by_source.sort_by_key(|(sid, _)| sid.as_str().to_owned());
    for (sid, stats) in by_source {
        println!(
            "  {sid:18} contributions={} kept={} tokens={} priority={:?}",
            stats.contributions, stats.kept, stats.tokens, stats.priority
        );
    }

    println!("\n--- Brief (JSON) ---");
    match serde_json::to_string_pretty(&brief) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("serialize failed: {e}"),
    }
}
