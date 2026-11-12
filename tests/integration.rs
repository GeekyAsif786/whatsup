//! Integration tests (DESIGN.md section 17.2): real listeners, the real
//! scanner, and the built `whatsup` binary. Run in CI on Linux, macOS and
//! Windows.
//!
//! Most tests use a tiny Rust listener (see [`listener_helper`]): this test
//! binary re-runs itself to bind a socket and wait. It is listening in
//! milliseconds. Python's `http.server` takes ~35 s to start on macOS
//! runners, so only one test uses it, to prove the "python http"
//! classification on a real `python -m http.server` (needs Python 3 on
//! PATH: `python3`, or `python` on Windows).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

use whatsup::model::{Entry, Exposure, Kind};
use whatsup::scan::Scanner;
use whatsup::sources::SocketSource;
use whatsup::sources::sockets::NativeSockets;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Set to `addr` (e.g. `127.0.0.1`) to turn [`listener_helper`] into a
/// listener process.
const LISTEN_ENV: &str = "WHATSUP_TEST_LISTEN";
/// What the helper prints once it is listening, followed by the port.
const READY: &str = "whatsup-test-listening-on=";
/// The helper exits on its own after this long, in case a test runner dies
/// without killing it.
const HELPER_LIFETIME: Duration = Duration::from_secs(120);

/// Not a test: when [`LISTEN_ENV`] is set, binds a TCP listener on that
/// address (port 0: the OS picks a free one), prints [`READY`] and the port,
/// and accepts connections until killed. Without the variable it does
/// nothing and passes.
#[test]
fn listener_helper() {
    let Ok(addr) = std::env::var(LISTEN_ENV) else {
        return;
    };
    let listener = match TcpListener::bind((addr.as_str(), 0)) {
        Ok(l) => l,
        Err(e) => panic!("helper could not bind {addr}: {e}"),
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => panic!("helper has no local address: {e}"),
    };
    // bind() has already called listen(): the port is in LISTEN now.
    let mut out = std::io::stdout();
    let _ = writeln!(out, "\n{READY}{port}");
    let _ = out.flush();
    std::thread::spawn(|| {
        sleep(HELPER_LIFETIME);
        std::process::exit(0);
    });
    for conn in listener.incoming() {
        drop(conn);
    }
}

/// A listener process for a test. Kills and reaps it when dropped, so it
/// never outlives the test, even when an assertion fails.
struct Server {
    child: Child,
    port: u16,
    /// Kept open so the child never writes to a closed pipe.
    _stdout: Option<BufReader<ChildStdout>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts the Rust listener on `addr`; returns once it is listening.
fn listener(addr: &str) -> Result<Server, Box<dyn std::error::Error>> {
    let child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "listener_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LISTEN_ENV, addr)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut server = Server {
        child,
        port: 0,
        _stdout: None,
    };
    let stdout = server.child.stdout.take().ok_or("helper has no stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(format!("listener helper on {addr} exited before listening").into());
        }
        if let Some(port) = line.trim().strip_prefix(READY) {
            server.port = port.parse()?;
            server._stdout = Some(reader);
            return Ok(server);
        }
    }
}

