//! `whatsup` binary: CLI parsing, dispatch and exit codes (DESIGN.md
//! section 12). All logic lives in the library; this file wires it to the
//! terminal.

use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use whatsup::actions::{OsControl, Timing, stop_port};
use whatsup::model::{Entry, StopMode};
use whatsup::output::{self, Style};
use whatsup::scan::Scanner;
use whatsup::sources::docker::DockerTimeout;

/// See what is listening on your dev machine: ports, dev servers, databases
/// and containers.
#[derive(Debug, Parser)]
#[command(name = "whatsup", version)]
struct Cli {
    /// Show every listener, not only dev servers
    #[arg(long, global = true)]
    all: bool,
    /// Disable colour (also respects the NO_COLOR environment variable)
    #[arg(long, global = true)]
    no_color: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// List listening ports (dev servers only, unless --all)
    #[command(visible_alias = "ls")]
    List {
        /// Print {"schema": 1, "entries": [...]} instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Show what is on a port. Exit code: 0 = in use, 1 = free
    Port {
        /// Port number, e.g. 3000 or :3000
        #[arg(value_parser = parse_port)]
        port: u16,
    },
    /// Stop what is on a port (asks first).
    ///
    /// Exit code: 0 = stopped or nothing there, 1 = failed or declined,
    /// 2 = no terminal to confirm in (pass -y), 3 = still running or respawned.
    /// On Windows the process is terminated immediately: there is no
    /// graceful stop signal.
    Kill {
        /// Port number, e.g. 3000 or :3000
        #[arg(value_parser = parse_port)]
        port: u16,
        /// Don't ask for confirmation
        #[arg(short = 'y', long = "yes")]
        yes: bool,
        /// Kill (SIGKILL) if it is still running after the 3 s grace period
        #[arg(short, long)]
        force: bool,
    },
}

/// Accepts `3000` or `:3000`; port 0 is not a port you can look up.
fn parse_port(s: &str) -> Result<u16, String> {
    let digits = s.strip_prefix(':').unwrap_or(s);
    match digits.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!("'{s}' is not a port (1-65535, e.g. 3000 or :3000)")),
        Ok(p) => Ok(p),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout_tty = io::stdout().is_terminal();
    let style = Style::detect(
        stdout_tty,
        cli.no_color,
        std::env::var("NO_COLOR").ok().as_deref(),
    );
    match run(cli, style, stdout_tty) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("whatsup: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli, style: Style, stdout_tty: bool) -> anyhow::Result<u8> {
    match cli.command {
        None => {
            if stdout_tty {
                println!("(The interactive TUI arrives in the next version; showing the list.)\n");
            }
            list(cli.all, false, style)
        }
        Some(Cmd::List { json }) => list(cli.all, json, style),
        Some(Cmd::Port { port }) => port_detail(port, style),
        Some(Cmd::Kill { port, yes, force }) => kill(port, yes, force, style),
    }
}

/// Scans, then prints the docker-timeout note (section 7.3) to stderr.
fn scan_with_notes(scanner: &mut Scanner) -> anyhow::Result<Vec<Entry>> {
    let entries = scanner.scan()?;
    if scanner.docker_timed_out() {
        eprintln!("{DockerTimeout}");
    }
    Ok(entries)
}

fn list(all: bool, json: bool, style: Style) -> anyhow::Result<u8> {
    let entries = scan_with_notes(&mut Scanner::system()?)?;
    let total = entries.len();
    let shown: Vec<Entry> = entries.into_iter().filter(|e| all || e.dev).collect();
    if json {
        println!("{}", output::render_json(&shown)?);
    } else {
        print!(
            "{}",
            output::render_table(&shown, total - shown.len(), style)
        );
        let is_macos = cfg!(target_os = "macos");
        if let Some(note) = output::macos_hidden_note(is_macos, is_root(), all) {
            println!("{}", style.dim(note));
        }
    }
    Ok(0)
}

/// Whether whatsup runs with root privileges (effective uid 0).
#[cfg(unix)]
fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

fn on_port(scanner: &mut Scanner, port: u16) -> anyhow::Result<Vec<Entry>> {
    Ok(scanner
        .scan()?
        .into_iter()
        .filter(|e| e.port == port)
        .collect())
}

fn port_detail(port: u16, style: Style) -> anyhow::Result<u8> {
    let entries: Vec<Entry> = scan_with_notes(&mut Scanner::system()?)?
        .into_iter()
        .filter(|e| e.port == port)
        .collect();
    if entries.is_empty() {
        println!(":{port} is free");
        return Ok(1);
    }
    print!("{}", output::render_detail(&entries, style));
    Ok(0)
}

fn kill(port: u16, yes: bool, force: bool, style: Style) -> anyhow::Result<u8> {
    let mut scanner = Scanner::system()?;
    let entries = on_port(&mut scanner, port)?;
    if entries.is_empty() {
        println!("nothing is listening on :{port}");
        return Ok(0);
    }
    if !yes {
        if !io::stdin().is_terminal() {
            eprintln!("not a terminal: pass -y to confirm");
            return Ok(2);
        }
        print!("{}", output::render_kill_targets(&entries));
        if !confirm(entries.len())? {
            println!("aborted, nothing stopped");
            return Ok(1);
        }
    }
    let results = stop_port(
        &entries,
        StopMode::Listener,
        force,
        &mut scanner,
        &mut OsControl,
        &Timing::default(),
    );
    for (e, r) in entries.iter().zip(&results) {
        println!("{}", output::render_stop_result(e, r, force, style));
    }
    Ok(output::kill_exit_code(&results))
}

/// Asks `[y/N]` on the terminal. Ctrl-C exits 130; EOF counts as "no".
fn confirm(n: usize) -> anyhow::Result<bool> {
    // If the handler can't be installed, the default SIGINT behaviour
    // (terminate) still applies, so ignoring the error is safe.
    let _ = ctrlc::set_handler(|| {
        eprintln!();
        std::process::exit(130);
    });
    let question = if n == 1 {
        "Stop it?".to_string()
    } else {
        format!("Stop all {n}?")
    };
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn ports_accept_colon_prefix() {
        assert_eq!(parse_port("3000"), Ok(3000));
        assert_eq!(parse_port(":5173"), Ok(5173));
        assert_eq!(parse_port("65535"), Ok(65535));
        for bad in ["0", ":0", "65536", "abc", "", ":", "-1", "::80"] {
            assert!(parse_port(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_commands_and_global_flags() -> Result<(), clap::Error> {
        let c = Cli::try_parse_from(["whatsup", "ls", "--json", "--all"])?;
        assert!(c.all);
        assert!(matches!(c.command, Some(Cmd::List { json: true })));

        let c = Cli::try_parse_from(["whatsup", "--no-color", "kill", ":3000", "-y", "-f"])?;
        assert!(c.no_color);
        assert!(matches!(
            c.command,
            Some(Cmd::Kill {
                port: 3000,
                yes: true,
                force: true
            })
        ));

        let c = Cli::try_parse_from(["whatsup"])?;
        assert!(c.command.is_none());
        Ok(())
    }

    #[test]
    fn usage_errors_exit_2() {
        for args in [
            vec!["whatsup", "port"],
            vec!["whatsup", "port", "http"],
            vec!["whatsup", "nope"],
        ] {
            let err = Cli::try_parse_from(&args).err();
            assert_eq!(err.map(|e| e.exit_code()), Some(2), "{args:?}");
        }
    }
}
