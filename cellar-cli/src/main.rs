//! `cellar` CLI — talks to the running daemon over its UDS socket.
//!
//! Use cases:
//!
//! - Operate the daemon outside the Tauri UI (`cellar doctor`,
//!   `cellar status`, log-tailing).
//! - Scriptable rule management (`cellar rules add < rule.json`).
//! - Quick verification on a fresh install before the UI ships.
//!
//! The CLI is a thin wrapper around the locked IPC surface. Every
//! subcommand corresponds to one or two IPC methods. The `service`
//! subcommands are the exception — they manipulate the LaunchAgent
//! plist and call `launchctl`, so they work even when the daemon is
//! not running.
//!
//! LaunchAgent flow:
//! - `cellar service install` fills `__DAEMON_PATH__`/`__LOG_DIR__`
//!   in the bundled plist template, writes
//!   `~/Library/LaunchAgents/com.cellar.daemon.plist`, and loads it
//!   via `launchctl load -w`.
//! - `cellar service uninstall` unloads and removes the plist.
//! - `cellar service status` shows whether the plist is installed and
//!   whether launchd has the service loaded.

#![warn(rust_2018_idioms)]

mod doctor;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use cellar_ipc::params::confirmation::ConfirmationDecisionWire;
use cellar_ipc::params::events::EventsRecentParams;
use cellar_ipc::params::fires::FiresRecentParams;
use cellar_ipc::params::stream_filter::StreamFilter;
use cellar_ipc::params::system::SystemHelloParams;
use cellar_ipc::Client;
use clap::{Parser, Subcommand};
use serde_json::Value;

pub(crate) const PROTOCOL_VERSION: &str = "1";

/// `cellar` — operate and inspect the Cellar daemon from the shell.
#[derive(Debug, Parser)]
#[command(name = "cellar", version, about, long_about = None)]
struct Cli {
    /// Override the daemon socket path. Default:
    /// `$CELLAR_DAEMON_SOCK` if set, else `~/.cellar/daemon.sock`.
    #[arg(long, env = "CELLAR_DAEMON_SOCK", global = true)]
    socket: Option<PathBuf>,

    /// Emit pretty-printed JSON instead of human-friendly tables.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print `daemon.status` (uptime, rule + watchlist counts, health).
    Status,

    /// Health check: socket exists, daemon responds to system.hello +
    /// daemon.status, advertised capabilities listed, etc.
    Doctor,

    /// Print the daemon's `system.hello` capability set.
    Capabilities,

    /// Rule management.
    Rules {
        #[command(subcommand)]
        cmd: RulesCmd,
    },

    /// Watchlist management.
    Watchlists {
        #[command(subcommand)]
        cmd: WatchlistsCmd,
    },

    /// Webhook management.
    Webhooks {
        #[command(subcommand)]
        cmd: WebhooksCmd,
    },

    /// Activity inspection (events + fires).
    Activity {
        #[command(subcommand)]
        cmd: ActivityCmd,
    },

    /// Confirmation flow operations.
    Confirmation {
        #[command(subcommand)]
        cmd: ConfirmationCmd,
    },

    /// Manage the daemon as a macOS LaunchAgent (install / uninstall / status).
    ///
    /// These subcommands call `launchctl` directly and work even when the
    /// daemon is not running — no socket connection is required.
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
}

