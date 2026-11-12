//! Text output for the CLI (DESIGN.md section 12): the list table, the
//! `port` detail view, `kill` result lines, and JSON.
//!
//! Everything here returns a `String` and does no I/O, so formatting is unit
//! tested. Colour is decided by the caller ([`Style`]); with colour off the
//! output contains no ANSI escapes at all.

use std::fmt::Write as _;

use crate::model::{Entry, Exposure, ListOutput, StopResult};

/// Whether to emit ANSI colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
}

impl Style {
    /// Colour only when stdout is a terminal, `--no-color` was not given and
    /// `NO_COLOR` is unset or empty (no-color.org).
    pub fn detect(stdout_is_tty: bool, no_color_flag: bool, no_color_env: Option<&str>) -> Self {
        let env_off = no_color_env.is_some_and(|v| !v.is_empty());
        Self {
            color: stdout_is_tty && !no_color_flag && !env_off,
        }
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn bold(self, t: &str) -> String {
        self.paint("1", t)
    }

    fn yellow(self, t: &str) -> String {
        self.paint("33", t)
    }

    pub fn dim(self, t: &str) -> String {
        self.paint("2", t)
    }

    fn green(self, t: &str) -> String {
        self.paint("32", t)
    }

    fn red(self, t: &str) -> String {
        self.paint("31", t)
    }
}

/// `{"schema":1,"entries":[...]}`, pretty-printed.
pub fn render_json(entries: &[Entry]) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&ListOutput::new(entries))?)
}

/// Compact run time: `45s`, `12m`, `3h`, `2d`; `-` when unknown.
pub fn format_uptime(secs: Option<u64>) -> String {
    match secs {
        None => "-".to_string(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m", s / 60),
        Some(s) if s < 86_400 => format!("{}h", s / 3600),
        Some(s) => format!("{}d", s / 86_400),
    }
}

/// The PROCESS column / detail value: the name, or `(hidden)`.
pub fn process_label(e: &Entry) -> String {
    if e.process.is_empty() {
        "(hidden)".to_string()
    } else {
        e.process.clone()
    }
}

pub fn pid_label(e: &Entry) -> String {
    e.pid.map_or_else(|| "-".to_string(), |p| p.to_string())
}

pub fn exposure_label(x: Exposure) -> &'static str {
    match x {
        Exposure::Local => "local",
        Exposure::Network => "network",
    }
}

/// A hidden owner we can't name: no PID and not a container.
fn is_hidden(e: &Entry) -> bool {
    e.pid.is_none() && e.container.is_none()
}

/// Hint shown whenever an owner's PID is hidden.
pub const PID_HIDDEN_HINT: &str = "PID hidden: run with sudo to see it";

/// The list table: PORT, PID, KIND, PROCESS, EXPOSED, UPTIME, then a
/// warning line if any listener is network-exposed. `hidden_by_filter` is
/// how many rows the dev filter left out.
pub fn render_table(entries: &[Entry], hidden_by_filter: usize, style: Style) -> String {
    let mut out = String::new();
    if entries.is_empty() {
        out.push_str(if hidden_by_filter > 0 {
            "No dev servers listening.\n"
        } else {
            "Nothing is listening.\n"
        });
    } else {
        let header = ["PORT", "PID", "KIND", "PROCESS", "EXPOSED", "UPTIME"];
        let rows: Vec<[String; 6]> = entries
            .iter()
            .map(|e| {
                [
                    format!(":{}", e.port),
                    pid_label(e),
                    e.kind.to_string(),
                    process_label(e),
                    exposure_label(e.exposure).to_string(),
                    format_uptime(e.uptime_secs),
                ]
            })
            .collect();
        let mut widths = header.map(|h| h.chars().count());
        for r in &rows {
            for (w, cell) in widths.iter_mut().zip(r) {
                *w = (*w).max(cell.chars().count());
            }
        }
        let line = |cells: &[String]| -> String {
            let padded: Vec<String> = cells
                .iter()
                .zip(widths)
                .map(|(c, w)| format!("{c:<w$}"))
                .collect();
            padded.join("  ").trim_end().to_string()
        };
        let head: Vec<String> = header.iter().map(|h| h.to_string()).collect();
        let _ = writeln!(out, "{}", style.bold(&line(&head)));
        for (e, r) in entries.iter().zip(&rows) {
            let text = line(r);
            let text = if e.exposure == Exposure::Network {
                style.yellow(&text)
            } else {
                text
            };
            let _ = writeln!(out, "{text}");
        }
    }

    let exposed = entries
        .iter()
        .filter(|e| e.exposure == Exposure::Network)
        .count();
    if exposed > 0 {
        let _ = writeln!(out, "{}", style.yellow(&exposure_warning(exposed)));
    }
    if entries.iter().any(is_hidden) {
        let _ = writeln!(out, "{}", style.dim(PID_HIDDEN_HINT));
    }
    if hidden_by_filter > 0 {
        let _ = writeln!(
            out,
            "{}",
            style.dim(&format!(
                "{hidden_by_filter} more not shown: use --all to see everything"
            ))
        );
    }
    out
}

