//! Data model shared by every frontend (docs/DESIGN.md sections 6 and 11.5).
//!
//! Everything here is plain data and serialises with serde. The JSON shape of
//! [`Entry`] is a public contract: adding fields is fine, renaming or removing
//! them requires bumping [`SCHEMA_VERSION`].

use std::fmt;

use serde::{Serialize, Serializer};

/// Version of the `whatsup list --json` schema (DESIGN.md section 6).
pub const SCHEMA_VERSION: u32 = 1;

/// One row: a TCP port in LISTEN state and what owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// Local port in LISTEN state.
    pub port: u16,
    /// `None` when the OS hides it (other user, no privileges).
    pub pid: Option<u32>,
    /// Process name, or `container-name (image)` for Docker.
    pub process: String,
    /// What the listener is, in human terms (section 8).
    pub kind: Kind,
    /// `local` when every bound address is loopback (section 10).
    pub exposure: Exposure,
    /// Bound addresses; `*` for unspecified (`0.0.0.0` / `::`).
    pub addrs: Vec<String>,
    /// Full argv joined; may be empty if hidden.
    pub command: String,
    /// Working directory of the process, when the OS reveals it.
    pub cwd: Option<String>,
    /// Process run time in seconds.
    pub uptime_secs: Option<u64>,
    /// Resident memory in MiB.
    pub memory_mb: Option<u64>,
    /// Result of the dev heuristic (section 9).
    pub dev: bool,
    /// Set when the port is published by a running container.
    pub container: Option<Container>,
}

/// Whether a listener can be reached from other machines (section 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    /// Every bound address is loopback.
    Local,
    /// At least one bound address is not loopback (`0.0.0.0`, `::`, a LAN IP).
    Network,
}

/// A running container that publishes a host port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
}

/// The kind labels from the classification table (DESIGN.md section 8).
///
/// Serialises as the plain string from [`Kind::as_str`], so the JSON field
/// stays a string (`"next.js"`, `"python http"`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    // 1. Frontend frameworks
    Vite,
    NextJs,
    Webpack,
    ReactScripts,
    Astro,
    Nuxt,
    Expo,
    Metro,
    // 2. Backend frameworks
    Django,
    FastApi,
    Flask,
    Gunicorn,
    Rails,
    Puma,
    Laravel,
    Spring,
    // 3. Tools
    Jupyter,
    Streamlit,
    PythonHttp,
    Ollama,
    // 4. Databases / infra
    Postgres,
    Mysql,
    Mariadb,
    Redis,
    Mongodb,
    Nginx,
    Caddy,
    // 5. Container plumbing
    Docker,
    // 6. Generic runtimes
    Node,
    Bun,
    Deno,
    Python,
    Ruby,
    Php,
    Java,
    Dotnet,
    Go,
    // 7. Fallback
    Other,
}

impl Kind {
    /// The label shown to users and written to JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Vite => "vite",
            Kind::NextJs => "next.js",
            Kind::Webpack => "webpack",
            Kind::ReactScripts => "react-scripts",
            Kind::Astro => "astro",
            Kind::Nuxt => "nuxt",
            Kind::Expo => "expo",
            Kind::Metro => "metro",
            Kind::Django => "django",
            Kind::FastApi => "fastapi",
            Kind::Flask => "flask",
            Kind::Gunicorn => "gunicorn",
            Kind::Rails => "rails",
            Kind::Puma => "puma",
            Kind::Laravel => "laravel",
            Kind::Spring => "spring",
            Kind::Jupyter => "jupyter",
            Kind::Streamlit => "streamlit",
            Kind::PythonHttp => "python http",
            Kind::Ollama => "ollama",
            Kind::Postgres => "postgres",
            Kind::Mysql => "mysql",
            Kind::Mariadb => "mariadb",
            Kind::Redis => "redis",
            Kind::Mongodb => "mongodb",
            Kind::Nginx => "nginx",
            Kind::Caddy => "caddy",
            Kind::Docker => "docker",
            Kind::Node => "node",
            Kind::Bun => "bun",
            Kind::Deno => "deno",
            Kind::Python => "python",
            Kind::Ruby => "ruby",
            Kind::Php => "php",
            Kind::Java => "java",
            Kind::Dotnet => "dotnet",
            Kind::Go => "go",
            Kind::Other => "other",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Kind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The `{"schema": 1, "entries": [...]}` wrapper for `whatsup list --json`.
#[derive(Debug, Serialize)]
pub struct ListOutput<'a> {
    pub schema: u32,
    pub entries: &'a [Entry],
}