#[derive(Debug, Subcommand)]
enum RulesCmd {
    /// List all rules.
    List,
    /// Get one rule by id.
    Get {
        /// Rule id.
        id: String,
    },
    /// Add a rule from a JSON file. Use `-` for stdin.
    Add {
        /// Path to a JSON file containing the typed `Rule`. Use `-` for stdin.
        file: PathBuf,
    },
    /// Compile a natural-language rule (preview only — does not save).
    Compile {
        /// The user's natural-language rule sentence.
        nl: String,
    },
    /// Pause / resume / remove a rule by id.
    Pause {
        /// Rule id.
        id: String,
    },
    /// Re-enable a paused rule.
    Resume {
        /// Rule id.
        id: String,
    },
    /// Remove a rule by id.
    Remove {
        /// Rule id.
        id: String,
    },
    /// Replay the recent-events ring through this rule.
    Test {
        /// Rule id.
        id: String,
        /// Replay events newer than this RFC3339 timestamp (default: 1h ago).
        #[arg(long)]
        since: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum WatchlistsCmd {
    /// List all watchlists.
    List,
    /// Get one watchlist by name.
    Get {
        /// Watchlist name.
        name: String,
    },
    /// Replace a watchlist's items (creates if absent).
    Set {
        /// Watchlist name.
        name: String,
        /// Items to set.
        items: Vec<String>,
    },
    /// Add a single item to an existing watchlist.
    AddItem {
        /// Watchlist name.
        name: String,
        /// Item to add.
        item: String,
    },
    /// Remove a single item.
    RemoveItem {
        /// Watchlist name.
        name: String,
        /// Item to remove.
        item: String,
    },
    /// Delete an entire watchlist.
    Remove {
        /// Watchlist name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum WebhooksCmd {
    /// List all webhook configs.
    List,
    /// Add a webhook from a JSON file. Use `-` for stdin.
    Add {
        /// Path to a JSON file containing the typed `WebhookConfig`.
        file: PathBuf,
    },
    /// Send a test POST through a configured webhook.
    Test {
        /// Webhook id.
        id: String,
    },
    /// Remove a webhook by id.
    Remove {
        /// Webhook id.
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum ActivityCmd {
    /// Recent events from the ring buffer.
    Events {
        /// Optional kind filter (e.g., `file_deleted`, `process_started`).
        #[arg(long)]
        kind: Option<String>,
        /// Maximum number of events to return.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Recent fires from the ring buffer.
    Fires {
        /// Optional rule-id filter.
        #[arg(long)]
        rule: Option<String>,
        /// Maximum number of fires.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

#[derive(Debug, Subcommand)]
enum ConfirmationCmd {
    /// List pending confirmations.
    List,
    /// Resolve a pending confirmation. Decision: `allow` | `deny` | `always-allow`.
    Resolve {
        /// Confirmation id.
        id: String,
        /// Decision (allow / deny / always-allow).
        decision: String,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceCmd {
    /// Install the daemon as a macOS LaunchAgent and start it immediately.
    ///
    /// Writes `~/Library/LaunchAgents/com.cellar.daemon.plist` with the
    /// resolved binary path and log directory, then calls
    /// `launchctl load -w` to register and start the service.
    Install {
        /// Path to the `cel-cortex-daemon` binary.
        ///
        /// Defaults to `cel-cortex-daemon` in the same directory as the
        /// `cellar` binary (typical for release installs where both are
        /// placed in, e.g., `/usr/local/bin/`).
        #[arg(long)]
        daemon_path: Option<PathBuf>,

        /// Directory for daemon log output (stdout + stderr).
        ///
        /// Defaults to `$HOME/.cellar/logs`. The directory is created
        /// automatically if it does not exist.
        #[arg(long)]
        log_dir: Option<PathBuf>,
    },

    /// Unload and remove the daemon LaunchAgent.
    ///
    /// Calls `launchctl unload -w` to stop and deregister the service,
    /// then removes the plist file.
    Uninstall {
        /// Remove the plist even if `launchctl unload` fails (e.g. the
        /// service was already unloaded manually).
        #[arg(long)]
        force: bool,
    },

    /// Show the daemon LaunchAgent install and load status.
    ///
    /// Checks whether the plist file is present and whether launchd
    /// has the service loaded (via `launchctl print`).
    Status,
}

/// Plist template for the Cellar daemon LaunchAgent.
///
/// Placeholders:
/// - `__DAEMON_PATH__` — absolute path to the `cel-cortex-daemon` binary.
/// - `__LOG_DIR__` — directory for stdout/stderr log capture.
const LAUNCH_AGENT_PLIST_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Managed by `cellar service install`. Edit the placeholder values or
     re-run `cellar service install --daemon-path ... --log-dir ...` to
     regenerate. Manual edits survive `cellar service status` but are
     overwritten by the next `cellar service install`. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.cellar.daemon</string>

  <key>ProgramArguments</key>
  <array>
    <string>__DAEMON_PATH__</string>
  </array>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
    <key>Crashed</key>
    <true/>
  </dict>

  <key>StandardOutPath</key>
  <string>__LOG_DIR__/daemon.log</string>

  <key>StandardErrorPath</key>
  <string>__LOG_DIR__/daemon.log</string>

  <key>EnvironmentVariables</key>
  <dict>
    <!-- Tracing level. Override at install time via --log-dir or by
         editing this file after install. -->
    <key>RUST_LOG</key>
    <string>info,cel_cortex_daemon=info</string>
  </dict>

  <key>ProcessType</key>
  <string>Interactive</string>

  <!-- 10 s minimum between crash-restarts; macOS enforces a hard floor. -->
  <key>ThrottleInterval</key>
  <integer>10</integer>
</dict>
</plist>
"#;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;

    // `service` subcommands interact with launchd directly — they work even
    // when the daemon is not running, so we skip the socket connection entirely.
    if let Command::Service { cmd } = cli.cmd {
        return run_service(cmd, json).await;
    }

    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
    let client = connect(&socket).await?;

    match cli.cmd {
        Command::Status => run_status(&client, json).await,
        Command::Doctor => {
            // Doctor manages its own exit code: every check runs, then the
            // report drives the process exit (0 if all pass/warn, 1 if any
            // fail). Other commands fall through to the default `Result<()>`
            // path, which only fails on hard CLI errors.
            let code = run_doctor(&client, &socket, json).await?;
            std::process::exit(code);
        }
        Command::Capabilities => run_capabilities(&client, json).await,
        Command::Rules { cmd } => run_rules(&client, cmd, json).await,
        Command::Watchlists { cmd } => run_watchlists(&client, cmd, json).await,
        Command::Webhooks { cmd } => run_webhooks(&client, cmd, json).await,
        Command::Activity { cmd } => run_activity(&client, cmd, json).await,
        Command::Confirmation { cmd } => run_confirmation(&client, cmd, json).await,
        // Already handled above — the compiler requires exhaustiveness.
        Command::Service { .. } => unreachable!(),
    }
}

fn default_socket_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cellar").join("daemon.sock")
    } else {
        PathBuf::from("/tmp/cellar-daemon.sock")
    }
}

async fn connect(socket: &PathBuf) -> Result<Client> {
    if !socket.exists() {
        bail!(
            "daemon socket not found at {} — is `cel-cortex-daemon` running? \
             Override with --socket or $CELLAR_DAEMON_SOCK.",
            socket.display()
        );
    }
    let (client, _rx) = Client::connect_unix(socket)
        .await
        .with_context(|| format!("connect to daemon at {}", socket.display()))?;
    // Locked protocol: hello before anything else. We don't need the
    // result here — the daemon's `system.hello` does the version check
    // and capability advertisement.
    let _: cellar_ipc::results::system::SystemHelloResult = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "cellar-cli".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                supported_protocol_versions: vec![PROTOCOL_VERSION.into()],
            },
        )
        .await
        .map_err(|e| anyhow!("system.hello: {e}"))?;
    Ok(client)
}

fn emit_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(value)?;
    println!("{s}");
    Ok(())
}

// ───── status / doctor / capabilities ─────

async fn run_status(client: &Client, json: bool) -> Result<()> {
    let r: cellar_ipc::results::daemon::DaemonStatusResult = client
        .call("daemon.status", serde_json::json!({}))
        .await
        .map_err(|e| anyhow!("daemon.status: {e}"))?;
    if json {
        return emit_json(&r);
    }
    println!("healthy:        {}", r.healthy);
    println!("uptime:         {}s", r.uptime_s);
    println!(
        "rules:          {} ({} enabled)",
        r.rules.total, r.rules.enabled
    );
    println!("watchlists:     {}", r.watchlists.total);
    println!("fires (24h):    {}", r.recent_fires_24h);
    println!("pending confs:  {}", r.pending_confirmations);
    println!("agent sessions: {}", r.agent_sessions_active);
    println!("version:        {}", r.daemon_version);
    Ok(())
}

async fn run_capabilities(client: &Client, json: bool) -> Result<()> {
    let r: cellar_ipc::results::system::SystemHelloResult = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "cellar-cli".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                supported_protocol_versions: vec![PROTOCOL_VERSION.into()],
            },
        )
        .await
        .map_err(|e| anyhow!("system.hello: {e}"))?;
    if json {
        return emit_json(&r);
    }
    println!("protocol version: {}", r.protocol_version);
    println!("daemon version:   {}", r.daemon_version);
    println!("uptime:           {}s", r.daemon_uptime_s);
    println!("session id:       {}", r.session_id);
    println!("capabilities:");
    let mut caps = r.capabilities.clone();
    caps.sort();
    for c in &caps {
        println!("  - {c}");
    }
    Ok(())
}

/// Run the doctor battery and return the process exit code.
///
/// The check suite lives in [`doctor`]. This thin wrapper:
/// - chooses the production [`doctor::ReqwestProbe`] for webhook reachability
///   (3 s per-request timeout — interactive command, no need to wait long),
/// - resolves the LaunchAgent plist from `$HOME` when available,
/// - either renders the human-readable report or, with `--json`, emits a
///   structured JSON document for tooling.
///
/// The exit code is `0` if every check is `Pass` or `Warn`, `1` if any
/// check is `Fail`. The caller in `main()` propagates this via
/// `std::process::exit`.
async fn run_doctor(client: &Client, socket: &Path, json: bool) -> Result<i32> {
    let probe = doctor::ReqwestProbe::default();
    let plist = doctor::default_launch_agent_plist();
    let plist_ref = plist.as_deref();
    let report = doctor::assemble_report(client, socket, plist_ref, &probe).await;
    if json {
        emit_json(&serde_json::json!({
            "socket": socket.display().to_string(),
            "plist": plist.as_ref().map(|p| p.display().to_string()),
            "checks": report.rows.iter().map(|r| serde_json::json!({
                "name": r.name,
                "status": match r.status {
                    doctor::CheckStatus::Pass => "pass",
                    doctor::CheckStatus::Warn => "warn",
                    doctor::CheckStatus::Fail => "fail",
                },
                "message": r.message,
            })).collect::<Vec<_>>(),
        }))?;
        let fails = report
            .rows
            .iter()
            .filter(|r| r.status == doctor::CheckStatus::Fail)
            .count();
        Ok(if fails > 0 { 1 } else { 0 })
    } else {
        Ok(doctor::render_report(&report.rows))
    }
}

// ───── rules ─────

async fn run_rules(client: &Client, cmd: RulesCmd, json: bool) -> Result<()> {
    match cmd {
        RulesCmd::List => {
            let r: cellar_ipc::results::rules::RulesListResult = client
                .call(
                    "rules.list",
                    cellar_ipc::params::rules::RulesListParams::default(),
                )
                .await
                .map_err(|e| anyhow!("rules.list: {e}"))?;
            if json {
                return emit_json(&r);
            }
            if r.rules.is_empty() {
                println!("no rules");
                return Ok(());
            }
            for rule in &r.rules {
                let enabled = if rule.enabled { " " } else { "[paused]" };
                println!(
                    "{:<8} {:<32} {} {}",
                    enabled,
                    rule.id,
                    serde_json::to_string(&rule.kind).unwrap_or_default(),
                    rule.name
                );
            }
            Ok(())
        }
        RulesCmd::Get { id } => {
            let r: cellar_ipc::results::rules::RulesGetResult = client
                .call(
                    "rules.get",
                    cellar_ipc::params::rules::RulesGetParams { id: id.clone() },
                )
                .await
                .map_err(|e| anyhow!("rules.get: {e}"))?;
            match r.rule {
                Some(rule) => emit_json(&rule),
                None => {
                    bail!("rule '{id}' not found");
                }
            }
        }
        RulesCmd::Add { file } => {
            let bytes = read_input(&file).await?;
            let rule: cellar_types::Rule =
                serde_json::from_slice(&bytes).context("parse rule JSON")?;
            let r: cellar_ipc::results::rules::RulesAddResult = client
                .call(
                    "rules.add",
                    cellar_ipc::params::rules::RulesAddParams { rule },
                )
                .await
                .map_err(|e| anyhow!("rules.add: {e}"))?;
            if json {
                return emit_json(&r);
            }
            println!("added rule: {}", r.rule_id);
            Ok(())
        }
        RulesCmd::Compile { nl } => {
            let r: cellar_ipc::results::rules::RulesCompileResult = client
                .call(
                    "rules.compile",
                    cellar_ipc::params::rules::RulesCompileParams { nl_string: nl },
                )
                .await
                .map_err(|e| anyhow!("rules.compile: {e}"))?;
            if json {
                return emit_json(&r);
            }
            println!("--- summary ---");
            println!("{}", r.human_readable);
            if !r.warnings.is_empty() {
                println!("--- warnings ---");
                for w in &r.warnings {
                    println!("  ! {w}");
                }
            }
            println!("--- draft rule (JSON) ---");
            println!("{}", serde_json::to_string_pretty(&r.draft_rule)?);
            println!(
                "\nSave it with:  cellar rules add <(echo '{}')",
                serde_json::to_string(&r.draft_rule)?
            );
            Ok(())
        }
        RulesCmd::Pause { id } => {
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "rules.pause",
                    cellar_ipc::params::rules::RuleIdParams { id: id.clone() },
                )
                .await
                .map_err(|e| anyhow!("rules.pause: {e}"))?;
            println!("paused: {id}");
            Ok(())
        }
        RulesCmd::Resume { id } => {
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "rules.resume",
                    cellar_ipc::params::rules::RuleIdParams { id: id.clone() },
                )
                .await
                .map_err(|e| anyhow!("rules.resume: {e}"))?;
            println!("resumed: {id}");
            Ok(())
        }
        RulesCmd::Remove { id } => {
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "rules.remove",
                    cellar_ipc::params::rules::RuleIdParams { id: id.clone() },
                )
                .await
                .map_err(|e| anyhow!("rules.remove: {e}"))?;
            println!("removed: {id}");
            Ok(())
        }
        RulesCmd::Test { id, since } => {
            let since_ts = match since {
                Some(s) => chrono::DateTime::parse_from_rfc3339(&s)
                    .context("parse --since RFC3339 timestamp")?
                    .with_timezone(&chrono::Utc),
                None => chrono::Utc::now() - chrono::Duration::hours(1),
            };
            let r: cellar_ipc::results::rules::RulesTestResult = client
                .call(
                    "rules.test",
                    cellar_ipc::params::rules::RulesTestParams {
                        id: id.clone(),
                        since: since_ts,
                    },
                )
                .await
                .map_err(|e| anyhow!("rules.test: {e}"))?;
            if json {
                return emit_json(&r);
            }
            println!("rule '{}' matched {} event(s)", id, r.matched_events.len());
            for ev in &r.matched_events {
                let ts = ev["ts"].as_str().unwrap_or("?");
                let kind = ev["kind"].as_str().unwrap_or("?");
                println!("  {ts}  {kind}");
            }
            Ok(())
        }
    }
}

