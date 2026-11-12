//! Fixed entries for the state-machine and snapshot tests, shaped like the
//! DESIGN.md section 13 mock. Sorted the way the scanner sorts: dev first,
//! then by port.

use crate::model::{Container, Entry, Exposure, Kind};

fn base(port: u16, pid: Option<u32>, process: &str, kind: Kind) -> Entry {
    Entry {
        port,
        pid,
        process: process.to_string(),
        kind,
        exposure: Exposure::Local,
        addrs: vec!["127.0.0.1".to_string()],
        command: String::new(),
        cwd: None,
        uptime_secs: None,
        memory_mb: None,
        dev: true,
        container: None,
    }
}

pub fn nextjs() -> Entry {
    Entry {
        command: "node /home/me/code/web/node_modules/.bin/next dev".to_string(),
        cwd: Some("/home/me/code/web".to_string()),
        uptime_secs: Some(7200),
        memory_mb: Some(412),
        addrs: vec!["127.0.0.1".to_string(), "::1".to_string()],
        ..base(3000, Some(41822), "node", Kind::NextJs)
    }
}

pub fn vite() -> Entry {
    Entry {
        command: "node /home/me/code/site/node_modules/.bin/vite".to_string(),
        cwd: Some("/home/me/code/site".to_string()),
        uptime_secs: Some(840),
        memory_mb: Some(1536),
        ..base(5173, Some(41901), "node", Kind::Vite)
    }
}

pub fn postgres_container() -> Entry {
    Entry {
        exposure: Exposure::Network,
        addrs: vec!["*".to_string()],
        uptime_secs: Some(3 * 86_400),
        container: Some(Container {
            id: "3f2a9c1d7e4b".to_string(),
            name: "db".to_string(),
            image: "postgres:16".to_string(),
        }),
        ..base(5432, None, "db (postgres:16)", Kind::Docker)
    }
}

pub fn uvicorn() -> Entry {
    Entry {
        exposure: Exposure::Network,
        addrs: vec!["*".to_string()],
        command: "python -m uvicorn main:app --reload --host 0.0.0.0".to_string(),
        cwd: Some("/home/me/code/api".to_string()),
        uptime_secs: Some(42),
        memory_mb: Some(18),
        ..base(8000, Some(5150), "python3", Kind::FastApi)
    }
}

pub fn spotify() -> Entry {
    Entry {
        exposure: Exposure::Network,
        addrs: vec!["*".to_string()],
        command: "/Applications/Spotify.app/Contents/MacOS/Spotify".to_string(),
        uptime_secs: Some(90_000),
        memory_mb: Some(260),
        dev: false,
        ..base(57621, Some(812), "Spotify", Kind::Other)
    }
}

/// A second owner of a port, e.g. a pre-forked worker.
pub fn worker(port: u16, pid: u32) -> Entry {
    Entry {
        command: "node server.js".to_string(),
        ..base(port, Some(pid), "node", Kind::Node)
    }
}

/// Four dev rows (two exposed) and one non-dev row.
pub fn entries() -> Vec<Entry> {
    vec![nextjs(), vite(), postgres_container(), uvicorn(), spotify()]
}
