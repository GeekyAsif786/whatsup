//! Real [`SocketSource`] backed by the OS socket table via `netstat2`
//! (DESIGN.md section 7.1): netlink + /proc on Linux, libproc on macOS,
//! `GetExtendedTcpTable` on Windows. No `lsof` / `ss` / `netstat` parsing.

use anyhow::Context;
use netstat2::{
    AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState, iterate_sockets_info,
};

use super::{Listener, SocketSource};

/// Lists TCP sockets in LISTEN state, IPv4 and IPv6.
#[derive(Debug, Default)]
pub struct NativeSockets;

impl SocketSource for NativeSockets {
    fn listeners(&mut self) -> anyhow::Result<Vec<Listener>> {
        let af = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
        let iter = iterate_sockets_info(af, ProtocolFlags::TCP)
            .context("could not read the OS socket table")?;
        // A socket can vanish while we read the table; skip entries that
        // fail instead of failing the whole scan.
        Ok(to_listeners(
            iter.filter_map(Result::ok)
                .map(|s| (s.protocol_socket_info, s.associated_pids)),
        ))
    }
}

/// Keeps TCP LISTEN sockets and emits one [`Listener`] per owning PID.
///
/// A socket shared by several processes (forked servers such as gunicorn
/// workers) yields one listener per PID. A socket with no visible owner
/// (another user's process without privileges) yields one with `pid: None`.
/// Exact duplicates are dropped; the order of first appearance is kept.
///
/// Takes `(socket, owning pids)` pairs rather than `netstat2::SocketInfo`,
/// whose other fields differ per OS, so tests can build inputs everywhere.
fn to_listeners(
    sockets: impl IntoIterator<Item = (ProtocolSocketInfo, Vec<u32>)>,
) -> Vec<Listener> {
    let mut out: Vec<Listener> = Vec::new();
    for (info, owners) in sockets {
        let ProtocolSocketInfo::Tcp(tcp) = info else {
            continue;
        };
        if tcp.state != TcpState::Listen {
            continue;
        }
        let pids: Vec<Option<u32>> = if owners.is_empty() {
            vec![None]
        } else {
            owners.into_iter().map(Some).collect()
        };
        for pid in pids {
            let l = Listener {
                addr: tcp.local_addr,
                port: tcp.local_port,
                pid,
            };
            if !out.contains(&l) {
                out.push(l);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener};

    use netstat2::{TcpSocketInfo, UdpSocketInfo};

    use super::*;

    fn tcp(
        addr: IpAddr,
        port: u16,
        state: TcpState,
        pids: &[u32],
    ) -> (ProtocolSocketInfo, Vec<u32>) {
        let info = ProtocolSocketInfo::Tcp(TcpSocketInfo {
            local_addr: addr,
            local_port: port,
            remote_addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            remote_port: 0,
            state,
        });
        (info, pids.to_vec())
    }

    const LO4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const ANY6: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);

    #[test]
    fn keeps_only_tcp_listen() {
        let udp = (
            ProtocolSocketInfo::Udp(UdpSocketInfo {
                local_addr: LO4,
                local_port: 53,
            }),
            vec![1],
        );
        let got = to_listeners([
            tcp(LO4, 5173, TcpState::Listen, &[10]),
            tcp(LO4, 5173, TcpState::Established, &[10]),
            tcp(LO4, 6000, TcpState::TimeWait, &[]),
            udp,
        ]);
        assert_eq!(
            got,
            [Listener {
                addr: LO4,
                port: 5173,
                pid: Some(10)
            }]
        );
    }

    #[test]
    fn hidden_owner_is_none() {
        let got = to_listeners([tcp(ANY6, 22, TcpState::Listen, &[])]);
        assert_eq!(got[0].pid, None);
        assert_eq!(got[0].addr, ANY6);
    }

    #[test]
    fn shared_socket_yields_one_listener_per_pid() {
        let got = to_listeners([tcp(LO4, 8000, TcpState::Listen, &[100, 101, 100])]);
        let pids: Vec<_> = got.iter().map(|l| l.pid).collect();
        assert_eq!(pids, [Some(100), Some(101)]);
    }

    /// Real OS socket table: a listener we open ourselves must show up with
    /// our own PID, for IPv4 and (where the host supports it) IPv6.
    #[test]
    fn finds_our_own_listener() -> anyhow::Result<()> {
        let v4 = TcpListener::bind("127.0.0.1:0")?;
        let port = v4.local_addr()?.port();
        let me = std::process::id();
        let found = NativeSockets.listeners()?;
        assert!(
            found
                .iter()
                .any(|l| l.port == port && l.pid == Some(me) && l.addr == LO4),
            "port {port} not found in {found:?}"
        );

        if let Ok(v6) = TcpListener::bind("[::1]:0") {
            let port6 = v6.local_addr()?.port();
            let found = NativeSockets.listeners()?;
            assert!(
                found
                    .iter()
                    .any(|l| l.port == port6 && l.addr == IpAddr::V6(Ipv6Addr::LOCALHOST)),
                "IPv6 port {port6} not found"
            );
        }
        Ok(())
    }
}