// ───── watchlists ─────

async fn run_watchlists(client: &Client, cmd: WatchlistsCmd, json: bool) -> Result<()> {
    match cmd {
        WatchlistsCmd::List => {
            let r: cellar_ipc::results::watchlists::WatchlistsListResult = client
                .call(
                    "watchlists.list",
                    cellar_ipc::params::watchlists::WatchlistsListParams::default(),
                )
                .await
                .map_err(|e| anyhow!("watchlists.list: {e}"))?;
            if json {
                return emit_json(&r);
            }
            for w in &r.watchlists {
                println!("{:<32} ({} items)", w.name, w.items.len());
                for item in &w.items {
                    println!("  - {item}");
                }
            }
            Ok(())
        }
        WatchlistsCmd::Get { name } => {
            let r: cellar_ipc::results::watchlists::WatchlistsGetResult = client
                .call(
                    "watchlists.get",
                    cellar_ipc::params::watchlists::WatchlistNameParams { name: name.clone() },
                )
                .await
                .map_err(|e| anyhow!("watchlists.get: {e}"))?;
            match r.watchlist {
                Some(w) => emit_json(&w),
                None => bail!("watchlist '{name}' not found"),
            }
        }
        WatchlistsCmd::Set { name, items } => {
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "watchlists.set",
                    cellar_ipc::params::watchlists::WatchlistsSetParams {
                        name: name.clone(),
                        items: items.clone(),
                    },
                )
                .await
                .map_err(|e| anyhow!("watchlists.set: {e}"))?;
            println!("set: {} ({} items)", name, items.len());
            Ok(())
        }
        WatchlistsCmd::AddItem { name, item } => {
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "watchlists.add_item",
                    cellar_ipc::params::watchlists::WatchlistsItemParams {
                        name: name.clone(),
                        item: item.clone(),
                    },
                )
                .await
                .map_err(|e| anyhow!("watchlists.add_item: {e}"))?;
            println!("added: {item} to {name}");
            Ok(())
        }
        WatchlistsCmd::RemoveItem { name, item } => {
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "watchlists.remove_item",
                    cellar_ipc::params::watchlists::WatchlistsItemParams {
                        name: name.clone(),
                        item: item.clone(),
                    },
                )
                .await
                .map_err(|e| anyhow!("watchlists.remove_item: {e}"))?;
            println!("removed: {item} from {name}");
            Ok(())
        }
        WatchlistsCmd::Remove { name } => {
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "watchlists.remove",
                    cellar_ipc::params::watchlists::WatchlistNameParams { name: name.clone() },
                )
                .await
                .map_err(|e| anyhow!("watchlists.remove: {e}"))?;
            println!("removed: {name}");
            Ok(())
        }
    }
}

