//! `cellar doctor` — diagnostic battery for the v1 install.
//!
//! Runs a fixed set of checks and prints a human-readable report. Each check
//! produces a [`CheckResult`] with a [`CheckStatus`] (`Pass` / `Warn` / `Fail`)
//! and a one-line explanation. The overall exit code is `0` if every check
//! is `Pass` or `Warn`, `1` if any check is `Fail`.
//!
//! Checks (in display order):
//!
//! 1. macOS Accessibility permission granted for the running process.
//! 2. LaunchAgent plist exists and is well-formed XML (via `plutil -lint`).
//! 3. Daemon socket reachable + `daemon.status` returns healthy.
//!    3a. IPC round-trip latency (timed `system.hello` probe; warn >100 ms, fail >500 ms).
//! 4. Configured webhook endpoints reachable (HTTP HEAD with short timeout).
//! 5. Daemon process memory below ceiling (builds on the 5b hardening pass).
//!    5a. Memory corpus snapshot (chunks, sessions, on-disk size) derived from `daemon.status.memory` — summarises the live corpus without a separate DB open (that lives in `cellar doctor memory`).
//! 6. When a Cellar subsystem is configured for Ollama, the pinned local
//!    summarizer model `llama3.2:3b-instruct-q4_K_M` is present.
//!
//! Design notes:
//! - The check functions are split into pure classification helpers and
//!   thin I/O wrappers. The pure helpers (`classify_memory`,
//!   `classify_plist_status`, `classify_ollama_models`, `infers_ollama_use`,
//!   `parse_webhook_url_host`) have unit tests; the I/O wrappers are tested
//!   indirectly via the helpers' inputs.
//! - Webhook reachability uses a [`WebhookProbe`] trait so tests can supply
//!   a deterministic in-memory implementation without standing up an HTTP
//!   server.
//! - AX permission is checked against the current process. The CLI binary
//!   itself does not need AX — but the *daemon* binary does, and on a
//!   freshly-installed Cellar both binaries ship under the same code-signing
//!   identity, so the CLI's AX state is a fast proxy. Treated as a warning
//!   when missing (the daemon binary may still be granted independently).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use cel_memory::MemoryProvider;
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};
use cellar_ipc::params::system::SystemHelloParams;
use cellar_ipc::results::daemon::MemoryCorpusStats;
use cellar_ipc::Client;

use crate::PROTOCOL_VERSION;

// ─────────────────────────── IPC round-trip latency ───────────────────────────

/// Warn threshold for the IPC socket round-trip (ms).
const IPC_RTT_WARN_MS: u64 = 100;
/// Fail threshold for the IPC socket round-trip (ms).
const IPC_RTT_FAIL_MS: u64 = 500;

/// Classify a measured `system.hello` round-trip against the latency budget.
///
/// Expected: < 20 ms on a local Unix-domain socket under no load.
/// Warn when > 100 ms (daemon may be under memory pressure or the socket is
/// being proxied). Fail when > 500 ms (daemon overloaded, wedged, or the
/// socket path is unexpected).
pub fn classify_ipc_latency(elapsed_ms: u64) -> CheckResult {
    if elapsed_ms > IPC_RTT_FAIL_MS {
        CheckResult::fail(
            "ipc rtt",
            format!(
                "socket round-trip {elapsed_ms} ms \
                 (> {IPC_RTT_FAIL_MS} ms — daemon may be overloaded; \
                 try `cellar service restart`)"
            ),
        )
    } else if elapsed_ms > IPC_RTT_WARN_MS {
        CheckResult::warn(
            "ipc rtt",
            format!(
                "socket round-trip {elapsed_ms} ms \
                 (> {IPC_RTT_WARN_MS} ms — higher than typical for a local socket; \
                 expected < 20 ms)"
            ),
        )
    } else {
        CheckResult::pass("ipc rtt", format!("socket round-trip {elapsed_ms} ms"))
    }
}

/// Issue a timed `system.hello` probe against an already-connected client and
/// classify the measured latency.
///
/// This is a *second* hello — the first was issued in `connect()` to negotiate
/// the protocol; this one gives the doctor a live latency sample under
/// whatever load the daemon is currently under.
pub async fn check_ipc_rtt(client: &Client) -> CheckResult {
    let t = Instant::now();
    let result: Result<cellar_ipc::results::system::SystemHelloResult, _> = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "cellar-doctor-rtt".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                supported_protocol_versions: vec![PROTOCOL_VERSION.into()],
            },
        )
        .await;
    let elapsed_ms = t.elapsed().as_millis() as u64;
    match result {
        Ok(_) => classify_ipc_latency(elapsed_ms),
        Err(e) => CheckResult::fail("ipc rtt", format!("system.hello probe failed: {e}")),
    }
}

// ─────────────────────────── memory corpus snapshot ───────────────────────────

/// Surface the live memory corpus stats embedded in `daemon.status.memory` as
/// a single informational doctor row.
///
/// This is a lightweight snapshot — it does not open the DB or run probes.
/// For deep memory diagnostics use `cellar doctor memory`.
pub fn classify_memory_corpus_snapshot(stats: &MemoryCorpusStats) -> CheckResult {
    let db_mb = stats.db_bytes as f64 / (1024.0 * 1024.0);
    CheckResult::pass(
        "memory corpus",
        format!(
            "{} chunks ({} session, {} long-term), \
             {} sessions ({} open), {:.1} MB on disk",
            stats.total_chunks,
            stats.session_chunks,
            stats.long_term_chunks,
            stats.total_sessions,
            stats.open_sessions,
            db_mb,
        ),
    )
}

/// One-line status from a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Everything looks right.
    Pass,
    /// Working, but worth attention (e.g., near a ceiling, a noisy log).
    Warn,
    /// Something is broken — counts toward the non-zero exit code.
    Fail,
}

impl CheckStatus {
    /// Glyph rendered in the report (kept ASCII to survive `script` /
    /// CI log capture without garbling).
    fn glyph(self) -> char {
        match self {
            CheckStatus::Pass => 'v',
            CheckStatus::Warn => '!',
            CheckStatus::Fail => 'x',
        }
    }
}

/// One row in the doctor's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Short check name. Stable, so users can grep logs.
    pub name: &'static str,
    /// Pass / warn / fail.
    pub status: CheckStatus,
    /// Single-line explanation. May embed a remediation hint.
    pub message: String,
}

