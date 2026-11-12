//! The interactive TUI (DESIGN.md section 13): `whatsup` with no arguments
//! on a terminal.
//!
//! - [`app`]: the pure state machine (`on_key` returns an [`Effect`]).
//! - [`view`]: pure rendering.
//! - this module: the event loop, which owns every side effect. Scans run on
//!   a worker thread (a slow `docker ps` never freezes the keyboard) and each
//!   stop runs on its own thread (the grace period and respawn check take
//!   seconds). Workers report back over a channel.
//!
//! The terminal is always restored: [`TerminalGuard`]'s `Drop` covers quit
//! and errors, and a panic hook restores it before the panic message prints.

pub mod app;
#[cfg(test)]
mod fixtures;
pub mod view;

use std::io::{self, Stdout};
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::actions::{OsControl, Timing, local_url, open_in_browser, stop_port};
use crate::model::{Entry, FailReason, StopMode, StopResult};
use crate::scan::Scanner;
use app::{App, Effect, Level, Mode, StatusLine, on_key};
use view::Theme;

/// Auto-refresh interval (section 13).
const REFRESH_EVERY: Duration = Duration::from_secs(2);
/// How long a status message stays before the key hints return.
const STATUS_FOR: Duration = Duration::from_secs(4);
/// Longest wait for a key before re-checking timers and worker results.
const TICK: Duration = Duration::from_millis(250);

