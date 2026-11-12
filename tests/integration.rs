//! Integration tests (DESIGN.md section 17.2): real listeners, the real
//! scanner, and the built `whatsup` binary. Run in CI on Linux, macOS and
//! Windows. Needs Python 3 on PATH (`python3`, or `python` on Windows).

use std::io::Read;
use std::net::TcpListener;
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use whatsup::model::{Entry, Exposure, Kind};
use whatsup::scan::Scanner;
use whatsup::sources::SocketSource;
use whatsup::sources::sockets::NativeSockets;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Kills and reaps the server when dropped, so it never outlives a test,
/// even when an assertion fails.
struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A port that was free a moment ago: bind to port 0, read it, release it.
fn free_port(bind: &str) -> std::io::Result<u16> {
    let l = TcpListener::bind((bind, 0))?;
    l.local_addr().map(|a| a.port())
}

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// True when the OS socket table has a TCP listener on `port`.
///
/// We check the socket table instead of connecting: on macOS runners the
/// application firewall can hold a connection to a 0.0.0.0 listener open
/// (pending approval) for minutes, while the socket is already listening.
fn is_listening(port: u16) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(NativeSockets.listeners()?.iter().any(|l| l.port == port))
}

/// Starts `python -m http.server <port> --bind <bind>` and waits until it
/// is listening. On timeout, the error says whether Python exited, what it
/// printed, and which ports the socket table did show.
fn http_server(bind: &str) -> Result<Server, Box<dyn std::error::Error>> {
    let port = free_port(bind)?;
    let child = Command::new(python())
        .args(["-m", "http.server", &port.to_string(), "--bind", bind])
        .current_dir(std::env::temp_dir())
        // Diagnostics only: lets a SIGABRT on timeout dump Python's stack.
        .env("PYTHONFAULTHANDLER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // http.server only logs requests here and we make none, so the pipe
        // can't fill up while it runs.
        .stderr(Stdio::piped())
        .spawn()?;
    let mut server = Server { child, port };
    let deadline = Instant::now() + Duration::from_secs(60);
    while !is_listening(port)? {
        let exited = server.child.try_wait()?;
        if exited.is_some() || Instant::now() > deadline {
            let mut lsof = String::new();
            if exited.is_none() {
                let pid = server.child.id().to_string();
                if cfg!(unix) {
                    // Where is it stuck? Socket states, then a stack dump.
                    if let Ok(o) = Command::new("lsof")
                        .args(["-nP", "-a", "-i", "-p", &pid])
                        .output()
                    {
                        lsof = String::from_utf8_lossy(&o.stdout).into_owned();
                    }
                    let _ = Command::new("kill").args(["-ABRT", &pid]).status();
                    sleep(Duration::from_millis(500));
                }
                let _ = server.child.kill();
            }
            let status = server.child.wait()?;
            let mut stderr = String::new();
            if let Some(mut pipe) = server.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let mut seen: Vec<u16> = NativeSockets.listeners()?.iter().map(|l| l.port).collect();
            seen.sort_unstable();
            seen.dedup();
            return Err(format!(
                "http.server on {bind}:{port} never came up (exited: {}, status {status}); \
                 stderr: {stderr}\nlsof: {lsof}\nlistening ports seen: {seen:?}",
                exited.is_some()
            )
            .into());
        }
        sleep(Duration::from_millis(100));
    }
    Ok(server)
}

fn find(entries: &[Entry], port: u16) -> Result<&Entry, String> {
    entries
        .iter()
        .find(|e| e.port == port)
        .ok_or_else(|| format!("port {port} not in scan: {entries:#?}"))
}

fn whatsup(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_whatsup"))
        .args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn scanner_sees_local_and_network_http_servers() -> TestResult {
    let local = http_server("127.0.0.1")?;
    let network = http_server("0.0.0.0")?;

    let entries = Scanner::system()?.scan()?;

    let l = find(&entries, local.port)?;
    assert_eq!(l.exposure, Exposure::Local, "{l:#?}");
    assert_eq!(l.kind, Kind::PythonHttp, "{l:#?}");
    assert_eq!(l.kind.as_str(), "python http");
    assert!(l.dev, "{l:#?}");
    assert!(l.pid.is_some(), "{l:#?}");
    assert!(l.command.contains("http.server"), "{l:#?}");

    let n = find(&entries, network.port)?;
    assert_eq!(n.exposure, Exposure::Network, "{n:#?}");
    assert_eq!(n.kind, Kind::PythonHttp, "{n:#?}");
    assert_eq!(n.addrs, ["*"], "{n:#?}");
    Ok(())
}

#[test]
fn binary_port_list_kill_round_trip() -> TestResult {
    let server = http_server("127.0.0.1")?;
    let p = server.port.to_string();

    let out = whatsup(&["port", &p])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("python http"));

    let out = whatsup(&["list", "--json"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(json["schema"], 1);
    let entries = json["entries"]
        .as_array()
        .ok_or("entries is not an array")?;
    assert!(
        entries.iter().any(|e| e["port"] == server.port
            && e["kind"] == "python http"
            && e["exposure"] == "local"),
        "{}",
        text(&out)
    );

    let out = whatsup(&["kill", &format!(":{p}"), "-y"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("stopped"));

    let out = whatsup(&["port", &p])?;
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(!is_listening(server.port)?);
    Ok(())
}

#[test]
fn kill_without_terminal_or_yes_is_a_usage_error() -> TestResult {
    let server = http_server("127.0.0.1")?;
    let out = whatsup(&["kill", &server.port.to_string()])?;
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pass -y"));
    // Nothing was stopped.
    assert!(is_listening(server.port)?);
    Ok(())
}

#[test]
fn piped_output_has_no_ansi_even_without_no_color() -> TestResult {
    let _server = http_server("127.0.0.1")?;
    let out = Command::new(env!("CARGO_BIN_EXE_whatsup"))
        .args(["list", "--all"])
        .env_remove("NO_COLOR")
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!out.stdout.contains(&0x1b), "{}", text(&out));
    Ok(())
}

#[test]
fn free_port_exits_1_and_kill_on_free_port_exits_0() -> TestResult {
    let p = free_port("127.0.0.1")?.to_string();
    let out = whatsup(&["port", &p])?;
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let out = whatsup(&["kill", &p, "-y"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(())
}

#[test]
fn bare_whatsup_lists_when_piped() -> TestResult {
    let out = whatsup(&[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    // Not a TTY: no TUI note, just the list.
    assert!(!String::from_utf8_lossy(&out.stdout).contains("TUI"));
    Ok(())
}
