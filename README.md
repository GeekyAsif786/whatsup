# whatsup

**Every port, dev server, database and container on your machine in one terminal view: see what it is, whether the network can reach it, and stop it with one key.**

<!-- DEMO -->

Linux, macOS and Windows. One small native binary, no runtime.

## Install

```sh
brew install GeekyAsif786/tap/whatsup   # macOS (arm64, x86_64), Linux x86_64
npx whatsup-cli                         # run without installing (Node 18+)
npm install -g whatsup-cli              # then: whatsup
cargo install whatsup-cli --locked      # from source, Rust 1.95+
```

Or download a binary from [GitHub Releases](https://github.com/GeekyAsif786/whatsup/releases):
`whatsup-v<version>-<target>.tar.gz` (`.zip` on Windows) for x86_64/arm64 Linux, arm64/x86_64
macOS and x86_64 Windows. Each release has a `SHA256SUMS` file, and every archive has a GitHub
build attestation you can check:

```sh
sha256sum --check --ignore-missing SHA256SUMS     # macOS: shasum -a 256 -c --ignore-missing SHA256SUMS
gh attestation verify whatsup-v0.1.0-aarch64-apple-darwin.tar.gz --repo GeekyAsif786/whatsup
```

**macOS Gatekeeper.** The binaries are not signed with an Apple Developer ID. If you download an
archive with a browser, macOS quarantines it and refuses to run `whatsup` ("cannot be opened
because the developer cannot be verified"). Remove the quarantine flag once:

```sh
xattr -d com.apple.quarantine ./whatsup
```

Homebrew, npm, cargo and `curl` downloads are not quarantined, so they need nothing.

## Why

- **Knows your stack.** Names what each listener is (Vite, Next.js, Django, FastAPI/uvicorn,
  Rails, Laravel, Jupyter, Ollama, Postgres, Redis and more) from its command line, not just
  "node" or "python3". By default it shows only dev servers and hides system services.
- **Docker-aware.** A port published by a container shows the container's name and image instead
  of `docker-proxy` or `com.docker.backend`, and stopping it runs `docker stop`.
- **Flags exposed servers.** Anything bound to `0.0.0.0`, `::` or a LAN address is marked
  `network`: other machines on the café or office Wi-Fi can reach it unless a firewall blocks
  them.
- **Stops things safely.** Always asks first (unless `-y`), checks the PID still owns the port
  right before signalling, sends SIGTERM and waits 3 s before anything harsher, and tells you if
  a reloader (nodemon, `uvicorn --reload`, ...) started it again.

## Usage

```console
$ whatsup                 # interactive view (prints the list when piped)
$ whatsup list            # dev servers only; alias: whatsup ls
PORT    PID    KIND         PROCESS            EXPOSED  UPTIME
:5173   41822  vite         node               local    2h
:8000   5150   python http  python3            network  42s
:5432   1203   docker       db (postgres:16)   network  3d
⚠ 2 listeners are exposed to the network (reachable from other machines if the firewall allows)

$ whatsup list --all      # every listener, including system services
$ whatsup list --json     # machine-readable, see below
$ whatsup port 5173       # what is on :5173? exit 0 = in use, 1 = free
$ whatsup kill :5173      # asks, then stops it; -y skips the prompt, -f force-kills
```

### Interactive view

`whatsup` on its own opens a live view of the same list. It refreshes every 2 s, keeps your
selection on the same port and PID, and shows the command, working directory, addresses and
container of the selected row. `--all` and `--no-color` / `NO_COLOR` apply here too.

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
pauses while the stop prompt is open, so rows never move under it. The view needs a terminal of
at least 60×12. The terminal is restored however whatsup exits: quit, error, panic, or a
SIGTERM / SIGHUP (closing the window).

### Stopping

`kill` sends SIGTERM, waits up to 3 s, and with `-f` then sends SIGKILL. Containers are stopped
with `docker stop`. On Windows there is no graceful signal, so the process is terminated
immediately. If something restarts the server within 1.5 s, `kill` says so and names the parent.

| Command | Exit code |
|---|---|
| `list` | 0; 1 on error |
| `port <n>` | 0 in use, 1 free |
| `kill <n>` | 0 stopped (or nothing there), 1 failed or declined, 2 no terminal to confirm in (pass `-y`), 3 still running or respawned, 130 Ctrl-C at the prompt |

So `whatsup kill 3000 -y && npm start` only starts the new server once the port is really free.

Options: `--all`, `--no-color` (and the `NO_COLOR` environment variable), `--version`. Colour is
never used when output is piped.

### Permissions

whatsup never elevates itself. Without root / an elevated terminal:

- **Linux:** other users' listeners are listed, but with no PID ("PID hidden: run with sudo to
  see it"), and they can't be stopped.
- **macOS:** other users' listeners are not shown at all; `list --all` says so. Run
  `sudo whatsup list --all` to see everything.
- **Windows:** other users' processes and services need an elevated terminal.

To stop another user's process, run the same command with `sudo` (or as administrator).

### JSON

`whatsup list --json` prints a stable schema. Fields may be added; renaming or removing one bumps
`schema`.

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

`--json` respects `--all`. Note that `command` is the full command line and may contain secrets
passed as arguments (e.g. `--password=...`).

## How it works

1. **Sockets:** TCP listeners (IPv4 and IPv6) from the OS's native socket tables, through the
   [`netstat2`](https://crates.io/crates/netstat2) crate. No `lsof` or `netstat` parsing.
2. **Processes:** name, command line, working directory, memory and start time of each owner,
   through [`sysinfo`](https://crates.io/crates/sysinfo), refreshing only those fields.
3. **Containers:** `docker ps` (run directly, never through a shell; 2 s timeout, cached for 5 s;
   silently skipped if Docker isn't there). A port forwarded by Docker's plumbing is shown as the
   container that publishes it.
4. **Classification:** an ordered table of rules in
   [`src/classify.rs`](https://github.com/GeekyAsif786/whatsup/blob/main/src/classify.rs)
   matched against the command line and process name; frameworks come before the runtimes they
   run on, so `node …/vite` is `vite`, not `node`.
5. **Dev or noise:** a listener counts as a dev server if it was classified, or if it is yours,
   on a port from 1024 to 9999, and not under a system path.

The full design, with the reasoning behind each decision, is in
[docs/DESIGN.md](https://github.com/GeekyAsif786/whatsup/blob/main/docs/DESIGN.md).

## Contributing

Bug reports, framework rules and fixes are welcome. Teaching whatsup a new framework is one rule
and one test; [CONTRIBUTING.md](https://github.com/GeekyAsif786/whatsup/blob/main/CONTRIBUTING.md)
walks through it. Please report security problems privately, as described in
[SECURITY.md](https://github.com/GeekyAsif786/whatsup/blob/main/SECURITY.md).

## License

[MIT](https://github.com/GeekyAsif786/whatsup/blob/main/LICENSE)