impl<'a> ListOutput<'a> {
    /// Wraps `entries` with the current [`SCHEMA_VERSION`].
    pub fn new(entries: &'a [Entry]) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            entries,
        }
    }
}

/// What `stop()` targets (section 11.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StopMode {
    /// Stop only the process that owns the listening socket (v0.1 default).
    #[default]
    Listener,
    /// Stop the nearest dev-command ancestor and its descendants (v0.2).
    Tree,
}

/// A process identified by PID and name, e.g. the parent that respawned a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcRef {
    pub pid: u32,
    pub name: String,
}

/// Outcome of `stop()` (section 11.5). Exit codes are in section 12.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum StopResult {
    /// The process exited (or became a zombie) after the graceful stop.
    Stopped,
    /// The process ignored the graceful stop and was killed (`--force`).
    ForceKilled,
    /// The process is still alive after the grace period; suggest `--force`.
    StillRunning,
    /// The port was busy again shortly after stopping, with a new PID
    /// (`None` when the new owner's PID is hidden).
    Respawned {
        pid: Option<u32>,
        parent: Option<ProcRef>,
    },
    /// The stop could not be attempted or failed.
    Failed { reason: FailReason },
}

impl StopResult {
    /// Process exit code for `whatsup kill` (DESIGN.md section 12).
    pub const fn exit_code(&self) -> i32 {
        match self {
            StopResult::Stopped | StopResult::ForceKilled => 0,
            StopResult::Failed { .. } => 1,
            StopResult::StillRunning | StopResult::Respawned { .. } => 3,
        }
    }
}

/// Why a stop failed. `Display` gives the short, actionable message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum FailReason {
    /// The OS did not reveal the owning PID.
    PidHidden,
    /// The process belongs to another user.
    PermissionDenied,
    /// Between scan and stop, the PID stopped owning the port.
    PidReused,
    /// Anything else, e.g. `docker stop`'s stderr.
    Other(String),
}

