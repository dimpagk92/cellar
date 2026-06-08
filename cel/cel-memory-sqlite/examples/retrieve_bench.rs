//! Retrieve-latency micro-benchmark for `SqliteMemoryProvider`.
//!
//! The on-device retrieve latency budget is **p95 ≤ 150 ms** at k=8 against
//! the full corpus. This
//! example produces the measurement against any embedder you wire in: the
//! default `MockEmbedder` (deterministic, ~µs per call) measures pure
//! SQLite + RRF cost; switch to `FastEmbedEmbedder` (gated on the
//! `fastembed` feature) to measure the production hot path.
//!
//! Default workload:
//!   - N=1000 chunks (`Chat` kind, 384-dim vectors via the configured embedder)
//!   - M=200 retrieves at k=8 with `RetrievalProfile::AgentChatTurn`
//!   - Cache is cleared once after ingest so the first reads exercise the
//!     cold SQL path; subsequent reads benefit from the 30 s TTL LRU
//!     (`cel_memory_sqlite::cache::RetrieveCache`).
//!
//! Output:
//!   ```
//!   wrote 1000 chunks in 814 ms
//!   retrieve k=8 over 1000 chunks, 200 runs:
//!     p50:  3.2 ms
//!     p95:  4.1 ms
//!     p99:  4.8 ms
//!     mean: 3.3 ms
//!   ```
//!
//! Run: `cargo run -p cel-memory-sqlite --example retrieve_bench --release`
//! (release flags are important — debug builds inflate SQLite overhead.)
//!
//! Tweak `N_CHUNKS` and `N_RETRIEVES` below to scale the workload up; the
//! sqlite-vec scan is brute-force, so latency grows roughly linearly in
//! `N_CHUNKS`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cel_memory::{
    CallerScope, ChunkKind, ChunkSource, MemoryProvider, MemoryQuery, NewMemoryChunk,
    RetrievalProfile,
};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};

const N_CHUNKS: usize = 1_000;
const N_RETRIEVES: usize = 200;
const K: usize = 8;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use a file-backed provider so the measurement reflects the same
    // I/O path the daemon uses. Discarded on exit.
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("bench.sqlite");
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open(&db_path, embedder).await?;

    // Ingest. We write through `write_batch` in chunks of 50 so the
    // single-call optimisation in the provider kicks in (one embed call
    // per batch, one transaction).
    let mut total_write = Duration::ZERO;
    let start = Instant::now();
    let batch_size = 50;
    let mut batch = Vec::with_capacity(batch_size);
    for i in 0..N_CHUNKS {
        batch.push(NewMemoryChunk {
            kind: ChunkKind::Chat,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: "bench".into(),
            content: format!(
                "chunk {i}: synthetic content. category={} sample={}",
                i % 50,
                i * 7919 % 10_000
            ),
            metadata: serde_json::json!({"i": i}),
            importance: None,
            shareable: false,
            pinned: false,
        });
        if batch.len() == batch_size || i + 1 == N_CHUNKS {
            let drained: Vec<_> = std::mem::take(&mut batch);
            let t = Instant::now();
            provider.write_batch(drained).await?;
            total_write += t.elapsed();
        }
    }
    let wall_write = start.elapsed();
    println!(
        "wrote {N_CHUNKS} chunks in {} ms (wall) / {} ms (writer-only)",
        wall_write.as_millis(),
        total_write.as_millis()
    );

    // Vary the query each iteration so the LRU cache doesn't collapse
    // every read to a hit. Mix 50 "category" buckets, 200 distinct shapes.
    let mut samples = Vec::with_capacity(N_RETRIEVES);
    for run in 0..N_RETRIEVES {
        let q = MemoryQuery {
            text: format!("category={} content {}", run % 50, run),
            kinds: Some(vec![ChunkKind::Chat]),
            since: None,
            until: None,
            session_id: None,
            caller_scope: CallerScope::Global,
            project_root_prefix: None,
            k: K,
            include_rollups: false,
            min_importance: None,
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: "bench".into(),
        };
        let t = Instant::now();
        let hits = provider.retrieve(q).await?;
        samples.push(t.elapsed());
        if run == 0 {
            // Sanity print on the first run so a totally broken bench is
            // immediately obvious (zero hits ⇒ the workload didn't seed
            // correctly).
            println!("  first retrieve returned {} hits", hits.len());
        }
    }

    samples.sort();
    let p = |q: f64| samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)];
    let mean: Duration = samples.iter().sum::<Duration>() / samples.len() as u32;
    println!(
        "retrieve k={K} over {N_CHUNKS} chunks, {N_RETRIEVES} runs:\n  \
         p50: {:>6.1} ms\n  p95: {:>6.1} ms\n  p99: {:>6.1} ms\n  \
         mean: {:>5.1} ms",
        p(0.50).as_secs_f64() * 1000.0,
        p(0.95).as_secs_f64() * 1000.0,
        p(0.99).as_secs_f64() * 1000.0,
        mean.as_secs_f64() * 1000.0,
    );

    // Budget check: p95 ≤ 150 ms. Exit non-zero so
    // CI can flag regressions automatically.
    let p95 = p(0.95);
    if p95 > Duration::from_millis(150) {
        eprintln!(
            "WARN: p95 {} ms exceeds the 150 ms Phase 2 budget",
            p95.as_millis()
        );
    }

    Ok(())
}
