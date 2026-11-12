# whatsup

Every port, dev server, database and container on your machine in one terminal view: see what it is, whether the network can reach it, and stop it.

**Work in progress** (v0.1 milestones in [docs/DESIGN.md](docs/DESIGN.md)). Linux, macOS and Windows.

## Usage

```console
$ whatsup list            # dev servers only; alias: whatsup ls
PORT    PID    KIND         PROCESS            EXPOSED  UPTIME
:5173   41822  vite         node               local    2h
:8000   5150   python http  python3            network  42s
:5432   1203   docker       db (postgres:16)   network  3d
⚠ 2 listeners are exposed to the network (reachable from other machines if the firewall allows)

$ whatsup list --all      # every listener, including system services
$ whatsup port 5173       # what is on :5173? exit 0 = in use, 1 = free
$ whatsup kill :5173      # asks, then stops it; -y skips the prompt, -f force-kills
```

## Interactive view

`whatsup` on its own opens a live view of the same list (when output is piped, it prints the
list instead). It refreshes every 2 s, keeps your selection on the same port and PID, and shows
the command, working directory, addresses and container of the selected row. `--all` and
`--no-color` / `NO_COLOR` apply here too.

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | Move the selection (wraps) |
| `x` / `Del` | Stop what is on the selected port: `y` stops, `Y` force-kills, any other key cancels |
| `o` / `Enter` | Open `http://localhost:<port>` in your browser (not offered for databases) |
| `/` | Filter by port, process, kind or command; `Enter` keeps the filter, `Esc` clears it |
| `a` | Toggle dev servers only / every listener |
| `r` | Refresh now |
| `q` / `Esc` / `Ctrl-C` | Quit (`Ctrl-C` works in every mode) |

Stopping works exactly like `whatsup kill`: every owner of the port is stopped, and the results
(including "still running" and "respawned by ...") appear at the bottom for 4 s. Auto-refresh
pauses while the stop prompt is open, so rows never move under it. The view needs a terminal
of at least 60×12.

**Exposed** means the listener is bound to a non-loopback address (`0.0.0.0`, `::`, a LAN IP),
so other machines on your network can reach it unless a firewall blocks them.

`kill` sends SIGTERM, waits up to 3 s, and with `-f` then sends SIGKILL. Containers are
stopped with `docker stop`. On Windows there is no graceful signal, so the process is
terminated immediately. If something restarts the server within 1.5 s (nodemon,
`uvicorn --reload`, ...), `kill` says so and names the parent.

| Command | Exit code |
|---|---|
| `list` | 0; 1 on error |
| `port <n>` | 0 in use, 1 free |
| `kill <n>` | 0 stopped (or nothing there), 1 failed or declined, 2 no terminal to confirm in (pass `-y`), 3 still running or respawned, 130 Ctrl-C at the prompt |

Options: `--all`, `--no-color` (and the `NO_COLOR` environment variable), `--version`.
Colour is never used when output is piped. Other users' processes show no PID unless you run
with sudo / an elevated terminal; whatsup never elevates itself.

## JSON

`whatsup list --json` prints a stable schema. Fields may be added; renaming or removing one
bumps `schema`.

```json
{
  "schema": 1,
  "entries": [
    {
      "port": 5173,
      "pid": 41822,
      "process": "node",
      "kind": "vite",
      "exposure": "local",
      "addrs": ["127.0.0.1", "::1"],
      "command": "node /home/me/web/node_modules/.bin/vite",
      "cwd": "/home/me/web",
      "uptime_secs": 7200,
      "memory_mb": 412,
      "dev": true,
      "container": null
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `pid` | `null` when hidden (another user's process) or for a container with no visible listener |
| `kind` | `vite`, `next.js`, `django`, `python http`, `postgres`, `docker`, `node`, …, `other` |
| `exposure` | `local` (all addresses loopback) or `network` |
| `addrs` | bound addresses; `*` means all interfaces (`0.0.0.0` / `::`) |
| `cwd`, `uptime_secs`, `memory_mb` | `null` when the OS doesn't reveal them |
| `container` | `{ "id", "name", "image" }` when the port is published by a Docker container |

`--json` respects `--all`. Note that `command` is the full command line and may contain
secrets passed as arguments (e.g. `--password=...`).

## License

MIT
