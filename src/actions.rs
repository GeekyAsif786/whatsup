//! Actions: stopping what owns a port (DESIGN.md sections 11.1, 11.2, 11.5).
//!
//! The algorithm talks to the OS only through the [`Scanner`]'s sources and
//! the [`Control`] trait (signals, `docker stop`, sleeping), so PID reuse,
//! grace periods and respawns are unit-tested with fakes and a fake clock.

use std::process::{Command, Stdio};
use std::time::Duration;

use crate::model::{Entry, FailReason, ProcRef, StopMode, StopResult};
use crate::scan::Scanner;

/// Timings from section 11.1 / 11.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// How often to check whether the process is gone.
    pub poll: Duration,
    /// How long to wait after the graceful stop.
    pub grace: Duration,
    /// How long to wait after SIGKILL before re-checking.
    pub kill_recheck: Duration,
    /// How long after a successful stop to re-scan for a respawn.
    pub respawn_delay: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(100),
            grace: Duration::from_secs(3),
            kill_recheck: Duration::from_millis(300),
            respawn_delay: Duration::from_millis(1500),
        }
    }
}

/// Why a signal could not be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalError {
    /// The process belongs to another user (EPERM / ERROR_ACCESS_DENIED).
    PermissionDenied,
    /// The process no longer exists (ESRCH).
    NoSuchProcess,
    Other(String),
}

/// Side effects used by [`stop`]. The real implementation is [`OsControl`].
pub trait Control {
    /// Graceful stop: SIGTERM on Unix, TerminateProcess on Windows (which has
    /// no graceful signal for console processes).
    fn terminate(&mut self, pid: u32) -> Result<(), SignalError>;
    /// Forced stop: SIGKILL on Unix, TerminateProcess on Windows.
    fn kill(&mut self, pid: u32) -> Result<(), SignalError>;
    /// Runs `docker stop <id>`; the error is docker's stderr.
    fn docker_stop(&mut self, id: &str) -> Result<(), String>;
    fn sleep(&mut self, d: Duration);
}

/// Stops one entry: [`stop_once`], then the respawn check (section 11.2).
pub fn stop(
    entry: &Entry,
    mode: StopMode,
    force: bool,
    scanner: &mut Scanner,
    ctl: &mut dyn Control,
    timing: &Timing,
) -> StopResult {
    stop_port(
        std::slice::from_ref(entry),
        mode,
        force,
        scanner,
        ctl,
        timing,
    )
    .pop()
    .unwrap_or(StopResult::Stopped)
}

/// Stops every owner of one port, in order, then checks once for a respawn.
///
/// One result per entry. The respawn check runs once after all owners are
/// handled and ignores every original PID, so owner B still being up while
/// A is checked is never mistaken for a respawn. If the port is busy again
/// with a new owner, every successful process stop becomes `Respawned`.
/// An owner that already exited before its turn (e.g. a worker that went
/// down with its master) counts as `Stopped`.
pub fn stop_port(
    entries: &[Entry],
    mode: StopMode,
    force: bool,
    scanner: &mut Scanner,
    ctl: &mut dyn Control,
    timing: &Timing,
) -> Vec<StopResult> {
    let mut results: Vec<StopResult> = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let r = match e.pid {
            Some(pid) if i > 0 && e.container.is_none() && is_gone(scanner, pid) => {
                StopResult::Stopped
            }
            _ => stop_once(e, mode, force, scanner, ctl, timing),
        };
        results.push(r);
    }

    let stopped_processes = entries
        .iter()
        .zip(&results)
        .any(|(e, r)| e.container.is_none() && is_success(r));
    let Some(port) = entries.first().map(|e| e.port) else {
        return results;
    };
    if !stopped_processes {
        return results;
    }
    let old: Vec<u32> = entries.iter().filter_map(|e| e.pid).collect();
    if let Some(respawn) = check_respawn(port, &old, scanner, ctl, timing) {
        for (e, r) in entries.iter().zip(results.iter_mut()) {
            if e.container.is_none() && is_success(r) {
                *r = respawn.clone();
            }
        }
    }
    results
}

