//! Real [`ProcessSource`] backed by `sysinfo` (DESIGN.md section 7.2).
//!
//! Uses a targeted refresh (memory, user, cmd, cwd, exe) of only the PIDs we
//! were asked about, never a full refresh. cmd / cwd / exe never change during
//! a process's life, so they are loaded once (`UpdateKind::OnlyIfNotSet`). The
//! `System` is kept between calls so that cache actually applies.

use std::collections::HashMap;

use anyhow::Context;
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

use super::{ProcInfo, ProcessSource};

/// sysinfo-backed process table.
pub struct SysinfoProcesses {
    sys: System,
}

impl SysinfoProcesses {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            sys: System::new().context("could not open the OS process table")?,
        })
    }

    /// The refresh kinds from section 7.2. Without cmd/cwd the classifier
    /// silently degrades to 'node'/'python' (the prototype bug).
    fn refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_user(UpdateKind::Always)
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::OnlyIfNotSet)
    }
}

impl ProcessSource for SysinfoProcesses {
    fn processes(&mut self, pids: &[u32]) -> anyhow::Result<HashMap<u32, ProcInfo>> {
        let wanted: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&wanted),
            true,
            Self::refresh_kind(),
        );
        let mut out = HashMap::new();
        for pid in &wanted {
            let Some(p) = self.sys.process(*pid) else {
                continue;
            };
            // A zombie has exited and is only waiting to be reaped: gone.
            if p.status() == ProcessStatus::Zombie {
                continue;
            }
            let info = ProcInfo {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cmd: p
                    .cmd()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
                cwd: p.cwd().map(|c| c.display().to_string()),
                exe: p.exe().map(|e| e.display().to_string()),
                user_id: p.user_id().map(|u| u.to_string()),
                uptime_secs: Some(p.run_time()),
                memory_mb: Some(p.memory() / (1024 * 1024)),
                parent: p.parent().map(Pid::as_u32),
            };
            out.insert(info.pid, info);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use super::*;

    /// Kills and reaps the child even when an assertion fails.
    struct KillOnDrop(Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn spawn_sleeper() -> std::io::Result<KillOnDrop> {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.current_dir(std::env::temp_dir())
            .stdout(std::process::Stdio::null());
        cmd.spawn().map(KillOnDrop)
    }

    /// Section 7.2 lesson: the targeted refresh must load cmd (and cwd on
    /// Linux/macOS) or classification silently degrades.
    ///
    /// Right after spawn, Linux can briefly report an empty cmdline (execve
    /// has not set up the new argv yet). OnlyIfNotSet treats empty as "not
    /// set", so a later refresh of the same source fills it: poll briefly.
    #[test]
    fn spawned_child_has_cmd_and_cwd() -> anyhow::Result<()> {
        let child = spawn_sleeper()?;
        let pid = child.0.id();
        let mut src = SysinfoProcesses::new()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let info = loop {
            let info = src
                .processes(&[pid])?
                .remove(&pid)
                .context("child not found")?;
            if !info.cmd.is_empty() || Instant::now() > deadline {
                break info;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(info.pid, pid);
        assert!(!info.name.is_empty());
        assert!(!info.cmd.is_empty(), "cmd empty: {info:?}");
        assert!(info.cmd.iter().any(|a| a == "30"), "{info:?}");
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(
                info.cwd.as_deref().is_some_and(|c| !c.is_empty()),
                "cwd empty: {info:?}"
            );
        }
        assert!(info.user_id.is_some(), "{info:?}");
        assert_eq!(info.parent, Some(std::process::id()));
        Ok(())
    }

    /// Polls until `pid` is no longer listed (the OS may lag a little).
    fn assert_disappears(src: &mut SysinfoProcesses, pid: u32) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while src.processes(&[pid])?.contains_key(&pid) {
            assert!(Instant::now() < deadline, "pid {pid} still listed");
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    #[test]
    fn exited_process_is_left_out() -> anyhow::Result<()> {
        let mut child = spawn_sleeper()?;
        let pid = child.0.id();
        child.0.kill()?;
        child.0.wait()?;
        let mut src = SysinfoProcesses::new()?;
        assert_disappears(&mut src, pid)
    }

    /// A killed but un-reaped child is a zombie on Unix: it counts as gone.
    #[cfg(unix)]
    #[test]
    fn zombie_is_left_out() -> anyhow::Result<()> {
        let mut child = spawn_sleeper()?;
        let pid = child.0.id();
        child.0.kill()?;
        // Not waited yet: the child stays a zombie until the guard reaps it.
        let mut src = SysinfoProcesses::new()?;
        assert_disappears(&mut src, pid)
    }

    #[test]
    fn unknown_pid_is_left_out() -> anyhow::Result<()> {
        let mut src = SysinfoProcesses::new()?;
        assert!(src.processes(&[u32::MAX - 1])?.is_empty());
        assert!(src.processes(&[])?.is_empty());
        Ok(())
    }
}