/// Shown under the `--all` table on macOS when not root (section 14):
/// libproc only shows our own processes' sockets, so other users' listeners
/// are missing entirely rather than listed with a hidden PID.
pub const MACOS_HIDDEN_NOTE: &str = "Some system listeners are hidden on macOS without sudo; run `sudo whatsup list --all` to see everything.";

/// The macOS visibility note, when it applies: macOS, not root, `--all`.
/// The caller supplies the facts (OS, euid) so this stays pure.
pub fn macos_hidden_note(is_macos: bool, is_root: bool, all: bool) -> Option<&'static str> {
    (is_macos && !is_root && all).then_some(MACOS_HIDDEN_NOTE)
}

/// The one-line exposure warning (section 10).
pub fn exposure_warning(n: usize) -> String {
    let what = if n == 1 {
        "listener is"
    } else {
        "listeners are"
    };
    format!(
        "⚠ {n} {what} exposed to the network (reachable from other machines if the firewall allows)"
    )
}

/// The `whatsup port <n>` detail view for every owner of the port.
pub fn render_detail(entries: &[Entry], style: Style) -> String {
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "{}",
            style.bold(&format!(":{}  {}  {}", e.port, e.kind, process_label(e)))
        );
        let mut field = |k: &str, v: String| {
            let _ = writeln!(out, "  {}  {v}", style.dim(&format!("{k:<9}")));
        };
        field(
            "pid",
            e.pid
                .map_or_else(|| PID_HIDDEN_HINT.to_string(), |p| p.to_string()),
        );
        if let Some(c) = &e.container {
            field("container", format!("{} ({}) id {}", c.name, c.image, c.id));
        }
        if !e.command.is_empty() {
            field("command", e.command.clone());
        }
        field("cwd", e.cwd.clone().unwrap_or_else(|| "-".to_string()));
        field("address", e.addrs.join(", "));
        let exposure = exposure_label(e.exposure);
        field(
            "exposed",
            if e.exposure == Exposure::Network {
                style.yellow(&format!("{exposure} ⚠ reachable from other machines"))
            } else {
                exposure.to_string()
            },
        );
        field("uptime", format_uptime(e.uptime_secs));
        field(
            "memory",
            e.memory_mb
                .map_or_else(|| "-".to_string(), |m| format!("{m} MB")),
        );
    }
    out
}

/// "vite (pid 41822) on :5173" / "db (postgres:16) on :5432".
pub fn describe(e: &Entry) -> String {
    let name = process_label(e);
    match e.pid {
        Some(pid) if e.container.is_none() => format!("{name} (pid {pid}) on :{}", e.port),
        _ => format!("{name} on :{}", e.port),
    }
}

/// What `kill` is about to stop, before the confirmation prompt.
pub fn render_kill_targets(entries: &[Entry]) -> String {
    let mut out = String::new();
    for e in entries {
        let how = if e.container.is_some() {
            "docker stop"
        } else {
            "stop"
        };
        let _ = writeln!(out, "  {how} {} [{}]", describe(e), e.kind);
    }
    out
}

/// One line describing the outcome of stopping `e`.
pub fn render_stop_result(e: &Entry, r: &StopResult, force: bool, style: Style) -> String {
    let what = describe(e);
    match r {
        StopResult::Stopped => format!("{} stopped {what}", style.green("✓")),
        StopResult::ForceKilled => format!("{} force-killed {what}", style.green("✓")),
        StopResult::StillRunning if force => {
            format!("{} {what} is still running after SIGKILL", style.red("✗"))
        }
        StopResult::StillRunning => format!(
            "{} {what} is still running after 3 s: retry with -f to force",
            style.yellow("!")
        ),
        StopResult::Respawned { pid, parent } => {
            let who = match (pid, parent) {
                (Some(p), Some(par)) => {
                    format!(
                        "as pid {p} by {} (pid {}): stop that to free the port",
                        par.name, par.pid
                    )
                }
                (Some(p), None) => format!("as pid {p}"),
                (None, _) => "(PID hidden: run with sudo to see it)".to_string(),
            };
            format!(
                "{} stopped {what}, but :{} is busy again: respawned {who}",
                style.yellow("!"),
                e.port
            )
        }
        StopResult::Failed { reason } => {
            format!("{} could not stop {what}: {reason}", style.red("✗"))
        }
    }
}