// ───── webhooks ─────

async fn run_webhooks(client: &Client, cmd: WebhooksCmd, json: bool) -> Result<()> {
    match cmd {
        WebhooksCmd::List => {
            let r: cellar_ipc::results::webhooks::WebhooksListResult = client
                .call(
                    "webhooks.list",
                    cellar_ipc::params::webhooks::WebhooksListParams::default(),
                )
                .await
                .map_err(|e| anyhow!("webhooks.list: {e}"))?;
            if json {
                return emit_json(&r);
            }
            for w in &r.webhooks {
                println!("{:<32} {}", w.id, w.url);
            }
            Ok(())
        }
        WebhooksCmd::Add { file } => {
            let bytes = read_input(&file).await?;
            let config: cellar_types::WebhookConfig =
                serde_json::from_slice(&bytes).context("parse webhook JSON")?;
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "webhooks.add",
                    cellar_ipc::params::webhooks::WebhooksAddParams { config },
                )
                .await
                .map_err(|e| anyhow!("webhooks.add: {e}"))?;
            println!("added");
            Ok(())
        }
        WebhooksCmd::Test { id } => {
            let r: cellar_ipc::results::webhooks::WebhooksTestResult = client
                .call(
                    "webhooks.test",
                    cellar_ipc::params::webhooks::WebhookIdParams { id: id.clone() },
                )
                .await
                .map_err(|e| anyhow!("webhooks.test: {e}"))?;
            if json {
                return emit_json(&r);
            }
            if r.reachable {
                println!(
                    "ok: status {} in {}ms",
                    r.status_code.unwrap_or(0),
                    r.elapsed_ms.unwrap_or(0)
                );
            } else {
                println!("unreachable: {}", r.error.unwrap_or_default());
            }
            Ok(())
        }
        WebhooksCmd::Remove { id } => {
            client
                .call::<_, cellar_ipc::results::OkResult>(
                    "webhooks.remove",
                    cellar_ipc::params::webhooks::WebhookIdParams { id: id.clone() },
                )
                .await
                .map_err(|e| anyhow!("webhooks.remove: {e}"))?;
            println!("removed: {id}");
            Ok(())
        }
    }
}

