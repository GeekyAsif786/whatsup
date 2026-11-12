//! TUI rendering (DESIGN.md section 13). Rendering only: no I/O, no
//! clocks. Everything shown comes from [`App`], so frames are snapshot-tested
//! with ratatui's `TestBackend`.
//!
//! ```text
//!  whatsup   3 listening (dev)   ⚠ 1 exposed to network          header
//! ╭────────────────────────────────────────────────────────╮
//! │  PORT    KIND          PROCESS     PID     EXPOSED UP  MEM│  table
//! │▶ :3000   next.js       node        41822   local   2h  412M│
//! ╰────────────────────────────────────────────────────────╯
//! cmd       node .../next dev                                    detail (4)
//! cwd       ~/code/web
//! addr      127.0.0.1, ::1 (local)
//! container -
//!  ↑↓ move · x stop · o open · / filter · a all/dev · r refresh · q quit
//! ```

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap,
};

use super::app::{App, Level, Mode};
use crate::classify::{KindGroup, kind_group, opens_in_browser};
use crate::model::{Entry, Exposure};
use crate::output::{describe, exposure_label, format_uptime, pid_label, process_label};

/// Smallest usable terminal (section 13).
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 12;

/// Rows of the detail pane: cmd, cwd, addr, container.
const DETAIL_HEIGHT: u16 = 4;
/// At most this many status lines; the table gives up the room.
const MAX_STATUS_LINES: u16 = 3;

/// Colour on or off (`--no-color`, `NO_COLOR`). Off means no foreground or
/// background colour at all; bold and reverse video still mark headings and
/// the selection, and every coloured fact is also written out as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub color: bool,
}

impl Theme {
    fn fg(self, c: Color) -> Style {
        if self.color {
            Style::new().fg(c)
        } else {
            Style::new()
        }
    }

    fn kind(self, e: &Entry) -> Style {
        self.fg(match kind_group(e.kind) {
            KindGroup::Frontend => Color::Green,
            KindGroup::Python => Color::Yellow,
            KindGroup::Database => Color::Magenta,
            KindGroup::Docker => Color::Cyan,
            KindGroup::Other => Color::DarkGray,
        })
    }

    fn warn(self) -> Style {
        self.fg(Color::Yellow).add_modifier(Modifier::BOLD)
    }

    fn dim(self) -> Style {
        self.fg(Color::DarkGray)
    }

    fn level(self, level: Level) -> Style {
        match level {
            Level::Info => Style::new(),
            Level::Ok => self.fg(Color::Green),
            Level::Warn => self.fg(Color::Yellow),
            Level::Error => self.fg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }
}

/// Draws one frame.
pub fn render(frame: &mut Frame, app: &App, theme: Theme) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_too_small(frame, area);
        return;
    }
    let status_lines = app.status().len() as u16;
    let footer_height = status_lines.clamp(1, MAX_STATUS_LINES);
    let [header, table, detail, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(DETAIL_HEIGHT),
        Constraint::Length(footer_height),
    ])
    .areas(area);

    let visible = app.visible();
    frame.render_widget(header_line(app, &visible, theme), header);
    render_table(frame, table, app, &visible, theme);
    frame.render_widget(detail_lines(app, theme), detail);
    render_footer(frame, footer, app, theme);
}

