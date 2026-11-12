//! The TUI's panic hook restores the terminal *before* the panic message is
//! printed (DESIGN.md section 13). Its own test binary, because the panic
//! hook is process-global.

use std::panic;
use std::sync::Mutex;

static EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn record(event: &'static str) {
    EVENTS.lock().unwrap_or_else(|p| p.into_inner()).push(event);
}

fn fake_restore() {
    record("restore");
}

#[test]
fn panic_hook_restores_before_the_message_prints() {
    // Stands in for the default hook, which prints the message.
    panic::set_hook(Box::new(|_| record("print")));
    whatsup::tui::chain_panic_hook(fake_restore);

    let result = panic::catch_unwind(|| panic!("boom"));
    let _ = panic::take_hook();

    assert!(result.is_err());
    let events = EVENTS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(events, ["restore", "print"]);
}
