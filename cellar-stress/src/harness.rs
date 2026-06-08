//! Harness orchestration — wires the daemon, spawns load generators,
//! samples metrics on the configured cadence, classifies against thresholds,
//! and emits the JSONL stream + final summary.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use chrono::Utc;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio::task::JoinSet;

use cel_act_gateway::AgentGateway;
use cel_cortex_daemon::bus::EventBus;
use cel_cortex_daemon::Daemon;
use cel_memory::MemoryProvider;
use cellar_ipc::Handler;

use crate::cli::Args;
use crate::load::LoadProfile;
use crate::metrics::{append_jsonl, DaemonGauge, MemoryGauge, MetricSample, MetricStream};
use crate::report::Summary;
use crate::thresholds::{ThresholdBreach, Thresholds};

/// What the harness returns to the binary.
#[derive(Debug)]
pub struct HarnessOutcome {
    /// Aggregated end-of-run summary.
    pub summary: Summary,
    /// Where the JSONL log was written.
    pub metrics_path: PathBuf,
    /// Where the JSON summary was written.
    pub summary_path: PathBuf,
    /// All samples taken (for callers that want to inspect them, e.g. tests).
    pub samples: Vec<MetricSample>,
}

/// Exit code semantics. `Ok` ↔ exit 0; `Tripped` ↔ exit 2 (per
/// `cellar-app-v1.md` §16 "exits 0 only if no failure thresholds tripped").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessExit {
    /// All thresholds passed.
    Ok,
    /// At least one threshold was breached at some sample.
    Tripped,
}

impl HarnessExit {
    /// Map to a process exit code.
    pub fn code(self) -> i32 {
        match self {
            HarnessExit::Ok => 0,
            HarnessExit::Tripped => 2,
        }
    }
}

impl HarnessOutcome {
    /// Derive the exit verdict.
    pub fn exit(&self) -> HarnessExit {
        if self.summary.green {
            HarnessExit::Ok
        } else {
            HarnessExit::Tripped
        }
    }
}

