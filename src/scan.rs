//! The Scanner (DESIGN.md section 5): joins the three sources into the rows
//! every frontend shows.
//!
//! 1. Group IPv4/IPv6 twins: listeners with the same `(port, pid)` become one
//!    [`Entry`] with several addresses.
//! 2. Join PID → process info, then compute kind, exposure and dev.
//! 3. Join host port → container (section 7.3): a row whose owner is
//!    container plumbing or hidden becomes the container's row; a visible,
//!    non-plumbing owner is kept as is. Published ports with no listener at
//!    all get a synthesized container row.
//! 4. Sort dev first, then by port.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;

use crate::classify::{classify, is_container_plumbing};
use crate::filter::{exposure, is_dev};
use crate::model::{Entry, Kind};
use crate::sources::docker::{DockerContainers, DockerTimeout};
use crate::sources::processes::SysinfoProcesses;
use crate::sources::sockets::NativeSockets;
use crate::sources::{ContainerSource, ProcInfo, ProcessSource, PublishedContainer, SocketSource};

/// Joins sources into entries. Owns the sources so that [`crate::actions`]
/// can reuse them to re-verify and re-scan.
pub struct Scanner {
    pub sockets: Box<dyn SocketSource>,
    pub processes: Box<dyn ProcessSource>,
    pub containers: Box<dyn ContainerSource>,
    /// User id of the process running whatsup, for the dev heuristic.
    self_user: Option<String>,
    /// Whether `docker ps` timed out during the last scan.
    docker_timed_out: bool,
}

impl Scanner {
    pub fn new(
        sockets: Box<dyn SocketSource>,
        processes: Box<dyn ProcessSource>,
        containers: Box<dyn ContainerSource>,
        self_user: Option<String>,
    ) -> Self {
        Self {
            sockets,
            processes,
            containers,
            self_user,
            docker_timed_out: false,
        }
    }

    /// True when the last [`Scanner::scan`] went without containers because
    /// `docker ps` didn't answer in time (see [`DockerTimeout`]).
    pub fn docker_timed_out(&self) -> bool {
        self.docker_timed_out
    }

    /// A scanner over the real OS sources.
    pub fn system() -> anyhow::Result<Self> {
        let mut processes = SysinfoProcesses::new()?;
        let me = std::process::id();
        let self_user = processes
            .processes(&[me])?
            .remove(&me)
            .and_then(|p| p.user_id);
        Ok(Self::new(
            Box::new(NativeSockets),
            Box::new(processes),
            Box::new(DockerContainers::new()),
            self_user,
        ))
    }

    /// One full scan: every listener, joined and interpreted, sorted.
    pub fn scan(&mut self) -> anyhow::Result<Vec<Entry>> {
        let listeners = self.sockets.listeners()?;

        // (port, pid) -> addresses. BTreeMap keeps the output deterministic.
        let mut groups: BTreeMap<(u16, Option<u32>), Vec<IpAddr>> = BTreeMap::new();
        for l in listeners {
            let addrs = groups.entry((l.port, l.pid)).or_default();
            if !addrs.contains(&l.addr) {
                addrs.push(l.addr);
            }
        }

        let pids: Vec<u32> = groups
            .keys()
            .filter_map(|(_, pid)| *pid)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let procs = self.processes.processes(&pids)?;
        // Docker problems never break a scan (section 7.3); a timeout is
        // remembered so the CLI can say containers are missing.
        let containers = match self.containers.containers() {
            Ok(map) => {
                self.docker_timed_out = false;
                map
            }
            Err(e) => {
                self.docker_timed_out = e.downcast_ref::<DockerTimeout>().is_some();
                HashMap::new()
            }
        };

        // Join containers while addresses are still IpAddrs, so merged rows
        // get their exposure from the union of addresses.
        let mut rows: Vec<Row<'_>> = Vec::with_capacity(groups.len());
        for ((port, pid), addrs) in groups {
            let info = pid.and_then(|p| procs.get(&p));
            let container = containers
                .get(&port)
                .filter(|_| info.is_none_or(|i| is_container_plumbing(&i.name)));
            // One docker-proxy per address family (both visible under sudo)
            // is still one container row.
            if let Some(pc) = container
                && let Some(twin) = rows.iter_mut().find(|r| {
                    r.port == port
                        && r.container
                            .is_some_and(|c| c.container.id == pc.container.id)
                })
            {
                for a in addrs {
                    if !twin.addrs.contains(&a) {
                        twin.addrs.push(a);
                    }
                }
                continue;
            }
            rows.push(Row {
                port,
                pid,
                info,
                addrs,
                container,
            });
        }

        let mut entries: Vec<Entry> = rows.into_iter().map(|r| self.entry(r)).collect();

        let listened: HashSet<u16> = entries.iter().map(|e| e.port).collect();
        for (port, pc) in &containers {
            if !listened.contains(port) {
                entries.push(synthesized_container_row(*port, pc));
            }
        }

        sort_entries(&mut entries);
        Ok(entries)
    }

