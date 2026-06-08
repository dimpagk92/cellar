//! `cel-cortex-daemon` binary entry point.
//!
//! Wires every subsystem, binds the IPC socket at `~/.cellar/daemon.sock`,
//! runs the accept loop, waits for `SIGINT` (Ctrl-C), then cleans up.
//!
//! Phase 1 remaining work: event bus, Cortex goalless mode, process poller,
//! FSEvents adapter, MCP server. All plug into the locked trait surfaces
//! already wired here.

use std::path::PathBuf;
use std::sync::Arc;

use cel_cortex_daemon::memory_offdevice_governance::MatcherOffdeviceHook;
use cel_cortex_daemon::memory_write_governance::MatcherWriteHook;
use cel_cortex_daemon::sweeper::{self, SweeperConfig, WallClock};
use cel_cortex_daemon::{fsevents, matcher_task, process_poller, signals_poller, Daemon};
use cel_memory::{MemoryProvider, MemoryWriteHook, OffdeviceCallHook};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};
use cel_signals::{PlatformSignalBus, SignalBus};
use cel_summarizer::{AnthropicSummarizer, OllamaSummarizer, ANTHROPIC_API_KEY_ENV, PROVIDER_ENV};
use cellar_ipc::Server;
use cellar_rules_store::SqliteRulesStore;
use tokio::task::JoinSet;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _log_guard = init_logging()?;

    tracing::info!("cel-cortex-daemon starting");

    let rules_db_path = rules_db_path()?;
    tracing::info!(path = %rules_db_path.display(), "opening rules store");

    // Open the rules store first so we can hand a clone to
    // `MatcherWriteHook` before constructing the memory provider. The same
    // `Arc<SqliteRulesStore>` flows on into `Daemon::wire_…` so writes via
    // `daemon.rules_store` and reads from the matcher-write hook share one
    // hot-reloadable snapshot.
    if let Some(parent) = rules_db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let rules_store = SqliteRulesStore::open(&rules_db_path)?;

    // Memory: default to a file-backed SqliteMemoryProvider so chunks
    // survive restart. Honors `CELLAR_MEMORY_DB` (special value `:memory:`
    // keeps the store in-memory only — useful for one-shot smoke runs).
    // Falls back to BasicMemoryProvider with a logged warning if SQLite
    // open or fastembed setup fails (so daemon boot is not blocked by
    // memory-subsystem trouble).
    //
    // `MatcherWriteHook` is attached here so rule-matcher governance runs
    // on every memory write — see `memory_write_governance.rs`.
    let write_hook: Arc<dyn MemoryWriteHook> = Arc::new(MatcherWriteHook::new(
        Arc::clone(&rules_store),
        Arc::clone(&rules_store),
    ));
    // `MatcherOffdeviceHook` lets `Veto` rules on
    // `MemoryOffdeviceCallAttempted` events block cloud summarizer calls
    // before they leave the device. Attached to the Anthropic summarizer
    // inside `build_memory_provider`.
    let offdevice_hook: Arc<dyn OffdeviceCallHook> = Arc::new(MatcherOffdeviceHook::new(
        Arc::clone(&rules_store),
        Arc::clone(&rules_store),
    ));
    let memory = build_memory_provider(Arc::clone(&write_hook), Arc::clone(&offdevice_hook)).await;

    let daemon = Daemon::wire_subsystems_with_store_and_memory(
        Arc::clone(&rules_store),
        Arc::clone(&memory),
    );

    let stats = daemon.memory.stats().await?;
    tracing::info!(?stats, "memory subsystem wired");
    let initial_rules = daemon.rules_store.list_rules().len();
    tracing::info!(
        rules = initial_rules,
        "rules store wired (sqlite, file-backed, hot-reload via Arc clones)"
    );
    tracing::info!("gateway subsystem wired");

    // Quick sanity-check that the gateway routes a synthetic action all the
    // way through the matcher and into the memory audit trail.
    let outcome = daemon
        .gateway
        .intercept(cel_act_gateway::ProposedAction {
            caller: "system".into(),
            action_type: "ping".into(),
            action_args: serde_json::Value::Null,
            agent_session_id: None,
            project_root: None,
        })
        .await?;
    tracing::info!(?outcome, "gateway self-test succeeded");

    // ---- Phase 1 background subsystems ----
    //
    // The event bus is the fan-in point for every ambient source (signals,
    // process poller, FSEvents). The matcher consumer task is the fan-out
    // into the rule matcher + memory audit trail. Both run for the
    // lifetime of the daemon.
    //
    // Rules and watchlists come from the shared `Arc<SqliteRulesStore>` —
    // every write through `daemon.rules_store` is visible here on the
    // matcher's next snapshot, without any reload signal. See
    // `cellar-rules-store/tests/hot_reload.rs` for the contract test.
    let event_bus = &daemon.event_bus;

    // Ring-filler tasks: drain the broadcast buses into bounded ring
    // buffers for `events.recent` / `fires.recent` backfill. These run
    // for the daemon's lifetime — dropping the buses on shutdown ends
    // them naturally.
    let event_ring = daemon.event_ring.clone();
    let mut event_ring_rx = event_bus.subscribe();
    let event_ring_task = tokio::spawn(async move {
        while let Ok(event) = event_ring_rx.recv().await {
            event_ring.push(event);
        }
    });
    let fire_ring = daemon.fire_ring.clone();
    let mut fire_ring_rx = daemon.fire_bus.subscribe();
    let fire_ring_task = tokio::spawn(async move {
        while let Ok(fire) = fire_ring_rx.recv().await {
            fire_ring.push(fire);
        }
    });
    tracing::info!("ring filler tasks spawned (events + fires)");

    let matcher_handle = matcher_task::spawn(
        event_bus,
        daemon.rules_store.clone(),
        daemon.rules_store.clone(),
        daemon.memory.clone(),
        Some(daemon.cooldown.clone()),
        daemon.webhook_hook.clone(),
        Some(daemon.fire_bus.clone()),
        Some(daemon.confirmation_broker.clone()),
    );
    tracing::info!(
        cooldown = true,
        webhooks = daemon.webhook_hook.is_some(),
        fires = true,
        "matcher consumer task spawned"
    );

    let poller_handle = process_poller::spawn(event_bus, process_poller::PollerConfig::default());
    tracing::info!("process poller spawned");

    // The signals poller is the v1 stand-in for "Cortex goalless mode" — it
    // surfaces the AX-derived slice the matcher needs (frontmost app +
    // visible window changes) without dragging the full perception
    // pipeline into the daemon. See `signals_poller.rs` module docs.
    let signal_bus: Arc<dyn SignalBus> = Arc::new(PlatformSignalBus::new());
    let signals_handle = signals_poller::spawn(
        event_bus,
        signal_bus,
        signals_poller::SignalsPollerConfig::default(),
    );
    tracing::info!("signals poller spawned");

    // FSEvents is best-effort: if the user's home isn't watchable (e.g.
    // running in a sandbox), log and continue without filesystem events.
    let fsevents_handle = match fsevents::spawn(event_bus, fsevents::AdapterConfig::default()) {
        Ok(h) => {
            tracing::info!("fsevents adapter spawned");
            Some(h)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "fsevents adapter failed to start; continuing without filesystem events"
            );
            None
        }
    };

    // Memory sweeper: daily aging + daily rollup + weekly rule rollup.
    // Default schedule from `SweeperConfig::production()`. The sweeper
    // gracefully no-ops when the memory provider returns
    // `NotImplemented` (e.g., no summarizer configured), so this is
    // safe to enable unconditionally.
    let sweeper_handle = sweeper::spawn(
        daemon.memory.clone(),
        SweeperConfig::production(),
        Arc::new(WallClock),
    );
    tracing::info!("memory sweeper spawned");

    // Compute the IPC socket path. Honours `CELLAR_DAEMON_SOCK` for tests
    // and packaging overrides; defaults to `$HOME/.cellar/daemon.sock` per
    // cellar-app-v1.md §15.
    let socket_path = ipc_socket_path()?;
    tracing::info!(path = %socket_path.display(), "binding IPC socket");

    let server = Server::bind_with_arc(&socket_path, Arc::clone(&daemon.ipc_handler)).await?;
    let bound_path = server.socket_path().to_path_buf();
    tracing::info!(
        path = %bound_path.display(),
        "ipc server listening (mode 0600, owner only)"
    );

    // Run the accept loop in a JoinSet so per-connection tasks are tracked.
    let mut conn_tasks: JoinSet<()> = JoinSet::new();
    let server_task = tokio::spawn(async move {
        if let Err(e) = server.run(&mut conn_tasks).await {
            tracing::error!(error = %e, "ipc server loop exited with error");
        }
    });

    tracing::info!("daemon ready — waiting for SIGINT (Ctrl-C) to stop");

    // Wait for Ctrl-C. The server task is aborted on shutdown; if it
    // exits unexpectedly before then we'll catch that as the abort
    // succeeding immediately on a non-running task.
    tokio::signal::ctrl_c().await?;
    tracing::info!("SIGINT received, shutting down");

    // Best-effort cleanup. Aborting the server task drops the listener;
    // aborting the background tasks unregisters the FSEvents watcher and
    // stops the process poller. The socket file is removed if still present.
    server_task.abort();
    matcher_handle.abort();
    poller_handle.abort();
    signals_handle.abort();
    event_ring_task.abort();
    fire_ring_task.abort();
    sweeper_handle.abort();
    daemon.subscription_registry.abort_all();
    if let Some(h) = &fsevents_handle {
        h.abort();
    }
    let _ = server_task.await;
    let _ = matcher_handle.await;
    let _ = poller_handle.await;
    let _ = signals_handle.await;
    let _ = sweeper_handle.await;
    if let Some(h) = fsevents_handle {
        let _ = h.await;
    }
    let _ = tokio::fs::remove_file(&bound_path).await;
    tracing::info!("daemon stopped");

    Ok(())
}

