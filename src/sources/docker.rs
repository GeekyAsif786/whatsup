//! Docker container source (DESIGN.md section 7.3).
//!
//! M0 contains only the parser for the `Ports` column of
//! `docker ps --format '{{json .}}'`. Running docker (with a 5 s cache) comes
//! in M1.

/// One published TCP port: `host_port` on the machine maps to
/// `container_port` inside the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublishedPort {
    pub host_port: u16,
    pub container_port: u16,
}

/// Parses docker's `Ports` string into the published TCP ports.
///
/// Handles `0.0.0.0:5432->5432/tcp`, IPv6 twins (`:::5432->...` and
/// `[::]:5432->...`, deduplicated with their IPv4 twin) and ranges
/// (`127.0.0.1:8000-8002->8000-8002/tcp`, expanded pairwise). Unpublished
/// ports (`6379/tcp`), UDP mappings (no UDP in v0.1) and anything malformed
/// are skipped. Results are in input order.
pub fn parse_docker_ports(ports: &str) -> Vec<PublishedPort> {
    let mut out: Vec<PublishedPort> = Vec::new();
    for item in ports.split(',') {
        for p in parse_item(item.trim()).unwrap_or_default() {
            if !out.contains(&p) {
                out.push(p);
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
    let (_ip, host_ports) = host.rsplit_once(':')?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn pp(host_port: u16, container_port: u16) -> PublishedPort {
        PublishedPort {
            host_port,
            container_port,
        }
    }

    #[test]
    fn single_port() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:5432->5432/tcp"),
            vec![pp(5432, 5432)]
        );
    }

    #[test]
    fn host_port_differs_from_container_port() {
        assert_eq!(
            parse_docker_ports("127.0.0.1:15432->5432/tcp"),
            vec![pp(15432, 5432)]
        );
    }

    #[test]
    fn ipv6_twin_is_deduplicated() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:5432->5432/tcp, :::5432->5432/tcp"),
            vec![pp(5432, 5432)]
        );
    }

    #[test]
    fn bracketed_ipv6_twin_is_deduplicated() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:8080->80/tcp, [::]:8080->80/tcp"),
            vec![pp(8080, 80)]
        );
    }

    #[test]
    fn ipv6_only_mapping() {
        assert_eq!(
            parse_docker_ports(":::3000->3000/tcp"),
            vec![pp(3000, 3000)]
        );
    }

    #[test]
    fn range_is_expanded() {
        assert_eq!(
            parse_docker_ports("127.0.0.1:8000-8002->8000-8002/tcp"),
            vec![pp(8000, 8000), pp(8001, 8001), pp(8002, 8002)]
        );
    }

    #[test]
    fn range_with_offset_is_paired() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:9000-9001->80-81/tcp"),
            vec![pp(9000, 80), pp(9001, 81)]
        );
    }

    #[test]
    fn unpublished_ports_are_ignored() {
        assert_eq!(parse_docker_ports("6379/tcp"), vec![]);
        assert_eq!(
            parse_docker_ports("6379/tcp, 0.0.0.0:5432->5432/tcp"),
            vec![pp(5432, 5432)]
        );
    }

    #[test]
    fn udp_is_ignored() {
        assert_eq!(
            parse_docker_ports("0.0.0.0:53->53/udp, 0.0.0.0:53->53/tcp"),
            vec![pp(53, 53)]
        );
    }

    #[test]
    fn several_containers_ports_keep_input_order() {
        assert_eq!(
            parse_docker_ports(
                "0.0.0.0:8080->80/tcp, :::8080->80/tcp, 0.0.0.0:8443->443/tcp, :::8443->443/tcp"
            ),
            vec![pp(8080, 80), pp(8443, 443)]
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
        ];
        for g in garbage {
            assert_eq!(parse_docker_ports(g), vec![], "input: {g:?}");
        }
    }

    #[test]
    fn garbage_next_to_valid_items_keeps_the_valid_ones() {
        assert_eq!(
            parse_docker_ports("nonsense, 0.0.0.0:5432->5432/tcp, 1-2-3"),
            vec![pp(5432, 5432)]
        );
    }
}