// ───── activity ─────

async fn run_activity(client: &Client, cmd: ActivityCmd, json: bool) -> Result<()> {
    match cmd {
        ActivityCmd::Events { kind, limit } => {
            let filter = StreamFilter {
                kinds: kind.map(|k| vec![k]),
                limit: Some(limit),
                ..Default::default()
            };
            let r: Vec<Value> = client
                .call("events.recent", EventsRecentParams { filter })
                .await
                .map_err(|e| anyhow!("events.recent: {e}"))?;
            if json {
                return emit_json(&r);
            }
            for ev in &r {
                let ts = ev["ts"].as_str().unwrap_or("?");
                let source = ev["source"].as_str().unwrap_or("?");
                let kind = ev["kind"].as_str().unwrap_or("?");
                println!("{ts:<30}  {source:<14}  {kind}");
            }
            Ok(())
        }
        ActivityCmd::Fires { rule, limit } => {
            let filter = StreamFilter {
                rule_ids: rule.map(|r| vec![r]),
                limit: Some(limit),
                ..Default::default()
            };
            let r: Vec<Value> = client
                .call("fires.recent", FiresRecentParams { filter })
                .await
                .map_err(|e| anyhow!("fires.recent: {e}"))?;
            if json {
                return emit_json(&r);
            }
            for fire in &r {
                let ts = fire["fired_at"].as_str().unwrap_or("?");
                let rule = fire["rule_id"].as_str().unwrap_or("?");
                let kind = fire["event_kind"].as_str().unwrap_or("?");
                println!("{ts:<30}  {rule:<20}  on {kind}");
            }
            Ok(())
        }
    }
}

