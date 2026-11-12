//! TUI state machine (DESIGN.md section 13): Normal ⇄ Filter, Normal →
//! Confirm → Normal.
//!
//! [`on_key`] is pure: it updates [`App`] and returns an [`Effect`] for the
//! event loop to carry out (scan, stop, open a browser, quit). Nothing here
//! performs I/O or reads a clock, so key sequences are unit-tested without a
//! terminal.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::classify::opens_in_browser;
use crate::filter::matches_query;
use crate::model::{Entry, StopMode, StopResult};
use crate::output::{Style, describe, render_stop_result};

/// What the keyboard is driving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Moving the selection; single-key commands.
    Normal,
    /// Typing the `/` filter.
    Filter,
    /// Asking `y` / `Y` / anything else about stopping this entry's port.
    Confirm(Box<Entry>),
}

/// A side effect for the event loop to perform after a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Scan now.
    Refresh,
    /// Stop every owner of one port, like `whatsup kill` (`bool` = force).
    Stop(Vec<Entry>, StopMode, bool),
    /// Open `http://localhost:<port>`.
    Open(u16),
    Quit,
}

/// How a status line is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Error,
}

/// One line of the footer status area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub level: Level,
    pub text: String,
}

impl StatusLine {
    pub fn new(level: Level, text: impl Into<String>) -> Self {
        Self {
            level,
            text: text.into(),
        }
    }
}

/// Selection identity across refreshes (section 13): `(port, pid)`.
type Key = (u16, Option<u32>);

fn key_of(e: &Entry) -> Key {
    (e.port, e.pid)
}

/// Everything the view needs to draw a frame.
#[derive(Debug, Clone)]
pub struct App {
    /// Every listener from the last scan, dev or not, in scan order.
    entries: Vec<Entry>,
    /// Index into [`App::visible`]; meaningless when that is empty.
    selected: usize,
    /// Footer status lines; the loop clears them 4 s after `status_gen` changes.
    status: Vec<StatusLine>,
    status_gen: u64,
    /// `--all` / `a`: show every listener, not only dev servers.
    pub show_all: bool,
    /// The `/` filter text (kept after Enter, cleared by Esc).
    pub filter: String,
    pub mode: Mode,
    /// False until the first scan arrives.
    pub loaded: bool,
    /// Set while a stop runs in the background: what is being stopped.
    pub busy: Option<String>,
    /// The user's home directory, to show cwd as `~/...`.
    pub home: Option<String>,
    /// Shown when switching to "all" (macOS without root, section 14).
    pub all_note: Option<&'static str>,
    /// Whether the last scan reported a docker timeout (to notify once).
    docker_timed_out: bool,
}