/// Bring up the daemon, run the generator mix for `args.duration`, sample
/// every `args.sample_interval`, and emit the JSONL stream + summary.
///
/// The harness exits early (and clean) if the supplied `cancel` Notify is
/// triggered — used by the binary's SIGINT handler.
pub async fn run(args: &Args, cancel: Arc<Notify>) -> Result<HarnessOutcome> {
    // Resolve thresholds and output dir.
    let thresholds = Thresholds {
        max_rss_mib: args.max_rss_mib,
        max_retrieve_p95_ms: args.max_retrieve_p95_ms,
        max_error_rate_per_min: args.max_error_rate_per_min,
    };
    // `_keep` is kept alive for the duration of the harness so the temp
    // directory survives until we're done writing into it.
    let (output_dir, _keep): (PathBuf, Option<TempDir>) = match &args.output_dir {
        Some(p) => {
            tokio::fs::create_dir_all(p)
                .await
                .with_context(|| format!("create output dir {}", p.display()))?;
            (p.clone(), None)
        }
        None => {
            let tmp = tempfile::tempdir().context("create temp output dir")?;
            (tmp.path().to_path_buf(), Some(tmp))
        }
    };
    tracing::info!(
        output_dir = %output_dir.display(),
        duration = ?args.duration_std(),
        sample_interval = ?args.sample_interval_std(),
        "harness: starting"
    );

    // Wire the daemon in-process. We use `wire_subsystems()` (in-memory
    // rules store) because persistence isn't useful for a stress run and
    // would add unnecessary I/O noise to the RSS measurements.
    let daemon = Daemon::wire_subsystems();

    let stream = Arc::new(MetricStream::new());
    let stop = Arc::new(AtomicBool::new(false));
    let profile: LoadProfile = args.load_profile.into();

    // Open the JSONL writer. `append=true` + truncate-on-create is the
    // intent; if the user passed --output_dir with a pre-existing
    // metrics.jsonl, we overwrite.
    let metrics_path = output_dir.join("metrics.jsonl");
    let mut metrics_file = tokio::fs::File::create(&metrics_path)
        .await
        .with_context(|| format!("create {}", metrics_path.display()))?;

    // Spawn generators.
    let mut tasks: JoinSet<()> = JoinSet::new();
    spawn_generators(
        &mut tasks,
        Arc::clone(&stop),
        Arc::clone(&stream),
        Arc::clone(&daemon.memory) as Arc<dyn MemoryProvider>,
        daemon.event_bus.clone(),
        Arc::clone(&daemon.gateway) as Arc<dyn AgentGateway>,
        Arc::clone(&daemon.ipc_handler) as Arc<dyn Handler>,
        profile,
        output_dir.clone(),
    )?;
    let generator_count = tasks.len();
    tracing::info!(generators = generator_count, "harness: generators spawned");

    // Sampling loop.
    let start_wall = Utc::now();
    let start_instant = Instant::now();
    let mut samples: Vec<MetricSample> = Vec::new();
    let mut breaches: Vec<ThresholdBreach> = Vec::new();
    let mut ticker = tokio::time::interval(args.sample_interval_std());
    // We don't want the first sample at t=0 (no load yet).
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume the immediate first tick

    let total_duration = args.duration_std();
    let cancel_for_loop = Arc::clone(&cancel);
    let pid = std::process::id();
    let mut sysinfo_state = SysInfoState::new(pid);
    loop {
        // Decide whether to take another sample or to stop.
        let elapsed = start_instant.elapsed();
        if elapsed >= total_duration {
            break;
        }
        let remaining = total_duration - elapsed;
        let next_tick = tokio::time::sleep(args.sample_interval_std().min(remaining));
        tokio::select! {
            _ = next_tick => {}
            _ = cancel_for_loop.notified() => {
                tracing::info!("harness: cancellation received, exiting sampling loop");
                break;
            }
        }
        let sample = collect_sample(&daemon, &stream, start_instant, &mut sysinfo_state).await;
        let breach_set = thresholds.classify(&sample, args.sample_interval_std());
        if !breach_set.is_empty() {
            tracing::warn!(
                breaches = breach_set.len(),
                uptime_s = sample.uptime_s,
                "harness: threshold breach(es) recorded for this sample"
            );
        }
        if let Err(e) = append_jsonl(&mut metrics_file, &sample).await {
            tracing::warn!(error = %e, "harness: failed to append sample to jsonl");
        }
        samples.push(sample);
        breaches.extend(breach_set);
    }

    // Stop generators and await them.
    stop.store(true, Ordering::SeqCst);
    let drain_deadline = Instant::now() + Duration::from_secs(2);
    while !tasks.is_empty() {
        let now = Instant::now();
        if now >= drain_deadline {
            tracing::warn!("harness: generator drain deadline reached, aborting remaining tasks");
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            break;
        }
        let remaining = drain_deadline - now;
        match tokio::time::timeout(remaining, tasks.join_next()).await {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                tracing::warn!("harness: timed out waiting on generators, aborting");
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                break;
            }
        }
    }

    metrics_file.flush().await.context("flush metrics.jsonl")?;
    drop(metrics_file);

    let end_wall = Utc::now();
    let summary = Summary::aggregate(start_wall, end_wall, &samples, breaches);

    // Persist the summary alongside the metrics log.
    let summary_path = output_dir.join("summary.json");
    let summary_json = serde_json::to_vec_pretty(&summary).context("serialize summary")?;
    tokio::fs::write(&summary_path, &summary_json)
        .await
        .with_context(|| format!("write {}", summary_path.display()))?;

    Ok(HarnessOutcome {
        summary,
        metrics_path,
        summary_path,
        samples,
    })
}