// ───── confirmation ─────

async fn run_confirmation(client: &Client, cmd: ConfirmationCmd, json: bool) -> Result<()> {
    match cmd {
        ConfirmationCmd::List => {
            let r: cellar_ipc::results::confirmation::ConfirmationListPendingResult = client
                .call(
                    "confirmation.list_pending",
                    cellar_ipc::params::confirmation::ConfirmationListPendingParams::default(),
                )
                .await
                .map_err(|e| anyhow!("confirmation.list_pending: {e}"))?;
            if json {
                return emit_json(&r);
            }
            if r.pending.is_empty() {
                println!("no pending confirmations");
                return Ok(());
            }
            for p in &r.pending {
                println!(
                    "{:<40}  rule: {}  expires: {}",
                    p.id, p.rule.name, p.expires_at
                );
            }
            Ok(())
        }
        ConfirmationCmd::Resolve { id, decision } => {
            let wire = match decision.to_lowercase().as_str() {
                "allow" => ConfirmationDecisionWire::Allow,
                "deny" => ConfirmationDecisionWire::Deny,
                "always-allow" | "always_allow" => ConfirmationDecisionWire::AlwaysAllow,
                other => bail!("decision must be allow|deny|always-allow; got '{}'", other),
            };
            let r: cellar_ipc::results::confirmation::ConfirmationResolveResult = client
                .call(
                    "confirmation.resolve",
                    cellar_ipc::params::confirmation::ConfirmationResolveParams {
                        id: id.clone(),
                        decision: wire,
                        remember_kind: None,
                    },
                )
                .await
                .map_err(|e| anyhow!("confirmation.resolve: {e}"))?;
            if json {
                return emit_json(&r);
            }
            println!("resolved: {}  outcome: {}", r.resolved, r.action_outcome);
            Ok(())
        }
    }
}

// ───── service (launchd) ─────