    /// Builds one entry from a joined row. A container row keeps the
    /// listener's pid, addresses, uptime and memory.
    fn entry(&self, row: Row<'_>) -> Entry {
        let Row {
            port,
            pid,
            info,
            addrs,
            container,
        } = row;
        let name = info.map(|i| i.name.clone()).unwrap_or_default();
        let command = info.map(|i| i.cmd.join(" ")).unwrap_or_default();
        let kind = classify(&name, &command);
        let same_user = match (info.and_then(|i| i.user_id.as_ref()), &self.self_user) {
            (Some(theirs), Some(mine)) => theirs == mine,
            _ => false,
        };
        let exe = info.and_then(|i| i.exe.as_deref());
        let mut e = Entry {
            port,
            pid,
            process: name,
            kind,
            exposure: exposure(&addrs),
            addrs: addr_strings(addrs),
            command,
            cwd: info.and_then(|i| i.cwd.clone()),
            uptime_secs: info.and_then(|i| i.uptime_secs),
            memory_mb: info.and_then(|i| i.memory_mb),
            dev: is_dev(kind, port, same_user, exe),
            container: None,
        };
        if let Some(pc) = container {
            attach_container(&mut e, pc);
        }
        e
    }
}

/// A listener group after the PID and container joins.
struct Row<'a> {
    port: u16,
    pid: Option<u32>,
    /// `None` when the owner is hidden (no PID, or no process info).
    info: Option<&'a ProcInfo>,
    addrs: Vec<IpAddr>,
    container: Option<&'a PublishedContainer>,
}

/// Turns a row into the container's row: name, kind and stop target change.
fn attach_container(e: &mut Entry, pc: &PublishedContainer) {
    let c = &pc.container;
    e.process = format!("{} ({})", c.name, c.image);
    e.kind = Kind::Docker;
    e.dev = true; // a recognised kind is always dev (section 9)
    e.container = Some(c.clone());
}

/// A container row for a published port with no listener at all (Docker's
/// userland proxy disabled): no pid, addresses from the published binding.
fn synthesized_container_row(port: u16, pc: &PublishedContainer) -> Entry {
    let mut e = Entry {
        port,
        pid: None,
        process: String::new(),
        kind: Kind::Docker,
        exposure: exposure(&pc.addrs),
        addrs: addr_strings(pc.addrs.clone()),
        command: String::new(),
        cwd: None,
        uptime_secs: None,
        memory_mb: None,
        dev: true,
        container: None,
    };
    attach_container(&mut e, pc);
    e
}

