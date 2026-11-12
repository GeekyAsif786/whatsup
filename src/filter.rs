//! Interpretation rules: the dev heuristic (DESIGN.md section 9) and exposure
//! detection (section 10).
//!
//! Pure code: no I/O, no clocks, no process spawning. Facts that need the OS
//! (such as "is this my user?") are computed by the caller and passed in.

use std::net::IpAddr;

use crate::model::{Entry, Exposure, Kind};

/// Executable locations that mark a process as part of the OS (section 9).
const SYSTEM_PATHS: &[&str] = &[
    "/System",
    "/usr/libexec",
    "/usr/sbin",
    "/sbin",
    "C:\\Windows",
    "/lib/systemd",
];

/// The 'dev vs noise' heuristic from section 9:
///
/// ```text
/// dev = kind != "other"
///    || ( same_user_as_whatsup
///         && 1024 <= port < 10000
///         && exe not under a system path )
/// ```
///
/// `exe` is `None` when the OS hides it; an unknown exe is not treated as
/// being under a system path.
pub fn is_dev(kind: Kind, port: u16, same_user: bool, exe: Option<&str>) -> bool {
    kind != Kind::Other
        || (same_user && (1024..10000).contains(&port) && !exe.is_some_and(is_system_path))
}

/// True when `exe` lives under one of [`SYSTEM_PATHS`].
///
/// Case-insensitive (macOS and Windows file systems usually are), and only
/// matches whole path components: `/sbin/x` matches `/sbin`, `/sbinx` does not.
fn is_system_path(exe: &str) -> bool {
    SYSTEM_PATHS.iter().any(|root| {
        exe.get(..root.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(root))
            && matches!(exe.as_bytes().get(root.len()), None | Some(b'/' | b'\\'))
    })
}

/// Exposure from section 10: `Local` if every bound address is loopback
/// (127.0.0.0/8, ::1), `Network` otherwise (including 0.0.0.0, :: and LAN IPs).
///
/// IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`) are judged by their IPv4
/// form. An empty list is `Network`: with no evidence that the listener is
/// loopback-only, fail safe and flag it.
pub fn exposure(addrs: &[IpAddr]) -> Exposure {
    if !addrs.is_empty() && addrs.iter().all(|a| a.to_canonical().is_loopback()) {
        Exposure::Local
    } else {
        Exposure::Network
    }
}