async fn run_service(cmd: ServiceCmd, _json: bool) -> Result<()> {
    match cmd {
        ServiceCmd::Install {
            daemon_path,
            log_dir,
        } => {
            // Resolve daemon binary path.
            let daemon_bin = match daemon_path {
                Some(p) => p,
                None => {
                    // Default: `cel-cortex-daemon` next to the `cellar` binary.
                    let self_path =
                        std::env::current_exe().context("resolve current executable path")?;
                    let bin_dir = self_path.parent().unwrap_or_else(|| Path::new("."));
                    let candidate = bin_dir.join("cel-cortex-daemon");
                    if candidate.exists() {
                        candidate
                    } else {
                        bail!(
                            "cannot find `cel-cortex-daemon` binary next to `cellar` at {}.\n\
                             Pass --daemon-path /path/to/cel-cortex-daemon explicitly.",
                            bin_dir.display()
                        );
                    }
                }
            };
            let daemon_bin = daemon_bin
                .canonicalize()
                .with_context(|| format!("canonicalize daemon path {}", daemon_bin.display()))?;

            // Resolve log directory.
            let log_dir = match log_dir {
                Some(p) => p,
                None => {
                    let home = std::env::var("HOME")
                        .map_err(|_| anyhow!("HOME unset; pass --log-dir explicitly"))?;
                    PathBuf::from(home).join(".cellar").join("logs")
                }
            };
            std::fs::create_dir_all(&log_dir)
                .with_context(|| format!("create log dir {}", log_dir.display()))?;

            // Fill in the template. Paths are XML-escaped because they
            // become PCDATA inside `<string>` elements — any `&`, `<`, or
            // `>` would otherwise produce an invalid plist that macOS
            // silently fails to load. (`launchctl load` even exits 0 on
            // malformed XML, so the failure mode is fully silent.)
            let daemon_str = daemon_bin.to_string_lossy();
            let log_dir_str = log_dir.to_string_lossy();
            let plist = LAUNCH_AGENT_PLIST_TEMPLATE
                .replace("__DAEMON_PATH__", &xml_escape(&daemon_str))
                .replace("__LOG_DIR__", &xml_escape(&log_dir_str));

            // Write the plist.
            let plist_path = launch_agents_dir()?.join("com.cellar.daemon.plist");
            if let Some(parent) = plist_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            std::fs::write(&plist_path, &plist)
                .with_context(|| format!("write plist to {}", plist_path.display()))?;
            println!("✓ wrote {}", plist_path.display());

            // If the service is already loaded (re-install case), `launchctl
            // load` prints "Load failed: 5: Input/output error" to stderr
            // but exits 0 — leaving the OLD plist active. Best-effort
            // unload first to make re-install behave like fresh install.
            let _ = std::process::Command::new("launchctl")
                .args(["unload", "-w", &plist_path.to_string_lossy()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();

            // `launchctl load -w <plist>` — registers + starts the service.
            // We capture stderr because launchctl loves to print errors
            // there while exiting 0; we treat any "Load failed" line as
            // a load failure even when the exit code says otherwise.
            let output = std::process::Command::new("launchctl")
                .args(["load", "-w", &plist_path.to_string_lossy()])
                .output()
                .context("run launchctl load")?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            let load_failed_in_stderr = stderr.lines().any(|l| {
                let t = l.trim();
                t.starts_with("Load failed") || t.starts_with("load failed")
            });
            if output.status.success() && !load_failed_in_stderr {
                println!("✓ launchctl load succeeded — daemon is starting");
                println!("  binary:   {daemon_str}");
                println!("  log file: {log_dir_str}/daemon.log");
                println!(
                    "\nRun `cellar service status` to verify it is loaded,\n\
                     or `cellar doctor` once the daemon has started."
                );
            } else {
                let stderr_msg = stderr.trim();
                bail!(
                    "launchctl load failed (exit {}, stderr: {}).\n\
                     Plist written to {} — inspect the binary path and run\n\
                     `plutil -lint {}` to check the plist is valid, then\n\
                     `launchctl load -w {}` once the issue is fixed.",
                    output.status.code().unwrap_or(-1),
                    if stderr_msg.is_empty() {
                        "<empty>"
                    } else {
                        stderr_msg
                    },
                    plist_path.display(),
                    plist_path.display(),
                    plist_path.display()
                );
            }
            Ok(())
        }

        ServiceCmd::Uninstall { force } => {
            let plist_path = launch_agents_dir()?.join("com.cellar.daemon.plist");
            if !plist_path.exists() {
                if force {
                    println!("plist not found — nothing to uninstall");
                    return Ok(());
                }
                bail!(
                    "LaunchAgent plist not found at {}.\n\
                     Was the daemon installed with `cellar service install`?",
                    plist_path.display()
                );
            }

            // `launchctl unload -w <plist>` — stops and deregisters.
            let status = std::process::Command::new("launchctl")
                .args(["unload", "-w", &plist_path.to_string_lossy()])
                .status()
                .context("run launchctl unload")?;
            if status.success() {
                println!("✓ launchctl unload succeeded");
            } else if force {
                eprintln!(
                    "  warning: launchctl unload failed (exit {}) — proceeding due to --force",
                    status.code().unwrap_or(-1)
                );
            } else {
                bail!(
                    "launchctl unload failed (exit {}).\n\
                     Use --force to remove the plist file anyway.",
                    status.code().unwrap_or(-1)
                );
            }

            std::fs::remove_file(&plist_path)
                .with_context(|| format!("remove {}", plist_path.display()))?;
            println!("✓ removed {}", plist_path.display());
            Ok(())
        }

        ServiceCmd::Status => {
            let plist_path = launch_agents_dir()?.join("com.cellar.daemon.plist");
            if plist_path.exists() {
                println!("✓ plist installed: {}", plist_path.display());
            } else {
                println!("✗ plist not installed (run `cellar service install`)");
            }

            // Ask launchd whether it has the service loaded.
            // `launchctl print gui/<uid>/com.cellar.daemon`
            let uid_out = std::process::Command::new("id")
                .arg("-u")
                .output()
                .context("run `id -u`")?;
            let uid = String::from_utf8_lossy(&uid_out.stdout).trim().to_string();
            let service_target = format!("gui/{uid}/com.cellar.daemon");

            let output = std::process::Command::new("launchctl")
                .args(["print", &service_target])
                .output()
                .context("run launchctl print")?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Surface the most useful fields from the verbose output.
                for line in stdout.lines() {
                    let t = line.trim();
                    if t.starts_with("pid")
                        || t.starts_with("state")
                        || t.starts_with("last exit")
                        || t.starts_with("path")
                        || t.starts_with("program")
                    {
                        println!("  {t}");
                    }
                }
                println!("✓ service is loaded by launchd");
            } else {
                println!(
                    "✗ service is NOT loaded (launchctl print {service_target} returned an error)"
                );
                println!("  Run `cellar service install` to register and start the daemon.");
            }
            Ok(())
        }
    }
}

/// Returns `~/Library/LaunchAgents/` as a `PathBuf`.
fn launch_agents_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow!("HOME is not set — cannot determine ~/Library/LaunchAgents/"))?;
    Ok(PathBuf::from(home).join("Library").join("LaunchAgents"))
}