impl App {
    pub fn new(show_all: bool) -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            status: Vec::new(),
            status_gen: 0,
            show_all,
            filter: String::new(),
            mode: Mode::Normal,
            loaded: false,
            busy: None,
            home: None,
            all_note: None,
            docker_timed_out: false,
        }
    }

    /// Rows on screen: dev (or all) entries that match the filter.
    pub fn visible(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| (self.show_all || e.dev) && matches_query(e, &self.filter))
            .collect()
    }

    /// Entries left out by the dev-only view (before the text filter).
    pub fn hidden_by_dev(&self) -> usize {
        if self.show_all {
            0
        } else {
            self.entries.iter().filter(|e| !e.dev).count()
        }
    }

    /// Position of the selection in [`App::visible`], if anything is shown.
    pub fn selected_index(&self) -> Option<usize> {
        let n = self.visible().len();
        (n > 0).then(|| self.selected.min(n - 1))
    }

    pub fn selected(&self) -> Option<&Entry> {
        let visible = self.visible();
        let i = self.selected_index()?;
        visible.get(i).copied()
    }

    /// Every owner of `port` from the last scan, whatever the view shows:
    /// what `whatsup kill <port>` would stop.
    pub fn owners(&self, port: u16) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.port == port).collect()
    }

    pub fn status(&self) -> &[StatusLine] {
        &self.status
    }

    /// Changes every time the status is set, so the loop can time it out.
    pub fn status_gen(&self) -> u64 {
        self.status_gen
    }

    pub fn set_status(&mut self, lines: Vec<StatusLine>) {
        self.status = lines;
        self.status_gen += 1;
    }

    pub fn clear_status(&mut self) {
        self.status.clear();
    }

    /// Replaces the entries with a new scan, keeping the selection on the
    /// same `(port, pid)` if it is still there, else on the same row number.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        let key = self.selected().map(key_of);
        self.entries = entries;
        self.loaded = true;
        self.reselect(key);
    }

    /// Applies a scan result from the loop. Errors and a new docker timeout
    /// become status lines; a failed scan keeps the previous rows.
    pub fn on_scan(&mut self, result: Result<Vec<Entry>, String>, docker_timed_out: bool) {
        match result {
            Ok(entries) => self.set_entries(entries),
            Err(e) => self.set_status(vec![StatusLine::new(
                Level::Error,
                format!("✗ scan failed: {e}"),
            )]),
        }
        if docker_timed_out && !self.docker_timed_out {
            self.set_status(vec![StatusLine::new(
                Level::Warn,
                crate::sources::docker::DockerTimeout.to_string(),
            )]);
        }
        self.docker_timed_out = docker_timed_out;
    }

    /// Shows the outcome of a stop, one line per owner, worded like `kill`.
    pub fn on_stopped(&mut self, entries: &[Entry], results: &[StopResult], force: bool) {
        self.busy = None;
        let lines = entries
            .iter()
            .zip(results)
            .map(|(e, r)| {
                let level = match r {
                    StopResult::Stopped | StopResult::ForceKilled => Level::Ok,
                    StopResult::StillRunning | StopResult::Respawned { .. } => Level::Warn,
                    StopResult::Failed { .. } => Level::Error,
                };
                StatusLine::new(
                    level,
                    render_stop_result(e, r, force, Style { color: false }),
                )
            })
            .collect();
        self.set_status(lines);
    }

    /// Puts the selection back on `key` if visible, else clamps the row.
    fn reselect(&mut self, key: Option<Key>) {
        let visible = self.visible();
        if let Some(i) = key.and_then(|k| visible.iter().position(|e| key_of(e) == k)) {
            self.selected = i;
        } else {
            self.selected = self.selected.min(visible.len().saturating_sub(1));
        }
    }

    /// Moves the selection by `delta` rows, wrapping at both ends.
    fn move_by(&mut self, delta: isize) {
        let n = self.visible().len();
        if n == 0 {
            return;
        }
        let cur = self.selected.min(n - 1);
        // `n` is a row count, far below isize::MAX.
        self.selected = (cur as isize + delta).rem_euclid(n as isize) as usize;
    }
}

/// Handles one key press. Pure: never performs I/O.
pub fn on_key(app: &mut App, key: KeyEvent) -> Effect {
    // Windows reports key releases too; act on presses (and auto-repeat).
    if key.kind == KeyEventKind::Release {
        return Effect::None;
    }
    // Ctrl-C quits from every mode (in Confirm it also cancels the stop).
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Effect::Quit;
    }
    match app.mode {
        Mode::Normal => normal_key(app, key),
        Mode::Filter => filter_key(app, key),
        Mode::Confirm(_) => confirm_key(app, key),
    }
}