/// Resolve the socket path the daemon should bind to.
///
/// Honors the `CELLAR_DAEMON_SOCK` env var when set (used by tests and
/// packaging overrides). Otherwise defaults to `$HOME/.cellar/daemon.sock`.
/// Returns an error if `$HOME` is unset and no override was provided.
fn ipc_socket_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(p) = std::env::var("CELLAR_DAEMON_SOCK") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME")
        .map_err(|_| "HOME env var not set; pass CELLAR_DAEMON_SOCK to override")?;
    Ok(PathBuf::from(home).join(".cellar").join("daemon.sock"))
}

/// Construct the daemon's memory provider.
///
/// Default: file-backed [`SqliteMemoryProvider`] at `$HOME/.cellar/memory.sqlite`.
/// Honors `CELLAR_MEMORY_DB` for the path override. The special value
/// `:memory:` falls back to in-memory SQLite (still SqliteMemoryProvider,
/// just no on-disk file).
///
/// If anything fails (HOME unset, file I/O error, fastembed init error),
/// logs a warning and falls back to [`cel_memory::BasicMemoryProvider`]
/// so the daemon still boots — memory writes happen in RAM and are lost
/// on restart, which matches the v1 behavior callers already tolerate.
///
/// v1 ships with [`MockEmbedder`] for production too: real embeddings via
/// fastembed are gated behind the `fastembed` feature on the
/// `cel-memory-sqlite` crate, which downloads a ~130 MB model on first
/// instantiation. Retrieval doesn't work in v1 (Phase 2 work) so the
/// embedder choice is mostly storage-only for now.
async fn build_memory_provider(
    write_hook: Arc<dyn MemoryWriteHook>,
    offdevice_hook: Arc<dyn OffdeviceCallHook>,
) -> Arc<dyn MemoryProvider> {
    let path_override = std::env::var("CELLAR_MEMORY_DB").ok();
    let path = match path_override.as_deref() {
        Some(":memory:") => {
            tracing::info!(
                "memory: CELLAR_MEMORY_DB=:memory: — using in-memory SqliteMemoryProvider"
            );
            None
        }
        Some(p) => Some(PathBuf::from(p)),
        None => match std::env::var("HOME") {
            Ok(home) => Some(PathBuf::from(home).join(".cellar").join("memory.sqlite")),
            Err(_) => {
                tracing::warn!(
                    "memory: HOME unset and no CELLAR_MEMORY_DB override — \
                     falling back to BasicMemoryProvider (in-memory, lost on restart)"
                );
                return Arc::new(
                    cel_memory::BasicMemoryProvider::new().with_write_hook(write_hook),
                );
            }
        },
    };

    let embedder = Arc::new(MockEmbedder::new());
    let result = match &path {
        Some(p) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            SqliteMemoryProvider::open(p, embedder).await
        }
        None => SqliteMemoryProvider::open_in_memory(embedder).await,
    };
    // Build the summarizer (Anthropic default, Ollama fallback). Wire
    // the off-device hook *only* into the Anthropic path — Ollama runs
    // locally so cloud-bound governance doesn't apply. Summarizer
    // construction is best-effort: if no provider is configured, the
    // daemon still boots; `rollup_day` and `summarize_session` then
    // return `NotImplemented`.
    let summarizer = build_summarizer_with_governance(Arc::clone(&offdevice_hook));
    match result {
        Ok(mut provider) => {
            tracing::info!(
                path = ?path,
                "memory: SqliteMemoryProvider open (sqlite-vec loaded, migrations applied, \
                 matcher-driven write hook attached)"
            );
            provider = provider.with_write_hook(write_hook);
            if let Some(s) = summarizer {
                provider = provider.with_summarizer(s);
                tracing::info!(
                    "memory: summarizer attached (rollup_day + summarize_session enabled)"
                );
            }
            Arc::new(provider)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "memory: SqliteMemoryProvider failed to open — falling back to \
                 BasicMemoryProvider. Chunks will be lost on restart."
            );
            Arc::new(cel_memory::BasicMemoryProvider::new().with_write_hook(write_hook))
        }
    }
}

