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
mod eval;
mod guide;
mod mcp;
mod workflow;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use cellar_ipc::params::confirmation::ConfirmationDecisionWire;
use cellar_ipc::params::events::{EventsPublishParams, EventsRecentParams};
use cellar_ipc::params::fires::FiresRecentParams;
use cellar_ipc::params::stream_filter::StreamFilter;
use cellar_ipc::params::system::SystemHelloParams;
use cellar_ipc::Client;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
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
    ///
    /// With `cellar doctor memory`, runs the memory-subsystem-specific
    /// battery (DB exists + readable, embedding-model present, corpus
    /// growth inside the configured cap, recent embedding latency p95
    /// inside budget).
    Doctor {
        #[command(subcommand)]
        cmd: Option<DoctorCmd>,
    },

    /// Offline benchmarks (memory recall today; more to come).
    Eval {
        #[command(subcommand)]
        cmd: eval::EvalCmd,
    },

    /// Print the daemon's `system.hello` capability set.
    Capabilities,

    /// Print an embeddable agent guide for driving CEL — no daemon needed.
    ///
    ///   cellar learn > CEL_GUIDE.md
    Learn,

    /// List CEL's surfaces: CLI verbs, native gateway actions, and MCP tools.
    /// No daemon needed (see `cellar capabilities` for daemon-advertised caps).
    Tools,

    /// Generate a shell-completion script (bash, zsh, fish, elvish,
    /// powershell) for the `cellar` CLI. Prints to stdout — no daemon needed.
    ///
    ///   cellar completions zsh > ~/.zsh/completions/_cellar
    Completions {
        /// Target shell.
        shell: Shell,
    },

    /// Run a workflow script — an ordered list of CEL actions in JSON. Each
    /// step is dispatched through the governed gateway (same path as
    /// `cellar act`), so every step is rule-checked and produces a receipt.
    ///
    ///   cellar run tidy.json            # execute (needs the daemon)
    ///   cellar run tidy.json --dry-run  # validate + print the plan, offline
    Run {
        /// Path to a JSON workflow script: { "name": ..., "steps": [ ... ] }.
        file: PathBuf,

        /// Validate the script and print the execution plan without
        /// connecting to the daemon or dispatching any action.
        #[arg(long)]
        dry_run: bool,

        /// Continue past steps the gateway did not execute (vetoed / denied /
        /// timed-out) instead of halting at the first one. Default is
        /// stop-on-failure.
        #[arg(long)]
        keep_going: bool,

        /// Retry a step up to N times on a *transient* failure — a transport
        /// error or a confirmation timeout. Deterministic outcomes (executed /
        /// vetoed / denied) are never retried. Default 0.
        #[arg(long, default_value_t = 0)]
        retries: u32,
    },

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

    /// Submit a proposed action to the daemon's cel_act gateway.
    ///
    /// Runs the rule matcher, applies allow / veto / require_confirmation
    /// decisions, and prints the outcome. If a `require_confirmation` rule
    /// fires, the call **blocks** until you resolve it in another terminal:
    ///
    ///   cellar confirmation list
    ///   cellar confirmation resolve <id> allow
    ///
    /// Blocks for up to the rule's `timeout_s` (default 60 s).
    Act {
        /// The action type — e.g. `copy_file`, `fs.move`, `shell.run`.
        action_type: String,

        /// Action arguments as a JSON object. E.g. `'{"source_path":"~/Documents/x.pdf","target_path":"/Volumes/Ext/"}'`.
        ///
        /// Defaults to `{}` when omitted.
        #[arg(long, short = 'a', default_value = "{}")]
        args: String,

        /// Caller label surfaced in rule conditions via `data.caller`.
        /// Defaults to `"cli"`.
        #[arg(long, default_value = "cli")]
        caller: String,

        /// Optional agent-session ID (links the action to a chat session).
        #[arg(long)]
        session_id: Option<String>,
    },

    /// Run a one-shot agent goal via the daemon's embedded agent (governed).
    ///
    /// Sends the goal to the daemon, which runs it to completion in a fresh
    /// session and returns the agent's final response. Every action the agent
    /// takes dispatches through the governed gateway. Blocks until the turn
    /// finishes. Requires the daemon configured with an LLM
    /// (`CELLAR_DEFAULT_PROVIDER` + `CELLAR_DEFAULT_MODEL`).
    ///
    ///   cellar agent "open Safari and search for the weather"
    ///   cellar agent "tidy my downloads folder" --dry-run
    Agent {
        /// The natural-language goal.
        goal: String,
        /// Plan only — describe the steps without dispatching tools.
        #[arg(long)]
        dry_run: bool,
    },

    // ── WS11: ergonomic human-automation verbs ──────────────────────────
    // Each is sugar over `cellar act <type> -a <json>`: it builds the action
    // args and submits them through the SAME governed gateway, so every verb
    // is rule-checked and produces a receipt.
    /// Click a UI element by its perception target id (governed).
    ///
    ///   cellar click el-42
    Click {
        /// Target element id (from a `cel_see` / perception snapshot).
        target_id: String,
        /// Caller label surfaced to rules via `data.caller`.
        #[arg(long, default_value = "cli")]
        caller: String,
    },

    /// Type text, optionally focusing a target element first (governed).
    ///
    ///   cellar type "hello world" --target el-7
    Type {
        /// The text to type.
        text: String,
        /// Optional target element id to focus before typing.
        #[arg(long)]
        target: Option<String>,
        #[arg(long, default_value = "cli")]
        caller: String,
    },

    /// Activate / launch / quit an application by name (governed).
    ///
    /// Default activates (brings to front). `--launch` starts the app
    /// (add `--background` to start it without stealing focus); `--quit`
    /// asks it to quit gracefully (like ⌘Q).
    ///
    ///   cellar app Safari
    ///   cellar app TextEdit --launch --background
    ///   cellar app TextEdit --quit
    App {
        /// Application name, e.g. "Safari".
        app_name: String,
        /// Launch (start) the app instead of activating it.
        #[arg(long)]
        launch: bool,
        /// Quit the app gracefully instead of activating it.
        #[arg(long, conflicts_with = "launch")]
        quit: bool,
        /// With `--launch`, start the app without bringing it to the front.
        #[arg(long)]
        background: bool,
        #[arg(long, default_value = "cli")]
        caller: String,
    },

    /// Window management — `op` is a tiling preset (left_half, right_half,
    /// maximize, …) or minimize / center / raise (governed).
    ///
    ///   cellar window left_half --app Safari
    Window {
        /// Window operation.
        op: String,
        /// Target app (defaults to the frontmost app).
        #[arg(long)]
        app: Option<String>,
        /// Window index (0 = the app's frontmost window).
        #[arg(long, default_value_t = 0)]
        index: usize,
        #[arg(long, default_value = "cli")]
        caller: String,
    },

    /// Menu-bar extra (status item): list the extras or click one (governed).
    ///
    ///   cellar menu list
    ///   cellar menu click --name Wi-Fi
    Menu {
        /// Operation: `list` or `click`.
        op: String,
        /// Status-item title to click (for `op = click`), case-insensitive.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "cli")]
        caller: String,
    },

    /// Read the current screen via Accessibility — a local AX snapshot (the
    /// focused element + interactive elements with bounds). No daemon.
    ///
    /// This is the lightweight CLI `see`; the warm, fused Cortex perception
    /// (diffs, stability, CDP page content) is the richer MCP `cel_see` surface.
    ///
    ///   cellar see
    ///   cellar see --json
    See,

    /// Recognize text in an image via on-device OCR (macOS Vision framework).
    ///
    /// Local, no LLM, no network — millisecond latency, deterministic text +
    /// pixel bounding boxes. The fast perception fallback for surfaces with no
    /// Accessibility tree (`<canvas>`, games, image-only documents). Reads an
    /// image file, or `--screen` to capture the main display first. Runs
    /// locally — no daemon.
    ///
    ///   cellar ocr screenshot.png
    ///   cellar ocr --screen --json
    ///   cellar ocr doc.png --fast --min-confidence 0.5
    Ocr {
        /// Path to an image file (PNG/JPEG/…). Required unless `--screen`.
        path: Option<String>,
        /// Capture the main display and OCR that instead of reading a file.
        #[arg(long)]
        screen: bool,
        /// Faster, lower-accuracy recognition pass (default is accurate).
        #[arg(long)]
        fast: bool,
        /// Drop recognized lines below this confidence (0.0..=1.0).
        #[arg(long, default_value_t = 0.0)]
        min_confidence: f32,
    },

    /// Record a macro — capture a timed sequence of keyboard + mouse input.
    ///
    /// ⚠️ Records your real input for the duration. By default only keycodes
    /// and pointer events are stored; pass `--capture-chars` to also record
    /// typed text (do not use while typing passwords). Requires Input
    /// Monitoring permission. Writes JSON to `--output` (or stdout). Runs
    /// locally — no daemon.
    ///
    ///   cellar record --seconds 5 --output macro.json
    Record {
        /// Capture duration in seconds.
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        /// Write the recording JSON here (default: stdout).
        #[arg(long)]
        output: Option<String>,
        /// Also record typed characters (privacy-sensitive; off by default).
        #[arg(long)]
        capture_chars: bool,
    },

    /// Replay a recorded macro through the input injector (governed/intrusive).
    ///
    /// ⚠️ Injects real keyboard + mouse events. Reads a recording produced by
    /// `cellar record`. Runs locally — no daemon.
    ///
    ///   cellar replay macro.json --speed 2
    Replay {
        /// Path to a recording JSON file (from `cellar record`).
        path: String,
        /// Playback speed multiplier (1.0 = real time, 2.0 = twice as fast).
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
    },

    /// Register a global hotkey that runs a command when pressed (macOS).
    ///
    /// Human-convenience surface: registers a system-wide shortcut; on each
    /// press it spawns the given command with `$CELLAR_HOTKEY` set to the
    /// combo. Blocks until interrupted. Runs locally — no daemon.
    ///
    ///   cellar hotkey cmd+shift+k -- cellar app Safari
    ///   cellar hotkey cmd+ctrl+o -- cellar ocr --screen --json
    Hotkey {
        /// Key combo, e.g. "cmd+shift+k".
        combo: String,
        /// Command to run on each press (everything after `--`).
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },

    /// Spaces / virtual desktops (macOS).
    ///
    /// ⚠️ Uses private SkyLight APIs — fragile across macOS releases. Degrades
    /// gracefully ("unavailable") when unsupported. Runs locally (no daemon);
    /// routing space changes through the governed gateway is a follow-up.
    ///
    ///   cellar space active
    ///   cellar space list
    Space {
        #[command(subcommand)]
        cmd: SpaceCmd,
    },

    /// Call *other* MCP servers (CEL as an MCP client — WS19).
    ///
    /// Registers servers and connects to them over stdio to list / call their
    /// tools. The complement of the cellar MCP *server*. Runs locally.
    ///
    ///   cellar mcp add fs -- npx -y @modelcontextprotocol/server-filesystem /tmp
    ///   cellar mcp inspect fs
    ///   cellar mcp call fs read_file --args '{"path":"/tmp/x.txt"}'
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },

    /// Capture microphone audio and print the live transcript (WS17).
    ///
    /// ⚠️ Accesses the microphone + a Whisper transcription backend (set
    /// `OPENAI_API_KEY` for the Whisper API). Runs locally — no daemon.
    /// `--list-devices` is read-only (no capture).
    ///
    ///   cellar listen --list-devices
    ///   cellar listen --seconds 10
    Listen {
        /// Capture duration in seconds.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Audio source: mic | system | both.
        #[arg(long, default_value = "mic")]
        source: String,
        /// List input devices and exit (no capture).
        #[arg(long)]
        list_devices: bool,
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

/// MCP-client operations (WS19). All run locally (spawn + talk to a server).
#[derive(Debug, Subcommand)]
enum McpCmd {
    /// Register an MCP server: `cellar mcp add <name> -- <command> [args...]`.
    Add {
        /// Local name for the server.
        name: String,
        /// The command + args to spawn it over stdio (everything after `--`).
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Remove a registered server.
    Remove { name: String },
    /// List registered servers.
    List,
    /// Connect to a server and list the tools it exposes.
    Inspect { name: String },
    /// Call a tool on a server.
    Call {
        /// Registered server name.
        name: String,
        /// Tool name (from `cellar mcp inspect`).
        tool: String,
        /// Tool arguments as a JSON object.
        #[arg(long, default_value = "{}")]
        args: String,
    },
}

/// Spaces operations (WS3). All run locally via cel-spaces (private SkyLight).
#[derive(Debug, Subcommand)]
enum SpaceCmd {
    /// Print the active space id on the main display.
    Active,
    /// List managed spaces across all displays.
    List,
    /// Switch a display (UUID from `space list`) to a space.
    Switch {
        /// Display UUID (the `display_uuid` field from `space list`).
        display_uuid: String,
        /// Target managed-space id.
        space_id: u64,
    },
    /// Move a window (CoreGraphics window id) to a space.
    MoveWindow {
        /// CoreGraphics window id.
        window_id: u32,
        /// Target managed-space id.
        space_id: u64,
    },
}

/// Sub-batteries for `cellar doctor`. Today only `memory` is split out
/// (the full system battery runs when no subcommand is given). Future
/// work (`cellar doctor llm`, `cellar doctor adapters`) hangs off the
/// same spine.
#[derive(Debug, Subcommand)]
enum DoctorCmd {
    /// Memory subsystem health: DB exists + readable, embedding model
    /// present, corpus growth within configured cap, embedding latency
    /// p95 inside budget.
    Memory {
        /// Override the memory DB path. Default:
        /// `$CELLAR_MEMORY_DB` if set, else `$HOME/.cellar/memory.sqlite`.
        ///
        /// Special value `:memory:` skips the file existence check and
        /// asks the daemon for `memory.stats` instead (which the
        /// in-memory provider serves the same way).
        #[arg(long)]
        db: Option<PathBuf>,

        /// Maximum corpus chunk count beyond which the doctor flags a
        /// warning. Defaults to 500_000 per the memory plan's §14.3
        /// sizing.
        #[arg(long, default_value_t = 500_000)]
        max_chunks: usize,
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
    /// Inject a synthetic event into the daemon's event bus (for testing rules).
    ///
    /// The event is evaluated by the rule matcher immediately, just like any
    /// natively-sourced event. Use this to verify that a `url_changed` rule
    /// fires correctly without needing Chrome open.
    Publish {
        /// Event kind in snake_case (e.g. `url_changed`, `file_deleted`).
        kind: String,
        /// Event source in snake_case. Default: `cortex_cdp`.
        #[arg(long, default_value = "cortex_cdp")]
        source: String,
        /// Data fields as `key=value` pairs (repeat for multiple fields).
        ///
        /// Values are interpreted as JSON when they start with `{`, `[`, or a
        /// digit; otherwise treated as plain strings.
        #[arg(long = "data", value_name = "KEY=VALUE")]
        data: Vec<String>,
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

    // `eval` runs purely against an on-disk corpus + a fresh in-memory
    // provider — no daemon needed, no socket connection attempt.
    if let Command::Eval { cmd } = cli.cmd {
        let code = eval::run(cmd, json).await?;
        std::process::exit(code);
    }

    // `completions` just prints a shell-completion script — no daemon needed.
    if let Command::Completions { shell } = cli.cmd {
        generate(shell, &mut Cli::command(), "cellar", &mut std::io::stdout());
        return Ok(());
    }

    // `learn` / `tools` are static agent-ergonomics prints — no daemon needed.
    if matches!(cli.cmd, Command::Learn) {
        guide::learn();
        return Ok(());
    }
    if matches!(cli.cmd, Command::Tools) {
        return guide::tools(json);
    }

    // `run --dry-run` validates a workflow script and prints the plan offline —
    // no daemon, no actions dispatched. (A live `run` connects, below.)
    if let Command::Run {
        file,
        dry_run: true,
        ..
    } = &cli.cmd
    {
        let script = workflow::WorkflowScript::load(file)?;
        print!("{}", script.plan_summary());
        println!(
            "\nvalidated {} step(s) — dry run (no actions dispatched).",
            script.steps.len()
        );
        return Ok(());
    }

    // `see` is a local Accessibility snapshot — no daemon.
    if matches!(cli.cmd, Command::See) {
        return run_see(json);
    }

    // `ocr` runs locally via cel-ocr (on-device Vision) — no daemon.
    if let Command::Ocr {
        path,
        screen,
        fast,
        min_confidence,
    } = &cli.cmd
    {
        return run_ocr(path.clone(), *screen, *fast, *min_confidence, json);
    }

    // `record` / `replay` run locally via cel-input — no daemon.
    if let Command::Record {
        seconds,
        output,
        capture_chars,
    } = &cli.cmd
    {
        return run_record(*seconds, output.clone(), *capture_chars, json);
    }
    if let Command::Replay { path, speed } = &cli.cmd {
        return run_replay(path.clone(), *speed, json);
    }
    if let Command::Hotkey { combo, command } = &cli.cmd {
        return run_hotkey(combo.clone(), command.clone());
    }

    // `space` runs locally via cel-spaces (private SkyLight) — no daemon.
    if let Command::Space { cmd } = cli.cmd {
        return run_space(cmd, json);
    }

    // `mcp` is an MCP *client* — it talks to other MCP servers, not the daemon.
    if let Command::Mcp { cmd } = cli.cmd {
        return run_mcp(cmd, json);
    }

    // `listen` captures audio locally (cel-audio) — no daemon.
    if let Command::Listen {
        seconds,
        source,
        list_devices,
    } = cli.cmd
    {
        return run_listen(seconds, source, list_devices, json);
    }

    // `doctor memory` runs purely against the on-disk memory DB and the
    // local Ollama model directory — no daemon needed. Other `doctor`
    // sub-batteries still talk to the daemon and so go through `connect`.
    if let Command::Doctor {
        cmd: Some(DoctorCmd::Memory { db, max_chunks }),
    } = &cli.cmd
    {
        let code = doctor::run_memory_doctor(db.clone(), *max_chunks, json).await?;
        std::process::exit(code);
    }

    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
    let client = connect(&socket).await?;

    match cli.cmd {
        Command::Status => run_status(&client, json).await,
        Command::Doctor { cmd: None } => {
            // Doctor manages its own exit code: every check runs, then the
            // report drives the process exit (0 if all pass/warn, 1 if any
            // fail). Other commands fall through to the default `Result<()>`
            // path, which only fails on hard CLI errors.
            let code = run_doctor(&client, &socket, json).await?;
            std::process::exit(code);
        }
        Command::Doctor { cmd: Some(_) } => {
            unreachable!("doctor sub-batteries that don't need the daemon are handled above")
        }
        Command::Capabilities => run_capabilities(&client, json).await,
        Command::Rules { cmd } => run_rules(&client, cmd, json).await,
        Command::Watchlists { cmd } => run_watchlists(&client, cmd, json).await,
        Command::Webhooks { cmd } => run_webhooks(&client, cmd, json).await,
        Command::Activity { cmd } => run_activity(&client, cmd, json).await,
        Command::Confirmation { cmd } => run_confirmation(&client, cmd, json).await,
        Command::Act {
            action_type,
            args,
            caller,
            session_id,
        } => run_act(&client, action_type, args, caller, session_id, json).await,
        Command::Agent { goal, dry_run } => run_agent(&client, goal, dry_run, json).await,
        Command::Click { target_id, caller } => {
            dispatch_action(
                &client,
                "click".into(),
                serde_json::json!({ "target_id": target_id }),
                caller,
                None,
                json,
            )
            .await
        }
        Command::Type {
            text,
            target,
            caller,
        } => {
            let mut args = serde_json::json!({ "text": text });
            if let Some(t) = target {
                args["target_id"] = Value::String(t);
            }
            dispatch_action(&client, "type".into(), args, caller, None, json).await
        }
        Command::App {
            app_name,
            launch,
            quit,
            background,
            caller,
        } => {
            let (action_type, args) = if quit {
                ("quit_app", serde_json::json!({ "app_name": app_name }))
            } else if launch {
                (
                    "launch_app",
                    serde_json::json!({ "app_name": app_name, "background": background }),
                )
            } else {
                ("activate_app", serde_json::json!({ "app_name": app_name }))
            };
            dispatch_action(&client, action_type.into(), args, caller, None, json).await
        }
        Command::Window {
            op,
            app,
            index,
            caller,
        } => {
            let mut args = serde_json::json!({ "op": op, "window_index": index });
            if let Some(a) = app {
                args["app"] = Value::String(a);
            }
            dispatch_action(&client, "window".into(), args, caller, None, json).await
        }
        Command::Menu { op, name, caller } => {
            let mut args = serde_json::json!({ "op": op });
            if let Some(n) = name {
                args["name"] = Value::String(n);
            }
            dispatch_action(&client, "menu_extra".into(), args, caller, None, json).await
        }
        // Already handled above — the compiler requires exhaustiveness.
        Command::Service { .. } => unreachable!(),
        Command::See => unreachable!("see is handled above"),
        Command::Ocr { .. } => unreachable!("ocr is handled above"),
        Command::Record { .. } => unreachable!("record is handled above"),
        Command::Replay { .. } => unreachable!("replay is handled above"),
        Command::Hotkey { .. } => unreachable!("hotkey is handled above"),
        Command::Space { .. } => unreachable!("space is handled above"),
        Command::Mcp { .. } => unreachable!("mcp is handled above"),
        Command::Listen { .. } => unreachable!("listen is handled above"),
        Command::Eval { .. } => unreachable!("eval is handled above"),
        Command::Completions { .. } => unreachable!("completions is handled above"),
        Command::Learn => unreachable!("learn is handled above"),
        Command::Tools => unreachable!("tools is handled above"),
        // `run --dry-run` is handled above (offline); a live `run` dispatches
        // each step through the gateway here.
        Command::Run {
            file,
            keep_going,
            retries,
            ..
        } => run_workflow(&client, &file, keep_going, retries, json).await,
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
        ActivityCmd::Publish { kind, source, data } => {
            let mut data_map = serde_json::Map::new();
            for pair in &data {
                let (k, v) = pair
                    .split_once('=')
                    .with_context(|| format!("--data must be KEY=VALUE, got: {pair:?}"))?;
                // Attempt to parse as JSON; fall back to a plain string.
                let val: Value =
                    serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.into()));
                data_map.insert(k.to_string(), val);
            }
            let params = EventsPublishParams {
                kind: kind.clone(),
                source: Some(source.clone()),
                data: data_map,
            };
            let _: cellar_ipc::results::OkResult = client
                .call("events.publish", params)
                .await
                .map_err(|e| anyhow!("events.publish: {e}"))?;
            if json {
                return emit_json(&serde_json::json!({"ok": true, "kind": kind, "source": source}));
            }
            println!("published: {kind}  (source: {source})");
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

// ───── act (gateway.intercept) ─────

/// Submit a single action to the daemon's governed gateway and print the
/// outcome. Shared by `cellar act` (raw action_type + JSON args) and the WS11
/// ergonomic verbs (click / type / app / window / menu), which build the args.
async fn dispatch_action(
    client: &Client,
    action_type: String,
    action_args: serde_json::Value,
    caller: String,
    session_id: Option<String>,
    json: bool,
) -> Result<()> {
    eprintln!("⏳  Submitting `{action_type}` to gateway (blocks until resolved or timed out)…");

    let r: cellar_ipc::results::gateway::GatewayInterceptResult = client
        .call(
            "gateway.intercept",
            cellar_ipc::params::gateway::GatewayInterceptParams {
                caller,
                action_type,
                action_args,
                agent_session_id: session_id,
                project_root: None,
            },
        )
        .await
        .map_err(|e| anyhow!("gateway.intercept: {e}"))?;

    if json {
        return emit_json(&r);
    }

    println!("{}", r.outcome.summary());
    Ok(())
}

async fn run_act(
    client: &Client,
    action_type: String,
    args: String,
    caller: String,
    session_id: Option<String>,
    json: bool,
) -> Result<()> {
    let action_args: serde_json::Value =
        serde_json::from_str(&args).with_context(|| format!("--args is not valid JSON: {args}"))?;
    dispatch_action(client, action_type, action_args, caller, session_id, json).await
}

/// Run a one-shot agent goal via the daemon's embedded agent (`agent.run`).
/// Blocks until the turn completes, then prints the agent's response.
async fn run_agent(client: &Client, goal: String, dry_run: bool, json: bool) -> Result<()> {
    eprintln!(
        "🤖  running agent goal{} (blocks until the turn completes)…",
        if dry_run { " [dry run]" } else { "" }
    );
    let r: cellar_ipc::results::agent::AgentRunResult = client
        .call(
            "agent.run",
            cellar_ipc::params::agent::AgentRunParams { goal, dry_run },
        )
        .await
        .map_err(|e| anyhow!("agent.run: {e}"))?;

    if json {
        return emit_json(&r);
    }
    println!("{}", r.response);
    eprintln!(
        "\n[session {} · {} tool call(s) dispatched]",
        r.session_id, r.tool_calls
    );
    Ok(())
}

/// Run a workflow script (WS10): dispatch each step through the governed
/// gateway (`gateway.intercept` — the same path as `cellar act`), printing a
/// per-step receipt. By default the run **stops at the first step the gateway
/// did not execute** (vetoed / denied / timed-out) and exits non-zero, so
/// automation sees an incomplete workflow; `--keep-going` continues past
/// blocked steps (exiting 0, with the blocked count reported). Transient
/// failures (transport errors, confirmation timeouts) are retried up to
/// `--retries` times before that logic applies; branching remains a follow-up.
async fn run_workflow(
    client: &Client,
    file: &Path,
    keep_going: bool,
    retries: u32,
    json: bool,
) -> Result<()> {
    let script = workflow::WorkflowScript::load(file)?;
    eprintln!(
        "▶  running workflow `{}` ({} steps) via the gateway…",
        script.name,
        script.steps.len()
    );

    let mut receipts: Vec<Value> = Vec::with_capacity(script.steps.len());
    let mut halted_at: Option<usize> = None;
    let mut blocked = 0usize;
    // Branch state: the executed status of the previous step that actually ran
    // (skipped steps don't change it).
    let mut prev_executed: Option<bool> = None;
    for (i, step) in script.steps.iter().enumerate() {
        let n = i + 1;
        let label = step.label.clone().unwrap_or_else(|| step.action.clone());

        // WS10 branching: skip steps whose `when` condition isn't met by the
        // previous run step's outcome. A skip is not a failure.
        if !step.when.should_run(prev_executed) {
            if !json {
                println!("  {n:>2}. {label} → ⤳ skipped (when={:?})", step.when);
            }
            continue;
        }

        // Dispatch with retry on TRANSIENT failures — a transport error or a
        // confirmation timeout. Deterministic outcomes (executed / vetoed /
        // denied) are never retried: re-issuing won't change them. Retries are
        // immediate and bounded by --retries.
        let mut attempt = 0u32;
        let outcome = loop {
            attempt += 1;
            let res: Result<cellar_ipc::results::gateway::GatewayInterceptResult, _> = client
                .call(
                    "gateway.intercept",
                    cellar_ipc::params::gateway::GatewayInterceptParams {
                        caller: step.caller.clone().unwrap_or_else(|| "cli.run".to_string()),
                        action_type: step.action.clone(),
                        action_args: step.params.clone(),
                        agent_session_id: None,
                        project_root: None,
                    },
                )
                .await;
            match res {
                Ok(r) => {
                    let transient = matches!(
                        r.outcome,
                        cellar_ipc::results::gateway::GatewayOutcomeWire::ConfirmationTimedOut { .. }
                    );
                    if transient && attempt <= retries {
                        eprintln!("  step {n} timed out — retry {attempt}/{retries}…");
                        continue;
                    }
                    break r.outcome;
                }
                Err(e) => {
                    if attempt <= retries {
                        eprintln!("  step {n} transport error ({e}) — retry {attempt}/{retries}…");
                        continue;
                    }
                    return Err(anyhow!(
                        "step {n} `{}` (gateway.intercept): {e}",
                        step.action
                    ));
                }
            }
        };

        let executed = outcome.executed();
        prev_executed = Some(executed);
        let summary = outcome.summary();
        if !json {
            println!("  {n:>2}. {label} → {summary}");
        }
        receipts.push(serde_json::json!({
            "step": n,
            "label": label,
            "action": step.action,
            "executed": executed,
            "outcome": summary,
        }));

        if !executed {
            blocked += 1;
            if !keep_going {
                halted_at = Some(n);
                break;
            }
        }
    }

    if json {
        emit_json(&receipts)?;
    }

    if let Some(n) = halted_at {
        // A governance outcome stopped the run before the end. Non-zero exit so
        // scripts/automation can tell the workflow did not complete.
        bail!(
            "workflow `{}` halted at step {} of {} — the gateway did not execute it \
             (see the receipt above). Re-run with --keep-going to continue past \
             blocked steps.",
            script.name,
            n,
            script.steps.len()
        );
    }

    if !json {
        if blocked > 0 {
            eprintln!(
                "✓  workflow `{}` finished all {} steps — {} blocked by the gateway (--keep-going).",
                script.name,
                script.steps.len(),
                blocked
            );
        } else {
            eprintln!(
                "✓  workflow `{}` complete ({} steps).",
                script.name,
                script.steps.len()
            );
        }
    }
    Ok(())
}

// ───── see (local AX snapshot) ─────

/// Read the current screen via Accessibility and print a snapshot — the focused
/// element + the interactive elements with bounds. Local (no daemon).
fn run_see(json: bool) -> Result<()> {
    let tree = cel_accessibility::create_tree();
    let focused = tree.focused_element().ok().flatten();
    let elements = tree.find_elements(None, None).unwrap_or_default();

    if json {
        return emit_json(&serde_json::json!({
            "focused": focused,
            "elements": elements,
        }));
    }

    match &focused {
        Some(f) => println!(
            "focused: {:?} {}",
            f.role,
            f.label.as_deref().or(f.value.as_deref()).unwrap_or("")
        ),
        None => println!("focused: (none)"),
    }

    println!("\nelements ({}):", elements.len());
    for e in elements.iter().take(60) {
        let label: String = e
            .label
            .as_deref()
            .or(e.value.as_deref())
            .unwrap_or("")
            .chars()
            .take(36)
            .collect();
        let bounds = e
            .bounds
            .as_ref()
            .map(|b| format!("[{},{} {}x{}]", b.x, b.y, b.width, b.height))
            .unwrap_or_default();
        println!(
            "  {:<6} {:<20} {:<38} {}",
            e.id,
            format!("{:?}", e.role),
            label,
            bounds
        );
    }
    if elements.len() > 60 {
        println!("  … {} more", elements.len() - 60);
    }
    Ok(())
}

// ───── ocr (on-device Vision framework) ─────

/// Run the `ocr` command locally via cel-ocr. Sources image bytes from a file
/// or a live screen capture (`--screen`), then recognizes text on-device.
fn run_ocr(
    path: Option<String>,
    screen: bool,
    fast: bool,
    min_confidence: f32,
    json: bool,
) -> Result<()> {
    if !cel_ocr::ocr_available() {
        bail!("OCR is unavailable on this platform (Vision is macOS-only)");
    }

    // Source the image bytes: a live screen capture, or a file on disk.
    let (image_bytes, source) = if screen {
        use cel_display::{encode_png, ScreenCapture, XcapCapture};
        let mut cap = XcapCapture::new();
        let frame = cap
            .capture_frame()
            .map_err(|e| anyhow!("screen capture failed: {e}"))?;
        let png = encode_png(&frame).map_err(|e| anyhow!("PNG encode failed: {e}"))?;
        (png, "screen".to_string())
    } else {
        let path = path.ok_or_else(|| anyhow!("provide an image path, or pass --screen"))?;
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {path}: {e}"))?;
        (bytes, path)
    };

    let opts = cel_ocr::OcrOptions {
        level: if fast {
            cel_ocr::RecognitionLevel::Fast
        } else {
            cel_ocr::RecognitionLevel::Accurate
        },
        min_confidence,
        ..Default::default()
    };
    let lines = cel_ocr::recognize_text_with(&image_bytes, &opts)
        .map_err(|e| anyhow!("OCR failed: {e}"))?;

    if json {
        return emit_json(&serde_json::json!({
            "source": source,
            "count": lines.len(),
            "lines": lines,
        }));
    }

    println!("ocr: {} ({} line(s))", source, lines.len());
    for l in &lines {
        let b = &l.bounds;
        println!(
            "  {:>3}% [{:>4.0},{:>4.0} {:>4.0}x{:>3.0}]  {}",
            (l.confidence * 100.0).round() as i32,
            b.x,
            b.y,
            b.width,
            b.height,
            l.text
        );
    }
    Ok(())
}

// ───── record / replay (macro, cel-input) ─────

/// Record a macro locally via cel-input. Captures real input for `seconds`,
/// then writes the recording JSON to `output` (or stdout).
fn run_record(seconds: u64, output: Option<String>, capture_chars: bool, json: bool) -> Result<()> {
    if capture_chars {
        eprintln!("⚠️  --capture-chars is ON — typed text WILL be recorded. Avoid passwords.");
    }
    eprintln!("● recording for {seconds}s (Input Monitoring permission required)…");

    let mut capture = cel_input::create_input_capture(capture_chars);
    let recording = cel_input::record(
        capture.as_mut(),
        std::time::Duration::from_secs(seconds),
        capture_chars,
    )
    .map_err(|e| anyhow!("record failed: {e}"))?;

    let body = recording
        .to_json()
        .map_err(|e| anyhow!("serialize recording: {e}"))?;

    match output {
        Some(path) => {
            std::fs::write(&path, &body).map_err(|e| anyhow!("write {path}: {e}"))?;
            if json {
                return emit_json(&serde_json::json!({
                    "ok": true,
                    "path": path,
                    "events": recording.events.len(),
                    "duration_ms": recording.duration_ms,
                }));
            }
            println!(
                "saved {} event(s) over {}ms → {path}",
                recording.events.len(),
                recording.duration_ms
            );
        }
        None => {
            // Recording JSON to stdout; keep the human note on stderr.
            eprintln!(
                "captured {} event(s) over {}ms",
                recording.events.len(),
                recording.duration_ms
            );
            println!("{body}");
        }
    }
    Ok(())
}

/// Replay a recorded macro locally via cel-input. Injects real input.
fn run_replay(path: String, speed: f64, json: bool) -> Result<()> {
    let body = std::fs::read_to_string(&path).map_err(|e| anyhow!("read {path}: {e}"))?;
    let recording =
        cel_input::Recording::from_json(&body).map_err(|e| anyhow!("parse {path}: {e}"))?;

    eprintln!(
        "▶ replaying {} event(s) at {speed}× (injects real input)…",
        recording.events.len()
    );
    let mut controller = cel_input::create_controller().map_err(|e| anyhow!("injector: {e}"))?;
    let stats = cel_input::replay(&recording, controller.as_mut(), speed)
        .map_err(|e| anyhow!("replay failed: {e}"))?;

    if json {
        return emit_json(&serde_json::json!({
            "ok": true,
            "injected": stats.injected,
            "skipped_keys": stats.skipped_keys,
        }));
    }
    println!(
        "replayed: {} injected, {} skipped",
        stats.injected, stats.skipped_keys
    );
    Ok(())
}

// ───── hotkey (global shortcuts, cel-hotkey) ─────

/// Register a global hotkey and run a command on each press. Blocks until
/// interrupted. Each press spawns the command with `$CELLAR_HOTKEY` = combo.
///
/// The whole lifecycle runs on a dedicated **OS thread**, not the async
/// runtime's worker: macOS delivers global-hotkey events on the run loop of the
/// thread that registered them, so the manager must be created and the loop
/// pumped on the *same* thread — and a `std::thread` keeps that thread stable
/// for the process lifetime (immune to tokio scheduling) instead of pinning a
/// worker in an infinite loop.
fn run_hotkey(combo: String, command: Vec<String>) -> Result<()> {
    let handle = std::thread::spawn(move || -> Result<()> {
        let mut reg = cel_hotkey::HotkeyRegistry::new().map_err(|e| anyhow!("hotkey init: {e}"))?;
        reg.register(&combo)
            .map_err(|e| anyhow!("register {combo}: {e}"))?;
        eprintln!(
            "⌨  registered {combo}; press to run `{}` ($CELLAR_HOTKEY set). Ctrl-C to stop.",
            command.join(" ")
        );
        // run() never returns; this thread blocks for the process lifetime.
        reg.run(|_id, fired| {
            eprintln!("▶ {fired}");
            if let Err(e) = std::process::Command::new(&command[0])
                .args(&command[1..])
                .env("CELLAR_HOTKEY", fired)
                .spawn()
            {
                eprintln!("  failed to spawn `{}`: {e}", command[0]);
            }
        });
        Ok(())
    });
    // Block here for the process lifetime. A setup error (bad combo / manager
    // init) returns from the thread early and is propagated; `run()` itself
    // never returns, so on success this join never completes.
    handle
        .join()
        .map_err(|_| anyhow!("hotkey thread panicked"))?
}

// ───── space (WS3, private SkyLight) ─────

/// Run a `space` subcommand locally via cel-spaces. Degrades gracefully when
/// Spaces support is unavailable (unsupported macOS / not compiled in).
fn run_space(cmd: SpaceCmd, json: bool) -> Result<()> {
    if !cel_spaces::spaces_available() {
        bail!(
            "Spaces unavailable on this system — the private SkyLight APIs are not \
             accessible (expected on unsupported macOS versions)."
        );
    }
    match cmd {
        SpaceCmd::Active => {
            let id = cel_spaces::active_space().map_err(|e| anyhow!("space active: {e}"))?;
            if json {
                emit_json(&serde_json::json!({ "active_space": id }))
            } else {
                println!("active space: {id}");
                Ok(())
            }
        }
        SpaceCmd::List => {
            let spaces = cel_spaces::list_spaces().map_err(|e| anyhow!("space list: {e}"))?;
            if json {
                return emit_json(&spaces);
            }
            if spaces.is_empty() {
                println!("(no spaces enumerated)");
            }
            for s in &spaces {
                let marker = if s.is_current { "*" } else { " " };
                println!(
                    "{marker} space {:<6} display {}",
                    s.space_id, s.display_uuid
                );
            }
            Ok(())
        }
        SpaceCmd::Switch {
            display_uuid,
            space_id,
        } => {
            cel_spaces::switch_to_space(&display_uuid, space_id)
                .map_err(|e| anyhow!("space switch: {e}"))?;
            println!("switched display {display_uuid} to space {space_id}");
            Ok(())
        }
        SpaceCmd::MoveWindow {
            window_id,
            space_id,
        } => {
            cel_spaces::move_window_to_space(window_id, space_id)
                .map_err(|e| anyhow!("space move-window: {e}"))?;
            println!("moved window {window_id} to space {space_id}");
            Ok(())
        }
    }
}

// ───── mcp client (WS19) ─────

/// Run an `mcp` subcommand: register / list / inspect / call other MCP servers.
fn run_mcp(cmd: McpCmd, json: bool) -> Result<()> {
    match cmd {
        McpCmd::Add { name, command } => {
            let (c, args) = command
                .split_first()
                .ok_or_else(|| anyhow!("missing command after `--`"))?;
            mcp::add(name, c.clone(), args.to_vec())
        }
        McpCmd::Remove { name } => mcp::remove(name),
        McpCmd::List => mcp::list(json),
        McpCmd::Inspect { name } => mcp::inspect(name, json),
        McpCmd::Call { name, tool, args } => mcp::call(name, tool, args, json),
    }
}

// ───── listen (WS17, audio input) ─────

/// Run `listen`: list input devices, or capture audio for `seconds` and print
/// the transcript. Local (cel-audio); degrades gracefully when capture or the
/// transcription backend is unavailable.
fn run_listen(seconds: u64, source: String, list_devices: bool, json: bool) -> Result<()> {
    if list_devices {
        let devices = cel_audio::list_input_devices();
        if json {
            let arr: Vec<_> = devices
                .iter()
                .map(
                    |(name, is_default)| serde_json::json!({ "name": name, "default": is_default }),
                )
                .collect();
            return emit_json(&arr);
        }
        if devices.is_empty() {
            println!("(no input devices found)");
        }
        for (name, is_default) in &devices {
            println!("{} {name}", if *is_default { "*" } else { " " });
        }
        return Ok(());
    }

    let src = match source.to_lowercase().as_str() {
        "mic" | "microphone" => cel_audio::AudioSource::Microphone,
        "system" | "output" => cel_audio::AudioSource::SystemOutput,
        "both" => cel_audio::AudioSource::Both,
        other => bail!("unknown --source `{other}` (use mic | system | both)"),
    };

    let mut capture = cel_audio::create_audio_capture();
    let config = cel_audio::AudioConfig {
        source: src,
        transcribe: true,
        ..Default::default()
    };
    capture
        .start(config)
        .map_err(|e| anyhow!("audio start: {e}"))?;
    eprintln!("🎙  listening {seconds}s (transcribing — Ctrl-C to stop early)…");

    let mut transcripts: Vec<cel_audio::TranscriptChunk> = Vec::new();
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < seconds {
        transcripts.extend(capture.drain_transcripts());
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    transcripts.extend(capture.drain_transcripts());
    let _ = capture.stop();

    if json {
        return emit_json(&transcripts);
    }
    if transcripts.is_empty() {
        println!(
            "(no transcript — the mic or the Whisper transcription backend may be \
             unavailable; set OPENAI_API_KEY for the Whisper API)"
        );
    }
    for t in &transcripts {
        println!("{}", t.text);
    }
    Ok(())
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
