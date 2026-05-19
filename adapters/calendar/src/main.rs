//! Apple Calendar adapter — ProcessDriver entrypoint.

#[cfg(target_os = "macos")]
fn main() {
    use adapter_calendar::CalendarAdapter;
    use cel_adapter_runtime::run_stdio_loop;
    run_stdio_loop(CalendarAdapter::new());
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("adapter-calendar is macOS-only");
    std::process::exit(1);
}
