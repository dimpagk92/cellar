//! `cel-brief` + `cel-memory` integration example.
//!
//! Wires a [`cel_brief::MemorySource`] over a [`cel_memory::MemoryProvider`]
//! and feeds the contributions into a hand-assembled [`cel_brief::Brief`].
//! Demonstrates:
//!
//! 1. Seeding memory chunks via [`cel_memory::MemoryProvider::write`].
//! 2. Building a [`cel_brief::BriefContext`] with a user message.
//! 3. Asking [`MemorySource`] to retrieve relevant chunks (Normal priority,
//!    `MemoryQueryStrategy::UserMessageThenGoal`).
//! 4. Stuffing the contributions into a [`cel_brief::Brief`] alongside a
//!    static system prompt and the user message itself.
//!
//! Why `BasicMemoryProvider` and not `SqliteMemoryProvider`? Two reasons:
//! the `MemoryProvider` trait is the contract, so the integration is
//! identical, and pulling `cel-memory-sqlite` into `cel-brief`'s dev-deps
//! drags in `rusqlite` + `sqlite-vec` (and optionally onnxruntime) just to
//! prove a one-line swap.
//!
//! To switch to SQLite in your own code:
//!
//! ```ignore
//! // Before — in-memory reference impl:
//! let provider = Arc::new(cel_memory::BasicMemoryProvider::new());
//!
//! // After — SQLite-backed, file-persistent:
//! let provider = Arc::new(
//!     cel_memory_sqlite::SqliteMemoryProvider::open("/tmp/memory.db").await?,
//! );
//! ```
//!
//! Run with: `cargo run -p cel-brief --features memory --example with_memory`

use std::sync::Arc;

use cel_brief::{
    Brief, BriefContext, BriefMessage, BriefReceipt, Contribution, ContributionContent,
    MemoryQueryStrategy, MemorySource, Source, SourceError, SourceId, SystemPromptSource,
    TokenBudget, UserMessageSource,
};
use cel_memory::{
    BasicMemoryProvider, ChunkKind, ChunkSource, MemoryProvider, NewMemoryChunk, NewMemorySession,
};
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Stand up an in-process memory provider and seed it with a few chunks.
    //    Anything that implements `cel_memory::MemoryProvider` plugs in here.
    let memory = Arc::new(BasicMemoryProvider::new());
    let session = memory
        .open_session(NewMemorySession {
            caller_id: "example".into(),
            title: Some("with_memory".into()),
            metadata: json!(null),
        })
        .await?;

    for content in [
        "User prefers dry-run mode for any destructive file operation.",
        "Q4 expense report is filed under ~/Workspace/q4-expenses.md",
        "Last user-confirmed action: copy ~/Documents/draft.md to ~/Workspace/",
    ] {
        memory
            .write(NewMemoryChunk {
                kind: ChunkKind::Chat,
                source: ChunkSource::Embedded,
                session_id: Some(session.id.clone()),
                project_root: None,
                caller_id: "example".into(),
                content: content.into(),
                metadata: json!(null),
                importance: Some(0.6),
                shareable: false,
                pinned: false,
            })
            .await?;
    }

    // 2. Build the per-turn context. We use a single keyword as the user
    //    message because `BasicMemoryProvider` does a substring match on the
    //    raw query text — fine for the example, since `SqliteMemoryProvider`
    //    (the production backend) does proper hybrid scoring under the same
    //    trait.
    let ctx = BriefContext::new(TokenBudget::default())
        .with_turn(1)
        .with_goal("answer the user's question")
        .with_user_message("Q4 expense report");

    // 3. Register all the Phase 3 default sources + MemorySource.
    let sources: Vec<Box<dyn Source>> = vec![
        Box::new(SystemPromptSource::new(
            "You are an assistant grounded in the user's local memory.",
        )),
        Box::new(UserMessageSource::new()),
        Box::new(
            MemorySource::new(memory.clone(), "example", 5)
                .with_strategy(MemoryQueryStrategy::UserMessageThenGoal),
        ),
    ];

    // 4. Fan out by hand so the example shows the Source contract directly.
    let mut system_text = String::new();
    let mut messages = Vec::<BriefMessage>::new();
    let mut total_contributions = 0usize;
    let mut total_skipped = 0usize;

    for source in &sources {
        let id = source.id();
        match source.contribute(&ctx).await {
            Ok(contributions) => {
                total_contributions += contributions.len();
                println!(
                    "source `{id}` (priority={:?}) returned {} contribution(s)",
                    source.priority(),
                    contributions.len()
                );
                for c in contributions {
                    push_contribution(&id, c, &mut system_text, &mut messages);
                }
            }
            Err(SourceError::Skipped(reason)) => {
                total_skipped += 1;
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
    println!(
        "--- assembled Brief ({total_contributions} contributions, {total_skipped} skipped) ---"
    );
    if let Some(system) = &brief.system {
        println!("system: {system}");
        println!();
    }
    for (idx, msg) in brief.messages.iter().enumerate() {
        match msg {
            BriefMessage::Text {
                role,
                content,
                source,
            } => {
                println!("[{idx}] role={role:?} src={source}");
                println!("    {content}");
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

    println!();
    println!(
        "receipt: total_tokens={} dropped={} redactions={}",
        brief.receipt.total_tokens,
        brief.receipt.dropped.len(),
        brief.receipt.redactions.len()
    );

    Ok(())
}

/// Translate one [`Contribution`] into the right slot on the brief.
fn push_contribution(
    source_id: &SourceId,
    contribution: Contribution,
    system_text: &mut String,
    messages: &mut Vec<BriefMessage>,
) {
    match contribution.content {
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
                source: source_id.clone(),
            });
        }
        ContributionContent::Image { role, data, alt } => {
            messages.push(BriefMessage::Image {
                role,
                data,
                alt,
                source: source_id.clone(),
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
                source: source_id.clone(),
            });
        }
        ContributionContent::ToolResult {
            id: call_id,
            content,
        } => {
            messages.push(BriefMessage::ToolResult {
                id: call_id,
                content,
                source: source_id.clone(),
            });
        }
        ContributionContent::Tool { schema: _ } => {
            // Tools are assembled into `Brief.tools` by the builder; this
            // hand-fanout example focuses on the message path.
        }
    }
}
