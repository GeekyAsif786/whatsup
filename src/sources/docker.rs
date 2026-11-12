//! Docker container source (DESIGN.md section 7.3).
//!
//! Runs `docker ps --format {{json .}}` (as an argument array, never through
//! a shell) with a 2 s timeout, parses one JSON object per line, and maps
//! every published host TCP port to its container. Results are cached for
//! 5 s. If docker is missing, not running, or fails, the source is silently
//! empty; if it doesn't answer in time, it returns [`DockerTimeout`].

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{ContainerSource, PublishedContainer};
use crate::model::Container;
use crate::run::{RunError, human, run_with_timeout};

/// How long one `docker ps` result is reused (section 15: ≤ 1 call per 5 s).
pub const CACHE_TTL: Duration = Duration::from_secs(5);

/// One published TCP port: `host_port` on the machine maps to
/// `container_port` inside the container, bound on `host_addrs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedPort {
    pub host_port: u16,
    pub container_port: u16,
    /// Host addresses the port is published on (`0.0.0.0`, `::`,
    /// `127.0.0.1`, ...). IPv4/IPv6 twins are merged into one entry.
    pub host_addrs: Vec<IpAddr>,
}

/// Parses docker's `Ports` string into the published TCP ports.
///
/// Handles `0.0.0.0:5432->5432/tcp`, IPv6 twins (`:::5432->...` and
/// `[::]:5432->...`, merged with their IPv4 twin) and ranges
/// (`127.0.0.1:8000-8002->8000-8002/tcp`, expanded pairwise). Unpublished
/// ports (`6379/tcp`), UDP mappings (no UDP in v0.1) and anything malformed
/// are skipped. Results are in input order.
pub fn parse_docker_ports(ports: &str) -> Vec<PublishedPort> {
    let mut out: Vec<PublishedPort> = Vec::new();
    for item in ports.split(',') {
        for p in parse_item(item.trim()).unwrap_or_default() {
            match out
                .iter_mut()
                .find(|o| o.host_port == p.host_port && o.container_port == p.container_port)
            {
                Some(existing) => {
                    for a in p.host_addrs {
                        if !existing.host_addrs.contains(&a) {
                            existing.host_addrs.push(a);
                        }
                    }
                }
                None => out.push(p),
            }
        }
    }
    out
}

/// Parses one `host_ip:host_ports->container_ports/proto` item.
/// `None` means "skip this item".
fn parse_item(item: &str) -> Option<Vec<PublishedPort>> {
    // No "->" means the port is exposed but not published on the host.
    let (host, container) = item.split_once("->")?;
    let (container_ports, proto) = container.split_once('/')?;
    if proto != "tcp" {
        return None;
    }
    // The host side is "ip:ports"; the ip itself may contain ':' (IPv6).
    let (ip, host_ports) = host.rsplit_once(':')?;
    let ip = ip.trim_start_matches('[').trim_end_matches(']');
    let addr: IpAddr = ip.parse().ok()?;

    let (h_lo, h_hi) = parse_range(host_ports)?;
    let (c_lo, c_hi) = parse_range(container_ports)?;
    if h_hi - h_lo != c_hi - c_lo {
        return None;
    }
    Some(
        (h_lo..=h_hi)
            .zip(c_lo..=c_hi)
            .map(|(host_port, container_port)| PublishedPort {
                host_port,
                container_port,
                host_addrs: vec![addr],
            })
            .collect(),
    )
}

/// Parses `"5432"` or `"8000-8002"` into an inclusive `(low, high)` range.
fn parse_range(s: &str) -> Option<(u16, u16)> {
    let (lo, hi) = match s.split_once('-') {
        Some((lo, hi)) => (lo.parse().ok()?, hi.parse().ok()?),
        None => {
            let p = s.parse().ok()?;
            (p, p)
        }
    };
    (lo <= hi && lo != 0).then_some((lo, hi))
}

/// The fields we use from one `docker ps --format {{json .}}` line.
#[derive(Debug, Deserialize)]
struct PsLine {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Names")]
    names: String,
    #[serde(rename = "Image")]
    image: String,
    #[serde(rename = "Ports", default)]
    ports: String,
}