/// Spawn all load generators into `tasks`. Each generator loops until
/// `stop` is set. Returns early if any setup fails (e.g., can't create the
/// FS workdir).
#[allow(clippy::too_many_arguments)]
fn spawn_generators(
    tasks: &mut JoinSet<()>,
    stop: Arc<AtomicBool>,
    stream: Arc<MetricStream>,
    memory: Arc<dyn MemoryProvider>,
    event_bus: EventBus,
    gateway: Arc<dyn AgentGateway>,
    _handler: Arc<dyn Handler>,
    profile: LoadProfile,
    output_dir: PathBuf,
) -> Result<()> {
    // Filesystem load (real file create/modify/delete in a tempdir + matched
    // synthetic Fsevents publishes to the in-process event bus). The
    // tempdir lives inside the output dir so a debugger / human can poke
    // around after a crash, and it's cleaned up explicitly when the
    // generator stops.
    let fs_workdir = output_dir.join("fs_workdir");
    std::fs::create_dir_all(&fs_workdir)
        .with_context(|| format!("create {}", fs_workdir.display()))?;
    if let Some(interval) = LoadProfile::interval(profile.fs_ops_per_sec) {
        let stop_cl = Arc::clone(&stop);
        let stream_cl = Arc::clone(&stream);
        let event_bus_cl = event_bus.clone();
        let workdir = fs_workdir.clone();
        tasks.spawn(async move {
            generators::fs_loop(stop_cl, stream_cl, event_bus_cl, workdir, interval).await;
        });
    }

    // Synthetic process events.
    if let Some(interval) = LoadProfile::interval(profile.process_events_per_sec) {
        let stop_cl = Arc::clone(&stop);
        let stream_cl = Arc::clone(&stream);
        let event_bus_cl = event_bus.clone();
        tasks.spawn(async move {
            generators::process_loop(stop_cl, stream_cl, event_bus_cl, interval).await;
        });
    }

    // Gateway intercept calls.
    if let Some(interval) = LoadProfile::interval(profile.gateway_calls_per_sec) {
        let stop_cl = Arc::clone(&stop);
        let stream_cl = Arc::clone(&stream);
        let gw_cl = Arc::clone(&gateway);
        tasks.spawn(async move {
            generators::gateway_loop(stop_cl, stream_cl, gw_cl, interval).await;
        });
    }

    // Synthetic agent chats. We open one session up front and write chat
    // chunks against it; the embedded agent runtime isn't required (and
    // isn't available without an LLM provider configured in tests).
    if let Some(interval) = LoadProfile::interval(profile.agent_chats_per_sec) {
        let stop_cl = Arc::clone(&stop);
        let stream_cl = Arc::clone(&stream);
        let mem_cl = Arc::clone(&memory);
        tasks.spawn(async move {
            generators::agent_chat_loop(stop_cl, stream_cl, mem_cl, interval).await;
        });
    }

    // Direct memory writes.
    if let Some(interval) = LoadProfile::interval(profile.memory_writes_per_sec) {
        let stop_cl = Arc::clone(&stop);
        let stream_cl = Arc::clone(&stream);
        let mem_cl = Arc::clone(&memory);
        tasks.spawn(async move {
            generators::memory_write_loop(stop_cl, stream_cl, mem_cl, interval).await;
        });
    }

    // Memory retrieves (the headline latency metric).
    if let Some(interval) = LoadProfile::interval(profile.memory_retrieves_per_sec) {
        let stop_cl = Arc::clone(&stop);
        let stream_cl = Arc::clone(&stream);
        let mem_cl = Arc::clone(&memory);
        tasks.spawn(async move {
            generators::memory_retrieve_loop(stop_cl, stream_cl, mem_cl, interval).await;
        });
    }

    Ok(())
}

/// Cached `sysinfo::System` so we don't pay the rescan cost every sample.
struct SysInfoState {
    sys: sysinfo::System,
    pid: sysinfo::Pid,
}

impl SysInfoState {
    fn new(pid: u32) -> Self {
        use sysinfo::{ProcessRefreshKind, RefreshKind, System};
        let sys = System::new_with_specifics(
            RefreshKind::new().with_processes(ProcessRefreshKind::new().with_memory().with_cpu()),
        );
        Self {
            sys,
            pid: sysinfo::Pid::from(pid as usize),
        }
    }

    fn snapshot(&mut self) -> (f64, f64) {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
        let kind = ProcessRefreshKind::new().with_memory().with_cpu();
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::Some(&[self.pid]), true, kind);
        if let Some(p) = self.sys.process(self.pid) {
            // `sysinfo::Process::memory()` returns bytes on 0.32 (and matches
            // RSS on macOS).
            let mib = p.memory() as f64 / (1024.0 * 1024.0);
            let cpu = f64::from(p.cpu_usage());
            (mib, cpu)
        } else {
            (0.0, 0.0)
        }
    }
}