fn is_success(r: &StopResult) -> bool {
    matches!(r, StopResult::Stopped | StopResult::ForceKilled)
}

/// The section 11.1 algorithm for one entry, without the respawn check.
pub fn stop_once(
    entry: &Entry,
    mode: StopMode,
    force: bool,
    scanner: &mut Scanner,
    ctl: &mut dyn Control,
    timing: &Timing,
) -> StopResult {
    let failed = |reason| StopResult::Failed { reason };

    if let Some(c) = &entry.container {
        return match ctl.docker_stop(&c.id) {
            Ok(()) => StopResult::Stopped,
            Err(stderr) => failed(FailReason::Other(stderr)),
        };
    }
    let Some(pid) = entry.pid else {
        return failed(FailReason::PidHidden);
    };
    if mode == StopMode::Tree {
        return failed(FailReason::Other(
            "tree stop is not available yet (planned for v0.2)".to_string(),
        ));
    }

    // PID-reuse race (section 16): re-verify right before signalling.
    match owns_port(scanner, pid, entry.port) {
        Ok(true) => {}
        Ok(false) => return failed(FailReason::PidReused),
        Err(e) => return failed(FailReason::Other(format!("{e:#}"))),
    }

    match ctl.terminate(pid) {
        Ok(()) => {}
        Err(SignalError::NoSuchProcess) => return StopResult::Stopped,
        Err(SignalError::PermissionDenied) => return failed(FailReason::PermissionDenied),
        Err(SignalError::Other(m)) => return failed(FailReason::Other(m)),
    }

    if wait_gone(scanner, ctl, pid, timing) {
        return StopResult::Stopped;
    }
    if !force {
        return StopResult::StillRunning;
    }

    match ctl.kill(pid) {
        Ok(()) => {}
        // It exited between the last poll and the kill: the TERM worked.
        Err(SignalError::NoSuchProcess) => return StopResult::Stopped,
        Err(SignalError::PermissionDenied) => return failed(FailReason::PermissionDenied),
        Err(SignalError::Other(m)) => return failed(FailReason::Other(m)),
    }
    ctl.sleep(timing.kill_recheck);
    if is_gone(scanner, pid) {
        StopResult::ForceKilled
    } else {
        StopResult::StillRunning
    }
}

/// True when `pid` still has a listening socket on `port`.
fn owns_port(scanner: &mut Scanner, pid: u32, port: u16) -> anyhow::Result<bool> {
    Ok(scanner
        .sockets
        .listeners()?
        .iter()
        .any(|l| l.port == port && l.pid == Some(pid)))
}

/// True when the process has exited. The process source leaves out exited
/// and zombie processes. A lookup error counts as "still there": we never
/// report a stop we did not see.
fn is_gone(scanner: &mut Scanner, pid: u32) -> bool {
    scanner
        .processes
        .processes(&[pid])
        .is_ok_and(|m| !m.contains_key(&pid))
}

/// Polls every `timing.poll` for up to `timing.grace`.
fn wait_gone(scanner: &mut Scanner, ctl: &mut dyn Control, pid: u32, timing: &Timing) -> bool {
    let polls = timing.grace.as_millis() / timing.poll.as_millis().max(1);
    for _ in 0..polls {
        if is_gone(scanner, pid) {
            return true;
        }
        ctl.sleep(timing.poll);
    }
    is_gone(scanner, pid)
}

/// Section 11.2 (v0.1): after `respawn_delay`, is the port busy again with a
/// PID that is not one we just stopped? Returns `Respawned` if so.
fn check_respawn(
    port: u16,
    old: &[u32],
    scanner: &mut Scanner,
    ctl: &mut dyn Control,
    timing: &Timing,
) -> Option<StopResult> {
    ctl.sleep(timing.respawn_delay);
    // If the socket table can't be read, we can't claim a respawn.
    let listeners = scanner.sockets.listeners().ok()?;
    let new_owners: Vec<Option<u32>> = listeners
        .iter()
        .filter(|l| l.port == port && !l.pid.is_some_and(|p| old.contains(&p)))
        .map(|l| l.pid)
        .collect();
    if new_owners.is_empty() {
        return None;
    }
    // Prefer a visible PID; fall back to a hidden owner.
    let pid = new_owners.iter().flatten().next().copied();
    let parent = pid.and_then(|p| parent_of(scanner, p));
    Some(StopResult::Respawned { pid, parent })
}

