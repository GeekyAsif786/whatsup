//! SIGTERM / SIGHUP restore the terminal (DESIGN.md section 13): the real
//! `whatsup` binary runs its TUI in a pseudo-terminal, gets a signal, and
//! must leave the alternate screen, turn raw mode off and exit 0 (the quit
//! flag path) rather than die from the signal.
//!
//! Unix only: Windows has no SIGTERM/SIGHUP and no pty in std; there the
//! flag path is covered by the unit test in `src/tui/mod.rs`.
#![cfg(unix)]

use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{LocalFlags, tcgetattr};
use nix::unistd::Pid;

const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Waits until `cond` holds on the captured output, or panics after 15 s.
fn wait_for(out: &Mutex<Vec<u8>>, what: &str, cond: impl Fn(&[u8]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        {
            let buf = out.lock().unwrap_or_else(|p| p.into_inner());
            if cond(&buf) {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}; output: {:?}",
                    String::from_utf8_lossy(&buf)
                );
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("whatsup did not exit after the signal");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn raw_mode_is_off(slave: &OwnedFd) -> bool {
    let t = tcgetattr(slave).expect("tcgetattr");
    t.local_flags
        .contains(LocalFlags::ICANON | LocalFlags::ECHO)
}

fn signal_restores_terminal(sig: Signal) {
    let size = Winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(&size, None).expect("openpty");
    assert!(raw_mode_is_off(&pty.slave), "pty starts in cooked mode");

    let stdio = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup slave"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_whatsup"))
        .env("TERM", "xterm-256color")
        .stdin(stdio(&pty.slave))
        .stdout(stdio(&pty.slave))
        .stderr(stdio(&pty.slave))
        // Its own process group, so the signal goes to whatsup alone.
        .process_group(0)
        .spawn()
        .expect("spawn whatsup");

    // Drain the master so the child never blocks on a full pty buffer.
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut master = std::fs::File::from(pty.master);
    let sink = Arc::clone(&out);
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap_or_else(|p| p.into_inner())
                .extend_from_slice(&buf[..n]);
        }
    });

    wait_for(&out, "the alternate screen", |b| contains(b, ENTER_ALT));
    // Raw mode is switched on before the alternate screen is entered.
    assert!(!raw_mode_is_off(&pty.slave), "TUI put the pty in raw mode");

    let pid = Pid::from_raw(i32::try_from(child.id()).expect("pid fits i32"));
    kill(pid, sig).expect("send signal");
    let status = wait_exit(&mut child);

    assert_eq!(
        status.code(),
        Some(0),
        "{sig}: exited via the quit flag, not the signal"
    );
    wait_for(&out, "leaving the alternate screen", |b| {
        contains(b, LEAVE_ALT)
    });
    assert!(raw_mode_is_off(&pty.slave), "{sig}: raw mode turned off");
}

#[test]
fn sigterm_restores_the_terminal() {
    signal_restores_terminal(Signal::SIGTERM);
}

#[test]
fn sighup_restores_the_terminal() {
    signal_restores_terminal(Signal::SIGHUP);
}