/// Build one `MetricSample` from the current state of the daemon + stream.
async fn collect_sample(
    daemon: &Daemon,
    stream: &MetricStream,
    start: Instant,
    sysinfo: &mut SysInfoState,
) -> MetricSample {
    let uptime_s = start.elapsed().as_secs();
    let (rss_mib, cpu_pct) = sysinfo.snapshot();
    let win = stream.drain_window();
    // Cumulative errors and successes are folded back onto the sample so
    // post-hoc JSONL inspection doesn't require running totals.
    let cumulative_ok = win.cumulative_ok;
    let cumulative_errors = win.cumulative_err;

    // Memory provider stats (chunks, sessions, db size).
    let memory_gauge = match daemon.memory.stats().await {
        Ok(s) => MemoryGauge {
            total_chunks: s.total_chunks as u64,
            open_sessions: s.open_sessions as u64,
            db_bytes: s.db_bytes,
        },
        Err(e) => {
            tracing::debug!(error = %e, "memory stats unavailable");
            MemoryGauge::default()
        }
    };

    // Daemon status (rule count, watchlist count, recent fires, …).
    let daemon_gauge = match daemon.ipc_handler.daemon_status().await {
        Ok(d) => DaemonGauge {
            rules_total: d.rules.total,
            watchlists_total: d.watchlists.total,
            recent_fires_24h: d.recent_fires_24h,
            agent_sessions_active: d.agent_sessions_active,
            pending_confirmations: d.pending_confirmations,
        },
        Err(e) => {
            tracing::debug!(error = %e, "daemon.status unavailable");
            DaemonGauge::default()
        }
    };

    MetricSample {
        ts: Utc::now(),
        uptime_s,
        rss_mib,
        cpu_pct,
        latencies_ms: win.latencies,
        ok_counts: win.ok_counts,
        err_counts: win.err_counts,
        cumulative_errors,
        cumulative_ok,
        memory: memory_gauge,
        daemon: daemon_gauge,
    }
}