/// The nearest ancestor of `pid`, if the OS shows it.
fn parent_of(scanner: &mut Scanner, pid: u32) -> Option<ProcRef> {
    let ppid = scanner
        .processes
        .processes(&[pid])
        .ok()?
        .get(&pid)?
        .parent?;
    let info = scanner.processes.processes(&[ppid]).ok()?.remove(&ppid)?;
    Some(ProcRef {
        pid: ppid,
        name: info.name,
    })
}

/// The real [`Control`]: OS signals, the docker CLI and a real sleep.
#[derive(Debug, Default)]
pub struct OsControl;

impl Control for OsControl {
    fn terminate(&mut self, pid: u32) -> Result<(), SignalError> {
        os::terminate(pid)
    }

    fn kill(&mut self, pid: u32) -> Result<(), SignalError> {
        os::kill(pid)
    }

    fn docker_stop(&mut self, id: &str) -> Result<(), String> {
        // `id` comes from docker itself and is passed as one argument, never
        // through a shell (section 16).
        let out = Command::new("docker")
            .args(["stop", id])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run docker: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("docker stop failed ({})", out.status)
        } else {
            stderr
        })
    }

    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

#[cfg(unix)]
mod os {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill as send};
    use nix::unistd::Pid;

    use super::SignalError;

    fn signal(pid: u32, sig: Signal) -> Result<(), SignalError> {
        // pid_t is i32; 0 and negatives mean process groups, never send those.
        let raw = i32::try_from(pid)
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| SignalError::Other(format!("invalid pid {pid}")))?;
        send(Pid::from_raw(raw), sig).map_err(|e| match e {
            Errno::EPERM => SignalError::PermissionDenied,
            Errno::ESRCH => SignalError::NoSuchProcess,
            other => SignalError::Other(format!("could not signal pid {pid}: {other}")),
        })
    }

    pub fn terminate(pid: u32) -> Result<(), SignalError> {
        signal(pid, Signal::SIGTERM)
    }

    pub fn kill(pid: u32) -> Result<(), SignalError> {
        signal(pid, Signal::SIGKILL)
    }
}

#[cfg(windows)]
mod os {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    use super::SignalError;

    fn last_error(pid: u32) -> SignalError {
        let err = std::io::Error::last_os_error();
        match err.raw_os_error().map(|c| c as u32) {
            Some(ERROR_ACCESS_DENIED) => SignalError::PermissionDenied,
            Some(ERROR_INVALID_PARAMETER) => SignalError::NoSuchProcess,
            _ => SignalError::Other(format!("could not stop pid {pid}: {err}")),
        }
    }