/// The TUI's `/` filter (DESIGN.md section 13): a case-insensitive substring
/// of the port (`:5173` or `5173`), process, kind or command. Surrounding
/// whitespace is ignored; an empty query matches everything.
pub fn matches_query(entry: &Entry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    format!(":{}", entry.port).contains(&q)
        || entry.process.to_lowercase().contains(&q)
        || entry.kind.as_str().contains(&q)
        || entry.command.to_lowercase().contains(&q)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    struct Row {
        case: &'static str,
        kind: Kind,
        port: u16,
        same_user: bool,
        exe: Option<&'static str>,
        dev: bool,
    }

    /// Every row of the DESIGN.md section 9 table, then the boundaries.
    const ROWS: &[Row] = &[
        Row {
            case: "Your Vite server on 5173: recognised kind",
            kind: Kind::Vite,
            port: 5173,
            same_user: true,
            exe: Some("/usr/local/bin/node"),
            dev: true,
        },
        Row {
            case: "Your custom Go API on 8080 (unrecognised): your process on a dev port",
            kind: Kind::Other,
            port: 8080,
            same_user: true,
            exe: Some("/home/me/code/api/bin/api"),
            dev: true,
        },
        Row {
            case: "Spotify on a 57621-style port: high ephemeral port",
            kind: Kind::Other,
            port: 57621,
            same_user: true,
            exe: Some("/Applications/Spotify.app/Contents/MacOS/Spotify"),
            dev: false,
        },
        Row {
            case: "Discord on a high ephemeral port",
            kind: Kind::Other,
            port: 58231,
            same_user: true,
            exe: Some("/Applications/Discord.app/Contents/MacOS/Discord"),
            dev: false,
        },
        Row {
            case: "sshd: other user and system path",
            kind: Kind::Other,
            port: 22,
            same_user: false,
            exe: Some("/usr/sbin/sshd"),
            dev: false,
        },
        Row {
            case: "cupsd: other user and system path",
            kind: Kind::Other,
            port: 631,
            same_user: false,
            exe: Some("/usr/sbin/cupsd"),
            dev: false,
        },
        Row {
            case: "rapportd: own user, dev-range port, but system path",
            kind: Kind::Other,
            port: 5000,
            same_user: true,
            exe: Some("/usr/libexec/rapportd"),
            dev: false,
        },
        Row {
            case: "Postgres running as the postgres user: recognised kind",
            kind: Kind::Postgres,
            port: 5432,
            same_user: false,
            exe: Some("/usr/lib/postgresql/16/bin/postgres"),
            dev: true,
        },
        // Boundaries and the remaining system paths.
        Row {
            case: "port 1023 is below the dev range",
            kind: Kind::Other,
            port: 1023,
            same_user: true,
            exe: None,
            dev: false,
        },
        Row {
            case: "port 1024 is the first dev port",
            kind: Kind::Other,
            port: 1024,
            same_user: true,
            exe: None,
            dev: true,
        },
        Row {
            case: "port 9999 is the last dev port",
            kind: Kind::Other,
            port: 9999,
            same_user: true,
            exe: None,
            dev: true,
        },
        Row {
            case: "port 10000 is above the dev range",
            kind: Kind::Other,
            port: 10000,
            same_user: true,
            exe: None,
            dev: false,
        },
        Row {
            case: "unrecognised, other user, dev port",
            kind: Kind::Other,
            port: 8080,
            same_user: false,
            exe: Some("/home/alice/bin/server"),
            dev: false,
        },
        Row {
            case: "macOS /System path",
            kind: Kind::Other,
            port: 7000,
            same_user: true,
            exe: Some(
                "/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter",
            ),
            dev: false,
        },
        Row {
            case: "/sbin path",
            kind: Kind::Other,
            port: 1234,
            same_user: true,
            exe: Some("/sbin/launchd"),
            dev: false,
        },
        Row {
            case: "Windows path, any case",
            kind: Kind::Other,
            port: 5040,
            same_user: true,
            exe: Some("c:\\windows\\System32\\svchost.exe"),
            dev: false,
        },
        Row {
            case: "/lib/systemd path",
            kind: Kind::Other,
            port: 5355,
            same_user: true,
            exe: Some("/lib/systemd/systemd-resolved"),
            dev: false,
        },
        Row {
            case: "a sibling of a system path is not a system path",
            kind: Kind::Other,
            port: 3000,
            same_user: true,
            exe: Some("/sbinx/server"),
            dev: true,
        },
        Row {
            case: "recognised kind wins even under a system path",
            kind: Kind::Nginx,
            port: 80,
            same_user: false,
            exe: Some("/usr/sbin/nginx"),
            dev: true,
        },
    ];

    #[test]
    fn dev_heuristic_table() {
        for row in ROWS {
            assert_eq!(
                is_dev(row.kind, row.port, row.same_user, row.exe),
                row.dev,
                "{}",
                row.case
            );
        }
    }

    fn ip(s: &str) -> IpAddr {
        match s.parse() {
            Ok(a) => a,
            Err(e) => panic!("bad test address {s}: {e}"),
        }
    }

    #[test]
    fn ipv4_loopback_is_local() {
        assert_eq!(
            exposure(&[IpAddr::V4(Ipv4Addr::LOCALHOST)]),
            Exposure::Local
        );
        // The whole 127.0.0.0/8 block is loopback.
        assert_eq!(exposure(&[ip("127.0.0.53")]), Exposure::Local);
    }

    #[test]
    fn ipv6_loopback_is_local() {
        assert_eq!(
            exposure(&[IpAddr::V6(Ipv6Addr::LOCALHOST)]),
            Exposure::Local
        );
    }

    #[test]
    fn loopback_twins_are_local() {
        assert_eq!(exposure(&[ip("127.0.0.1"), ip("::1")]), Exposure::Local);
    }

    #[test]
    fn ipv4_unspecified_is_network() {
        assert_eq!(exposure(&[ip("0.0.0.0")]), Exposure::Network);
    }

    #[test]
    fn ipv6_unspecified_is_network() {
        assert_eq!(exposure(&[ip("::")]), Exposure::Network);
    }

    #[test]
    fn lan_ip_is_network() {
        assert_eq!(exposure(&[ip("192.168.1.42")]), Exposure::Network);
        assert_eq!(
            exposure(&[ip("fe80::1c2b:3aff:fe4d:5e6f")]),
            Exposure::Network
        );
    }

    #[test]
    fn any_non_loopback_address_makes_it_network() {
        assert_eq!(exposure(&[ip("127.0.0.1"), ip("::")]), Exposure::Network);
    }

    #[test]
    fn ipv4_mapped_loopback_is_local() {
        assert_eq!(exposure(&[ip("::ffff:127.0.0.1")]), Exposure::Local);
        assert_eq!(exposure(&[ip("::ffff:0.0.0.0")]), Exposure::Network);
    }

    #[test]
    fn empty_address_list_is_network() {
        assert_eq!(exposure(&[]), Exposure::Network);
    }

    #[test]
    fn query_matches_port_process_kind_and_command() {
        let e = Entry {
            port: 5173,
            pid: Some(41822),
            process: "node".to_string(),
            kind: Kind::Vite,
            exposure: Exposure::Local,
            addrs: vec!["127.0.0.1".to_string()],
            command: "node /home/me/Web/node_modules/.bin/vite".to_string(),
            cwd: Some("/home/me/web".to_string()),
            uptime_secs: None,
            memory_mb: None,
            dev: true,
            container: None,
        };
        for q in [
            "",
            "  ",
            "5173",
            ":5173",
            ":51",
            "73",
            "NODE",
            "vit",
            "/home/me/web",
            " vite ",
        ] {
            assert!(matches_query(&e, q), "{q:?}");
        }
        for q in ["5174", "::5173", "python", "django", "127.0.0.1"] {
            assert!(!matches_query(&e, q), "{q:?}");
        }
    }
}