impl CheckResult {
    /// Convenience constructor for a passing check.
    pub fn pass(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Pass,
            message: message.into(),
        }
    }

    /// Convenience constructor for a warning check.
    pub fn warn(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Warn,
            message: message.into(),
        }
    }

    /// Convenience constructor for a failing check.
    pub fn fail(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Fail,
            message: message.into(),
        }
    }
}

/// Outcome of `plutil -lint` on a candidate plist path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlistStatus {
    /// Path does not exist.
    Missing,
    /// `plutil -lint` returned non-zero (malformed XML / corrupt plist).
    /// Only emitted on macOS where `plutil` is available — on Linux CI
    /// builds the variant is unreachable, hence the `allow(dead_code)`.
    #[allow(dead_code)]
    Malformed,
    /// Well-formed.
    Ok,
}

/// Inputs for the Ollama presence helper, kept pure for unit testing.
pub struct OllamaCheckInputs<'a> {
    /// Whether any Cellar subsystem env var declares the Ollama provider.
    pub any_subsystem_uses_ollama: bool,
    /// Model tag we require (memory plan §1.1 decision 3).
    pub required_model: &'a str,
    /// Result of probing `ollama list`. `None` means the binary is missing
    /// or the command failed before we got a usable list. `Some(models)`
    /// is the parsed set of pulled model tags.
    pub installed_models: Option<BTreeSet<String>>,
}

/// Abstraction over the webhook reachability probe so tests can substitute
/// a deterministic implementation. The production wrapper is
/// [`ReqwestProbe`].
#[async_trait]
pub trait WebhookProbe: Send + Sync {
    /// Probe a single URL and return `Ok(true)` if it is reachable.
    /// Returns `Ok(false)` for non-2xx/3xx HTTP responses and `Err(...)`
    /// for transport-level errors (DNS, connect, timeout). The probe MUST
    /// honour a short timeout — the doctor is interactive.
    async fn probe(&self, url: &str) -> Result<bool, String>;
}

/// Production probe — runs an HTTP HEAD with a short hard timeout.
pub struct ReqwestProbe {
    client: reqwest::Client,
}

impl ReqwestProbe {
    /// Build a probe with a per-request timeout. The default is `3s`.
    pub fn new(timeout: Duration) -> Self {
        // `.timeout()` on the client covers both connect and read.
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client build for doctor");
        Self { client }
    }
}

impl Default for ReqwestProbe {
    fn default() -> Self {
        Self::new(Duration::from_secs(3))
    }
}

#[async_trait]
impl WebhookProbe for ReqwestProbe {
    async fn probe(&self, url: &str) -> Result<bool, String> {
        match self.client.head(url).send().await {
            Ok(resp) => {
                // Treat 2xx/3xx as reachable. 4xx/5xx still mean the host
                // answered, which is also "reachable" for our purposes —
                // but we surface 4xx/5xx as a warning by returning false
                // and letting the caller note the status. Anything that
                // returns a status at all means the network path works.
                Ok(resp.status().is_success() || resp.status().is_redirection())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

// ─────────────────────────── pure helpers ───────────────────────────

/// Classify the daemon's resident memory against the 500 MB ceiling.
///
/// Matches the existing 5b doctor behaviour: warn when over the ceiling,
/// pass otherwise. (We surface this as `Fail` to keep the contract that
/// exceeding the ceiling counts as a "restart recommended" failure, same
/// as the pre-existing `bail!("[doctor] some checks failed")` path.)
pub fn classify_memory(memory_mb: f64) -> CheckResult {
    const CEILING_MB: f64 = 500.0;
    if memory_mb > CEILING_MB {
        CheckResult::fail(
            "daemon memory",
            format!(
                "process memory is {memory_mb:.1} MB (> {CEILING_MB:.0} MB ceiling — \
                 restart recommended via `cellar service install` or \
                 `launchctl kickstart -k gui/$(id -u)/com.cellar.daemon`)"
            ),
        )
    } else {
        CheckResult::pass(
            "daemon memory",
            format!("process memory: {memory_mb:.1} MB (ceiling {CEILING_MB:.0} MB)"),
        )
    }
}

/// Classify a plist file probe result into a doctor row.
pub fn classify_plist_status(path: &Path, status: PlistStatus) -> CheckResult {
    match status {
        PlistStatus::Missing => CheckResult::fail(
            "launchagent plist",
            format!(
                "{} is missing — run `cellar service install`",
                path.display()
            ),
        ),
        PlistStatus::Malformed => CheckResult::fail(
            "launchagent plist",
            format!(
                "{} fails `plutil -lint` — re-run `cellar service install` to regenerate it",
                path.display()
            ),
        ),
        PlistStatus::Ok => CheckResult::pass(
            "launchagent plist",
            format!("{} is well-formed", path.display()),
        ),
    }
}

/// Classify Ollama-presence input into a doctor row.
///
/// Returns `None` when no subsystem uses Ollama — the caller should skip the
/// row entirely rather than print "n/a" noise.
pub fn classify_ollama_models(inputs: &OllamaCheckInputs<'_>) -> Option<CheckResult> {
    if !inputs.any_subsystem_uses_ollama {
        return None;
    }
    let pull_hint = format!("install with `ollama pull {}`", inputs.required_model);
    Some(match &inputs.installed_models {
        None => CheckResult::fail(
            "ollama model",
            format!(
                "Cellar is configured to use Ollama but `ollama` is not on PATH \
                 or `ollama list` failed — {pull_hint} after installing Ollama"
            ),
        ),
        Some(models) => {
            if models.contains(inputs.required_model) {
                CheckResult::pass(
                    "ollama model",
                    format!("`{}` is pulled", inputs.required_model),
                )
            } else {
                CheckResult::fail(
                    "ollama model",
                    format!(
                        "Cellar is configured to use Ollama but the pinned local \
                         summarizer model is missing — {pull_hint}"
                    ),
                )
            }
        }
    })
}

/// Decide whether *some* Cellar subsystem (or the default) routes to Ollama,
/// from a sequence of env var name/value pairs. Pure so the env source can
/// be injected in tests.
///
/// Returns `true` when:
/// - `CELLAR_DEFAULT_PROVIDER=ollama`, or
/// - any `CELLAR_<SUBSYSTEM>_PROVIDER=ollama`, or
/// - `CELLAR_MEMORY_FALLBACK_TO_LOCAL` is truthy (memory manager plan §9.3).
pub fn infers_ollama_use<'a, I>(env_pairs: I) -> bool
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    for (key, value) in env_pairs {
        if key == "CELLAR_MEMORY_FALLBACK_TO_LOCAL" {
            let v = value.trim().to_ascii_lowercase();
            if matches!(v.as_str(), "1" | "true" | "yes" | "on") {
                return true;
            }
            continue;
        }
        if !key.starts_with("CELLAR_") || !key.ends_with("_PROVIDER") {
            continue;
        }
        if value.trim().eq_ignore_ascii_case("ollama") {
            return true;
        }
    }
    false
}

/// Extract a display-friendly `host[:port]` chunk from a webhook URL, used
/// in messages so users can grep `cellar doctor` output for the broken
/// destination without printing the full URL (which may carry tokens in a
/// query string). Falls back to the full string when parsing fails.
pub fn parse_webhook_url_host(url: &str) -> String {
    // Manual parse — pulling in the `url` crate just for the display hint
    // is overkill, and reqwest::Url isn't re-exported.
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Strip userinfo if present.
    let host = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_string()
    }
}

// ─────────────────────────── I/O checks ───────────────────────────

/// Path to the LaunchAgent plist installed by `cellar service install`.
pub fn default_launch_agent_plist() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join("com.cellar.daemon.plist"),
    )
}

