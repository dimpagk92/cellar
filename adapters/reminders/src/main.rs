//! Apple Reminders adapter — ProcessDriver entrypoint.

#![cfg(target_os = "macos")]

use adapter_reminders::RemindersAdapter;
use cel_adapter_runtime::run_stdio_loop;

fn main() {
    run_stdio_loop(RemindersAdapter::new());
}