impl fmt::Display for FailReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FailReason::PidHidden => {
                f.write_str("PID hidden — run with sudo / elevated terminal")
            }
            FailReason::PermissionDenied => f.write_str(
                "permission denied: the process belongs to another user — run with sudo / elevated terminal",
            ),
            FailReason::PidReused => {
                f.write_str("the process no longer owns the port — refresh and try again")
            }
            FailReason::Other(msg) => f.write_str(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vite_entry() -> Entry {
        Entry {
            port: 5173,
            pid: Some(41822),
            process: "node".to_string(),
            kind: Kind::Vite,
            exposure: Exposure::Local,
            addrs: vec!["127.0.0.1".to_string(), "::1".to_string()],
            command: "node /home/me/web/node_modules/.bin/vite".to_string(),
            cwd: Some("/home/me/web".to_string()),
            uptime_secs: Some(7200),
            memory_mb: Some(412),
            dev: true,
            container: None,
        }
    }

    fn docker_entry() -> Entry {
        Entry {
            port: 5432,
            pid: None,
            process: "db (postgres:16)".to_string(),
            kind: Kind::Docker,
            exposure: Exposure::Network,
            addrs: vec!["*".to_string()],
            command: String::new(),
            cwd: None,
            uptime_secs: None,
            memory_mb: None,
            dev: true,
            container: Some(Container {
                id: "3f2a9c1d7e4b".to_string(),
                name: "db".to_string(),
                image: "postgres:16".to_string(),
            }),
        }
    }

    /// Snapshot of the schema-1 JSON contract. If this test fails because a
    /// field was renamed or removed, bump SCHEMA_VERSION and update README.
    #[test]
    fn list_json_shape_is_schema_1() -> Result<(), serde_json::Error> {
        let entries = [vite_entry(), docker_entry()];
        let json = serde_json::to_string_pretty(&ListOutput::new(&entries))?;
        let expected = r#"{
  "schema": 1,
  "entries": [
    {
      "port": 5173,
      "pid": 41822,
      "process": "node",
      "kind": "vite",
      "exposure": "local",
      "addrs": [
        "127.0.0.1",
        "::1"
      ],
      "command": "node /home/me/web/node_modules/.bin/vite",
      "cwd": "/home/me/web",
      "uptime_secs": 7200,
      "memory_mb": 412,
      "dev": true,
      "container": null
    },
    {
      "port": 5432,
      "pid": null,
      "process": "db (postgres:16)",
      "kind": "docker",
      "exposure": "network",
      "addrs": [
        "*"
      ],
      "command": "",
      "cwd": null,
      "uptime_secs": null,
      "memory_mb": null,
      "dev": true,
      "container": {
        "id": "3f2a9c1d7e4b",
        "name": "db",
        "image": "postgres:16"
      }
    }
  ]
}"#;
        assert_eq!(json, expected);
        Ok(())
    }

    #[test]
    fn empty_list_still_has_schema() -> Result<(), serde_json::Error> {
        let json = serde_json::to_string(&ListOutput::new(&[]))?;
        assert_eq!(json, r#"{"schema":1,"entries":[]}"#);
        Ok(())
    }

    #[test]
    fn kind_serialises_as_its_label() -> Result<(), serde_json::Error> {
        assert_eq!(serde_json::to_string(&Kind::NextJs)?, r#""next.js""#);
        assert_eq!(
            serde_json::to_string(&Kind::PythonHttp)?,
            r#""python http""#
        );
        assert_eq!(Kind::Other.to_string(), "other");
        Ok(())
    }

    #[test]
    fn stop_mode_defaults_to_listener() {
        assert_eq!(StopMode::default(), StopMode::Listener);
    }

    #[test]
    fn stop_result_exit_codes_match_spec_section_12() {
        let respawned = StopResult::Respawned {
            pid: Some(4242),
            parent: Some(ProcRef {
                pid: 4000,
                name: "nodemon".to_string(),
            }),
        };
        let cases = [
            (StopResult::Stopped, 0),
            (StopResult::ForceKilled, 0),
            (
                StopResult::Failed {
                    reason: FailReason::PidHidden,
                },
                1,
            ),
            (StopResult::StillRunning, 3),
            (respawned, 3),
        ];
        for (result, code) in cases {
            assert_eq!(result.exit_code(), code, "{result:?}");
        }
    }

    #[test]
    fn stop_result_json_is_tagged() -> Result<(), serde_json::Error> {
        let failed = StopResult::Failed {
            reason: FailReason::Other("no such container".to_string()),
        };
        assert_eq!(
            serde_json::to_string(&failed)?,
            r#"{"result":"failed","reason":{"code":"other","detail":"no such container"}}"#
        );
        let respawned = StopResult::Respawned {
            pid: Some(7),
            parent: None,
        };
        assert_eq!(
            serde_json::to_string(&respawned)?,
            r#"{"result":"respawned","pid":7,"parent":null}"#
        );
        let hidden = StopResult::Respawned {
            pid: None,
            parent: None,
        };
        assert_eq!(
            serde_json::to_string(&hidden)?,
            r#"{"result":"respawned","pid":null,"parent":null}"#
        );
        assert_eq!(serde_json::to_string(&StopMode::Tree)?, r#""tree""#);
        Ok(())
    }

    #[test]
    fn fail_reasons_have_actionable_messages() {
        assert!(FailReason::PidHidden.to_string().contains("sudo"));
        assert!(FailReason::PermissionDenied.to_string().contains("sudo"));
        assert!(FailReason::PidReused.to_string().contains("refresh"));
        assert_eq!(FailReason::Other("boom".to_string()).to_string(), "boom");
    }
}