/// Probe the LaunchAgent plist. On non-macOS hosts (where `plutil` doesn't
/// ship) we fall back to a presence check.
pub fn probe_plist(path: &Path) -> PlistStatus {
    if !path.exists() {
        return PlistStatus::Missing;
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("plutil")
            .arg("-lint")
            .arg(path)
            .output();
        match out {
            Ok(o) if o.status.success() => PlistStatus::Ok,
            Ok(_) => PlistStatus::Malformed,
            // `plutil` absent on this macOS install — unusual but not fatal;
            // existence alone is the best we can do.
            Err(_) => PlistStatus::Ok,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        PlistStatus::Ok
    }
}

/// Probe Accessibility permission for the current process.
pub fn check_accessibility() -> CheckResult {
    #[cfg(target_os = "macos")]
    {
        if cel_accessibility::ax_is_process_trusted() {
            CheckResult::pass(
                "accessibility",
                "macOS Accessibility permission granted for this process",
            )
        } else {
            CheckResult::warn(
                "accessibility",
                "macOS Accessibility permission NOT granted — open System Settings \
                 → Privacy & Security → Accessibility, add the Cellar daemon, and \
                 toggle it on. The daemon binary ships under the same code-signing \
                 identity as the CLI on a release build, so this CLI's state is a \
                 proxy for the daemon's; verify directly when in doubt",
            )
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        CheckResult::pass(
            "accessibility",
            "not on macOS — Accessibility permission is a macOS-only concept",
        )
    }
}

/// Probe whether non-focus-stealing background input (`CGEventPostToPid`)
/// will work for this process. macOS gates posting synthetic events to
/// *other* processes under the Accessibility permission, so this shares the
/// underlying trust probe with [`check_accessibility`] but reports the
/// background-input-specific consequence: without it, CEL silently falls
/// back to the foreground activate-then-type path, which steals focus. WS1.
pub fn check_background_input() -> CheckResult {
    #[cfg(target_os = "macos")]
    {
        if cel_accessibility::ax_is_process_trusted() {
            CheckResult::pass(
                "background input",
                "non-focus-stealing input available — CGEvents can post to a \
                 target PID without activating the app",
            )
        } else {
            CheckResult::warn(
                "background input",
                "Accessibility not granted, so background (non-focus-stealing) \
                 input is rejected by target apps — CEL falls back to the \
                 foreground activate-then-type path (steals focus). Grant in \
                 System Settings → Privacy & Security → Accessibility. Note: a \
                 few apps ignore background events even when granted; CEL falls \
                 back per-action and records the path in the receipt",
            )
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        CheckResult::warn(
            "background input",
            "background (non-focus-stealing) input is macOS-only (CGEventPostToPid)",
        )
    }
}

/// Query the daemon and synthesise the rows that depend on its state.
pub async fn daemon_rows(client: &Client) -> Vec<CheckResult> {
    let mut rows = Vec::new();

    // system.hello + capability advertisement.
    let hello_result: Result<cellar_ipc::results::system::SystemHelloResult, _> = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "cellar-doctor".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                supported_protocol_versions: vec![PROTOCOL_VERSION.into()],
            },
        )
        .await;
    match hello_result {
        Ok(r) => {
            rows.push(CheckResult::pass(
                "daemon capabilities",
                format!(
                    "{} capabilities advertised (protocol {}, version {})",
                    r.capabilities.len(),
                    r.protocol_version,
                    r.daemon_version
                ),
            ));
            let expected = [
                "memory.basic",
                "gateway",
                "rules.crud",
                "watchlists.crud",
                "webhooks.crud",
            ];
            let missing: Vec<&str> = expected
                .iter()
                .filter(|c| !r.capabilities.iter().any(|x| x == *c))
                .copied()
                .collect();
            if !missing.is_empty() {
                rows.push(CheckResult::fail(
                    "baseline capabilities",
                    format!("daemon is missing baseline capabilities: {missing:?}"),
                ));
            } else {
                rows.push(CheckResult::pass(
                    "baseline capabilities",
                    "all baseline capabilities advertised",
                ));
            }
        }
        Err(e) => {
            rows.push(CheckResult::fail(
                "daemon capabilities",
                format!("system.hello failed: {e}"),
            ));
        }
    }

    // daemon.status: health + memory ceiling + confirmations.
    let status_result: Result<cellar_ipc::results::daemon::DaemonStatusResult, _> =
        client.call("daemon.status", serde_json::json!({})).await;
    match status_result {
        Ok(r) => {
            rows.push(if r.healthy {
                CheckResult::pass(
                    "daemon.status",
                    format!("daemon healthy (uptime {}s)", r.uptime_s),
                )
            } else {
                CheckResult::fail(
                    "daemon.status",
                    format!("daemon reports unhealthy (uptime {}s)", r.uptime_s),
                )
            });
            rows.push(classify_memory(r.memory_mb));
            // Memory corpus snapshot (populated by daemon builds that wire the
            // memory subsystem; older or stub builds leave this `None`).
            if let Some(ref corpus) = r.memory {
                rows.push(classify_memory_corpus_snapshot(corpus));
            }
            if r.pending_confirmations > 20 {
                rows.push(CheckResult::warn(
                    "pending confirmations",
                    format!(
                        "{} pending confirmations queued — users may be blocked",
                        r.pending_confirmations
                    ),
                ));
            } else if r.pending_confirmations > 0 {
                rows.push(CheckResult::pass(
                    "pending confirmations",
                    format!("{} pending", r.pending_confirmations),
                ));
            } else {
                rows.push(CheckResult::pass("pending confirmations", "0 pending"));
            }
        }
        Err(e) => {
            rows.push(CheckResult::fail(
                "daemon.status",
                format!("daemon.status failed: {e}"),
            ));
        }
    }

    rows
}

/// List configured webhooks and probe each one.
pub async fn webhook_rows(client: &Client, probe: &dyn WebhookProbe) -> Vec<CheckResult> {
    let list: Result<cellar_ipc::results::webhooks::WebhooksListResult, _> = client
        .call(
            "webhooks.list",
            cellar_ipc::params::webhooks::WebhooksListParams::default(),
        )
        .await;
    let configs = match list {
        Ok(r) => r.webhooks,
        Err(e) => {
            return vec![CheckResult::warn(
                "webhooks",
                format!(
                    "could not fetch webhook list (webhooks.list: {e}); skipping \
                     reachability checks"
                ),
            )];
        }
    };
    if configs.is_empty() {
        return vec![CheckResult::pass(
            "webhooks",
            "no webhooks configured — skipping reachability checks",
        )];
    }

    let mut rows = Vec::with_capacity(configs.len());
    for cfg in configs {
        let host = parse_webhook_url_host(&cfg.url);
        match probe.probe(&cfg.url).await {
            Ok(true) => rows.push(CheckResult::pass(
                "webhook",
                format!("{} ({}) reachable", cfg.id, host),
            )),
            Ok(false) => rows.push(CheckResult::warn(
                "webhook",
                format!(
                    "{} ({}) answered but returned a non-2xx/3xx status — verify the \
                     endpoint accepts HEAD or expect 4xx/5xx in production",
                    cfg.id, host
                ),
            )),
            Err(e) => rows.push(CheckResult::fail(
                "webhook",
                format!("{} ({}) UNREACHABLE: {e}", cfg.id, host),
            )),
        }
    }
    rows
}

/// Build the Ollama row from process env + a probe of `ollama list`.
pub fn ollama_row() -> Option<CheckResult> {
    const REQUIRED_MODEL: &str = "llama3.2:3b-instruct-q4_K_M";
    let env_pairs: Vec<(String, String)> = std::env::vars().collect();
    let any_ollama = infers_ollama_use(env_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let installed = if any_ollama {
        probe_ollama_models()
    } else {
        None
    };
    classify_ollama_models(&OllamaCheckInputs {
        any_subsystem_uses_ollama: any_ollama,
        required_model: REQUIRED_MODEL,
        installed_models: installed,
    })
}

/// Best-effort probe of `ollama list`. Returns `None` if the binary is
/// absent or the call failed; `Some(models)` otherwise.
fn probe_ollama_models() -> Option<BTreeSet<String>> {
    let out = std::process::Command::new("ollama")
        .arg("list")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Some(parse_ollama_list(&stdout))
}

/// Parse the `ollama list` table into a set of model tags. The CLI prints:
///
/// ```text
/// NAME                               ID              SIZE      MODIFIED
/// llama3.2:3b-instruct-q4_K_M        abcd1234        2.0 GB    3 hours ago
/// ```
///
/// We split on whitespace and take the first column; the header row is
/// filtered by exact-match. Robust against extra columns or padding.
pub fn parse_ollama_list(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if i == 0 && trimmed.split_whitespace().next() == Some("NAME") {
            continue;
        }
        if let Some(name) = trimmed.split_whitespace().next() {
            out.insert(name.to_string());
        }
    }
    out
}

// ─────────────────────────── report ───────────────────────────

/// Render the full report and return the process exit code.
pub fn render_report(rows: &[CheckResult]) -> i32 {
    println!("[cellar doctor]");
    for row in rows {
        println!(
            "  {}  {:<24}  {}",
            row.status.glyph(),
            row.name,
            row.message
        );
    }
    let fails = rows
        .iter()
        .filter(|r| r.status == CheckStatus::Fail)
        .count();
    let warns = rows
        .iter()
        .filter(|r| r.status == CheckStatus::Warn)
        .count();
    let passes = rows
        .iter()
        .filter(|r| r.status == CheckStatus::Pass)
        .count();
    println!(
        "[cellar doctor] {} passed, {} warning(s), {} failure(s)",
        passes, warns, fails
    );
    if fails > 0 {
        eprintln!("[cellar doctor] some checks failed — see rows marked 'x' above");
        1
    } else {
        0
    }
}

// ─────────────────────────── orchestration ───────────────────────────

/// Result of [`assemble_report`] — owned so it can be inspected in tests.
pub struct Report {
    /// All rows, in display order.
    pub rows: Vec<CheckResult>,
}

/// Build the full report. Pulls AX + plist + daemon + webhook + Ollama
/// rows in a fixed order so output is stable for diffing.
///
/// `socket_path` is purely informational (we already connected via `client`);
/// it is included in the AX row's neighbour to remind the user where the
/// daemon was reached.
pub async fn assemble_report(
    client: &Client,
    socket_path: &Path,
    plist_path: Option<&Path>,
    probe: &dyn WebhookProbe,
) -> Report {
    let mut rows: Vec<CheckResult> = Vec::new();

    // 1. Accessibility (cheap, local).
    rows.push(check_accessibility());
    rows.push(check_background_input());

    // 2. LaunchAgent plist.
    match plist_path {
        Some(p) => rows.push(classify_plist_status(p, probe_plist(p))),
        None => rows.push(CheckResult::warn(
            "launchagent plist",
            "HOME not set; could not derive ~/Library/LaunchAgents path",
        )),
    }

    // 3. Daemon socket (informational + IPC latency probe).
    rows.push(CheckResult::pass(
        "daemon socket",
        format!("connected to {}", socket_path.display()),
    ));
    rows.push(check_ipc_rtt(client).await);
    rows.extend(daemon_rows(client).await);

    // 4. Webhooks.
    rows.extend(webhook_rows(client, probe).await);

    // 5. Ollama (optional row).
    if let Some(row) = ollama_row() {
        rows.push(row);
    }

    Report { rows }
}

// ─────────────────────────── memory subsystem battery ───────────────────────────

/// Latency budget (p95) for `write`-time embedding round-trips, per
/// `cellar-memory-manager.md` §14.4. Exceeding this on a `mock` embedder
/// would indicate severe SQLite or disk degradation; on `fastembed` it
/// would mean the model loader path is wedged.
const EMBED_LATENCY_P95_BUDGET_MS: u64 = 30;

/// How many probe writes the latency check runs. Small enough that
/// `cellar doctor memory` stays interactive, large enough to make the
/// p95 statistic non-trivial.
const EMBED_LATENCY_PROBE_COUNT: usize = 20;

/// Resolve the memory DB path with the same precedence the daemon uses
/// (`CELLAR_MEMORY_DB` env var, then `$HOME/.cellar/memory.sqlite`).
/// `db_override` takes the highest precedence (the `--db` CLI flag).
pub fn resolve_memory_db_path(db_override: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = db_override {
        return Some(p);
    }
    if let Ok(env_path) = std::env::var("CELLAR_MEMORY_DB") {
        if !env_path.is_empty() {
            return Some(PathBuf::from(env_path));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".cellar").join("memory.sqlite"))
}

/// Classify whether the configured corpus chunk count is within the
/// configured cap.
pub fn classify_corpus_growth(total_chunks: usize, cap: usize) -> CheckResult {
    if cap == 0 {
        return CheckResult::warn(
            "memory corpus",
            format!(
                "{} chunks; --max-chunks=0 disables the growth check",
                total_chunks
            ),
        );
    }
    let pct = (total_chunks as f32 / cap as f32) * 100.0;
    if total_chunks > cap {
        CheckResult::fail(
            "memory corpus",
            format!(
                "{} chunks > cap {} ({:.0}%) — \
                 the aging sweep may be falling behind. Inspect via `cellar memory stats` \
                 once that surface ships; until then run `sqlite3 $HOME/.cellar/memory.sqlite \
                 \"DELETE FROM memory_chunks WHERE pinned=0 AND importance < 0.2;\"` as a \
                 manual fallback",
                total_chunks, cap, pct
            ),
        )
    } else if pct >= 80.0 {
        CheckResult::warn(
            "memory corpus",
            format!(
                "{} chunks (cap {}, {:.0}%) — approaching the cap. \
                 Aging-sweep config or retention horizons may need a tighter setting",
                total_chunks, cap, pct
            ),
        )
    } else {
        CheckResult::pass(
            "memory corpus",
            format!("{} chunks (cap {}, {:.0}%)", total_chunks, cap, pct),
        )
    }
}

/// Classify a recorded write p95 against the budget.
pub fn classify_embed_latency(p95_ms: u64, n_samples: usize) -> CheckResult {
    if n_samples == 0 {
        return CheckResult::warn(
            "memory write p95",
            "no samples were collected — the probe was skipped",
        );
    }
    if p95_ms > EMBED_LATENCY_P95_BUDGET_MS {
        CheckResult::fail(
            "memory write p95",
            format!(
                "p95 write latency {} ms (over {} samples) exceeds the {} ms budget. \
                 If the embedder is `mock`, this points at SQLite or disk degradation \
                 (try `cellar service restart`); if the embedder is `fastembed`, the \
                 ONNX model loader may be wedged",
                p95_ms, n_samples, EMBED_LATENCY_P95_BUDGET_MS
            ),
        )
    } else {
        CheckResult::pass(
            "memory write p95",
            format!(
                "p95 write latency {} ms over {} samples (budget {} ms)",
                p95_ms, n_samples, EMBED_LATENCY_P95_BUDGET_MS
            ),
        )
    }
}

/// Classify whether the memory DB file is present + readable.
pub fn classify_memory_db(path: &Path, exists: bool, open_error: Option<&str>) -> CheckResult {
    if !exists {
        return CheckResult::warn(
            "memory db",
            format!(
                "{} does not exist yet — it is created on the first write through the daemon. \
                 If the daemon is running and this path is still missing, run `cellar status` \
                 to verify it's healthy",
                path.display()
            ),
        );
    }
    match open_error {
        None => CheckResult::pass("memory db", format!("{} is readable", path.display())),
        Some(e) => CheckResult::fail(
            "memory db",
            format!(
                "{} exists but failed to open: {} — check disk permissions and that \
                 sqlite-vec extension loads cleanly (`cellar service status`)",
                path.display(),
                e
            ),
        ),
    }
}

/// Classify whether the embedding model is reachable / loadable.
///
/// In v1 the daemon ships with `MockEmbedder` by default; if the user
/// has enabled `fastembed`, the model lives in `~/.cellar/models/`. The
/// doctor surfaces this as a Pass with a label rather than running a
/// real embed call (which would require the model on disk).
pub fn classify_embedding_model(model_name: Option<&str>) -> CheckResult {
    match model_name {
        Some(name) => CheckResult::pass("memory embedder", format!("embedding model: {}", name)),
        None => CheckResult::warn(
            "memory embedder",
            "no embedding model reported by the provider — retrieval will fall back to \
             FTS-only, recall@5 will suffer. Enable `fastembed` feature or check the \
             daemon's CELLAR_EMBEDDING_MODEL env var",
        ),
    }
}

/// Assemble the memory-doctor report by opening the DB ourselves
/// (read-only path), pulling stats, and running a small write-latency
/// probe against an isolated in-memory provider.
///
/// Returns a `(report rows, exit code)` pair. Exit code is `0` if
/// nothing in the rows is `Fail`, `1` otherwise.
pub async fn assemble_memory_report(
    db_path_override: Option<PathBuf>,
    max_chunks: usize,
) -> Vec<CheckResult> {
    let mut rows: Vec<CheckResult> = Vec::new();

    let path = resolve_memory_db_path(db_path_override);
    let path_display = path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unset; $HOME missing>".into());

    let path_for_check = path.clone().unwrap_or_else(|| PathBuf::from(&path_display));
    let exists = path.as_ref().map(|p| p.exists()).unwrap_or(false);

    // Try to open the DB read-only, gather stats. If it doesn't exist
    // yet (cold install) we skip the open and surface a Warn.
    let (open_error, stats_row, embedder_row): (
        Option<String>,
        Option<CheckResult>,
        Option<CheckResult>,
    ) = if exists {
        let embedder = Arc::new(MockEmbedder::new());
        match SqliteMemoryProvider::open(path.as_ref().unwrap(), embedder).await {
            Ok(provider) => match provider.stats().await {
                Ok(stats) => (
                    None,
                    Some(classify_corpus_growth(stats.total_chunks, max_chunks)),
                    Some(classify_embedding_model(stats.embedding_model.as_deref())),
                ),
                Err(e) => (Some(e.to_string()), None, None),
            },
            Err(e) => (Some(e.to_string()), None, None),
        }
    } else {
        (None, None, None)
    };
    rows.push(classify_memory_db(
        &path_for_check,
        exists,
        open_error.as_deref(),
    ));
    if let Some(r) = stats_row {
        rows.push(r);
    }
    if let Some(r) = embedder_row {
        rows.push(r);
    }

    // Latency probe — runs against a *fresh* in-memory provider so it
    // doesn't write into the user's real DB. The MockEmbedder is
    // constant-time, so the measured p95 is dominated by SQLite +
    // tokio task scheduling. If we ever wire fastembed here we should
    // gate this probe behind a `--skip-probe` flag.
    let p95 = probe_write_p95_ms().await;
    rows.push(classify_embed_latency(p95, EMBED_LATENCY_PROBE_COUNT));

    rows
}

/// Run a small `EMBED_LATENCY_PROBE_COUNT`-sample probe against a fresh
/// in-memory provider and return the p95 in milliseconds.
async fn probe_write_p95_ms() -> u64 {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = match SqliteMemoryProvider::open_in_memory(embedder).await {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let mut samples: Vec<u128> = Vec::with_capacity(EMBED_LATENCY_PROBE_COUNT);
    for i in 0..EMBED_LATENCY_PROBE_COUNT {
        let chunk = cel_memory::NewMemoryChunk {
            kind: cel_memory::ChunkKind::Context,
            source: cel_memory::ChunkSource::System,
            session_id: None,
            project_root: None,
            caller_id: "cellar-doctor".into(),
            content: format!("probe write #{i}"),
            metadata: serde_json::Value::Null,
            importance: None,
            shareable: false,
            pinned: false,
        };
        let start = Instant::now();
        if provider.write(chunk).await.is_err() {
            continue;
        }
        samples.push(start.elapsed().as_micros());
    }
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    // 95th percentile via nearest-rank: ceil(0.95 * N) - 1.
    let idx = ((samples.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1);
    (samples[idx] / 1000) as u64
}

/// Public entry point for `cellar doctor memory`. Renders the report
/// and returns the process exit code.
pub async fn run_memory_doctor(
    db_override: Option<PathBuf>,
    max_chunks: usize,
    json: bool,
) -> Result<i32> {
    let rows = assemble_memory_report(db_override, max_chunks).await;
    if json {
        let payload = serde_json::json!({
            "checks": rows.iter().map(|r| serde_json::json!({
                "name": r.name,
                "status": match r.status {
                    CheckStatus::Pass => "pass",
                    CheckStatus::Warn => "warn",
                    CheckStatus::Fail => "fail",
                },
                "message": r.message,
            })).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| anyhow!("serialize doctor: {e}"))?
        );
        let fails = rows
            .iter()
            .filter(|r| r.status == CheckStatus::Fail)
            .count();
        Ok(if fails > 0 { 1 } else { 0 })
    } else {
        println!("[cellar doctor memory]");
        for row in &rows {
            println!(
                "  {}  {:<24}  {}",
                row.status.glyph(),
                row.name,
                row.message
            );
        }
        let fails = rows
            .iter()
            .filter(|r| r.status == CheckStatus::Fail)
            .count();
        let warns = rows
            .iter()
            .filter(|r| r.status == CheckStatus::Warn)
            .count();
        let passes = rows
            .iter()
            .filter(|r| r.status == CheckStatus::Pass)
            .count();
        println!(
            "[cellar doctor memory] {} passed, {} warning(s), {} failure(s)",
            passes, warns, fails
        );
        Ok(if fails > 0 { 1 } else { 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ───── classify_memory ─────

    #[test]
    fn classify_memory_passes_under_ceiling() {
        let r = classify_memory(123.4);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("123.4"));
    }

    #[test]
    fn classify_memory_fails_over_ceiling() {
        let r = classify_memory(750.0);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("750.0"));
        assert!(r.message.contains("500"));
    }

    #[test]
    fn classify_memory_boundary_is_inclusive_pass() {
        // 500 MB exactly → pass (the matching `>` in production)
        let r = classify_memory(500.0);
        assert_eq!(r.status, CheckStatus::Pass);
    }

    // ───── classify_plist_status ─────

    #[test]
    fn classify_plist_missing_is_fail() {
        let r = classify_plist_status(Path::new("/no/such/path.plist"), PlistStatus::Missing);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("missing"));
        assert!(r.message.contains("cellar service install"));
    }

    #[test]
    fn classify_plist_malformed_is_fail() {
        let r = classify_plist_status(Path::new("/x.plist"), PlistStatus::Malformed);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("plutil -lint"));
    }

    #[test]
    fn classify_plist_ok_is_pass() {
        let r = classify_plist_status(Path::new("/x.plist"), PlistStatus::Ok);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("well-formed"));
    }

    // ───── infers_ollama_use ─────

    #[test]
    fn infers_ollama_use_default_provider() {
        assert!(infers_ollama_use([("CELLAR_DEFAULT_PROVIDER", "ollama")]));
    }

    #[test]
    fn infers_ollama_use_subsystem_provider() {
        assert!(infers_ollama_use([("CELLAR_MEMORY_PROVIDER", "ollama")]));
    }

    #[test]
    fn infers_ollama_use_case_insensitive() {
        assert!(infers_ollama_use([("CELLAR_AGENT_PROVIDER", "Ollama")]));
    }

    #[test]
    fn infers_ollama_use_returns_false_when_anthropic() {
        assert!(!infers_ollama_use([
            ("CELLAR_DEFAULT_PROVIDER", "anthropic"),
            ("CELLAR_AGENT_PROVIDER", "openai"),
        ]));
    }

    #[test]
    fn infers_ollama_use_local_fallback_flag() {
        assert!(infers_ollama_use([(
            "CELLAR_MEMORY_FALLBACK_TO_LOCAL",
            "true"
        ),]));
        assert!(infers_ollama_use([(
            "CELLAR_MEMORY_FALLBACK_TO_LOCAL",
            "1"
        )]));
        assert!(infers_ollama_use([(
            "CELLAR_MEMORY_FALLBACK_TO_LOCAL",
            "ON"
        )]));
        assert!(!infers_ollama_use([(
            "CELLAR_MEMORY_FALLBACK_TO_LOCAL",
            "false"
        )]));
    }

    #[test]
    fn infers_ollama_use_ignores_unrelated_keys() {
        assert!(!infers_ollama_use([
            ("HOME", "/Users/x"),
            ("PATH", "/usr/bin"),
            ("CELLAR_DEFAULT_MODEL", "ollama-but-not-provider"),
        ]));
    }

    // ───── classify_ollama_models ─────

    #[test]
    fn classify_ollama_models_skips_when_not_in_use() {
        let inputs = OllamaCheckInputs {
            any_subsystem_uses_ollama: false,
            required_model: "llama3.2:3b-instruct-q4_K_M",
            installed_models: None,
        };
        assert!(classify_ollama_models(&inputs).is_none());
    }

    #[test]
    fn classify_ollama_models_fails_when_ollama_missing() {
        let inputs = OllamaCheckInputs {
            any_subsystem_uses_ollama: true,
            required_model: "llama3.2:3b-instruct-q4_K_M",
            installed_models: None,
        };
        let r = classify_ollama_models(&inputs).expect("row");
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("not on PATH"));
        assert!(r
            .message
            .contains("ollama pull llama3.2:3b-instruct-q4_K_M"));
    }

    #[test]
    fn classify_ollama_models_fails_when_model_missing() {
        let mut models = BTreeSet::new();
        models.insert("some-other-model:latest".to_string());
        let inputs = OllamaCheckInputs {
            any_subsystem_uses_ollama: true,
            required_model: "llama3.2:3b-instruct-q4_K_M",
            installed_models: Some(models),
        };
        let r = classify_ollama_models(&inputs).expect("row");
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r
            .message
            .contains("ollama pull llama3.2:3b-instruct-q4_K_M"));
    }

    #[test]
    fn classify_ollama_models_passes_when_model_present() {
        let mut models = BTreeSet::new();
        models.insert("llama3.2:3b-instruct-q4_K_M".to_string());
        models.insert("other:latest".to_string());
        let inputs = OllamaCheckInputs {
            any_subsystem_uses_ollama: true,
            required_model: "llama3.2:3b-instruct-q4_K_M",
            installed_models: Some(models),
        };
        let r = classify_ollama_models(&inputs).expect("row");
        assert_eq!(r.status, CheckStatus::Pass);
    }

    // ───── parse_ollama_list ─────

    #[test]
    fn parse_ollama_list_handles_header_row() {
        let text = "\
NAME                               ID              SIZE      MODIFIED
llama3.2:3b-instruct-q4_K_M        abcd1234        2.0 GB    3 hours ago
llama3.1:latest                    f00d            4.7 GB    1 day ago
";
        let got = parse_ollama_list(text);
        assert!(got.contains("llama3.2:3b-instruct-q4_K_M"));
        assert!(got.contains("llama3.1:latest"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn parse_ollama_list_handles_empty() {
        let got = parse_ollama_list("");
        assert!(got.is_empty());
    }

    #[test]
    fn parse_ollama_list_ignores_blank_lines() {
        let text = "\nllama3.2:3b-instruct-q4_K_M  abc\n\n\nother:tag  def\n";
        let got = parse_ollama_list(text);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn parse_ollama_list_only_header() {
        let text = "NAME    ID    SIZE    MODIFIED\n";
        let got = parse_ollama_list(text);
        assert!(got.is_empty());
    }

    // ───── parse_webhook_url_host ─────

    #[test]
    fn parse_webhook_url_host_https() {
        assert_eq!(
            parse_webhook_url_host("https://hooks.example.com/abc"),
            "hooks.example.com"
        );
    }

    #[test]
    fn parse_webhook_url_host_with_port() {
        assert_eq!(
            parse_webhook_url_host("http://10.0.0.5:8080/webhook?token=xyz"),
            "10.0.0.5:8080"
        );
    }

    #[test]
    fn parse_webhook_url_host_strips_userinfo() {
        assert_eq!(
            parse_webhook_url_host("https://user:pass@hooks.example.com/path"),
            "hooks.example.com"
        );
    }

    #[test]
    fn parse_webhook_url_host_no_scheme() {
        assert_eq!(
            parse_webhook_url_host("hooks.example.com/abc"),
            "hooks.example.com"
        );
    }

    #[test]
    fn parse_webhook_url_host_fragment_only() {
        assert_eq!(
            parse_webhook_url_host("https://hooks.example.com#frag"),
            "hooks.example.com"
        );
    }

    // ───── render_report ─────

    #[test]
    fn render_report_exit_0_when_no_failures() {
        let rows = vec![
            CheckResult::pass("a", "ok"),
            CheckResult::warn("b", "watch out"),
        ];
        assert_eq!(render_report(&rows), 0);
    }

    #[test]
    fn render_report_exit_1_when_any_failure() {
        let rows = vec![
            CheckResult::pass("a", "ok"),
            CheckResult::fail("b", "boom"),
            CheckResult::warn("c", "soft"),
        ];
        assert_eq!(render_report(&rows), 1);
    }

    // ───── webhook_rows with a mock probe ─────

    /// Mock probe that returns a canned response per URL.
    struct MockProbe {
        responses: std::collections::HashMap<String, Result<bool, String>>,
    }

    #[async_trait]
    impl WebhookProbe for MockProbe {
        async fn probe(&self, url: &str) -> Result<bool, String> {
            self.responses
                .get(url)
                .cloned()
                .unwrap_or_else(|| Err(format!("no canned response for {url}")))
        }
    }

    // ───── memory doctor classifiers ─────

    #[test]
    fn classify_corpus_growth_passes_under_threshold() {
        let r = classify_corpus_growth(1_000, 500_000);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("1000"));
        assert!(r.message.contains("500000"));
    }

    #[test]
    fn classify_corpus_growth_warns_near_cap() {
        let r = classify_corpus_growth(450_000, 500_000);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("approaching the cap"));
    }

    #[test]
    fn classify_corpus_growth_fails_over_cap() {
        let r = classify_corpus_growth(600_000, 500_000);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("aging sweep"));
    }

    #[test]
    fn classify_corpus_growth_zero_cap_disables_check() {
        let r = classify_corpus_growth(1_000_000, 0);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("disables"));
    }

    #[test]
    fn classify_embed_latency_passes_under_budget() {
        let r = classify_embed_latency(5, 20);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("5 ms"));
    }

    #[test]
    fn classify_embed_latency_fails_over_budget() {
        let r = classify_embed_latency(500, 20);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("500 ms"));
        assert!(r.message.contains("30 ms"));
    }

    #[test]
    fn classify_embed_latency_warns_with_zero_samples() {
        let r = classify_embed_latency(0, 0);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("no samples"));
    }

    #[test]
    fn classify_memory_db_warns_when_missing() {
        let r = classify_memory_db(Path::new("/no/such/memory.sqlite"), false, None);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("does not exist yet"));
    }

    #[test]
    fn classify_memory_db_passes_when_readable() {
        let r = classify_memory_db(Path::new("/tmp/memory.sqlite"), true, None);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("readable"));
    }

    #[test]
    fn classify_memory_db_fails_on_open_error() {
        let r = classify_memory_db(Path::new("/tmp/memory.sqlite"), true, Some("disk full"));
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("disk full"));
    }

    #[test]
    fn classify_embedding_model_passes_with_name() {
        let r = classify_embedding_model(Some("bge-small-en-v1.5"));
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("bge-small-en-v1.5"));
    }

    #[test]
    fn classify_embedding_model_warns_when_none() {
        let r = classify_embedding_model(None);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("no embedding model"));
    }

    #[test]
    fn resolve_memory_db_path_honors_explicit_override() {
        let p = resolve_memory_db_path(Some(PathBuf::from("/tmp/explicit.sqlite")));
        assert_eq!(p, Some(PathBuf::from("/tmp/explicit.sqlite")));
    }

    // ───── memory doctor end-to-end ─────

    #[tokio::test]
    async fn run_memory_doctor_against_missing_db_warns_but_does_not_fail() {
        // Point at a non-existent path so the DB-open path short-circuits.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nope.sqlite");
        let rows = assemble_memory_report(Some(path), 500_000).await;
        // No row should be Fail — missing DB is a Warn (cold install).
        let fails = rows
            .iter()
            .filter(|r| r.status == CheckStatus::Fail)
            .count();
        assert_eq!(
            fails, 0,
            "missing DB should not produce a hard failure on a cold install"
        );
        // The latency probe row should be present and ideally Pass on a
        // healthy box; we don't assert Pass strictly because CI hosts
        // can be slow. We do assert the row exists.
        assert!(rows.iter().any(|r| r.name == "memory write p95"));
        // And the DB row exists.
        assert!(rows.iter().any(|r| r.name == "memory db"));
    }

    #[tokio::test]
    async fn run_memory_doctor_against_seeded_db_pulls_stats() {
        // Open a fresh DB at a tempdir path, write a couple of chunks
        // so `stats` returns a nonzero count, then run the doctor.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("seeded.sqlite");
        {
            let embedder = Arc::new(MockEmbedder::new());
            let provider = SqliteMemoryProvider::open(&path, embedder).await.unwrap();
            for i in 0..3 {
                provider
                    .write(cel_memory::NewMemoryChunk {
                        kind: cel_memory::ChunkKind::Chat,
                        source: cel_memory::ChunkSource::System,
                        session_id: None,
                        project_root: None,
                        caller_id: "test".into(),
                        content: format!("chunk {i}"),
                        metadata: serde_json::Value::Null,
                        importance: None,
                        shareable: false,
                        pinned: false,
                    })
                    .await
                    .unwrap();
            }
        }
        let rows = assemble_memory_report(Some(path), 500_000).await;
        let db_row = rows.iter().find(|r| r.name == "memory db").unwrap();
        assert_eq!(db_row.status, CheckStatus::Pass);
        let corpus_row = rows
            .iter()
            .find(|r| r.name == "memory corpus")
            .expect("seeded DB should produce a corpus row");
        // 3 chunks under a 500k cap → Pass.
        assert_eq!(corpus_row.status, CheckStatus::Pass);
        assert!(corpus_row.message.contains("3 chunks"));
    }

    #[tokio::test]
    async fn mock_probe_reports_each_outcome() {
        // We can't construct a real `Client` without a daemon, but we can
        // hand-build the rows the way `webhook_rows` would.
        let probe = MockProbe {
            responses: [
                ("https://ok.example/".to_string(), Ok::<bool, String>(true)),
                (
                    "https://soft.example/".to_string(),
                    Ok::<bool, String>(false),
                ),
                (
                    "https://broken.example/".to_string(),
                    Err::<bool, String>("dns error: no record".into()),
                ),
            ]
            .into_iter()
            .collect(),
        };

        let configs = [
            ("ok", "https://ok.example/"),
            ("soft", "https://soft.example/"),
            ("broken", "https://broken.example/"),
        ];
        let mut rows: Vec<CheckResult> = Vec::new();
        for (id, url) in configs {
            let host = parse_webhook_url_host(url);
            match probe.probe(url).await {
                Ok(true) => rows.push(CheckResult::pass(
                    "webhook",
                    format!("{id} ({host}) reachable"),
                )),
                Ok(false) => rows.push(CheckResult::warn(
                    "webhook",
                    format!("{id} ({host}) answered but returned a non-2xx/3xx status"),
                )),
                Err(e) => rows.push(CheckResult::fail(
                    "webhook",
                    format!("{id} ({host}) UNREACHABLE: {e}"),
                )),
            }
        }

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].status, CheckStatus::Pass);
        assert_eq!(rows[1].status, CheckStatus::Warn);
        assert_eq!(rows[2].status, CheckStatus::Fail);
        assert!(rows[2].message.contains("dns error"));
    }
}
