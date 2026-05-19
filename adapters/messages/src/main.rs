//! Apple Messages adapter — ProcessDriver entrypoint (read-only).

#[cfg(target_os = "macos")]
fn main() {
    use adapter_messages::MessagesAdapter;
    use cel_adapter_runtime::run_stdio_loop;
    run_stdio_loop(MessagesAdapter::new());
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("adapter-messages is macOS-only");
    std::process::exit(1);
}
