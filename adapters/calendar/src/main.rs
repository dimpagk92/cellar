//! Apple Calendar adapter — ProcessDriver entrypoint.

#![cfg(target_os = "macos")]

use adapter_calendar::CalendarAdapter;
use cel_adapter_runtime::run_stdio_loop;

fn main() {
    run_stdio_loop(CalendarAdapter::new());
}