/// Best-effort summarizer construction. Selection follows
/// `CELLAR_MEMORY_SUMMARIZER_PROVIDER` (default `anthropic`, fallback
/// `ollama`). The Anthropic path wires the off-device hook so the
/// rule matcher can govern cloud calls; the Ollama path bypasses
/// because it's local.
///
/// Returns `None` (with a warning) if neither provider could be
/// constructed — the daemon still boots, rollup APIs return
/// `NotImplemented`.
fn build_summarizer_with_governance(
    offdevice_hook: Arc<dyn OffdeviceCallHook>,
) -> Option<Arc<dyn cel_memory::Summarizer>> {
    let kind = std::env::var(PROVIDER_ENV)
        .ok()
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "anthropic".to_string());
    match kind.as_str() {
        "ollama" => match OllamaSummarizer::from_env() {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "memory: summarizer construction failed (ollama); rollups disabled"
                );
                None
            }
        },
        _ => {
            // Anthropic (or unknown — treat as anthropic w/ fallback).
            let key_present = std::env::var(ANTHROPIC_API_KEY_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .is_some();
            if key_present {
                match AnthropicSummarizer::from_env() {
                    Ok(s) => Some(Arc::new(s.with_offdevice_hook(offdevice_hook))),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "memory: Anthropic summarizer construction failed; trying Ollama"
                        );
                        match OllamaSummarizer::from_env() {
                            Ok(s) => Some(Arc::new(s)),
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "memory: summarizer construction failed (ollama fallback); rollups disabled"
                                );
                                None
                            }
                        }
                    }
                }
            } else {
                tracing::info!(
                    "memory: ANTHROPIC_API_KEY not set; trying Ollama fallback for summarizer"
                );
                match OllamaSummarizer::from_env() {
                    Ok(s) => Some(Arc::new(s)),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "memory: summarizer construction failed; rollups disabled"
                        );
                        None
                    }
                }
            }
        }
    }
}