/// Parses `docker ps --format {{json .}}` output (JSON lines) into a map from
/// published host port to container. Lines that are not valid JSON objects
/// with the expected fields are skipped. If two containers claim the same
/// host port (e.g. on different host IPs), the first one wins.
pub fn parse_ps_output(out: &str) -> HashMap<u16, PublishedContainer> {
    let mut map: HashMap<u16, PublishedContainer> = HashMap::new();
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(ps) = serde_json::from_str::<PsLine>(line) else {
            continue;
        };
        // `Names` is comma-separated when a container has several names.
        let name = ps.names.split(',').next().unwrap_or_default().trim();
        let container = Container {
            id: ps.id,
            name: name.to_string(),
            image: ps.image,
        };
        for p in parse_docker_ports(&ps.ports) {
            map.entry(p.host_port)
                .or_insert_with(|| PublishedContainer {
                    container: container.clone(),
                    addrs: p.host_addrs.clone(),
                });
        }
    }
    map
}

/// How long `docker ps` may take before containers are treated as absent.
/// Docker Desktop on macOS in particular can hang instead of failing.
pub const PS_TIMEOUT: Duration = Duration::from_secs(2);

/// `docker ps` did not answer within [`PS_TIMEOUT`]. Returned as the error of
/// [`DockerContainers::containers`]; the scan goes on without containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockerTimeout;

impl fmt::Display for DockerTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Docker didn't respond within {}; containers not shown",
            human(PS_TIMEOUT)
        )
    }
}

impl std::error::Error for DockerTimeout {}

/// Outcome of one `docker ps` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PsRun {
    /// Its stdout.
    Output(String),
    /// Docker not installed, daemon not running, non-zero exit, bad UTF-8.
    Failed,
    /// Still running after [`PS_TIMEOUT`] (killed and reaped).
    TimedOut,
}

/// Runs `docker ps --format {{json .}}` with a [`PS_TIMEOUT`] deadline.
fn run_docker_ps() -> PsRun {
    let cmd = &mut Command::new("docker");
    match run_with_timeout(cmd.args(["ps", "--format", "{{json .}}"]), PS_TIMEOUT) {
        Err(RunError::Timeout(_)) => PsRun::TimedOut,
        Err(RunError::Io(_)) => PsRun::Failed,
        Ok(out) if !out.status.success() => PsRun::Failed,
        Ok(out) => String::from_utf8(out.stdout).map_or(PsRun::Failed, PsRun::Output),
    }
}

/// Produces one `docker ps` outcome. Boxed so tests can swap in a fake.
type Runner = Box<dyn FnMut() -> PsRun>;

/// A cached result: the port map, or the fact that docker timed out.
type Cached = Result<HashMap<u16, PublishedContainer>, DockerTimeout>;

/// [`ContainerSource`] backed by the docker CLI, with a 5 s cache.
pub struct DockerContainers {
    run: Runner,
    ttl: Duration,
    cache: Option<(Instant, Cached)>,
}

impl DockerContainers {
    pub fn new() -> Self {
        Self::with_runner(Box::new(run_docker_ps), CACHE_TTL)
    }

    /// Uses `run` instead of the real docker CLI (for tests).
    fn with_runner(run: Runner, ttl: Duration) -> Self {
        Self {
            run,
            ttl,
            cache: None,
        }
    }
}

impl Default for DockerContainers {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerSource for DockerContainers {
    /// The published ports, empty when docker is missing or fails, or
    /// `Err(DockerTimeout)` when it didn't answer in time. Every outcome,
    /// timeouts included, is cached for the TTL, so a hung Docker costs at
    /// most one 2 s wait per 5 s.
    fn containers(&mut self) -> anyhow::Result<HashMap<u16, PublishedContainer>> {
        let fresh = self
            .cache
            .as_ref()
            .filter(|(at, _)| at.elapsed() < self.ttl)
            .map(|(_, cached)| cached.clone());
        let result = match fresh {
            Some(cached) => cached,
            None => {
                let result = match (self.run)() {
                    PsRun::Output(s) => Ok(parse_ps_output(&s)),
                    PsRun::Failed => Ok(HashMap::new()),
                    PsRun::TimedOut => Err(DockerTimeout),
                };
                self.cache = Some((Instant::now(), result.clone()));
                result
            }
        };
        Ok(result?)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn ip(s: &str) -> IpAddr {
        match s.parse() {
            Ok(a) => a,
            Err(e) => panic!("bad test address {s}: {e}"),
        }
    }

    fn pp(host_port: u16, container_port: u16, addrs: &[&str]) -> PublishedPort {
        PublishedPort {
            host_port,
            container_port,
            host_addrs: addrs.iter().map(|a| ip(a)).collect(),
        }
    }

    #[test]
    fn single_port() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:5432->5432/tcp"),
            vec![pp(5432, 5432, &["0.0.0.0"])]
        );
    }

