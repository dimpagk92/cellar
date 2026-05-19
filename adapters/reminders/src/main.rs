//! Apple Reminders adapter — ProcessDriver entrypoint.

#[cfg(target_os = "macos")]
fn main() {
    use adapter_reminders::RemindersAdapter;
    use cel_adapter_runtime::run_stdio_loop;
    run_stdio_loop(RemindersAdapter::new());
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("adapter-reminders is macOS-only");
    std::process::exit(1);
}