/// Exit code for a `kill` with several owners: 1 if any failed, else 3 if
/// any is still running or respawned, else 0 (section 12).
pub fn kill_exit_code(results: &[StopResult]) -> u8 {
    let codes: Vec<i32> = results.iter().map(StopResult::exit_code).collect();
    if codes.contains(&1) {
        1
    } else if codes.contains(&3) {
        3
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Container, FailReason, Kind, ProcRef};

    const PLAIN: Style = Style { color: false };
    const COLOR: Style = Style { color: true };

    fn vite() -> Entry {
        Entry {
            port: 5173,
            pid: Some(41822),
            process: "node".to_string(),
            kind: Kind::Vite,
            exposure: Exposure::Local,
            addrs: vec!["127.0.0.1".to_string(), "::1".to_string()],
            command: "node /w/node_modules/.bin/vite".to_string(),
            cwd: Some("/home/me/web".to_string()),
            uptime_secs: Some(7200),
            memory_mb: Some(412),
            dev: true,
            container: None,
        }
    }

    fn exposed_http() -> Entry {
        Entry {
            port: 8000,
            pid: Some(5150),
            process: "python3".to_string(),
            kind: Kind::PythonHttp,
            exposure: Exposure::Network,
            addrs: vec!["*".to_string()],
            command: "python3 -m http.server --bind 0.0.0.0".to_string(),
            cwd: None,
            uptime_secs: Some(42),
            memory_mb: None,
            dev: true,
            container: None,
        }
    }

    fn docker_db() -> Entry {
        Entry {
            port: 5432,
            pid: None,
            process: "db (postgres:16)".to_string(),
            kind: Kind::Docker,
            exposure: Exposure::Network,
            addrs: vec!["*".to_string()],
            command: String::new(),
            cwd: None,
            uptime_secs: None,
            memory_mb: None,
            dev: true,
            container: Some(Container {
                id: "3f2a9c1d7e4b".to_string(),
                name: "db".to_string(),
                image: "postgres:16".to_string(),
            }),
        }
    }

    #[test]
    fn style_detection() {
        assert!(Style::detect(true, false, None).color);
        assert!(!Style::detect(false, false, None).color, "piped");
        assert!(!Style::detect(true, true, None).color, "--no-color");
        assert!(!Style::detect(true, false, Some("1")).color, "NO_COLOR");
        assert!(Style::detect(true, false, Some("")).color, "empty NO_COLOR");
    }

    #[test]
    fn uptime_formatting() {
        assert_eq!(format_uptime(None), "-");
        assert_eq!(format_uptime(Some(0)), "0s");
        assert_eq!(format_uptime(Some(59)), "59s");
        assert_eq!(format_uptime(Some(60)), "1m");
        assert_eq!(format_uptime(Some(3599)), "59m");
        assert_eq!(format_uptime(Some(3600)), "1h");
        assert_eq!(format_uptime(Some(86_399)), "23h");
        assert_eq!(format_uptime(Some(3 * 86_400)), "3d");
    }

    #[test]
    fn table_layout() {
        let t = render_table(&[vite(), exposed_http()], 0, PLAIN);
        let expected = "\
PORT   PID    KIND         PROCESS  EXPOSED  UPTIME
:5173  41822  vite         node     local    2h
:8000  5150   python http  python3  network  42s
⚠ 1 listener is exposed to the network (reachable from other machines if the firewall allows)
";
        assert_eq!(t, expected);
    }

    #[test]
    fn table_without_exposure_has_no_warning() {
        let t = render_table(&[vite()], 0, PLAIN);
        assert!(!t.contains('⚠'));
    }

    #[test]
    fn plain_output_has_no_ansi() {
        let t = render_table(&[vite(), exposed_http(), docker_db()], 3, PLAIN);
        let d = render_detail(&[exposed_http(), docker_db()], PLAIN);
        assert!(!t.contains('\x1b') && !d.contains('\x1b'));
    }

    #[test]
    fn color_marks_exposed_rows_yellow() {
        let t = render_table(&[exposed_http()], 0, COLOR);
        assert!(t.contains("\x1b[33m:8000"));
    }

    #[test]
    fn hidden_pid_and_filter_hints() {
        let mut hidden = vite();
        hidden.pid = None;
        hidden.process = String::new();
        let t = render_table(&[hidden], 2, PLAIN);
        assert!(t.contains("(hidden)"));
        assert!(t.contains(PID_HIDDEN_HINT));
        assert!(t.contains("2 more not shown: use --all"));
        // A container row has no PID but needs no sudo hint.
        assert!(!render_table(&[docker_db()], 0, PLAIN).contains(PID_HIDDEN_HINT));
    }

    #[test]
    fn empty_table_messages() {
        assert_eq!(render_table(&[], 0, PLAIN), "Nothing is listening.\n");
        assert!(render_table(&[], 4, PLAIN).starts_with("No dev servers listening.\n"));
    }

    #[test]
    fn detail_view() {
        let d = render_detail(&[vite()], PLAIN);
        let expected = "\
:5173  vite  node
  pid        41822
  command    node /w/node_modules/.bin/vite
  cwd        /home/me/web
  address    127.0.0.1, ::1
  exposed    local
  uptime     2h
  memory     412 MB
";
        assert_eq!(d, expected);
    }

    #[test]
    fn detail_view_container_and_exposure() {
        let d = render_detail(&[docker_db()], PLAIN);
        assert!(d.contains("container  db (postgres:16) id 3f2a9c1d7e4b"));
        assert!(d.contains("network ⚠"));
        assert!(d.contains(&format!("pid        {PID_HIDDEN_HINT}")));
    }

    #[test]
    fn json_is_schema_1() -> anyhow::Result<()> {
        let v: serde_json::Value = serde_json::from_str(&render_json(&[vite()])?)?;
        assert_eq!(v["schema"], 1);
        assert_eq!(v["entries"][0]["port"], 5173);
        assert_eq!(v["entries"][0]["kind"], "vite");
        Ok(())
    }

    #[test]
    fn stop_result_lines() {
        let e = vite();
        let line = |r: StopResult, force| render_stop_result(&e, &r, force, PLAIN);
        assert_eq!(
            line(StopResult::Stopped, false),
            "✓ stopped node (pid 41822) on :5173"
        );
        assert!(line(StopResult::ForceKilled, true).starts_with("✓ force-killed"));
        assert!(line(StopResult::StillRunning, false).contains("retry with -f"));
        assert!(line(StopResult::StillRunning, true).contains("after SIGKILL"));
        let resp = line(
            StopResult::Respawned {
                pid: Some(4243),
                parent: Some(ProcRef {
                    pid: 4000,
                    name: "nodemon".to_string(),
                }),
            },
            false,
        );
        assert!(resp.contains("busy again: respawned as pid 4243 by nodemon (pid 4000)"));
        let hidden = line(
            StopResult::Respawned {
                pid: None,
                parent: None,
            },
            false,
        );
        assert!(hidden.contains("PID hidden"));
        let failed = line(
            StopResult::Failed {
                reason: FailReason::PidHidden,
            },
            false,
        );
        assert!(failed.starts_with("✗ could not stop") && failed.contains("sudo"));
    }

    #[test]
    fn kill_targets_say_how() {
        let t = render_kill_targets(&[vite(), docker_db()]);
        assert!(t.contains("stop node (pid 41822) on :5173 [vite]"));
        assert!(t.contains("docker stop db (postgres:16) on :5432 [docker]"));
    }

    #[test]
    fn macos_note_only_for_macos_non_root_with_all() {
        assert_eq!(
            macos_hidden_note(true, false, true),
            Some(MACOS_HIDDEN_NOTE)
        );
        for (is_macos, is_root, all) in [
            (true, false, false), // dev-only list: no promise of "everything"
            (true, true, true),   // root sees every socket
            (false, false, true), // Linux/Windows show hidden PIDs instead
            (false, true, false),
        ] {
            assert_eq!(
                macos_hidden_note(is_macos, is_root, all),
                None,
                "{is_macos} {is_root} {all}"
            );
        }
        assert!(MACOS_HIDDEN_NOTE.contains("sudo whatsup list --all"));
    }

    #[test]
    fn worst_exit_code_wins() {
        let failed = StopResult::Failed {
            reason: FailReason::PidReused,
        };
        assert_eq!(
            kill_exit_code(&[StopResult::Stopped, StopResult::ForceKilled]),
            0
        );
        assert_eq!(
            kill_exit_code(&[StopResult::Stopped, StopResult::StillRunning]),
            3
        );
        assert_eq!(
            kill_exit_code(&[StopResult::StillRunning, failed.clone()]),
            1
        );
        assert_eq!(kill_exit_code(&[failed, StopResult::Stopped]), 1);
        assert_eq!(kill_exit_code(&[]), 0);
    }
}