    #[test]
    fn host_port_differs_from_container_port() {
        assert_eq!(
            parse_docker_ports("127.0.0.1:15432->5432/tcp"),
            vec![pp(15432, 5432, &["127.0.0.1"])]
        );
    }

    #[test]
    fn ipv6_twin_is_merged() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:5432->5432/tcp, :::5432->5432/tcp"),
            vec![pp(5432, 5432, &["0.0.0.0", "::"])]
        );
    }

    #[test]
    fn bracketed_ipv6_twin_is_merged() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:8080->80/tcp, [::]:8080->80/tcp"),
            vec![pp(8080, 80, &["0.0.0.0", "::"])]
        );
    }

    #[test]
    fn ipv6_only_mapping() {
        assert_eq!(
            parse_docker_ports(":::3000->3000/tcp"),
            vec![pp(3000, 3000, &["::"])]
        );
    }

    #[test]
    fn range_is_expanded() {
        assert_eq!(
            parse_docker_ports("127.0.0.1:8000-8002->8000-8002/tcp"),
            vec![
                pp(8000, 8000, &["127.0.0.1"]),
                pp(8001, 8001, &["127.0.0.1"]),
                pp(8002, 8002, &["127.0.0.1"])
            ]
        );
    }

    #[test]
    fn range_with_offset_is_paired() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:9000-9001->80-81/tcp"),
            vec![pp(9000, 80, &["0.0.0.0"]), pp(9001, 81, &["0.0.0.0"])]
        );
    }

    #[test]
    fn unpublished_ports_are_ignored() {
        assert_eq!(parse_docker_ports("6379/tcp"), vec![]);
        assert_eq!(
            parse_docker_ports("6379/tcp, 0.0.0.0:5432->5432/tcp"),
            vec![pp(5432, 5432, &["0.0.0.0"])]
        );
    }

    #[test]
    fn udp_is_ignored() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:53->53/udp, 0.0.0.0:53->53/tcp"),
            vec![pp(53, 53, &["0.0.0.0"])]
        );
    }

    #[test]
    fn several_containers_ports_keep_input_order() {
        assert_eq!(
            parse_docker_ports(
                "0.0.0.0:8080->80/tcp, :::8080->80/tcp, 0.0.0.0:8443->443/tcp, :::8443->443/tcp"
            ),
            vec![
                pp(8080, 80, &["0.0.0.0", "::"]),
                pp(8443, 443, &["0.0.0.0", "::"])
            ]
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(parse_docker_ports(""), vec![]);
        assert_eq!(parse_docker_ports("   "), vec![]);
    }

    #[test]
    fn garbage_input_is_skipped_without_panicking() {
        let garbage = [
            "hello world",
            "->",
            ":->/tcp",
            "0.0.0.0:abc->5432/tcp",
            "0.0.0.0:5432->xyz/tcp",
            "0.0.0.0:99999->5432/tcp",
            "0.0.0.0:0->5432/tcp",
            "0.0.0.0:8002-8000->8002-8000/tcp",
            "0.0.0.0:8000-8002->80/tcp",
            "5432->5432/tcp",
            "0.0.0.0:5432->5432",
            "0.0.0.0:5432->5432/sctp",
            ",,,",
            "0.0.0.0:5432->5432/tcp->1/tcp",
            "🦀:5432->5432/tcp💥",
            "not-an-ip:5432->5432/tcp",
        ];
        for g in garbage {
            assert_eq!(parse_docker_ports(g), vec![], "input: {g:?}");
        }
    }

    #[test]
    fn garbage_next_to_valid_items_keeps_the_valid_ones() {
        assert_eq!(
            parse_docker_ports("nonsense, 0.0.0.0:5432->5432/tcp, 1-2-3"),
            vec![pp(5432, 5432, &["0.0.0.0"])]
        );
    }

    /// Two real `docker ps --format {{json .}}` lines (Docker 27, Linux).
    const PS_FIXTURE: &str = concat!(
        r#"{"Command":"\"docker-entrypoint.s…\"","CreatedAt":"2026-11-10 09:12:44 +0000 UTC","ID":"3f2a9c1d7e4b","Image":"postgres:16","Labels":"","LocalVolumes":"1","Mounts":"0b9c…","Names":"db","Networks":"bridge","Ports":"0.0.0.0:5432->5432/tcp, :::5432->5432/tcp","RunningFor":"2 days ago","Size":"0B","State":"running","Status":"Up 2 days"}"#,
        "\n",
        r#"{"Command":"\"docker-entrypoint.s…\"","CreatedAt":"2026-11-12 08:01:02 +0000 UTC","ID":"a1b2c3d4e5f6","Image":"redis:7-alpine","Labels":"","LocalVolumes":"0","Mounts":"","Names":"cache,cache-alias","Networks":"bridge","Ports":"6379/tcp, 127.0.0.1:16379->6379/tcp","RunningFor":"3 hours ago","Size":"0B","State":"running","Status":"Up 3 hours"}"#,
        "\n",
        r#"{"Command":"\"sleep infinity\"","ID":"0000aaaa1111","Image":"busybox","Names":"idle","Ports":"","State":"running","Status":"Up 1 minute"}"#,
        "\n",
    );

    #[test]
    fn ps_output_maps_host_ports_to_containers() {
        let map = parse_ps_output(PS_FIXTURE);
        assert_eq!(map.len(), 2);

        let db = &map[&5432];
        assert_eq!(db.container.id, "3f2a9c1d7e4b");
        assert_eq!(db.container.name, "db");
        assert_eq!(db.container.image, "postgres:16");
        assert_eq!(db.addrs, [ip("0.0.0.0"), ip("::")]);

        // Host port, not container port; first name only; unpublished skipped.
        let cache = &map[&16379];
        assert_eq!(cache.container.name, "cache");
        assert_eq!(cache.addrs, [ip("127.0.0.1")]);
        assert!(!map.contains_key(&6379));
    }

    #[test]
    fn ps_output_skips_bad_lines() {
        let out = format!(
            "not json\n{{}}\n[1,2]\n\n{}",
            r#"{"ID":"x1","Names":"web","Image":"nginx","Ports":"0.0.0.0:8080->80/tcp"}"#
        );
        let map = parse_ps_output(&out);
        assert_eq!(map.len(), 1);
        assert_eq!(map[&8080].container.name, "web");
    }

    #[test]
    fn ps_output_empty() {
        assert!(parse_ps_output("").is_empty());
    }

    /// A runner that counts calls and returns `output`.
    fn counting(output: PsRun) -> (Rc<Cell<u32>>, Runner) {
        let calls = Rc::new(Cell::new(0));
        let c = Rc::clone(&calls);
        let run = Box::new(move || {
            c.set(c.get() + 1);
            output.clone()
        });
        (calls, run)
    }

    #[test]
    fn results_are_cached_within_ttl() -> anyhow::Result<()> {
        let (calls, run) = counting(PsRun::Output(PS_FIXTURE.to_string()));
        let mut src = DockerContainers::with_runner(run, Duration::from_secs(60));
        assert_eq!(src.containers()?.len(), 2);
        assert_eq!(src.containers()?.len(), 2);
        assert_eq!(calls.get(), 1);
        Ok(())
    }

    #[test]
    fn cache_expires_after_ttl() -> anyhow::Result<()> {
        let (calls, run) = counting(PsRun::Output(PS_FIXTURE.to_string()));
        let mut src = DockerContainers::with_runner(run, Duration::ZERO);
        src.containers()?;
        src.containers()?;
        assert_eq!(calls.get(), 2);
        Ok(())
    }

    #[test]
    fn docker_failure_is_silently_empty_and_cached() -> anyhow::Result<()> {
        let (calls, run) = counting(PsRun::Failed);
        let mut src = DockerContainers::with_runner(run, Duration::from_secs(60));
        assert!(src.containers()?.is_empty());
        assert!(src.containers()?.is_empty());
        assert_eq!(calls.get(), 1);
        Ok(())
    }

    #[test]
    fn timeout_is_an_error_and_is_cached() {
        let (calls, run) = counting(PsRun::TimedOut);
        let mut src = DockerContainers::with_runner(run, Duration::from_secs(60));
        for _ in 0..3 {
            let err = src.containers().err();
            assert!(
                err.as_ref()
                    .and_then(|e| e.downcast_ref::<DockerTimeout>())
                    .is_some(),
                "{err:?}"
            );
        }
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn timeout_cache_expires_too() {
        let (calls, run) = counting(PsRun::TimedOut);
        let mut src = DockerContainers::with_runner(run, Duration::ZERO);
        let _ = src.containers();
        let _ = src.containers();
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn timeout_message() {
        assert_eq!(
            DockerTimeout.to_string(),
            "Docker didn't respond within 2 s; containers not shown"
        );
    }
}