/// Resolve the rules SQLite path the daemon should open.
///
/// Honors `CELLAR_RULES_DB` when set (tests + packaging overrides). The
/// special value `:memory:` keeps the store in-memory — only useful for
/// dry-run smoke tests where rule persistence isn't wanted.
///
/// Default: `$HOME/.cellar/rules.sqlite`. The parent directory is created
/// on demand by `Daemon::wire_subsystems_with_db`.
fn rules_db_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(p) = std::env::var("CELLAR_RULES_DB") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME")
        .map_err(|_| "HOME env var not set; pass CELLAR_RULES_DB to override")?;
    Ok(PathBuf::from(home).join(".cellar").join("rules.sqlite"))
}

/// Initialise tracing.
///
/// - When `CELLAR_LOG_DIR` is set (or `$HOME/.cellar/logs` is writable),
///   logs are written via `tracing-appender::rolling::daily` to a file
///   per day plus a console layer. Returns a `WorkerGuard` that must be
///   held for the lifetime of the process so the appender's worker
///   thread flushes on shutdown.
/// - Otherwise (env var unset and HOME missing), falls back to
///   stdout-only — useful for one-shot smoke runs from the shell.
///
/// Rotation policy: daily files named `daemon.YYYY-MM-DD.log`. macOS's
/// `launchd` further redirects whatever the process writes to stderr to
/// the path in the LaunchAgent plist; the rolling file is the daemon's
/// own audit log, distinct from the LaunchAgent capture.
fn init_logging(
) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>, Box<dyn std::error::Error>> {
    use tracing_subscriber::prelude::*;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,cel_cortex_daemon=debug".into());

    let log_dir = std::env::var("CELLAR_LOG_DIR").ok().or_else(|| {
        std::env::var("HOME").ok().map(|h| {
            PathBuf::from(h)
                .join(".cellar")
                .join("logs")
                .display()
                .to_string()
        })
    });

    // Console layer is always on so the daemon is debuggable from the
    // shell even when started directly.
    let console_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);

    if let Some(dir) = log_dir {
        let dir_path = PathBuf::from(&dir);
        if let Err(e) = std::fs::create_dir_all(&dir_path) {
            // Fall back to console-only if we can't create the log dir.
            tracing_subscriber::registry()
                .with(env_filter)
                .with(console_layer)
                .init();
            tracing::warn!(
                error = %e,
                dir = %dir_path.display(),
                "could not create log dir; console logging only"
            );
            return Ok(None);
        }
        let file_appender = tracing_appender::rolling::daily(&dir_path, "daemon.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(non_blocking);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(console_layer)
            .with(file_layer)
            .init();

        eprintln!(
            "[cel-cortex-daemon] rolling daily log: {}/daemon.log.YYYY-MM-DD",
            dir_path.display()
        );
        Ok(Some(guard))
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(console_layer)
            .init();
        Ok(None)
    }
}
