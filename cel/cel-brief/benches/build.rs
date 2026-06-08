//! Microbenchmark for cel-brief's per-turn assembly path.
//!
//! Builds a realistic [`cel_brief::Brief`] from a representative mix of
//! built-in sources — system prompt + user message + tool catalog + history
//! + memory — and reports per-iteration timings plus a p95 estimate.
//!
//! Target per plan §9 Phase 4: under 50 ms p95.
//!
//! This is a hand-rolled `std::time::Instant` benchmark behind
//! `harness = false`. It currently covers the source-fan-out half of the
//! pipeline. A future benchmark can call `BriefBuilder::build` directly when
//! we want to measure fan-out → tokenize → prune → governance end to end.
//!
//! Run with:
//!
//! ```sh
//! cargo bench -p cel-brief --features memory --bench build
//! ```

use std::sync::Arc;
use std::time::Instant;

use cel_brief::{
    BriefContext, HistoryEntry, HistorySource, MemoryQueryStrategy, MemorySource, Role, Source,
    SourceError, SystemPromptSource, TokenBudget, ToolCatalogSource, ToolSchema, UserMessageSource,
};
use cel_memory::{
    BasicMemoryProvider, ChunkKind, ChunkSource, MemoryProvider, NewMemoryChunk, NewMemorySession,
};
use serde_json::json;

/// How many iterations to time.
const ITERS: usize = 200;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("cel-brief — hand-fanout microbenchmark");
    println!("======================================");

    // Build a realistic mix of sources, seeded once.
    let memory = Arc::new(build_seeded_memory().await);
    let tools = build_tool_catalog();
    let history: Vec<HistoryEntry> = build_history();

    let sys = SystemPromptSource::new(LONG_SYSTEM_PROMPT);
    let user = UserMessageSource::new();
    let tool_cat = ToolCatalogSource::new(tools);
    let hist = HistorySource::new(history, 20);
    let mem = MemorySource::new(memory, "bench", 8)
        .with_strategy(MemoryQueryStrategy::UserMessageThenGoal);

    let sources: Vec<Box<dyn Source>> = vec![
        Box::new(sys),
        Box::new(user),
        Box::new(tool_cat),
        Box::new(hist),
        Box::new(mem),
    ];

    let ctx = BriefContext::new(TokenBudget::new(16_000, 2_048))
        .with_turn(7)
        .with_goal("respond to the user's request grounded in memory")
        .with_user_message("Q4 report");

    // Warm-up — first iteration pulls dynamic strings into cache.
    let _ = fanout(&sources, &ctx).await;

    let mut timings = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let start = Instant::now();
        let contributions = fanout(&sources, &ctx).await;
        let elapsed = start.elapsed();
        timings.push(elapsed);
        // Black-box the result so the optimiser doesn't elide the fan-out.
        std::hint::black_box(contributions);
    }

    timings.sort();
    let p50 = timings[ITERS / 2];
    let p95 = timings[(ITERS * 95) / 100];
    let p99 = timings[(ITERS * 99) / 100];
    let min = timings.first().copied().unwrap_or_default();
    let max = timings.last().copied().unwrap_or_default();
    let mean: f64 = timings.iter().map(|d| d.as_secs_f64()).sum::<f64>() / timings.len() as f64;

    println!("iterations: {ITERS}");
    println!(
        "  min  = {:?}\n  p50  = {:?}\n  mean = {:.6} ms\n  p95  = {:?}\n  p99  = {:?}\n  max  = {:?}",
        min,
        p50,
        mean * 1_000.0,
        p95,
        p99,
        max,
    );

    let p95_ms = p95.as_secs_f64() * 1_000.0;
    if p95_ms < 50.0 {
        println!("\n[OK] p95 < 50 ms target (plan §9 Phase 4)");
    } else {
        println!("\n[WARN] p95 {p95_ms:.3} ms >= 50 ms target — investigate or update the budget");
    }
}

/// Run every source's `contribute` and collect everything into a single
/// `Vec<Contribution>`. Phase 2's `BriefBuilder` will replace this with a
/// parallel fan-out + tokenize + prune + governance pipeline; the bench can
/// then switch to measuring `build` end-to-end.
async fn fanout(sources: &[Box<dyn Source>], ctx: &BriefContext) -> Vec<cel_brief::Contribution> {
    let mut out = Vec::with_capacity(64);
    for source in sources {
        match source.contribute(ctx).await {
            Ok(cs) => out.extend(cs),
            Err(SourceError::Skipped(_)) => {}
            Err(e) => panic!("source {} failed: {e}", source.id()),
        }
    }
    out
}

async fn build_seeded_memory() -> BasicMemoryProvider {
    let p = BasicMemoryProvider::new();
    let session = p
        .open_session(NewMemorySession {
            caller_id: "bench".into(),
            title: Some("bench".into()),
            metadata: json!(null),
        })
        .await
        .expect("open session");
    for content in SEED_CHUNKS {
        p.write(NewMemoryChunk {
            kind: ChunkKind::Chat,
            source: ChunkSource::Embedded,
            session_id: Some(session.id.clone()),
            project_root: None,
            caller_id: "bench".into(),
            content: (*content).into(),
            metadata: json!(null),
            importance: Some(0.5),
            shareable: false,
            pinned: false,
        })
        .await
        .expect("write");
    }
    p
}

fn build_tool_catalog() -> Vec<ToolSchema> {
    let names = [
        "fs.read",
        "fs.write",
        "fs.copy",
        "fs.move",
        "shell.exec",
        "web.fetch",
        "calendar.create_event",
        "mail.send",
    ];
    names
        .iter()
        .map(|name| ToolSchema {
            name: (*name).into(),
            description: format!("Test tool {name} — does what it says on the tin."),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "a": {"type": "string"},
                    "b": {"type": "string"},
                    "c": {"type": "boolean"},
                },
                "required": ["a"],
            }),
            // Will be rewritten by `ToolCatalogSource::new`.
            source: cel_brief::SourceId::new("__unset__"),
        })
        .collect()
}

fn build_history() -> Vec<HistoryEntry> {
    let mut h = Vec::with_capacity(20);
    for i in 0..10 {
        h.push(HistoryEntry::Text {
            role: Role::User,
            content: format!("Turn {i}: user message about the Q{i} report."),
        });
        h.push(HistoryEntry::Text {
            role: Role::Assistant,
            content: format!(
                "Turn {i}: assistant response that summarises what the system did and why."
            ),
        });
    }
    h
}

const SEED_CHUNKS: &[&str] = &[
    "User prefers dry-run mode for any destructive file operation.",
    "Q4 report is filed under ~/Workspace/q4.md",
    "Last action: copy ~/Documents/draft.md to ~/Workspace/",
    "Calendar: standup is at 09:30 every weekday.",
    "Mail: drafts to alice@example.com go in the 'Outbox' folder.",
    "Shell prefers zsh; aliases live in ~/.zshrc.",
    "Editor of choice: nvim with the kanagawa-paper theme.",
    "Git: signed commits; never force-push to main.",
];

const LONG_SYSTEM_PROMPT: &str =
    "You are a helpful, careful assistant grounded in the user's local device. \
Read the user's question, consult the memories and tools you have, and respond concisely. \
When the user asks about files, prefer pointing them at exact paths over vague gestures. \
Never invent file contents; if a memory says something is at a path, you may quote it; \
otherwise say you would need to open the file to be sure. Use tools when calling them is \
cheaper than asking the user, and skip them when the user clearly wants a short answer. \
Treat tool failures as information, not as final answers — surface what happened and try \
an alternative when one is obviously available.";