fn render_too_small(frame: &mut Frame, area: Rect) {
    let text = vec![
        Line::from("terminal too small"),
        Line::from(format!(
            "need {MIN_WIDTH}×{MIN_HEIGHT}, have {}×{}",
            area.width, area.height
        )),
        Line::from("q quits"),
    ];
    let top = area.height.saturating_sub(text.len() as u16) / 2;
    let inner = Rect {
        y: area.y + top,
        height: area.height - top,
        ..area
    };
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn header_line<'a>(app: &App, visible: &[&Entry], theme: Theme) -> Line<'a> {
    let mut spans = vec![Span::styled(
        " whatsup",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if !app.loaded {
        spans.push(Span::raw("   scanning…"));
        return Line::from(spans);
    }
    let scope = if app.show_all { "all" } else { "dev" };
    spans.push(Span::raw(format!(
        "   {} listening ({scope})",
        visible.len()
    )));
    if !app.filter.is_empty() && app.mode != Mode::Filter {
        spans.push(Span::styled(format!("   /{}", app.filter), theme.dim()));
    }
    let exposed = visible
        .iter()
        .filter(|e| e.exposure == Exposure::Network)
        .count();
    if exposed > 0 {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            format!("⚠ {exposed} exposed to network"),
            theme.warn(),
        ));
    }
    Line::from(spans)
}

/// `412M`, `1.5G`; `-` when unknown.
fn format_memory(mb: Option<u64>) -> String {
    match mb {
        None => "-".to_string(),
        Some(m) if m < 1024 => format!("{m}M"),
        // Display only; precision loss above 2^52 MB is irrelevant.
        Some(m) => format!("{:.1}G", m as f64 / 1024.0),
    }
}

fn render_table(frame: &mut Frame, area: Rect, app: &App, visible: &[&Entry], theme: Theme) {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let right = |t: &'static str| Cell::from(Line::from(t).alignment(Alignment::Right));
    let header = Row::new([
        Cell::from("PORT"),
        Cell::from("KIND"),
        Cell::from("PROCESS"),
        Cell::from("PID"),
        Cell::from("EXPOSED"),
        right("UP"),
        right("MEM"),
    ])
    .style(bold);
    let rows = visible.iter().map(|e| {
        let exposed = match e.exposure {
            Exposure::Network => Cell::from(exposure_label(e.exposure)).style(theme.warn()),
            Exposure::Local => Cell::from(exposure_label(e.exposure)),
        };
        Row::new([
            Cell::from(format!(":{}", e.port)),
            Cell::from(e.kind.as_str()).style(theme.kind(e)),
            Cell::from(process_label(e)),
            Cell::from(pid_label(e)),
            exposed,
            Cell::from(Line::from(format_uptime(e.uptime_secs)).alignment(Alignment::Right)),
            Cell::from(Line::from(format_memory(e.memory_mb)).alignment(Alignment::Right)),
        ])
    });
    let widths = [
        Constraint::Length(6),
        Constraint::Length(13),
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(4),
        Constraint::Length(5),
    ];
    let block = Block::bordered().border_type(BorderType::Rounded);
    let inner = block.inner(area);
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = TableState::default().with_selected(app.selected_index());
    frame.render_stateful_widget(table, area, &mut state);

    if visible.is_empty() {
        let message = empty_message(app);
        // Below the column header, inside the border.
        let below = Rect {
            y: inner.y + 2,
            height: inner.height.saturating_sub(2),
            ..inner
        };
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .style(theme.dim())
                .wrap(Wrap { trim: true }),
            below,
        );
    }
}

fn empty_message(app: &App) -> String {
    let hidden = app.hidden_by_dev();
    if !app.loaded {
        "scanning…".to_string()
    } else if !app.filter.is_empty() {
        format!("Nothing matches /{}.", app.filter)
    } else if hidden > 0 {
        format!("No dev servers listening. Press a to show all {hidden}.")
    } else {
        "Nothing is listening.".to_string()
    }
}

/// `/home/me/code` → `~/code` when `home` is `/home/me`.
fn tilde(path: &str, home: Option<&str>) -> String {
    if let Some(home) = home.filter(|h| !h.is_empty())
        && let Some(rest) = path.strip_prefix(home)
        && (rest.is_empty() || rest.starts_with(['/', '\\']))
    {
        return format!("~{rest}");
    }
    path.to_string()
}

