//! Data sources (DESIGN.md sections 4, 5 and 7).
//!
//! Steps 1–3 of the pipeline (enumerate, attribute, enrich) are
//! platform-specific, so they sit behind these traits. Real implementations
//! (netstat2, sysinfo, docker CLI) live in the submodules; tests use the fakes
//! in [`fake`].

pub mod docker;
pub mod processes;
pub mod sockets;

use std::collections::HashMap;
use std::net::IpAddr;

use crate::model::Container;

/// One listening TCP socket, as reported by the OS socket table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub addr: IpAddr,
    pub port: u16,
    /// `None` when the OS hides the owner (other user, no privileges).
    pub pid: Option<u32>,
}

/// What the OS process table tells us about one PID.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    /// Full argv; empty when hidden.
    pub cmd: Vec<String>,
    pub cwd: Option<String>,
    pub exe: Option<String>,
    /// Owner user id, as an opaque string (uid on Unix, SID on Windows).
    pub user_id: Option<String>,
    pub uptime_secs: Option<u64>,
    pub memory_mb: Option<u64>,
    /// Parent PID, when the OS reports one.
    pub parent: Option<u32>,
}

/// Enumerates TCP sockets in LISTEN state (step 1 and 2).
pub trait SocketSource {
    fn listeners(&mut self) -> anyhow::Result<Vec<Listener>>;
}

/// Looks up process details by PID (step 3).
pub trait ProcessSource {
    /// Returns info for each requested PID that still exists. PIDs that have
    /// exited (including zombies waiting to be reaped) are left out rather
    /// than reported as errors.
    fn processes(&mut self, pids: &[u32]) -> anyhow::Result<HashMap<u32, ProcInfo>>;
}

/// A running container that publishes a host port, and the host addresses
/// it is published on (used for exposure when no listener row exists).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedContainer {
    pub container: Container,
    pub addrs: Vec<IpAddr>,
}

/// Maps published host ports to the running container behind them.
pub trait ContainerSource {
    fn containers(&mut self) -> anyhow::Result<HashMap<u16, PublishedContainer>>;
}

/// In-memory implementations for tests: no OS access, fully deterministic.
#[cfg(test)]
pub mod fake {
    use super::*;

    /// Returns a fixed list of listeners.
    #[derive(Debug, Default)]
    pub struct FakeSockets {
        pub listeners: Vec<Listener>,
    }

    impl SocketSource for FakeSockets {
        fn listeners(&mut self) -> anyhow::Result<Vec<Listener>> {
            Ok(self.listeners.clone())
        }
    }

    /// Returns processes from a fixed table.
    #[derive(Debug, Default)]
    pub struct FakeProcesses {
        pub by_pid: HashMap<u32, ProcInfo>,
    }

    impl FakeProcesses {
        pub fn new(procs: impl IntoIterator<Item = ProcInfo>) -> Self {
            Self {
                by_pid: procs.into_iter().map(|p| (p.pid, p)).collect(),
            }
        }
    }

    impl ProcessSource for FakeProcesses {
        fn processes(&mut self, pids: &[u32]) -> anyhow::Result<HashMap<u32, ProcInfo>> {
            Ok(pids
                .iter()
                .filter_map(|pid| self.by_pid.get(pid).map(|p| (*pid, p.clone())))
                .collect())
        }
    }

    /// Returns a fixed host-port → container map, or an error when `fail` is
    /// set (to exercise the "docker missing" path).
    #[derive(Debug, Default)]
    pub struct FakeContainers {
        pub by_port: HashMap<u16, PublishedContainer>,
        pub fail: bool,
        /// Return the docker timeout error instead.
        pub timed_out: bool,
    }

    impl ContainerSource for FakeContainers {
        fn containers(&mut self) -> anyhow::Result<HashMap<u16, PublishedContainer>> {
            if self.fail {
                anyhow::bail!("docker is not available");
            }
            if self.timed_out {
                return Err(super::docker::DockerTimeout.into());
            }
            Ok(self.by_port.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::fake::*;
    use super::*;

    #[test]
    fn fake_sockets_return_their_listeners() -> anyhow::Result<()> {
        let listeners = vec![
            Listener {
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 5173,
                pid: Some(10),
            },
            Listener {
                addr: IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                port: 5432,
                pid: None,
            },
        ];
        let mut src = FakeSockets {
            listeners: listeners.clone(),
        };
        // Used through the trait, the way the scanner will use it.
        let src: &mut dyn SocketSource = &mut src;
        assert_eq!(src.listeners()?, listeners);
        Ok(())
    }

    #[test]
    fn fake_processes_skip_unknown_pids() -> anyhow::Result<()> {
        let node = ProcInfo {
            pid: 10,
            name: "node".to_string(),
            cmd: vec!["node".to_string(), "vite".to_string()],
            ..ProcInfo::default()
        };
        let mut src = FakeProcesses::new([node.clone()]);
        let got = src.processes(&[10, 99])?;
        assert_eq!(got.len(), 1);
        assert_eq!(got.get(&10), Some(&node));
        Ok(())
    }

    #[test]
    fn fake_containers_can_fail() -> anyhow::Result<()> {
        let db = PublishedContainer {
            container: Container {
                id: "3f2a9c1d7e4b".to_string(),
                name: "db".to_string(),
                image: "postgres:16".to_string(),
            },
            addrs: vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
        };
        let mut ok = FakeContainers {
            by_port: HashMap::from([(5432, db.clone())]),
            ..FakeContainers::default()
        };
        assert_eq!(ok.containers()?.get(&5432), Some(&db));

        let mut broken = FakeContainers {
            fail: true,
            ..FakeContainers::default()
        };
        assert!(broken.containers().is_err());
        Ok(())
    }
}