    /// Windows has no SIGTERM for arbitrary processes: both stops terminate.
    pub fn terminate(pid: u32) -> Result<(), SignalError> {
        // SAFETY: OpenProcess has no memory-safety preconditions; a null
        // handle signals failure and is checked before use.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            return Err(last_error(pid));
        }
        // SAFETY: `handle` is a valid process handle opened above with
        // PROCESS_TERMINATE access, and is closed exactly once below.
        let ok = unsafe { TerminateProcess(handle, 1) };
        let result = if ok == 0 {
            Err(last_error(pid))
        } else {
            Ok(())
        };
        // SAFETY: as above; the handle is not used after this call.
        unsafe { CloseHandle(handle) };
        result
    }

    pub fn kill(pid: u32) -> Result<(), SignalError> {
        terminate(pid)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::rc::Rc;

    use super::*;
    use crate::model::{Container, Exposure, Kind};
    use crate::sources::fake::FakeContainers;
    use crate::sources::{Listener, ProcInfo, ProcessSource, SocketSource};

    const PORT: u16 = 5173;
    const LO: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// How a fake process reacts to SIGTERM.
    #[derive(Clone, Copy)]
    enum OnTerm {
        /// Exits this long after SIGTERM.
        ExitAfter(Duration),
        /// Ignores SIGTERM; SIGKILL stops it.
        Ignore,
        /// Ignores SIGTERM and SIGKILL (stuck in uninterruptible sleep).
        Unkillable,
    }

    #[derive(Clone, Copy, Debug)]
    enum Event {
        Exit(u32),
        /// A new owner takes the port: `pid` (None = hidden) under `parent`.
        Spawn {
            pid: Option<u32>,
            parent: Option<u32>,
        },
    }

    /// A tiny simulated OS whose clock only moves when `sleep` is called.
    struct World {
        now: Duration,
        procs: HashMap<u32, ProcInfo>,
        listeners: Vec<Listener>,
        on_term: HashMap<u32, OnTerm>,
        /// Extra PIDs that exit together with the key PID (forked workers).
        takes_down: HashMap<u32, Vec<u32>>,
        term_error: Option<SignalError>,
        events: Vec<(Duration, Event)>,
        /// After the first exit, schedule this spawn `delay` later.
        respawn: Option<(Duration, Event)>,
        signals: Vec<(u32, &'static str, Duration)>,
        docker_calls: Vec<String>,
        docker_result: Result<(), String>,
    }

    impl World {
        fn new() -> Self {
            Self {
                now: Duration::ZERO,
                procs: HashMap::new(),
                listeners: Vec::new(),
                on_term: HashMap::new(),
                takes_down: HashMap::new(),
                term_error: None,
                events: Vec::new(),
                respawn: None,
                signals: Vec::new(),
                docker_calls: Vec::new(),
                docker_result: Ok(()),
            }
        }

        fn add_proc(&mut self, pid: u32, name: &str, parent: Option<u32>) {
            let info = ProcInfo {
                pid,
                name: name.to_string(),
                parent,
                ..ProcInfo::default()
            };
            self.procs.insert(pid, info);
        }

        fn add_listener(&mut self, pid: Option<u32>) {
            self.listeners.push(Listener {
                addr: LO,
                port: PORT,
                pid,
            });
        }

        /// A process listening on PORT.
        fn add(&mut self, pid: u32, on_term: OnTerm) {
            self.add_proc(pid, "node", None);
            self.add_listener(Some(pid));
            self.on_term.insert(pid, on_term);
        }

        fn apply(&mut self, ev: Event) {
            match ev {
                Event::Exit(pid) => {
                    if self.procs.remove(&pid).is_none() {
                        return;
                    }
                    self.listeners.retain(|l| l.pid != Some(pid));
                    for other in self.takes_down.remove(&pid).unwrap_or_default() {
                        self.apply(Event::Exit(other));
                    }
                    if let Some((delay, spawn)) = self.respawn.take() {
                        self.events.push((self.now + delay, spawn));
                    }
                }
                Event::Spawn { pid, parent } => {
                    if let Some(p) = pid {
                        self.add_proc(p, "node", parent);
                    }
                    self.add_listener(pid);
                }
            }
        }

        fn run_due(&mut self) {
            loop {
                let Some(i) = self.events.iter().position(|(t, _)| *t <= self.now) else {
                    return;
                };
                let (_, ev) = self.events.remove(i);
                self.apply(ev);
            }
        }
    }

    #[derive(Clone)]
    struct Fake(Rc<RefCell<World>>);

    impl SocketSource for Fake {
        fn listeners(&mut self) -> anyhow::Result<Vec<Listener>> {
            Ok(self.0.borrow().listeners.clone())
        }
    }

    impl ProcessSource for Fake {
        fn processes(&mut self, pids: &[u32]) -> anyhow::Result<HashMap<u32, ProcInfo>> {
            let w = self.0.borrow();
            Ok(pids
                .iter()
                .filter_map(|p| w.procs.get(p).map(|i| (*p, i.clone())))
                .collect())
        }
    }

    impl Control for Fake {
        fn terminate(&mut self, pid: u32) -> Result<(), SignalError> {
            let mut w = self.0.borrow_mut();
            let now = w.now;
            w.signals.push((pid, "TERM", now));
            if let Some(e) = w.term_error.clone() {
                return Err(e);
            }
            if !w.procs.contains_key(&pid) {
                return Err(SignalError::NoSuchProcess);
            }
            if let Some(OnTerm::ExitAfter(d)) = w.on_term.get(&pid).copied() {
                w.events.push((now + d, Event::Exit(pid)));
                w.run_due();
            }
            Ok(())
        }

        fn kill(&mut self, pid: u32) -> Result<(), SignalError> {
            let mut w = self.0.borrow_mut();
            let now = w.now;
            w.signals.push((pid, "KILL", now));
            if !w.procs.contains_key(&pid) {
                return Err(SignalError::NoSuchProcess);
            }
            if !matches!(w.on_term.get(&pid), Some(OnTerm::Unkillable)) {
                w.apply(Event::Exit(pid));
            }
            Ok(())
        }

        fn docker_stop(&mut self, id: &str) -> Result<(), String> {
            let mut w = self.0.borrow_mut();
            w.docker_calls.push(id.to_string());
            w.docker_result.clone()
        }

        fn sleep(&mut self, d: Duration) {
            let mut w = self.0.borrow_mut();
            w.now += d;
            w.run_due();
        }
    }

    fn setup(world: World) -> (Fake, Scanner) {
        let fake = Fake(Rc::new(RefCell::new(world)));
        let scanner = Scanner::new(
            Box::new(fake.clone()),
            Box::new(fake.clone()),
            Box::new(FakeContainers::default()),
            None,
        );
        (fake, scanner)
    }

    fn entry(pid: Option<u32>) -> Entry {
        Entry {
            port: PORT,
            pid,
            process: "node".to_string(),
            kind: Kind::Vite,
            exposure: Exposure::Local,
            addrs: vec!["127.0.0.1".to_string()],
            command: "node vite".to_string(),
            cwd: None,
            uptime_secs: None,
            memory_mb: None,
            dev: true,
            container: None,
        }
    }

    fn run(world: World, e: &Entry, force: bool) -> (StopResult, Fake) {
        let (mut fake, mut scanner) = setup(world);
        let mut ctl = fake.clone();
        let r = stop(
            e,
            StopMode::Listener,
            force,
            &mut scanner,
            &mut ctl,
            &Timing::default(),
        );
        fake.sleep(Duration::ZERO);
        (r, fake)
    }

    fn signals(f: &Fake) -> Vec<(u32, &'static str)> {
        f.0.borrow()
            .signals
            .iter()
            .map(|(p, s, _)| (*p, *s))
            .collect()
    }

    fn failed(reason: FailReason) -> StopResult {
        StopResult::Failed { reason }
    }

    #[test]
    fn timing_defaults_match_spec() {
        let t = Timing::default();
        assert_eq!(t.poll, Duration::from_millis(100));
        assert_eq!(t.grace, Duration::from_secs(3));
        assert_eq!(t.kill_recheck, Duration::from_millis(300));
        assert_eq!(t.respawn_delay, Duration::from_millis(1500));
    }

    #[test]
    fn graceful_stop() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::from_millis(250)));
        let (r, f) = run(w, &entry(Some(10)), false);
        assert_eq!(r, StopResult::Stopped);
        assert_eq!(signals(&f), [(10, "TERM")]);
        // Detected on the 100 ms poll grid, then the 1.5 s respawn wait.
        assert_eq!(f.0.borrow().now, Duration::from_millis(300 + 1500));
    }

    #[test]
    fn hidden_pid_fails_without_signalling() {
        let mut w = World::new();
        w.add_listener(None);
        let (r, f) = run(w, &entry(None), true);
        assert_eq!(r, failed(FailReason::PidHidden));
        assert!(signals(&f).is_empty());
    }

    #[test]
    fn pid_reuse_is_detected_before_signalling() {
        // We scanned pid 10 on the port, but now pid 11 owns it.
        let mut w = World::new();
        w.add(11, OnTerm::ExitAfter(Duration::ZERO));
        w.add_proc(10, "unrelated", None);
        let (r, f) = run(w, &entry(Some(10)), true);
        assert_eq!(r, failed(FailReason::PidReused));
        assert!(signals(&f).is_empty());
    }

    #[test]
    fn ignoring_term_without_force_is_still_running() {
        let mut w = World::new();
        w.add(10, OnTerm::Ignore);
        let (r, f) = run(w, &entry(Some(10)), false);
        assert_eq!(r, StopResult::StillRunning);
        assert_eq!(signals(&f), [(10, "TERM")]);
        // Waited the full grace period; no respawn wait after a failure.
        assert_eq!(f.0.borrow().now, Duration::from_secs(3));
    }

    #[test]
    fn ignoring_term_with_force_is_force_killed_after_grace() {
        let mut w = World::new();
        w.add(10, OnTerm::Ignore);
        let (r, f) = run(w, &entry(Some(10)), true);
        assert_eq!(r, StopResult::ForceKilled);
        let sent = f.0.borrow().signals.clone();
        assert_eq!(sent[0], (10, "TERM", Duration::ZERO));
        assert_eq!(sent[1], (10, "KILL", Duration::from_secs(3)));
    }

    #[test]
    fn unkillable_is_still_running_even_with_force() {
        let mut w = World::new();
        w.add(10, OnTerm::Unkillable);
        let (r, _) = run(w, &entry(Some(10)), true);
        assert_eq!(r, StopResult::StillRunning);
    }

    #[test]
    fn permission_denied() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        w.term_error = Some(SignalError::PermissionDenied);
        let (r, _) = run(w, &entry(Some(10)), false);
        assert_eq!(r, failed(FailReason::PermissionDenied));
    }

    #[test]
    fn process_gone_at_signal_time_is_stopped() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        w.term_error = Some(SignalError::NoSuchProcess);
        let (r, _) = run(w, &entry(Some(10)), false);
        assert_eq!(r, StopResult::Stopped);
    }

    fn container_entry() -> Entry {
        let mut e = entry(None);
        e.kind = Kind::Docker;
        e.process = "db (postgres:16)".to_string();
        e.container = Some(Container {
            id: "3f2a9c1d7e4b".to_string(),
            name: "db".to_string(),
            image: "postgres:16".to_string(),
        });
        e
    }

    #[test]
    fn container_runs_docker_stop() {
        let (r, f) = run(World::new(), &container_entry(), false);
        assert_eq!(r, StopResult::Stopped);
        assert_eq!(f.0.borrow().docker_calls, ["3f2a9c1d7e4b"]);
        assert!(signals(&f).is_empty());
    }

    #[test]
    fn container_with_visible_proxy_pid_still_uses_docker() {
        let mut w = World::new();
        w.add(1203, OnTerm::ExitAfter(Duration::ZERO));
        let mut e = container_entry();
        e.pid = Some(1203);
        let (r, f) = run(w, &e, false);
        assert_eq!(r, StopResult::Stopped);
        assert!(signals(&f).is_empty());
    }

    #[test]
    fn docker_stop_failure_reports_stderr() {
        let mut w = World::new();
        w.docker_result = Err("Error response from daemon: No such container".to_string());
        let (r, _) = run(w, &container_entry(), false);
        assert_eq!(
            r,
            failed(FailReason::Other(
                "Error response from daemon: No such container".to_string()
            ))
        );
    }

    #[test]
    fn respawn_names_the_parent() {
        let mut w = World::new();
        w.add_proc(4000, "nodemon", None);
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        w.respawn = Some((
            Duration::from_millis(800),
            Event::Spawn {
                pid: Some(11),
                parent: Some(4000),
            },
        ));
        let (r, _) = run(w, &entry(Some(10)), false);
        assert_eq!(
            r,
            StopResult::Respawned {
                pid: Some(11),
                parent: Some(ProcRef {
                    pid: 4000,
                    name: "nodemon".to_string()
                }),
            }
        );
    }

    #[test]
    fn respawn_with_hidden_pid() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        w.respawn = Some((
            Duration::from_millis(500),
            Event::Spawn {
                pid: None,
                parent: None,
            },
        ));
        let (r, _) = run(w, &entry(Some(10)), false);
        assert_eq!(
            r,
            StopResult::Respawned {
                pid: None,
                parent: None
            }
        );
    }

    #[test]
    fn respawn_with_invisible_parent_has_no_parent() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        w.respawn = Some((
            Duration::from_millis(500),
            Event::Spawn {
                pid: Some(11),
                parent: Some(1),
            },
        ));
        let (r, _) = run(w, &entry(Some(10)), false);
        assert_eq!(
            r,
            StopResult::Respawned {
                pid: Some(11),
                parent: None
            }
        );
    }

    #[test]
    fn respawn_after_force_kill() {
        let mut w = World::new();
        w.add(10, OnTerm::Ignore);
        w.respawn = Some((
            Duration::from_millis(100),
            Event::Spawn {
                pid: Some(12),
                parent: None,
            },
        ));
        let (r, _) = run(w, &entry(Some(10)), true);
        assert!(
            matches!(r, StopResult::Respawned { pid: Some(12), .. }),
            "{r:?}"
        );
    }

    #[test]
    fn late_respawn_is_not_seen() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        w.respawn = Some((
            Duration::from_secs(2),
            Event::Spawn {
                pid: Some(11),
                parent: None,
            },
        ));
        let (r, _) = run(w, &entry(Some(10)), false);
        assert_eq!(r, StopResult::Stopped);
    }

    #[test]
    fn several_owners_are_all_stopped_without_false_respawn() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        w.add(11, OnTerm::ExitAfter(Duration::from_millis(200)));
        let (mut fake, mut scanner) = setup(w);
        let mut ctl = fake.clone();
        let r = stop_port(
            &[entry(Some(10)), entry(Some(11))],
            StopMode::Listener,
            false,
            &mut scanner,
            &mut ctl,
            &Timing::default(),
        );
        fake.sleep(Duration::ZERO);
        assert_eq!(r, [StopResult::Stopped, StopResult::Stopped]);
        assert_eq!(signals(&fake), [(10, "TERM"), (11, "TERM")]);
    }

    #[test]
    fn owner_that_went_down_with_an_earlier_one_is_stopped() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        w.add(11, OnTerm::Ignore);
        w.takes_down.insert(10, vec![11]);
        let (fake, mut scanner) = setup(w);
        let mut ctl = fake.clone();
        let r = stop_port(
            &[entry(Some(10)), entry(Some(11))],
            StopMode::Listener,
            false,
            &mut scanner,
            &mut ctl,
            &Timing::default(),
        );
        assert_eq!(r, [StopResult::Stopped, StopResult::Stopped]);
        assert_eq!(signals(&fake), [(10, "TERM")]);
    }

    #[test]
    fn tree_mode_is_not_available_yet() {
        let mut w = World::new();
        w.add(10, OnTerm::ExitAfter(Duration::ZERO));
        let (fake, mut scanner) = setup(w);
        let mut ctl = fake.clone();
        let r = stop(
            &entry(Some(10)),
            StopMode::Tree,
            false,
            &mut scanner,
            &mut ctl,
            &Timing::default(),
        );
        assert!(matches!(
            r,
            StopResult::Failed {
                reason: FailReason::Other(_)
            }
        ));
        assert!(signals(&fake).is_empty());
    }

    /// The real Control against a real child process.
    #[test]
    fn os_terminate_stops_a_real_child() -> anyhow::Result<()> {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        let mut child = cmd.stdout(Stdio::null()).spawn()?;
        let pid = child.id();
        let sent = OsControl.terminate(pid);
        let status = child.wait()?;
        assert_eq!(sent, Ok(()));
        assert!(!status.success());
        // Reaped now, so the PID is gone (or reused, which we can't rule out).
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn os_signal_errors_are_classified() {
        // No process can have PID i32::MAX (above every pid_max).
        assert_eq!(
            OsControl.terminate(i32::MAX as u32),
            Err(SignalError::NoSuchProcess)
        );
        // 0 and values above i32::MAX would address process groups: refused.
        assert!(matches!(OsControl.kill(0), Err(SignalError::Other(_))));
        assert!(matches!(
            OsControl.kill(u32::MAX),
            Err(SignalError::Other(_))
        ));
    }
}