/// Tracing helper — initialise a sane stdout subscriber when the binary is
/// run directly. Idempotent across multiple test runs (rejects re-init,
/// swallows the resulting error).
pub fn init_tracing(verbose: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = if verbose {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "cellar_stress=debug,info".into())
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,cellar_stress=info".into())
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Latency-clamp helper used by tests: if a generator's call is so slow we
/// can't time it accurately, treat anything > 60 s as 60 s. Private — but
/// exported into the unit tests through the module path.
#[doc(hidden)]
pub fn clamp_latency(d: Duration) -> Duration {
    const MAX: Duration = Duration::from_secs(60);
    if d > MAX {
        MAX
    } else {
        d
    }
}

// ─── Generator implementations ──────────────────────────────────────────

mod generators {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use cel_act_gateway::{AgentGateway, ProposedAction};
    use cel_cortex_daemon::bus::EventBus;
    use cel_memory::{
        CallerScope, ChunkKind, ChunkSource, MemoryProvider, MemoryQuery, NewMemoryChunk,
        NewMemorySession, RetrievalProfile,
    };
    use cellar_types::{Event, EventKind, EventSource};
    use serde_json::Value;

    use crate::metrics::MetricStream;

    const CALLER: &str = "stress-harness";

    /// File-system generator. Each iteration creates a small file,
    /// modifies it, deletes it, and publishes the three matching
    /// `Fsevents` events to the in-process bus (since the real FSEvents
    /// adapter only spawns from the daemon binary, not from
    /// `Daemon::wire_subsystems()`).
    pub async fn fs_loop(
        stop: Arc<AtomicBool>,
        stream: Arc<MetricStream>,
        event_bus: EventBus,
        workdir: PathBuf,
        interval: Duration,
    ) {
        let mut counter: u64 = 0;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Pre-tick eaten so we don't fire immediately during ramp-up.
        ticker.tick().await;
        while !stop.load(Ordering::SeqCst) {
            ticker.tick().await;
            counter = counter.wrapping_add(1);
            let path = workdir.join(format!("stress_{counter:08}.tmp"));

            // create
            let started = Instant::now();
            match tokio::fs::write(&path, b"create").await {
                Ok(()) => {
                    stream.record_ok("fs.create", started.elapsed());
                    event_bus.publish(
                        Event::now(EventSource::Fsevents, EventKind::FileCreated)
                            .with_data("path", Value::String(path.display().to_string()))
                            .with_data("size_bytes", Value::from(6u64)),
                    );
                }
                Err(e) => {
                    stream.record_err("fs.create");
                    tracing::trace!(error = %e, "fs create failed");
                    continue;
                }
            }

            // modify
            let started = Instant::now();
            match tokio::fs::write(&path, b"modify").await {
                Ok(()) => {
                    stream.record_ok("fs.modify", started.elapsed());
                    event_bus.publish(
                        Event::now(EventSource::Fsevents, EventKind::FileModified)
                            .with_data("path", Value::String(path.display().to_string()))
                            .with_data("size_bytes", Value::from(6u64)),
                    );
                }
                Err(_) => {
                    stream.record_err("fs.modify");
                }
            }

            // delete
            let started = Instant::now();
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {
                    stream.record_ok("fs.delete", started.elapsed());
                    event_bus.publish(
                        Event::now(EventSource::Fsevents, EventKind::FileDeleted)
                            .with_data("path", Value::String(path.display().to_string())),
                    );
                }
                Err(_) => {
                    stream.record_err("fs.delete");
                }
            }
        }

        // Best-effort: scrub the workdir on exit so a long run doesn't leave
        // gigabytes of `stress_*.tmp` behind.
        let _ = tokio::fs::remove_dir_all(&workdir).await;
    }

    /// Synthetic process event generator. Publishes alternating
    /// `ProcessStarted` / `ProcessStopped` events to the event bus.
    pub async fn process_loop(
        stop: Arc<AtomicBool>,
        stream: Arc<MetricStream>,
        event_bus: EventBus,
        interval: Duration,
    ) {
        let mut counter: u64 = 0;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        while !stop.load(Ordering::SeqCst) {
            ticker.tick().await;
            counter = counter.wrapping_add(1);
            let started = Instant::now();
            let kind = if counter.is_multiple_of(2) {
                EventKind::ProcessStopped
            } else {
                EventKind::ProcessStarted
            };
            event_bus.publish(
                Event::now(EventSource::Process, kind)
                    .with_data("pid", Value::from(10_000 + counter))
                    .with_data("name", Value::String(format!("stress-proc-{counter}"))),
            );
            stream.record_ok("events.process", started.elapsed());
        }
    }

    /// Gateway call generator. Drives `AgentGateway::intercept_tool_call`
    /// with a benign `ping` action. Each call also writes Action + (any)
    /// Fire chunks via the gateway's audit trail.
    pub async fn gateway_loop(
        stop: Arc<AtomicBool>,
        stream: Arc<MetricStream>,
        gateway: Arc<dyn AgentGateway>,
        interval: Duration,
    ) {
        let mut counter: u64 = 0;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        while !stop.load(Ordering::SeqCst) {
            ticker.tick().await;
            counter = counter.wrapping_add(1);
            let action = ProposedAction {
                caller: CALLER.into(),
                action_type: "ping".into(),
                action_args: serde_json::json!({ "seq": counter }),
                agent_session_id: None,
                project_root: None,
            };
            let started = Instant::now();
            match gateway.intercept_tool_call(action).await {
                Ok(_) => stream.record_ok("gateway.intercept", started.elapsed()),
                Err(e) => {
                    stream.record_err("gateway.intercept");
                    tracing::trace!(error = %e, "gateway intercept failed");
                }
            }
        }
    }

    /// Agent-chat generator. Opens a session up-front and writes chat
    /// chunks into it.
    pub async fn agent_chat_loop(
        stop: Arc<AtomicBool>,
        stream: Arc<MetricStream>,
        memory: Arc<dyn MemoryProvider>,
        interval: Duration,
    ) {
        let session = match memory
            .open_session(NewMemorySession {
                caller_id: CALLER.into(),
                title: Some("stress-chat".into()),
                metadata: Value::Null,
            })
            .await
        {
            Ok(s) => s,
            Err(e) => {
                stream.record_err("agent.session.create");
                tracing::warn!(error = %e, "agent chat: open_session failed");
                return;
            }
        };
        stream.record_ok("agent.session.create", Duration::from_millis(0));

        let mut counter: u64 = 0;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        while !stop.load(Ordering::SeqCst) {
            ticker.tick().await;
            counter = counter.wrapping_add(1);
            let chunk = NewMemoryChunk {
                kind: ChunkKind::Chat,
                source: ChunkSource::Embedded,
                session_id: Some(session.id.clone()),
                project_root: None,
                caller_id: CALLER.into(),
                content: format!("stress chat #{counter}: synthetic user message"),
                metadata: serde_json::json!({ "seq": counter }),
                importance: None,
                shareable: false,
                pinned: false,
            };
            let started = Instant::now();
            match memory.write(chunk).await {
                Ok(_) => stream.record_ok("agent.message", started.elapsed()),
                Err(_) => stream.record_err("agent.message"),
            }
        }

        let _ = memory
            .close_session(&session.id, cel_memory::SessionOutcome::Success)
            .await;
    }

    /// Direct memory-write generator. Writes `Observation` chunks (not
    /// associated with a session) at the configured rate.
    pub async fn memory_write_loop(
        stop: Arc<AtomicBool>,
        stream: Arc<MetricStream>,
        memory: Arc<dyn MemoryProvider>,
        interval: Duration,
    ) {
        let mut counter: u64 = 0;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        while !stop.load(Ordering::SeqCst) {
            ticker.tick().await;
            counter = counter.wrapping_add(1);
            let chunk = NewMemoryChunk {
                kind: ChunkKind::Observation,
                source: ChunkSource::System,
                session_id: None,
                project_root: None,
                caller_id: CALLER.into(),
                content: format!(
                    "stress observation #{counter}: synthetic write to test memory throughput"
                ),
                metadata: serde_json::json!({ "seq": counter }),
                importance: None,
                shareable: false,
                pinned: false,
            };
            let started = Instant::now();
            match memory.write(chunk).await {
                Ok(_) => stream.record_ok("write", started.elapsed()),
                Err(_) => stream.record_err("write"),
            }
        }
    }

    /// Memory-retrieve generator. Each retrieve hits the `AgentChatTurn`
    /// profile and is timed against the `retrieve` metric — this is the
    /// metric the acceptance gate's p95 check reads.
    pub async fn memory_retrieve_loop(
        stop: Arc<AtomicBool>,
        stream: Arc<MetricStream>,
        memory: Arc<dyn MemoryProvider>,
        interval: Duration,
    ) {
        let mut counter: u64 = 0;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        while !stop.load(Ordering::SeqCst) {
            ticker.tick().await;
            counter = counter.wrapping_add(1);
            let query = MemoryQuery {
                text: format!("stress query #{counter}"),
                kinds: None,
                since: None,
                until: None,
                session_id: None,
                caller_scope: CallerScope::Own,
                project_root_prefix: None,
                k: 8,
                include_rollups: true,
                min_importance: None,
                profile: RetrievalProfile::AgentChatTurn,
                caller_id: CALLER.into(),
            };
            let started = Instant::now();
            match memory.retrieve(query).await {
                Ok(_) => stream.record_ok(crate::thresholds::RETRIEVE_METHOD, started.elapsed()),
                Err(_) => stream.record_err(crate::thresholds::RETRIEVE_METHOD),
            }
        }
    }
}

/// Re-export the percentile helper at module level so it's reachable from
/// tests that want to assert tail behaviour on a custom sample set.
pub use crate::metrics::percentile;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_latency_passes_short() {
        let d = Duration::from_millis(50);
        assert_eq!(clamp_latency(d), d);
    }

    #[test]
    fn clamp_latency_caps_long() {
        let d = Duration::from_secs(3600);
        assert_eq!(clamp_latency(d), Duration::from_secs(60));
    }

    #[test]
    fn harness_exit_codes() {
        assert_eq!(HarnessExit::Ok.code(), 0);
        assert_eq!(HarnessExit::Tripped.code(), 2);
    }
}