/// XML-escape a string so it can be safely embedded as PCDATA inside a
/// `<string>` element in the plist.
///
/// macOS plist files are strict XML 1.0 — unescaped `&` / `<` / `>` in a
/// `<string>` value produce a malformed plist that `launchctl load`
/// silently rejects (it exits 0 but prints "Load failed" to stderr).
/// This matters for paths containing `&` (legal on macOS).
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

// ───── helpers ─────

async fn read_input(path: &PathBuf) -> Result<Vec<u8>> {
    if path.to_string_lossy() == "-" {
        // stdin
        let mut buf = Vec::new();
        use tokio::io::AsyncReadExt;
        tokio::io::stdin()
            .read_to_end(&mut buf)
            .await
            .context("read stdin")?;
        Ok(buf)
    } else {
        tokio::fs::read(path)
            .await
            .with_context(|| format!("read {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::xml_escape;

    #[test]
    fn xml_escape_passes_through_ascii_paths() {
        assert_eq!(
            xml_escape("/usr/local/bin/cel-cortex-daemon"),
            "/usr/local/bin/cel-cortex-daemon"
        );
    }

    #[test]
    fn xml_escape_handles_ampersand() {
        assert_eq!(xml_escape("a & b"), "a &amp; b");
    }

    #[test]
    fn xml_escape_handles_all_five_predefined_entities() {
        assert_eq!(
            xml_escape("<\"a & b\">'c'"),
            "&lt;&quot;a &amp; b&quot;&gt;&apos;c&apos;"
        );
    }

    #[test]
    fn xml_escape_does_not_double_escape() {
        // We escape the literal &amp; to &amp;amp; — which is correct;
        // round-tripping through an XML parser would yield &amp; back.
        assert_eq!(xml_escape("&amp;"), "&amp;amp;");
    }

    #[test]
    fn xml_escape_handles_realistic_macos_path() {
        // Spaces and unicode pass through untouched.
        assert_eq!(
            xml_escape("/Users/Alice & Bob/Cellar.app/bin"),
            "/Users/Alice &amp; Bob/Cellar.app/bin"
        );
    }
}