fn detail_lines<'a>(app: &App, theme: Theme) -> Paragraph<'a> {
    let Some(e) = app.selected() else {
        return Paragraph::new("");
    };
    let label = |k: &'static str| Span::styled(format!("{k:<9} "), theme.dim());
    let cmd = if !e.command.is_empty() {
        e.command.clone()
    } else if e.pid.is_none() && e.container.is_none() {
        crate::output::PID_HIDDEN_HINT.to_string()
    } else {
        "-".to_string()
    };
    let cwd = e
        .cwd
        .as_deref()
        .map_or_else(|| "-".to_string(), |c| tilde(c, app.home.as_deref()));
    let addrs = e
        .addrs
        .iter()
        .map(|a| if a == "*" { "* (all interfaces)" } else { a })
        .collect::<Vec<_>>()
        .join(", ");
    let exposure = match e.exposure {
        Exposure::Local => Span::raw(" (local)"),
        Exposure::Network => {
            Span::styled(" (network: reachable from other machines)", theme.warn())
        }
    };
    let container = e.container.as_ref().map_or_else(
        || "-".to_string(),
        |c| format!("{} ({}) id {}", c.name, c.image, c.id),
    );
    Paragraph::new(vec![
        Line::from(vec![label("cmd"), Span::raw(cmd)]),
        Line::from(vec![label("cwd"), Span::raw(cwd)]),
        Line::from(vec![label("addr"), Span::raw(addrs), exposure]),
        Line::from(vec![label("container"), Span::raw(container)]),
    ])
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let footer: Vec<Line> = match &app.mode {
        Mode::Confirm(target) => vec![confirm_line(app, target, theme)],
        Mode::Filter => {
            let prompt = format!(" /{}", app.filter);
            // Put the terminal cursor after the typed text.
            let x = area.x + (prompt.chars().count() as u16).min(area.width.saturating_sub(1));
            frame.set_cursor_position(Position { x, y: area.y });
            vec![Line::from(vec![
                Span::raw(prompt),
                Span::styled("   Enter keep · Esc clear · ↑↓ move", theme.dim()),
            ])]
        }
        Mode::Normal if !app.status().is_empty() => app
            .status()
            .iter()
            .take(MAX_STATUS_LINES as usize)
            .map(|s| Line::styled(format!(" {}", s.text), theme.level(s.level)))
            .collect(),
        Mode::Normal => match &app.busy {
            Some(what) => vec![Line::styled(format!(" stopping {what}…"), theme.dim())],
            None => vec![hints(app, theme)],
        },
    };
    frame.render_widget(Paragraph::new(footer), area);
}

fn hints<'a>(app: &App, theme: Theme) -> Line<'a> {
    let selected = app.selected();
    let mut keys: Vec<&str> = vec!["↑↓ move"];
    if selected.is_some() {
        keys.push("x stop");
    }
    if selected.is_some_and(opens_in_browser) {
        keys.push("o open");
    }
    keys.extend(["/ filter", "a all/dev", "r refresh", "q quit"]);
    Line::styled(format!(" {}", keys.join(" · ")), theme.dim())
}