/// True for Ctrl/Alt chords, which Normal and Filter mode ignore.
fn is_chord(key: &KeyEvent) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn normal_key(app: &mut App, key: KeyEvent) -> Effect {
    if is_chord(&key) {
        return Effect::None;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.move_by(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_by(1),
        KeyCode::Char('x') | KeyCode::Delete => {
            if let Some(what) = &app.busy {
                let msg = format!("still stopping {what}: wait for it to finish");
                app.set_status(vec![StatusLine::new(Level::Warn, msg)]);
            } else if let Some(e) = app.selected().cloned() {
                app.clear_status();
                app.mode = Mode::Confirm(Box::new(e));
            }
        }
        KeyCode::Char('o') | KeyCode::Enter => {
            if let Some(e) = app.selected() {
                if opens_in_browser(e) {
                    return Effect::Open(e.port);
                }
                let msg = format!("{} on :{} doesn't serve web pages", e.kind, e.port);
                app.set_status(vec![StatusLine::new(Level::Info, msg)]);
            }
        }
        KeyCode::Char('/') => app.mode = Mode::Filter,
        KeyCode::Char('a') => {
            let key = app.selected().map(key_of);
            app.show_all = !app.show_all;
            app.reselect(key);
            if app.show_all
                && let Some(note) = app.all_note
            {
                app.set_status(vec![StatusLine::new(Level::Info, note)]);
            }
        }
        KeyCode::Char('r') => return Effect::Refresh,
        KeyCode::Char('q') | KeyCode::Esc => return Effect::Quit,
        _ => {}
    }
    Effect::None
}

fn filter_key(app: &mut App, key: KeyEvent) -> Effect {
    let selected = app.selected().map(key_of);
    match key.code {
        KeyCode::Esc => {
            app.filter.clear();
            app.mode = Mode::Normal;
        }
        KeyCode::Enter => app.mode = Mode::Normal,
        KeyCode::Backspace => {
            app.filter.pop();
        }
        KeyCode::Up => {
            app.move_by(-1);
            return Effect::None;
        }
        KeyCode::Down => {
            app.move_by(1);
            return Effect::None;
        }
        KeyCode::Char(c) if !is_chord(&key) => app.filter.push(c),
        _ => return Effect::None,
    }
    // The text changed: keep the same row selected if it still matches.
    app.reselect(selected);
    Effect::None
}

fn confirm_key(app: &mut App, key: KeyEvent) -> Effect {
    let Mode::Confirm(target) = std::mem::replace(&mut app.mode, Mode::Normal) else {
        return Effect::None;
    };
    let force = match key.code {
        KeyCode::Char('y') => false,
        KeyCode::Char('Y') => true,
        _ => {
            app.set_status(vec![StatusLine::new(
                Level::Info,
                "cancelled: nothing stopped",
            )]);
            return Effect::None;
        }
    };
    let mut owners: Vec<Entry> = app.owners(target.port).into_iter().cloned().collect();
    if owners.is_empty() {
        owners.push(*target);
    }
    app.busy = Some(match owners.as_slice() {
        [one] => describe(one),
        many => format!("{} owners of :{}", many.len(), many[0].port),
    });
    Effect::Stop(owners, StopMode::Listener, force)
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyEventState;

    use super::*;
    use crate::model::{FailReason, ProcRef};
    use crate::tui::fixtures::{entries, postgres_container, vite, worker};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        press(KeyCode::Char(c))
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Sends keys and returns the last effect.
    fn keys(app: &mut App, keys: &[KeyEvent]) -> Effect {
        keys.iter().fold(Effect::None, |_, k| on_key(app, *k))
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            assert_eq!(on_key(app, ch(c)), Effect::None);
        }
    }

    fn loaded() -> App {
        let mut app = App::new(false);
        app.set_entries(entries());
        app
    }

    fn ports(app: &App) -> Vec<u16> {
        app.visible().iter().map(|e| e.port).collect()
    }

    fn selected_port(app: &App) -> Option<u16> {
        app.selected().map(|e| e.port)
    }

    #[test]
    fn dev_view_hides_noise_until_a() {
        let mut app = loaded();
        assert_eq!(ports(&app), [3000, 5173, 5432, 8000]);
        assert_eq!(app.hidden_by_dev(), 1);
        assert_eq!(on_key(&mut app, ch('a')), Effect::None);
        assert!(app.show_all);
        assert_eq!(ports(&app), [3000, 5173, 5432, 8000, 57621]);
        assert_eq!(app.hidden_by_dev(), 0);
        on_key(&mut app, ch('a'));
        assert!(!app.show_all);
        assert_eq!(ports(&app), [3000, 5173, 5432, 8000]);
    }

    #[test]
    fn toggling_all_keeps_the_selection_and_shows_the_macos_note() {
        let mut app = loaded();
        app.all_note = Some("note");
        keys(&mut app, &[ch('j'), ch('j'), ch('j')]);
        assert_eq!(selected_port(&app), Some(8000));
        on_key(&mut app, ch('a'));
        assert_eq!(selected_port(&app), Some(8000));
        assert_eq!(app.status(), [StatusLine::new(Level::Info, "note")]);
        // Back to dev with the noise row selected: clamps to the last row.
        keys(&mut app, &[ch('j')]);
        assert_eq!(selected_port(&app), Some(57621));
        on_key(&mut app, ch('a'));
        assert_eq!(selected_port(&app), Some(8000));
    }

    #[test]
    fn moves_and_wraps_with_arrows_and_jk() {
        let mut app = loaded();
        assert_eq!(selected_port(&app), Some(3000));
        on_key(&mut app, press(KeyCode::Down));
        assert_eq!(selected_port(&app), Some(5173));
        on_key(&mut app, ch('j'));
        assert_eq!(selected_port(&app), Some(5432));
        keys(&mut app, &[ch('j'), ch('j')]);
        assert_eq!(selected_port(&app), Some(3000), "wraps at the bottom");
        on_key(&mut app, ch('k'));
        assert_eq!(selected_port(&app), Some(8000), "wraps at the top");
        on_key(&mut app, press(KeyCode::Up));
        assert_eq!(selected_port(&app), Some(5432));
    }

    #[test]
    fn moving_an_empty_list_is_harmless() {
        let mut app = App::new(false);
        for k in [ch('j'), ch('k'), ch('x'), ch('o'), press(KeyCode::Enter)] {
            assert_eq!(on_key(&mut app, k), Effect::None);
        }
        assert_eq!(app.selected(), None);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn filter_typing_backspace_enter_esc() {
        let mut app = loaded();
        on_key(&mut app, ch('/'));
        assert_eq!(app.mode, Mode::Filter);
        // q, j, k, a, x are text in filter mode, not commands.
        typed(&mut app, "pyx");
        assert_eq!(app.filter, "pyx");
        assert_eq!(ports(&app), Vec::<u16>::new());
        on_key(&mut app, press(KeyCode::Backspace));
        assert_eq!(app.filter, "py");
        assert_eq!(ports(&app), [8000]);
        assert_eq!(selected_port(&app), Some(8000));

        // Enter keeps the filter and returns to Normal.
        on_key(&mut app, press(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.filter, "py");
        assert_eq!(ports(&app), [8000]);

        // Editing again starts from the kept text; Esc clears it.
        on_key(&mut app, ch('/'));
        typed(&mut app, "thon");
        assert_eq!(app.filter, "python");
        on_key(&mut app, press(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.filter, "");
        assert_eq!(ports(&app), [3000, 5173, 5432, 8000]);
    }

    #[test]
    fn filter_mode_letters_are_text_and_backspace_on_empty_is_harmless() {
        let mut app = loaded();
        on_key(&mut app, ch('/'));
        on_key(&mut app, press(KeyCode::Backspace));
        assert_eq!(app.filter, "");
        typed(&mut app, "q");
        assert_eq!(app.mode, Mode::Filter);
        // Ctrl chords are not typed.
        assert_eq!(
            on_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)
            ),
            Effect::None
        );
        assert_eq!(app.filter, "q");
        // Shifted letters are typed as given.
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT),
        );
        assert_eq!(app.filter, "qQ");
    }

    #[test]
    fn filter_keeps_selection_by_identity() {
        let mut app = loaded();
        keys(&mut app, &[ch('j')]);
        assert_eq!(selected_port(&app), Some(5173));
        keys(&mut app, &[ch('/'), ch('5')]);
        assert_eq!(ports(&app), [5173, 5432]);
        assert_eq!(selected_port(&app), Some(5173));
        // Arrows still move while typing.
        on_key(&mut app, press(KeyCode::Down));
        assert_eq!(selected_port(&app), Some(5432));
    }

    #[test]
    fn x_confirm_y_stops_every_owner_of_the_port() {
        let mut app = App::new(false);
        app.set_entries(vec![vite(), worker(5173, 41900), postgres_container()]);
        on_key(&mut app, ch('x'));
        assert_eq!(app.mode, Mode::Confirm(Box::new(vite())));
        let effect = on_key(&mut app, ch('y'));
        assert_eq!(
            effect,
            Effect::Stop(vec![vite(), worker(5173, 41900)], StopMode::Listener, false)
        );
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.busy.as_deref(), Some("2 owners of :5173"));
    }

    #[test]
    fn shift_y_forces_and_delete_also_asks() {
        let mut app = loaded();
        on_key(&mut app, press(KeyCode::Delete));
        assert!(matches!(app.mode, Mode::Confirm(_)));
        let effect = on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT),
        );
        let Effect::Stop(targets, StopMode::Listener, true) = effect else {
            panic!("expected a forced stop, got {effect:?}");
        };
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].port, 3000);
        assert_eq!(app.busy.as_deref(), Some("node (pid 41822) on :3000"));
    }

    #[test]
    fn any_other_key_cancels_the_confirm() {
        for k in [
            ch('n'),
            ch('x'),
            ch('q'),
            press(KeyCode::Enter),
            press(KeyCode::Esc),
            press(KeyCode::Down),
            KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
        ] {
            let mut app = loaded();
            on_key(&mut app, ch('x'));
            assert_eq!(on_key(&mut app, k), Effect::None, "{k:?}");
            assert_eq!(app.mode, Mode::Normal, "{k:?}");
            assert_eq!(app.busy, None);
            assert_eq!(app.status()[0].text, "cancelled: nothing stopped");
            // The next key is a normal command again.
            assert_eq!(on_key(&mut app, ch('q')), Effect::Quit);
        }
    }

    #[test]
    fn x_while_a_stop_runs_only_warns() {
        let mut app = loaded();
        keys(&mut app, &[ch('x'), ch('y')]);
        assert!(app.busy.is_some());
        assert_eq!(on_key(&mut app, ch('x')), Effect::None);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.status()[0].level, Level::Warn);
    }

    #[test]
    fn quit_keys() {
        for k in [ch('q'), press(KeyCode::Esc), ctrl('c')] {
            assert_eq!(on_key(&mut loaded(), k), Effect::Quit, "{k:?}");
        }
        // Ctrl-C quits from Filter and Confirm too; q and Esc don't.
        let mut app = loaded();
        on_key(&mut app, ch('/'));
        assert_eq!(on_key(&mut app, ch('q')), Effect::None);
        assert_eq!(on_key(&mut app, press(KeyCode::Esc)), Effect::None);
        on_key(&mut app, ch('/'));
        assert_eq!(on_key(&mut app, ctrl('c')), Effect::Quit);
        let mut app = loaded();
        on_key(&mut app, ch('x'));
        assert_eq!(on_key(&mut app, ctrl('c')), Effect::Quit);
    }

    #[test]
    fn r_refreshes_and_o_or_enter_opens_http_only() {
        let mut app = loaded();
        assert_eq!(on_key(&mut app, ch('r')), Effect::Refresh);
        assert_eq!(on_key(&mut app, ch('o')), Effect::Open(3000));
        assert_eq!(on_key(&mut app, press(KeyCode::Enter)), Effect::Open(3000));
        // 5432 is a postgres container: no browser, a hint instead.
        keys(&mut app, &[ch('j'), ch('j')]);
        assert_eq!(selected_port(&app), Some(5432));
        assert_eq!(on_key(&mut app, ch('o')), Effect::None);
        assert_eq!(
            app.status()[0].text,
            "docker on :5432 doesn't serve web pages"
        );
    }

    #[test]
    fn key_releases_are_ignored() {
        let mut app = loaded();
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
            ..ch('q')
        };
        assert_eq!(on_key(&mut app, release), Effect::None);
        let repeat = KeyEvent {
            kind: KeyEventKind::Repeat,
            ..ch('j')
        };
        on_key(&mut app, repeat);
        assert_eq!(selected_port(&app), Some(5173));
    }

    #[test]
    fn refresh_keeps_selection_by_port_and_pid() {
        let mut app = loaded();
        keys(&mut app, &[ch('j'), ch('j'), ch('j')]);
        assert_eq!(selected_port(&app), Some(8000));
        // A new row sorts in above: the selection follows 8000.
        let mut next = entries();
        next.insert(0, worker(1234, 7));
        app.set_entries(next);
        assert_eq!(selected_port(&app), Some(8000));
        // Same port, new pid (a restart): not the same row, so keep the row
        // number instead.
        let mut next = entries();
        next.insert(0, worker(1234, 7));
        for e in &mut next {
            if e.port == 8000 {
                e.pid = Some(99);
            }
        }
        let before = app.selected_index();
        app.set_entries(next);
        assert_eq!(app.selected_index(), before);
        // Rows vanish: clamp to the last one.
        app.set_entries(vec![vite()]);
        assert_eq!(selected_port(&app), Some(5173));
    }

    #[test]
    fn scan_results_errors_and_docker_timeout() {
        let mut app = App::new(false);
        assert!(!app.loaded);
        app.on_scan(Ok(entries()), false);
        assert!(app.loaded);
        assert!(app.status().is_empty());
        app.on_scan(Err("boom".to_string()), false);
        assert_eq!(app.visible().len(), 4, "keeps the previous rows");
        assert_eq!(app.status()[0].level, Level::Error);
        assert!(app.status()[0].text.contains("boom"));

        app.clear_status();
        app.on_scan(Ok(entries()), true);
        assert_eq!(app.status()[0].level, Level::Warn);
        let generation = app.status_gen();
        app.on_scan(Ok(entries()), true);
        assert_eq!(app.status_gen(), generation, "notified once per timeout");
    }

    #[test]
    fn stop_results_become_status_lines() {
        let mut app = loaded();
        app.busy = Some("x".to_string());
        let targets = [vite(), worker(5173, 41900), postgres_container()];
        let results = [
            StopResult::Stopped,
            StopResult::Respawned {
                pid: Some(5),
                parent: Some(ProcRef {
                    pid: 4,
                    name: "nodemon".to_string(),
                }),
            },
            StopResult::Failed {
                reason: FailReason::PermissionDenied,
            },
        ];
        let generation = app.status_gen();
        app.on_stopped(&targets, &results, false);
        assert_eq!(app.busy, None);
        assert!(app.status_gen() > generation);
        let levels: Vec<Level> = app.status().iter().map(|s| s.level).collect();
        assert_eq!(levels, [Level::Ok, Level::Warn, Level::Error]);
        let text: Vec<&str> = app.status().iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text[0], "✓ stopped node (pid 41901) on :5173");
        assert!(text[1].contains("respawned as pid 5 by nodemon (pid 4)"));
        assert!(text[2].contains("permission denied"));
        assert!(!text.iter().any(|t| t.contains('\x1b')));
    }
}