/// What the TUI starts with (from the CLI flags and environment).
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--all`: start in "all" instead of "dev only".
    pub show_all: bool,
    /// False with `--no-color` or `NO_COLOR`.
    pub color: bool,
    /// The user's home directory, for `~/...` paths.
    pub home: Option<String>,
    /// Note shown when switching to "all" (macOS without root).
    pub all_note: Option<&'static str>,
}

/// Runs the TUI until the user quits. The terminal is restored on return,
/// whether the loop ended normally or with an error.
pub fn run(opts: Options) -> anyhow::Result<()> {
    let guard = TerminalGuard::enter()?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("could not start the TUI")?;
    let mut app = App::new(opts.show_all);
    app.home = opts.home;
    app.all_note = opts.all_note;
    let result = event_loop(&mut terminal, &mut app, Theme { color: opts.color });
    drop(guard);
    result
}

/// Puts the terminal in raw mode on the alternate screen; `Drop` undoes it.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        install_panic_hook();
        enable_raw_mode().context("could not switch the terminal to raw mode")?;
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
        // From here on, dropping the guard (including via `?`) restores.
        let guard = TerminalGuard;
        execute!(io::stdout(), EnterAlternateScreen)
            .context("could not switch to the alternate screen")?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// True between entering raw mode and restoring the terminal.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Leaves raw mode and the alternate screen and shows the cursor, once:
/// later calls (the guard's `Drop` after the panic hook ran) do nothing.
/// That matters because leaving the alternate screen also restores the
/// cursor saved on entry, which would move the shell prompt back over a
/// panic message printed in between.
fn restore_terminal() {
    if !TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Installs (once) a panic hook that restores the terminal and then runs
/// the previous hook, so the panic message prints on a usable terminal
/// instead of being lost on the alternate screen.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| chain_panic_hook(restore_terminal));
}

/// Wraps the current panic hook: `restore` runs first, then the old hook
/// (which prints the message). Public so an integration test can check the
/// order without a terminal.
pub fn chain_panic_hook(restore: fn()) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        restore();
        previous(info);
    }));
}

/// Results from the worker threads.
enum Msg {
    Scanned {
        result: Result<Vec<Entry>, String>,
        docker_timed_out: bool,
    },
    Stopped {
        entries: Vec<Entry>,
        results: Vec<StopResult>,
        force: bool,
    },
    /// A worker panicked. The panic hook has already restored the terminal,
    /// so the loop must end.
    Crashed(&'static str),
}

/// Starts the scan worker. It owns one [`Scanner`] for the whole session,
/// so the docker cache (one `docker ps` per 5 s) carries across refreshes.
/// Each `()` sent asks for one scan.
fn spawn_scanner(tx: Sender<Msg>) -> Sender<()> {
    let (req_tx, req_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            let mut scanner = Scanner::system();
            while req_rx.recv().is_ok() {
                let (result, docker_timed_out) = match &mut scanner {
                    Ok(s) => (s.scan().map_err(|e| format!("{e:#}")), s.docker_timed_out()),
                    Err(e) => (Err(format!("{e:#}")), false),
                };
                let msg = Msg::Scanned {
                    result,
                    docker_timed_out,
                };
                if tx.send(msg).is_err() {
                    return; // the TUI has exited
                }
            }
        }));
        if outcome.is_err() {
            let _ = tx.send(Msg::Crashed("scan"));
        }
    });
    req_tx
}

/// Stops `entries` on a new thread, exactly like `whatsup kill`: the same
/// [`stop_port`], so the same PID re-verify, grace period, force, and
/// respawn check. A fresh [`Scanner`] keeps the scan worker free meanwhile.
fn spawn_stop(entries: Vec<Entry>, mode: StopMode, force: bool, tx: Sender<Msg>) {
    thread::spawn(move || {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| match Scanner::system() {
            Ok(mut scanner) => stop_port(
                &entries,
                mode,
                force,
                &mut scanner,
                &mut OsControl,
                &Timing::default(),
            ),
            Err(e) => {
                let reason = FailReason::Other(format!("{e:#}"));
                vec![StopResult::Failed { reason }; entries.len()]
            }
        }));
        let msg = match outcome {
            Ok(results) => Msg::Stopped {
                entries,
                results,
                force,
            },
            Err(_) => Msg::Crashed("stop"),
        };
        let _ = tx.send(msg);
    });
}

/// Timers the loop keeps outside [`App`] (which never reads a clock).
struct Timers {
    next_refresh: Instant,
    scan_in_flight: bool,
    /// The status generation being timed, and when it expires.
    status: Option<(u64, Instant)>,
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    theme: Theme,
) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let scan = spawn_scanner(tx.clone());
    let mut timers = Timers {
        next_refresh: Instant::now(),
        scan_in_flight: false,
        status: None,
    };
    loop {
        let now = Instant::now();
        let confirming = matches!(app.mode, Mode::Confirm(_));
        // Auto-refresh, paused while confirming so rows never shift under
        // the prompt.
        if !confirming && !timers.scan_in_flight && now >= timers.next_refresh {
            scan.send(())
                .map_err(|_| anyhow!("the scan worker stopped unexpectedly"))?;
            timers.scan_in_flight = true;
        }
        expire_status(app, &mut timers, now);

        terminal
            .draw(|f| view::render(f, app, theme))
            .context("could not draw the TUI")?;

        if event::poll(TICK).context("could not read the keyboard")?
            && let Event::Key(key) = event::read().context("could not read the keyboard")?
        {
            match on_key(app, key) {
                Effect::None => {}
                Effect::Quit => return Ok(()),
                Effect::Refresh => timers.next_refresh = Instant::now(),
                Effect::Stop(entries, mode, force) => spawn_stop(entries, mode, force, tx.clone()),
                Effect::Open(port) => {
                    let line = match open_in_browser(port) {
                        Ok(()) => {
                            StatusLine::new(Level::Info, format!("opening {}", local_url(port)))
                        }
                        Err(e) => StatusLine::new(Level::Error, format!("✗ {e}")),
                    };
                    app.set_status(vec![line]);
                }
            }
        }
        // Resize and other events need nothing: the next draw adapts.

        drain(&rx, app, &mut timers)?;
    }
}

/// Applies every worker result that has arrived.
fn drain(rx: &Receiver<Msg>, app: &mut App, timers: &mut Timers) -> anyhow::Result<()> {
    while let Ok(msg) = rx.try_recv() {
        match msg {
            Msg::Scanned {
                result,
                docker_timed_out,
            } => {
                timers.scan_in_flight = false;
                if matches!(app.mode, Mode::Confirm(_)) {
                    // Started before the prompt opened: drop it and rescan
                    // as soon as the prompt closes.
                    timers.next_refresh = Instant::now();
                } else {
                    app.on_scan(result, docker_timed_out);
                    timers.next_refresh = Instant::now() + REFRESH_EVERY;
                }
            }
            Msg::Stopped {
                entries,
                results,
                force,
            } => {
                app.on_stopped(&entries, &results, force);
                timers.next_refresh = Instant::now();
            }
            Msg::Crashed(what) => {
                return Err(anyhow!("internal error in the {what} worker (see above)"));
            }
        }
    }
    Ok(())
}

/// Clears the status 4 s after it was last set.
fn expire_status(app: &mut App, timers: &mut Timers, now: Instant) {
    if app.status().is_empty() {
        timers.status = None;
        return;
    }
    let generation = app.status_gen();
    match timers.status {
        Some((g, until)) if g == generation => {
            if now >= until {
                app.clear_status();
                timers.status = None;
            }
        }
        _ => timers.status = Some((generation, now + STATUS_FOR)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_expires_four_seconds_after_it_was_set() {
        let mut app = App::new(false);
        let mut timers = Timers {
            next_refresh: Instant::now(),
            scan_in_flight: false,
            status: None,
        };
        let t0 = Instant::now();
        app.set_status(vec![StatusLine::new(Level::Ok, "✓ done")]);
        expire_status(&mut app, &mut timers, t0);
        expire_status(&mut app, &mut timers, t0 + Duration::from_millis(3999));
        assert_eq!(app.status().len(), 1);
        // A new message restarts the clock.
        app.set_status(vec![StatusLine::new(Level::Ok, "✓ again")]);
        expire_status(&mut app, &mut timers, t0 + Duration::from_secs(3));
        expire_status(&mut app, &mut timers, t0 + Duration::from_millis(6999));
        assert_eq!(app.status().len(), 1);
        expire_status(&mut app, &mut timers, t0 + Duration::from_secs(7));
        assert!(app.status().is_empty());
    }

    #[test]
    fn scan_results_are_held_back_while_confirming() -> anyhow::Result<()> {
        let mut app = App::new(false);
        app.set_entries(vec![fixtures::vite()]);
        app.mode = Mode::Confirm(Box::new(fixtures::vite()));
        let mut timers = Timers {
            next_refresh: Instant::now() + Duration::from_secs(60),
            scan_in_flight: true,
            status: None,
        };
        let (tx, rx) = mpsc::channel();
        tx.send(Msg::Scanned {
            result: Ok(Vec::new()),
            docker_timed_out: false,
        })?;
        drain(&rx, &mut app, &mut timers)?;
        assert_eq!(app.visible().len(), 1, "rows unchanged under the prompt");
        assert!(!timers.scan_in_flight);
        assert!(
            timers.next_refresh <= Instant::now(),
            "rescan once it closes"
        );

        app.mode = Mode::Normal;
        tx.send(Msg::Scanned {
            result: Ok(Vec::new()),
            docker_timed_out: false,
        })?;
        drain(&rx, &mut app, &mut timers)?;
        assert!(app.visible().is_empty());
        assert!(timers.next_refresh > Instant::now());
        Ok(())
    }

    #[test]
    fn stop_results_show_and_trigger_a_refresh() -> anyhow::Result<()> {
        let mut app = App::new(false);
        app.busy = Some("x".to_string());
        let mut timers = Timers {
            next_refresh: Instant::now() + Duration::from_secs(60),
            scan_in_flight: false,
            status: None,
        };
        let (tx, rx) = mpsc::channel();
        tx.send(Msg::Stopped {
            entries: vec![fixtures::vite()],
            results: vec![StopResult::ForceKilled],
            force: true,
        })?;
        drain(&rx, &mut app, &mut timers)?;
        assert_eq!(app.busy, None);
        assert_eq!(
            app.status()[0].text,
            "✓ force-killed node (pid 41901) on :5173"
        );
        assert!(timers.next_refresh <= Instant::now());

        tx.send(Msg::Crashed("stop"))?;
        assert!(drain(&rx, &mut app, &mut timers).is_err());
        Ok(())
    }
}