/// Address strings for display and JSON: `*` for unspecified (`0.0.0.0`,
/// `::`), sorted (IPv4 before IPv6) and deduplicated.
fn addr_strings(mut addrs: Vec<IpAddr>) -> Vec<String> {
    addrs.sort();
    let mut out: Vec<String> = Vec::new();
    for a in addrs {
        let s = if a.is_unspecified() {
            "*".to_string()
        } else {
            a.to_string()
        };
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Dev rows first, then by port, then by pid (hidden last).
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_key(|e| (!e.dev, e.port, e.pid.is_none(), e.pid));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::model::{Container, Exposure};
    use crate::sources::Listener;
    use crate::sources::fake::{FakeContainers, FakeProcesses, FakeSockets};

    const ME: &str = "501";

    fn ip(s: &str) -> IpAddr {
        match s.parse() {
            Ok(a) => a,
            Err(e) => panic!("bad test address {s}: {e}"),
        }
    }

    fn l(addr: &str, port: u16, pid: Option<u32>) -> Listener {
        Listener {
            addr: ip(addr),
            port,
            pid,
        }
    }

    fn proc(pid: u32, name: &str, cmd: &str, user: &str) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.to_string(),
            cmd: cmd.split_whitespace().map(str::to_string).collect(),
            cwd: Some("/home/me/web".to_string()),
            exe: Some(format!("/usr/local/bin/{name}")),
            user_id: Some(user.to_string()),
            uptime_secs: Some(60),
            memory_mb: Some(42),
            parent: Some(1),
        }
    }

    fn db(addrs: &[&str]) -> PublishedContainer {
        PublishedContainer {
            container: Container {
                id: "3f2a9c1d7e4b".to_string(),
                name: "db".to_string(),
                image: "postgres:16".to_string(),
            },
            addrs: addrs.iter().map(|a| ip(a)).collect(),
        }
    }

    fn scanner(
        listeners: Vec<Listener>,
        procs: Vec<ProcInfo>,
        containers: HashMap<u16, PublishedContainer>,
    ) -> Scanner {
        Scanner::new(
            Box::new(FakeSockets { listeners }),
            Box::new(FakeProcesses::new(procs)),
            Box::new(FakeContainers {
                by_port: containers,
                ..FakeContainers::default()
            }),
            Some(ME.to_string()),
        )
    }

    #[test]
    fn ipv4_ipv6_twins_merge_into_one_entry() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![l("127.0.0.1", 5173, Some(10)), l("::1", 5173, Some(10))],
            vec![proc(10, "node", "node /w/node_modules/.bin/vite", ME)],
            HashMap::new(),
        );
        let e = s.scan()?;
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].addrs, ["127.0.0.1", "::1"]);
        assert_eq!(e[0].exposure, Exposure::Local);
        assert_eq!(e[0].kind, Kind::Vite);
        assert_eq!(e[0].process, "node");
        assert_eq!(e[0].command, "node /w/node_modules/.bin/vite");
        assert_eq!(e[0].cwd.as_deref(), Some("/home/me/web"));
        assert_eq!((e[0].uptime_secs, e[0].memory_mb), (Some(60), Some(42)));
        assert!(e[0].dev);
        Ok(())
    }

    #[test]
    fn same_port_different_pids_stay_separate() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![l("127.0.0.1", 8000, Some(10)), l("::1", 8000, Some(11))],
            vec![
                proc(10, "python3", "python3 -m http.server", ME),
                proc(11, "node", "node server.js", ME),
            ],
            HashMap::new(),
        );
        let e = s.scan()?;
        let pids: Vec<_> = e.iter().map(|e| e.pid).collect();
        assert_eq!(pids, [Some(10), Some(11)]);
        Ok(())
    }

    #[test]
    fn unspecified_address_is_star_and_network() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![l("0.0.0.0", 8000, Some(10)), l("::", 8000, Some(10))],
            vec![proc(10, "python3", "python3 -m http.server 8000", ME)],
            HashMap::new(),
        );
        let e = s.scan()?;
        assert_eq!(e[0].addrs, ["*"]);
        assert_eq!(e[0].exposure, Exposure::Network);
        assert_eq!(e[0].kind, Kind::PythonHttp);
        Ok(())
    }

    #[test]
    fn hidden_owner_is_joined_to_container() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![l("0.0.0.0", 5432, None), l("::", 5432, None)],
            vec![],
            HashMap::from([(5432, db(&["0.0.0.0", "::"]))]),
        );
        let e = s.scan()?;
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].pid, None);
        assert_eq!(e[0].process, "db (postgres:16)");
        assert_eq!(e[0].kind, Kind::Docker);
        assert_eq!(e[0].exposure, Exposure::Network);
        assert_eq!(
            e[0].container.as_ref().map(|c| c.id.as_str()),
            Some("3f2a9c1d7e4b")
        );
        assert!(e[0].dev);
        Ok(())
    }

    #[test]
    fn plumbing_owner_is_replaced_by_container() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![l("127.0.0.1", 5432, Some(1203))],
            vec![proc(
                1203,
                "com.docker.backend",
                "/Applications/Docker.app/Contents/MacOS/com.docker.backend",
                ME,
            )],
            HashMap::from([(5432, db(&["127.0.0.1"]))]),
        );
        let e = s.scan()?;
        assert_eq!(e.len(), 1);
        // Keeps the listener's pid, addresses, uptime and memory.
        assert_eq!(e[0].pid, Some(1203));
        assert_eq!(e[0].addrs, ["127.0.0.1"]);
        assert_eq!(e[0].exposure, Exposure::Local);
        assert_eq!((e[0].uptime_secs, e[0].memory_mb), (Some(60), Some(42)));
        assert_eq!(e[0].process, "db (postgres:16)");
        assert_eq!(e[0].kind, Kind::Docker);
        assert!(e[0].container.is_some());
        Ok(())
    }

    #[test]
    fn visible_non_plumbing_owner_is_not_joined() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![l("127.0.0.1", 5432, Some(10))],
            vec![proc(10, "node", "node server.js", ME)],
            HashMap::from([(5432, db(&["0.0.0.0"]))]),
        );
        let e = s.scan()?;
        // The real process row stays; no container attached, nothing synthesized.
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].process, "node");
        assert_eq!(e[0].kind, Kind::Node);
        assert_eq!(e[0].container, None);
        Ok(())
    }

    #[test]
    fn published_port_without_listener_is_synthesized() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![],
            vec![],
            HashMap::from([(5432, db(&["0.0.0.0", "::"])), (6379, db(&["127.0.0.1"]))]),
        );
        let e = s.scan()?;
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].port, 5432);
        assert_eq!(e[0].pid, None);
        assert_eq!(e[0].addrs, ["*"]);
        assert_eq!(e[0].exposure, Exposure::Network);
        assert_eq!(e[0].kind, Kind::Docker);
        assert_eq!(e[0].process, "db (postgres:16)");
        assert!(e[0].dev);
        assert_eq!(e[1].port, 6379);
        assert_eq!(e[1].exposure, Exposure::Local);
        Ok(())
    }

    #[test]
    fn docker_proxy_per_family_merges_into_one_container_row() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![l("0.0.0.0", 5432, Some(900)), l("::", 5432, Some(901))],
            vec![
                proc(
                    900,
                    "docker-proxy",
                    "/usr/bin/docker-proxy -host-ip 0.0.0.0",
                    "0",
                ),
                proc(
                    901,
                    "docker-proxy",
                    "/usr/bin/docker-proxy -host-ip ::",
                    "0",
                ),
            ],
            HashMap::from([(5432, db(&["0.0.0.0", "::"]))]),
        );
        let e = s.scan()?;
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].pid, Some(900));
        assert_eq!(e[0].addrs, ["*"]);
        Ok(())
    }

    #[test]
    fn docker_failure_does_not_break_the_scan() -> anyhow::Result<()> {
        let mut s = Scanner::new(
            Box::new(FakeSockets {
                listeners: vec![l("127.0.0.1", 3000, Some(10))],
            }),
            Box::new(FakeProcesses::new([proc(10, "node", "node server.js", ME)])),
            Box::new(FakeContainers {
                fail: true,
                ..FakeContainers::default()
            }),
            Some(ME.to_string()),
        );
        assert_eq!(s.scan()?.len(), 1);
        Ok(())
    }

    #[test]
    fn docker_timeout_is_reported_but_the_scan_goes_on() -> anyhow::Result<()> {
        let mut s = Scanner::new(
            Box::new(FakeSockets {
                listeners: vec![l("127.0.0.1", 3000, Some(10))],
            }),
            Box::new(FakeProcesses::new([proc(10, "node", "node server.js", ME)])),
            Box::new(FakeContainers {
                timed_out: true,
                ..FakeContainers::default()
            }),
            Some(ME.to_string()),
        );
        assert_eq!(s.scan()?.len(), 1);
        assert!(s.docker_timed_out());
        Ok(())
    }

    #[test]
    fn other_docker_failures_are_silent() -> anyhow::Result<()> {
        let mut s = scanner(vec![], vec![], HashMap::new());
        s.containers = Box::new(FakeContainers {
            fail: true,
            ..FakeContainers::default()
        });
        s.scan()?;
        assert!(!s.docker_timed_out());
        Ok(())
    }

    #[test]
    fn hidden_and_vanished_owners_are_other_and_not_dev() -> anyhow::Result<()> {
        // pid 77 is in the socket table but has no process info (exited or
        // hidden): treated like a hidden owner.
        let mut s = scanner(
            vec![l("127.0.0.1", 3000, None), l("127.0.0.1", 3001, Some(77))],
            vec![],
            HashMap::new(),
        );
        let e = s.scan()?;
        for row in &e {
            assert_eq!(row.kind, Kind::Other);
            assert_eq!(row.process, "");
            assert!(!row.dev);
        }
        assert_eq!(e[1].pid, Some(77));
        Ok(())
    }

    #[test]
    fn dev_heuristic_uses_the_scanning_user() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![
                l("127.0.0.1", 8080, Some(10)),
                l("127.0.0.1", 8081, Some(11)),
            ],
            vec![
                proc(10, "api", "/home/me/api/bin/api", ME),
                proc(11, "api", "/home/bob/api/bin/api", "1000"),
            ],
            HashMap::new(),
        );
        let e = s.scan()?;
        let dev: Vec<_> = e.iter().map(|e| (e.port, e.dev)).collect();
        assert_eq!(dev, [(8080, true), (8081, false)]);
        Ok(())
    }

    #[test]
    fn sorted_dev_first_then_by_port() -> anyhow::Result<()> {
        let mut s = scanner(
            vec![
                l("0.0.0.0", 22, None),
                l("127.0.0.1", 8000, Some(12)),
                l("127.0.0.1", 57621, Some(13)),
                l("127.0.0.1", 3000, Some(14)),
            ],
            vec![
                proc(12, "python3", "python3 -m http.server", ME),
                proc(13, "Spotify", "/Applications/Spotify.app/Spotify", ME),
                proc(14, "node", "node server.js", ME),
            ],
            HashMap::new(),
        );
        let ports: Vec<_> = s.scan()?.iter().map(|e| (e.port, e.dev)).collect();
        assert_eq!(
            ports,
            [(3000, true), (8000, true), (22, false), (57621, false)]
        );
        Ok(())
    }

    #[test]
    fn addr_strings_sort_and_star() {
        assert_eq!(
            addr_strings(vec![ip("::1"), ip("127.0.0.1"), ip("::"), ip("0.0.0.0")]),
            ["*", "127.0.0.1", "::1"]
        );
    }
}