/// Ports already handed out in this run, so parallel tests never pick the
/// same "free" port while a slow server is still starting.
static RESERVED: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// A port that was free a moment ago and that no other test in this run
/// will get: bind to port 0, read it, release it.
fn free_port(bind: &str) -> std::io::Result<u16> {
    loop {
        let port = TcpListener::bind((bind, 0))?.local_addr()?.port();
        // A test that panicked while holding the lock leaves the list intact.
        let mut reserved = RESERVED.lock().unwrap_or_else(|p| p.into_inner());
        if !reserved.contains(&port) {
            reserved.push(port);
            return Ok(port);
        }
    }
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
    let mut server = Server {
        child,
        port,
        _stdout: None,
    };
    let started = Instant::now();
    let deadline = started + Duration::from_secs(60);
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
    let took = started.elapsed();
    if took > Duration::from_secs(5) {
        // Written straight to stderr (not eprintln!) so it shows in CI logs
        // even when the test passes.
        let _ = writeln!(
            std::io::stderr(),
            "[slow] http.server on {bind}:{port} took {took:?} to listen"
        );
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

/// The one Python test: a real `python -m http.server` is classified as
/// "python http", counts as dev, and shows its command.
#[test]
fn python_http_server_is_classified() -> TestResult {
    let server = http_server("127.0.0.1")?;
    let entries = Scanner::system()?.scan()?;
    let e = find(&entries, server.port)?;
    assert_eq!(e.kind, Kind::PythonHttp, "{e:#?}");
    assert_eq!(e.kind.as_str(), "python http");
    assert_eq!(e.exposure, Exposure::Local, "{e:#?}");
    assert!(e.dev, "{e:#?}");
    assert_eq!(e.pid, Some(server.child.id()), "{e:#?}");
    assert!(e.command.contains("http.server"), "{e:#?}");

    let out = whatsup(&["port", &server.port.to_string()])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("python http"));
    Ok(())
}

#[test]
fn scanner_sees_local_and_network_listeners() -> TestResult {
    let local = listener("127.0.0.1")?;
    let network = listener("0.0.0.0")?;

    let entries = Scanner::system()?.scan()?;

    let l = find(&entries, local.port)?;
    assert_eq!(l.exposure, Exposure::Local, "{l:#?}");
    assert_eq!(l.addrs, ["127.0.0.1"], "{l:#?}");
    assert_eq!(l.pid, Some(local.child.id()), "{l:#?}");

    let n = find(&entries, network.port)?;
    assert_eq!(n.exposure, Exposure::Network, "{n:#?}");
    assert_eq!(n.addrs, ["*"], "{n:#?}");
    assert_eq!(n.pid, Some(network.child.id()), "{n:#?}");
    Ok(())
}

#[test]
fn binary_port_list_kill_round_trip() -> TestResult {
    let server = listener("127.0.0.1")?;
    let p = server.port.to_string();

    let out = whatsup(&["port", &p])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains(&format!(":{p}")));

    // The helper is an unrecognised process on an ephemeral port, so it is
    // not "dev": it needs --all.
    let out = whatsup(&["list", "--json", "--all"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(json["schema"], 1);
    let entries = json["entries"]
        .as_array()
        .ok_or("entries is not an array")?;
    assert!(
        entries.iter().any(|e| e["port"] == server.port
            && e["pid"] == server.child.id()
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
    let server = listener("127.0.0.1")?;
    let out = whatsup(&["kill", &server.port.to_string()])?;
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pass -y"));
    // Nothing was stopped.
    assert!(is_listening(server.port)?);
    Ok(())
}

#[test]
fn piped_output_has_no_ansi_even_without_no_color() -> TestResult {
    let server = listener("0.0.0.0")?;
    let out = Command::new(env!("CARGO_BIN_EXE_whatsup"))
        .args(["list", "--all"])
        .env_remove("NO_COLOR")
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The exposed listener is in the table and triggers the (yellow when
    // on a terminal) warning line.
    assert!(
        stdout.contains(&format!(":{}", server.port)),
        "{}",
        text(&out)
    );
    assert!(stdout.contains("exposed to the network"), "{}", text(&out));
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
    let server = listener("127.0.0.1")?;
    let out = Command::new(env!("CARGO_BIN_EXE_whatsup"))
        .arg("--all")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    // Not a TTY: the plain table, no TUI (no alternate screen, no escapes).
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("PORT"), "{}", text(&out));
    assert!(
        stdout.contains(&format!(":{}", server.port)),
        "{}",
        text(&out)
    );
    assert!(!out.stdout.contains(&0x1b), "{}", text(&out));
    Ok(())
}