fn confirm_line<'a>(app: &App, target: &Entry, theme: Theme) -> Line<'a> {
    let owners = app.owners(target.port);
    let question = match owners.as_slice() {
        [] | [_] => {
            let verb = if target.container.is_some() {
                "docker stop"
            } else {
                "Stop"
            };
            format!(" {verb} {}?", describe(target))
        }
        many => {
            let who: Vec<String> = many
                .iter()
                .map(|e| format!("{} pid {}", process_label(e), pid_label(e)))
                .collect();
            format!(
                " Stop all {} owners of :{} ({})?",
                many.len(),
                target.port,
                who.join(", ")
            )
        }
    };
    Line::from(vec![
        Span::styled(question, theme.warn()),
        Span::raw("  y stop · Y force · any other key cancels"),
    ])
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::tui::app::{StatusLine, on_key};
    use crate::tui::fixtures::{entries, nextjs, vite, worker};

    const COLOR: Theme = Theme { color: true };
    const NO_COLOR: Theme = Theme { color: false };

    fn draw(app: &App, theme: Theme, w: u16, h: u16) -> Buffer {
        let mut terminal = match Terminal::new(TestBackend::new(w, h)) {
            Ok(t) => t,
            Err(e) => panic!("test backend: {e}"),
        };
        if let Err(e) = terminal.draw(|f| render(f, app, theme)) {
            panic!("draw: {e}");
        }
        terminal.backend().buffer().clone()
    }

    /// The frame as plain text, one line per row, trailing spaces trimmed.
    fn text(buf: &Buffer) -> String {
        let w = buf.area.width as usize;
        buf.content
            .chunks(w)
            .map(|row| {
                let line: String = row.iter().map(|c| c.symbol()).collect();
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app_with(entries: Vec<Entry>) -> App {
        let mut app = App::new(false);
        app.home = Some("/home/me".to_string());
        app.set_entries(entries);
        app
    }

    fn key(app: &mut App, code: KeyCode) {
        on_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn normal_list() {
        let app = app_with(entries());
        assert_snapshot!(text(&draw(&app, COLOR, 80, 14)));
    }

    #[test]
    fn normal_list_colours() {
        // Styles only for the frame above: kind groups, exposed warning,
        // reverse-video selection.
        let buf = draw(&app_with(entries()), COLOR, 80, 14);
        let at = |x: u16, y: u16| buf[(x, y)].style();
        // Row 3 is :3000 next.js (selected), row 6 is :8000 fastapi.
        assert_eq!(at(10, 3).fg, Some(Color::Green));
        assert!(at(10, 3).add_modifier.contains(Modifier::REVERSED));
        assert_eq!(at(10, 4).fg, Some(Color::Green));
        assert_eq!(at(10, 5).fg, Some(Color::Cyan));
        assert_eq!(at(10, 6).fg, Some(Color::Yellow));
        let header = text(&buf);
        let warn_x = header.lines().next().and_then(|l| l.find('⚠'));
        assert!(warn_x.is_some());
    }

    fn first_line(app: &App) -> String {
        text(&draw(app, COLOR, 80, 14))
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    }

    fn last_line(app: &App, w: u16) -> String {
        text(&draw(app, COLOR, w, 14))
            .lines()
            .last()
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn exposed_header() {
        let mut app = app_with(entries());
        assert_snapshot!(text(&draw(&app, COLOR, 80, 14)));
        assert_eq!(
            first_line(&app),
            " whatsup   4 listening (dev)   ⚠ 2 exposed to network"
        );
        key(&mut app, KeyCode::Char('a'));
        assert_eq!(
            first_line(&app),
            " whatsup   5 listening (all)   ⚠ 3 exposed to network"
        );
        let local = app_with(vec![nextjs(), vite()]);
        assert_eq!(first_line(&local), " whatsup   2 listening (dev)");
    }

    #[test]
    fn empty_list() {
        let mut app = app_with(vec![crate::tui::fixtures::spotify()]);
        assert_snapshot!(text(&draw(&app, COLOR, 70, 12)));
        app = app_with(Vec::new());
        assert_snapshot!("empty_list_nothing", text(&draw(&app, COLOR, 70, 12)));
        let scanning = App::new(false);
        assert_snapshot!("empty_list_scanning", text(&draw(&scanning, COLOR, 70, 12)));
    }

    #[test]
    fn filter_mode() {
        let mut app = app_with(entries());
        key(&mut app, KeyCode::Char('/'));
        for c in "node".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert_snapshot!(text(&draw(&app, COLOR, 80, 14)));
        // Enter keeps the filter: it moves to the header.
        key(&mut app, KeyCode::Enter);
        assert_snapshot!("filter_kept", text(&draw(&app, COLOR, 80, 14)));
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Char('z'));
        assert_snapshot!("filter_no_match", text(&draw(&app, COLOR, 80, 14)));
    }

    #[test]
    fn confirm_prompt_single_owner() {
        let mut app = app_with(entries());
        key(&mut app, KeyCode::Char('x'));
        assert_snapshot!(text(&draw(&app, COLOR, 100, 14)));
    }

    #[test]
    fn confirm_prompt_container() {
        let mut app = app_with(entries());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Char('x'));
        assert_eq!(
            last_line(&app, 100),
            " docker stop db (postgres:16) on :5432?  y stop · Y force · any other key cancels"
        );
    }

    #[test]
    fn confirm_prompt_multi_owner() {
        let mut app = app_with(vec![vite(), worker(5173, 41950), nextjs()]);
        key(&mut app, KeyCode::Char('x'));
        assert_snapshot!(text(&draw(&app, COLOR, 100, 14)));
    }

    #[test]
    fn status_lines_and_busy() {
        let mut app = app_with(entries());
        app.busy = Some("node (pid 41822) on :3000".to_string());
        assert_eq!(last_line(&app, 80), " stopping node (pid 41822) on :3000…");
        app.set_status(vec![
            StatusLine::new(Level::Ok, "✓ stopped node (pid 41822) on :3000"),
            StatusLine::new(Level::Warn, "! stopped node (pid 41901) on :3000, but :3000 is busy again: respawned as pid 7 by nodemon (pid 6): stop that to free the port"),
            StatusLine::new(Level::Error, "✗ could not stop node (pid 41950) on :3000: permission denied"),
        ]);
        assert_snapshot!(text(&draw(&app, COLOR, 80, 14)));
    }

    #[test]
    fn open_hint_hidden_for_non_http() {
        let mut app = app_with(entries());
        let last = |app: &App| last_line(app, 80);
        assert!(last(&app).contains("o open"));
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.selected().map(|e| e.port), Some(5432));
        assert!(!last(&app).contains("o open"), "{}", last(&app));
        assert!(last(&app).contains("x stop"));
    }

    #[test]
    fn terminal_too_small() {
        let app = app_with(entries());
        assert_snapshot!(text(&draw(&app, COLOR, 59, 12)));
        let short = text(&draw(&app, COLOR, 80, 11));
        assert!(short.contains("terminal too small"), "{short}");
        assert!(short.contains("need 60×12, have 80×11"), "{short}");
        let ok = text(&draw(&app, COLOR, 60, 12));
        assert!(!ok.contains("terminal too small"), "{ok}");
    }

    #[test]
    fn no_color() {
        let mut app = app_with(entries());
        key(&mut app, KeyCode::Char('a'));
        app.set_status(vec![StatusLine::new(
            Level::Error,
            "✗ could not stop: permission denied",
        )]);
        let buf = draw(&app, NO_COLOR, 80, 14);
        for cell in &buf.content {
            assert_eq!(cell.fg, Color::Reset, "{cell:?}");
            assert_eq!(cell.bg, Color::Reset, "{cell:?}");
        }
        // The same facts are still there as text.
        assert_snapshot!(text(&buf));
    }

    #[test]
    fn memory_and_tilde_formatting() {
        assert_eq!(format_memory(None), "-");
        assert_eq!(format_memory(Some(0)), "0M");
        assert_eq!(format_memory(Some(1023)), "1023M");
        assert_eq!(format_memory(Some(1536)), "1.5G");
        assert_eq!(tilde("/home/me/code", Some("/home/me")), "~/code");
        assert_eq!(tilde("/home/me", Some("/home/me")), "~");
        assert_eq!(tilde("/home/meg/x", Some("/home/me")), "/home/meg/x");
        assert_eq!(tilde("C:\\Users\\me\\x", Some("C:\\Users\\me")), "~\\x");
        assert_eq!(tilde("/x", Some("")), "/x");
        assert_eq!(tilde("/x", None), "/x");
    }
}
