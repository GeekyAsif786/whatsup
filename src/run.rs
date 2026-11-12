//! Running external commands with a deadline (DESIGN.md sections 7.3, 11.1).
//!
//! whatsup must never hang because a tool it calls hangs (Docker Desktop on
//! macOS is the usual suspect). [`run_with_timeout`] spawns the command with
//! stdin closed, drains stdout and stderr on their own threads (so a chatty
//! child can never block on a full pipe), polls for exit, and on timeout
//! kills the child and reaps it so no zombie is left behind.

use std::fmt;
use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How often to check whether the child has exited.
const POLL: Duration = Duration::from_millis(10);

/// After the child exits, how long to wait for its output to be drained.
/// Bounded because a grandchild that inherited the pipes could keep them
/// open indefinitely.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// Why [`run_with_timeout`] produced no output.
#[derive(Debug)]
pub enum RunError {
    /// The command did not finish in time. It was killed and reaped.
    Timeout(Duration),
    /// The command could not be started (not installed, no permission) or
    /// waited on.
    Io(std::io::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Timeout(d) => write!(f, "timed out after {}", human(*d)),
            RunError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RunError {}

/// `2 s` for whole seconds, `200 ms` otherwise.
pub fn human(d: Duration) -> String {
    if d.subsec_nanos() == 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{} ms", d.as_millis())
    }
}

/// Runs `cmd` (stdin closed, stdout/stderr captured) and returns its output,
/// or [`RunError::Timeout`] if it is still running after `timeout`.
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output, RunError> {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Io)?;
    wait_with_timeout(child, timeout)
}

/// Which pipe a reader thread drained.
enum Pipe {
    Stdout,
    Stderr,
}

/// Reads `source` to the end on a new thread and sends the bytes on `tx`.
fn drain<R: Read + Send + 'static>(mut source: R, pipe: Pipe, tx: mpsc::Sender<(Pipe, Vec<u8>)>) {
    thread::spawn(move || {
        let mut buf = Vec::new();
        // A read error just means we got less output; never fatal here.
        let _ = source.read_to_end(&mut buf);
        // The receiver may have given up already; that's fine.
        let _ = tx.send((pipe, buf));
    });
}

fn kill_and_reap(child: &mut Child) {
    // kill fails if it already exited; wait still reaps it.
    let _ = child.kill();
    let _ = child.wait();
}

/// The waiting half of [`run_with_timeout`], split out so tests can spawn
/// the child themselves and know its PID.
fn wait_with_timeout(mut child: Child, timeout: Duration) -> Result<Output, RunError> {
    let deadline = Instant::now() + timeout;

    let (tx, rx) = mpsc::channel();
    if let Some(out) = child.stdout.take() {
        drain(out, Pipe::Stdout, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        drain(err, Pipe::Stderr, tx.clone());
    }
    // Only the reader threads hold senders now: when both finish, the
    // channel disconnects.
    drop(tx);

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_and_reap(&mut child);
                return Err(RunError::Io(e));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            kill_and_reap(&mut child);
            return Err(RunError::Timeout(timeout));
        }
        thread::sleep(POLL.min(deadline - now));
    };

    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    let drain_deadline = Instant::now() + DRAIN_GRACE;
    for _ in 0..2 {
        let left = drain_deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((Pipe::Stdout, bytes)) => output.stdout = bytes,
            Ok((Pipe::Stderr, bytes)) => output.stderr = bytes,
            // Timed out or both readers done: return what we have.
            Err(_) => break,
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child that runs ~5 s and prints nothing: `sleep 5`, or on Windows
    /// `ping -n 6 127.0.0.1` (one ping per second).
    fn slow_command() -> Command {
        if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "6", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("5");
            c
        }
    }

    /// True once `pid` is completely gone: not running and not a zombie.
    fn is_reaped(pid: u32) -> bool {
        if cfg!(unix) {
            // `ps` prints "Z" for a zombie and nothing for a reaped PID.
            Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim().is_empty())
        } else {
            use crate::sources::ProcessSource;
            crate::sources::processes::SysinfoProcesses::new()
                .and_then(|mut s| s.processes(&[pid]))
                .is_ok_and(|m| m.is_empty())
        }
    }

    #[test]
    fn slow_command_times_out_quickly() {
        let started = Instant::now();
        let r = run_with_timeout(&mut slow_command(), Duration::from_millis(200));
        assert!(
            matches!(r, Err(RunError::Timeout(d)) if d == Duration::from_millis(200)),
            "{r:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn timed_out_child_is_killed_and_reaped() -> std::io::Result<()> {
        let child = slow_command()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let pid = child.id();
        let r = wait_with_timeout(child, Duration::from_millis(200));
        assert!(matches!(r, Err(RunError::Timeout(_))), "{r:?}");
        // Windows may list an exited process for a moment; Unix reaping is
        // immediate, but poll the same way on every OS.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !is_reaped(pid) {
            assert!(Instant::now() < deadline, "pid {pid} left behind");
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    #[test]
    fn collects_stdout_and_status() -> Result<(), RunError> {
        let out = run_with_timeout(
            Command::new("rustc").arg("--version"),
            Duration::from_secs(30),
        )?;
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("rustc "));
        Ok(())
    }

    #[test]
    fn collects_stderr_of_a_failing_command() -> Result<(), RunError> {
        let out = run_with_timeout(
            Command::new("rustc").arg("--no-such-flag"),
            Duration::from_secs(30),
        )?;
        assert!(!out.status.success());
        assert!(!out.stderr.is_empty());
        Ok(())
    }

    /// 1 MB is far more than a pipe buffer (64 KiB on Linux): without the
    /// reader threads the child would block forever on a full pipe.
    #[test]
    fn large_output_does_not_deadlock() -> Result<(), RunError> {
        let python = if cfg!(windows) { "python" } else { "python3" };
        let out = run_with_timeout(
            Command::new(python).args(["-c", "import sys; sys.stdout.write('x' * 1000000)"]),
            Duration::from_secs(30),
        )?;
        assert!(out.status.success(), "{out:?}");
        assert_eq!(out.stdout.len(), 1_000_000);
        Ok(())
    }

    #[test]
    fn missing_program_is_an_io_error() {
        let r = run_with_timeout(
            &mut Command::new("whatsup-no-such-program-4242"),
            Duration::from_secs(1),
        );
        assert!(matches!(r, Err(RunError::Io(_))), "{r:?}");
    }

    #[test]
    fn messages() {
        assert_eq!(human(Duration::from_secs(2)), "2 s");
        assert_eq!(human(Duration::from_millis(200)), "200 ms");
        assert_eq!(
            RunError::Timeout(Duration::from_secs(15)).to_string(),
            "timed out after 15 s"
        );
    }
}
