//! Apple Mail adapter — ProcessDriver entrypoint.
//!
//! Stdio JSON-RPC loop around `MailAdapter`. Discovered by the cortex via
//! `adapters/mail/adapter.json` (runtime: process). The cortex spawns this
//! binary, talks to it via stdin/stdout, and unspawns it on shutdown.

#[cfg(target_os = "macos")]
fn main() {
    use adapter_mail::MailAdapter;
    use cel_adapter_runtime::run_stdio_loop;
    run_stdio_loop(MailAdapter::new());
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("adapter-mail is macOS-only");
    std::process::exit(1);
}
