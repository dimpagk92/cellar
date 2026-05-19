//! Apple Messages adapter — ProcessDriver entrypoint (read-only).

#![cfg(target_os = "macos")]

use adapter_messages::MessagesAdapter;
use cel_adapter_runtime::run_stdio_loop;

fn main() {
    run_stdio_loop(MessagesAdapter::new());
}
