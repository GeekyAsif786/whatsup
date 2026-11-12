# whatsup

**What's running on my dev machine?**
Architecture & system design, build plan, and review checklist

| | |
|---|---|
| Document | Design spec v1.0 |
| Status | Ready for implementation (open decisions marked in section 19) |
| Owner | Raj |
| Builder | Claude Code (prompts in Appendix A) |
| Reviewer | Claude (checklist in Appendix B) |
| Date | 12 Nov 2026 |

**One-line pitch.** Every port, dev server, database and container on your machine in one terminal
view. See what it is, where it came from, whether the whole network can reach it, and stop it with
one key.

**How to read this.** Sections 1–4 explain why. Sections 5–17 describe what to build and the reasoning
behind every non-obvious decision. Sections 18–19 cover the roadmap and open decisions. Appendix A
has copy-paste prompts for Claude Code, one per milestone. Appendix B is the checklist each pull
request is reviewed against.

> This Markdown file is a faithful conversion of `whatsup-design-spec.pdf` (v1.0) and replaces it
> as the source of truth.

---

## 1. Problem

A developer's machine slowly fills with forgotten servers: yesterday's `npm run dev`, a Jupyter
kernel, a Postgres container, a `python -m http.server` started for a quick file share. The
symptoms:

- `EADDRINUSE: address already in use :::3000`, followed by the usual `lsof -i :3000` →
  copy the PID → `kill -9` routine.
- No overview. `lsof`/`ss`/`netstat` list raw sockets but say nothing about what each one is (Vite?
  Django? a container?).
- A silent security problem: servers bound to `0.0.0.0` are reachable by everyone on the café,
  hostel or college Wi-Fi.

## 2. Goals and non-goals

### Goals (v0.1)

- List every TCP listener, show the owning process, and name the framework or service in
  human terms.
- Stop the thing on a port safely, from a CLI command or with one key in a TUI.
- Flag network-exposed listeners.
- Understand Docker-published ports (container name and image instead of docker-proxy).
- Single binary, no runtime dependencies. Linux, macOS and Windows are all first-class.
- Scriptable: stable `--json` output and meaningful exit codes.

### Non-goals (for now)

- Not a full process manager or htop replacement (no CPU graphs, no process tree browser).
- No remote machines, no daemon, no telemetry.
- No UDP listeners in v0.1 (noisy, rarely what a dev needs). Planned behind `--udp`.
- Never escalates privileges by itself (no automatic sudo).

## 3. Users and key scenarios

| # | Scenario | Expected experience |
|---|---|---|
| S1 | Port 3000 is busy and I don't know why | `whatsup port 3000` shows process, framework, cwd, command and uptime in under 100 ms |
| S2 | Free the port now | `whatsup kill 3000` → confirm → graceful stop; `-y` to skip the prompt, `-f` to force |
| S3 | What have I left running? | `whatsup` opens a TUI that refreshes live; I can browse, filter, open in browser and stop |
| S4 | Am I exposing anything on this Wi-Fi? | Header shows '⚠ N exposed to network'; rows bound to non-loopback addresses are marked yellow |
| S5 | Scripts and CI | `whatsup list --json` has a stable schema; `whatsup port 5432` exit code says used or free |
| S6 | Docker users | `:5432 docker db (postgres:16)`; stopping it runs `docker stop` |

## 4. First-principles breakdown

Every feature is a composition of five steps. If each step is correct and independently testable,
the product is too.

| Step | Question it answers | Source of truth |
|---|---|---|
| 1. Enumerate | Which sockets are in LISTEN state, on which address and port? | OS socket table |
| 2. Attribute | Which process owns each socket? | OS socket→PID mapping (needs privileges for other users' processes) |
| 3. Enrich | Name, command line, cwd, exe path, user, uptime, memory | OS process table |
| 4. Interpret | Is it Vite, Postgres, a container? Is it dev or system noise? Is it exposed? | Our rules (pure functions) |
| 5. Act | Stop it, open it | Signals / Windows terminate / docker CLI / OS URL opener |

**Key assumption:** steps 1–3 are platform-specific and messy; steps 4–5 are where the product's
value lives. So 1–3 sit behind interfaces (traits), and 4 is pure code with heavy unit tests.

## 5. Architecture

```text
┌────────────────────── Sources (traits, swappable, mockable) ──────────────────────┐
│ SocketSource             ProcessSource               ContainerSource              │
│ (netstat2 / native)      (sysinfo)                   (docker ps; later podman)    │
└──────────┬─────────────────────┬───────────────────────────┬──────────────────────┘
           │ Vec<Listener>       │ ProcInfo by PID           │ host port → Container
           ▼                     ▼                           ▼
   ┌──────────────────────────── Scanner ─────────────────────────────┐
   │ group IPv4/IPv6 twins · join PID → process · join port → container│
   └───────────────────────────────┬──────────────────────────────────┘
                                   ▼ Vec<Entry>
       ┌────────────── Interpretation (pure, no I/O) ───────────────┐
       │ Classifier (ordered rule table) · DevFilter · Exposure      │
       └───────────────────────────┬────────────────────────────────┘
                                   ▼
     ┌──────────────┬──────────────┬───────┴──────┬──────────────┐
     │ TUI          │ list / json  │ port <n>     │ kill <n>     │   Frontends
     └──────┬───────┴──────────────┴──────────────┴──────┬───────┘
            └────────────────────► Actions ◄──────────────┘
                      stop(entry, mode) · open_in_browser(port)
```

### Module layout (Rust; Go maps 1:1 to packages)

```text
src/
  main.rs              CLI parsing (clap), dispatch, exit codes
  model.rs             Entry, Exposure, Container, StopMode, StopResult    (serde-serialisable)
  sources/
     mod.rs            traits SocketSource, ProcessSource, ContainerSource
     sockets.rs        netstat2-backed implementation
     processes.rs      sysinfo-backed implementation (targeted refresh kinds)
     docker.rs         `docker ps --format {{json .}}` + port-string parser
  scan.rs              Scanner: joins sources into Vec<Entry>
  classify.rs          rule table + classify(name, cmd) -> Kind
  filter.rs            dev heuristic, text filter
  actions.rs           stop (term → grace → kill, tree mode, docker), open_in_browser
  tui/
     app.rs            state machine (Normal / Filter / Confirm), key handling
     view.rs           rendering only (no side effects)
  output.rs            table + detail printers, JSON
tests/
  integration.rs       spawns real listeners, asserts scan + kill
```

> **Rule:** `tui/view.rs` and `classify.rs` never perform I/O. That keeps the TUI snapshot-testable
> and the classifier unit-testable with plain strings.

## 6. Data model

| Field | Type | Meaning / notes |
|---|---|---|
| `port` | `u16` | Local port in LISTEN state |
| `pid` | `Option<u32>` | `None` when the OS hides it (other user, no privileges) |
| `process` | `String` | Process name, or 'container-name (image)' for Docker |
| `kind` | `String` (enum-like) | vite, next.js, django, fastapi, postgres, docker, node, python, …, other |
| `exposure` | `local \| network` | local = every bound address is loopback |
| `addrs` | `Vec<String>` | Bound addresses; `'*'` for unspecified (`0.0.0.0` / `::`) |
| `command` | `String` | Full argv joined; may be empty if hidden |
| `cwd` | `Option<String>` | Working directory, a strong hint for 'which project is this?' |
| `uptime_secs` | `Option<u64>` | Process run time |
| `memory_mb` | `Option<u64>` | Resident memory |
| `dev` | `bool` | Result of the dev heuristic (section 9) |
| `container` | `Option<{id, name, image}>` | Set when the port is published by a running container |
| `parent_chain` (v0.2) | `Vec<{pid, name}>` | Up to 4 ancestors, used for tree-stop (section 11) |

**JSON contract:** `whatsup list --json` emits `{"schema": 1, "entries": [...]}`. Adding
fields is allowed; renaming or removing them bumps `schema`. Document it in README.

## 7. Sources: decisions and trade-offs

### 7.1 Socket enumeration

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| Native APIs via library (netstat2: /proc + netlink, libproc, GetExtendedTcpTable) | Fast (ms), locale-proof, no external tools | Library maturity varies by OS | **Use this** |
| Shell out to lsof / ss / netstat | Zero platform code | Slow (lsof can take >1 s on macOS), output differs by version and locale, needs parsing | Fallback only, if ever |

### 7.2 Process info

- Use sysinfo with a **targeted refresh kind** (memory, user, cmd, cwd, exe), not a full refresh.
  A full refresh adds disk/CPU work every 2 s.
- Command line and cwd are fetched only when missing (`UpdateKind::OnlyIfNotSet`). They
  don't change during a process's life.
- **Lesson from the prototype:** the default refresh does not load cmd/cwd, so classification silently
  degrades to 'node'/'python'. Cover this with a test.

### 7.3 Containers

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| docker CLI: `docker ps --format '{{json .}}'` | Works with Docker Desktop, colima, OrbStack; no socket auth; tiny code | ~50–150 ms per call; needs docker on PATH | **Use this; cache 5 s** |
| Docker Engine API over unix socket / named pipe | Faster, richer | Per-platform socket paths, permissions, more deps | Later, if needed |

Port strings to parse: `0.0.0.0:5432->5432/tcp`, `:::5432->5432/tcp` and ranges
`127.0.0.1:8000-8002->8000-8002/tcp`. Unpublished ports (`6379/tcp`) are ignored. Join on host
port: the host listener belongs to docker-proxy / com.docker.backend / wslrelay, so its row is
replaced with the container.

**Join rules (normative).** A container is joined to a listener row on its published host port
only when the row's owner is:

- (a) container plumbing, by process name (lowercase, `.exe` stripped): `docker-proxy`,
  `com.docker.backend`, `vpnkit`, `wslrelay`, `rootlessport` (rootless Docker), `limactl`, `colima`, `ssh` (colima/lima
  forwarders), or any name starting with `orbstack` (`OrbStack Helper`); **or**
- (b) hidden: no PID, or no process info for the PID. On Linux, docker-proxy runs as root, so
  without sudo it is hidden.

If the owner is a *visible* process that is not plumbing (e.g. node, python), the container is
**not** attached: the real process row is kept (a real conflict or a stale `docker ps`; never
guess). A joined row keeps the listener's pid, addresses, uptime and memory; `process` becomes
`name (image)`, `kind` becomes `docker`, and stopping it runs `docker stop`. Several joined rows
for the same container and port (one docker-proxy per address family) merge into one.

When Docker runs with the userland proxy disabled, a published port has **no** listener. For
every published port with no listener row at all, the scanner adds a container row with
`pid: null`, `addrs` from the published host addresses, exposure from those addresses
(`0.0.0.0` / `::` → network) and kind `docker`.

If docker is missing, not running or fails, the container source is silently empty. Results,
failures included, are cached for 5 s.

**Timeouts (normative).** whatsup never hangs on an external command: every one runs with a
deadline (stdin closed, stdout/stderr drained on their own threads, and on timeout the child is
killed and reaped). `docker ps` gets **2 s** (Docker Desktop on macOS often hangs instead of
failing). On timeout, containers are treated as absent, and `list` / `port` print one line to
stderr: `Docker didn't respond within 2 s; containers not shown`. The timeout result is cached
for the same 5 s window, so a hung Docker costs at most one 2 s wait per 5 s. `docker stop`
timeouts are in section 11.1.

## 8. Classification

An ordered rule table; **the first match wins**. Order matters because frameworks run on generic
runtimes: checking for 'node' first would label every Vite and Next.js server as 'node'.

| Priority | Group | Match on (case-insensitive) | Examples → kind |
|---|---|---|---|
| 1 | Frontend frameworks | argv tokens | vite, next dev / next-server, webpack / react-scripts, astro, nuxt, expo / metro |
| 2 | Backend frameworks | argv tokens | manage.py runserver → django, uvicorn → fastapi, flask, gunicorn, rails / puma, artisan serve → laravel, spring |
| 3 | Tools | argv tokens | jupyter, streamlit, http.server → 'python http', ollama |
| 4 | Databases / infra | process name equals | postgres, mysqld / mariadbd, redis-server, mongod, nginx, caddy |
| 5 | Container plumbing | process name equals | docker-proxy, com.docker.backend, vpnkit, wslrelay → docker |
| 6 | Generic runtimes | process name starts with | node / bun / deno, python\*, ruby, php, java, dotnet, go (air, go-build) |
| 7 | Fallback | — | other |

- Strip `.exe` before matching on Windows.
- **Argv token matching (normative).** Rules 1–3 do not use substring search, so a framework
  name inside a path or a longer word (`invite-app`, `astronomy-site`, `export.js`,
  `springboard.jar`, `nextcloud-sync.js`) never matches.
  1. Split the command line on whitespace into tokens. For each token take its basename (the
     text after the last `/` or `\`), lowercase it, and strip one extension from:
     `.js .cjs .mjs .ts .py .exe .cmd .bat .ps1`.
  2. A single-word needle matches a token that **equals** the needle, or that starts with the
     needle followed by `-` (`jupyter` matches `jupyter-lab`; `next` does not match `nextcloud`).
  3. A multi-word needle (`manage.py runserver`, `next dev`, `artisan serve`) matches the same
     number of **consecutive** tokens, each compared as in step 2.
  4. A needle word containing `.` (`http.server`, `manage.py`) is compared with the whole
     basename, without stripping its extension.
  5. Process-name rules (equals / starts with) are unchanged.
- **node_modules package rule (normative).** A frontend (group 1) or tool (group 3) rule also
  matches when any argv token's path contains the segment `node_modules/<pkg>/` (or
  `node_modules\<pkg>\`), compared case-insensitively, and `<pkg>` equals the rule's package
  name: the needle's first word (`next dev` is the `next` package). This catches entry points
  whose basename says nothing, e.g. `node /app/node_modules/react-scripts/scripts/start.js` →
  react-scripts, `node /app/node_modules/next/dist/server/lib/start-server.js` → next.js.
  The package segment must be followed by another separator. Scoped packages
  (`node_modules/@scope/pkg/`) are compared as `@scope/pkg` and only match a rule whose needle
  is exactly that. A framework name elsewhere in a path (`/code/my-vite-notes/server.js`) or an
  unlisted package (`node_modules/express/`) still doesn't match. Plain `java -jar app.jar`
  stays java by design.
- Kind labels: every token in the table is its own kind (slash means "listed together", not
  "same kind"), except where the table gives an explicit `→ kind`:

  ```text
  1 vite, next.js (next dev / next-server), webpack, react-scripts, astro, nuxt, expo, metro
  2 django, fastapi, flask, gunicorn, rails, puma, laravel, spring
  3 jupyter, streamlit, python http, ollama
  4 postgres, mysql (mysqld), mariadb (mariadbd), redis (redis-server), mongodb (mongod), nginx, caddy
  5 docker
  6 node, bun, deno, python, ruby, php, java, dotnet, go
  7 other
  ```

- The `go` runtime rule matches a process named exactly `go` or `air`, or one starting with
  `go-build`. It does not use a bare `go` prefix, which would match 'Google Chrome'.
- Each rule has at least one unit test with a realistic argv string. A new framework = one rule +
  one test: the ideal first contribution.
- v0.2: move rules to an embedded TOML file so contributors don't need to touch Rust.

## 9. The 'dev vs noise' heuristic

By default users want their dev servers, not 40 system daemons. The heuristic is explicit so it can
be debated in issues:

```text
dev = kind != "other"
   || ( same_user_as_whatsup
        && 1024 <= port < 10000
        && exe not under a system path )   # /System, /usr/libexec, /usr/sbin, /sbin, C:\Windows, /lib/systemd
```

| Case | Result | Why |
|---|---|---|
| Your Vite server on 5173 | shown | Recognised kind |
| Your custom Go API on 8080 (unrecognised) | shown | Your process on a typical dev port |
| Spotify / Discord on 57621-style ports | hidden | High ephemeral port |
| sshd, cupsd, rapportd | hidden | Other user and/or system path |
| Postgres running as the postgres user | shown | Recognised kind (DBs are dev-relevant) |

Press `a` in the TUI (or pass `--all`) to show everything. Expected failure mode: some IDE helper
on port 6xxx will show up. That's acceptable, and fixable with a rule.

## 10. Exposure detection

- **local** if every bound address is loopback (`127.0.0.0/8`, `::1`); **network** otherwise, including
  `0.0.0.0`, `::`, and specific LAN IPs.
- An empty address list is **network** (fail safe): without evidence that a listener is
  loopback-only, flag it.
- This is the product's security angle, so make it visible: a header count, yellow rows, and a
  one-line warning in list output.
- Caveat to document: 'network' means *reachable if the firewall allows it*. whatsup does not
  inspect firewall rules (possible v1 `audit` feature).

## 11. Actions: stopping things safely

### 11.1 Stop algorithm

```text
stop(entry, mode):
  if entry.container:            run `docker stop <id>`; report its stderr on failure;
                                 give up after 15 s (above Docker's own 10 s stop grace):
                                 Failed(Other("docker stop timed out after 15 s"))
  if entry.pid is None:          fail: "PID hidden — run with sudo / elevated terminal"
  targets = [pid]                (mode = Listener, default)
          | process group / dev-command ancestors   (mode = Tree, v0.2)
  send SIGTERM to targets        (Windows: TerminateProcess — no graceful signal exists)
  poll every 100 ms up to 3 s:   done when the process is gone OR status == Zombie
  if still alive and --force:    SIGKILL, re-check after 300 ms
  else:                          report StillRunning (suggest --force)
```

### 11.2 The hard problem: process trees and respawning

`npm run dev → sh → node vite`. Kill only the listener and the parent may exit cleanly. But with
nodemon, `next dev` or `uvicorn --reload`, a supervisor respawns the child, and the port is busy
again a second later. Design:

- **v0.1:** stop the listener only. After stopping, re-scan after 1.5 s; if the port is busy again with a
  new PID, say so: 'respawned by parent `<name>` (pid N): press X to stop the whole tree'.
- **v0.2:** `X` / `--tree` stops the nearest 'dev command' ancestor (npm, pnpm, yarn, bun, nodemon,
  watchexec, uvicorn reloader) and its descendants. Never walk above the user's shell, and
  never touch the terminal or IDE process.
- Always show what will be stopped before confirming in tree mode.

### 11.3 Edge cases to test

- **Zombies:** in containers and some CI runners, killed processes linger as zombies until reaped.
  Treat Zombie status as stopped (a real bug found in the prototype).
- **Permission denied:** other user's process → a clear message, exit code 1, never a panic.
- **PID reuse:** between scan and kill, re-verify that the PID still owns the port before signalling.
- **Windows:** no SIGTERM; use terminate and document that shutdown is not graceful.

### 11.4 Open in browser

`o`/`Enter` opens `http://localhost:<port>` via `open` (macOS), `xdg-open` (Linux), or `cmd /C start`
(Windows), with stdin, stdout and stderr detached and no shell (`cmd /C start "" <url>` on Windows). Databases get no 'open': the hint is hidden and the key shows a short note
instead for database kinds, containers running a database image, and container plumbing with no
known container (`classify::opens_in_browser`).

### 11.5 Stop model

```text
StopMode   = Listener                       (default, v0.1)
           | Tree                           (v0.2; defined now, unused until then)

StopResult = Stopped                        SIGTERM / terminate / docker stop succeeded
           | ForceKilled                    SIGKILL after the grace period (--force / Y)
           | StillRunning                   alive after the grace period, no --force
           | Respawned { pid, parent }      port busy again 1.5 s later with a new PID;
                                            pid = Option<u32> (None: new owner hidden),
                                            parent = Option<{pid, name}>
           | Failed { reason }

FailReason = PidHidden                      "PID hidden — run with sudo / elevated terminal"
           | PermissionDenied               other user's process
           | PidReused                      PID no longer owns the port (section 16)
           | Other(String)                  e.g. docker stop's stderr
```

`FailReason` implements `Display` with the short, actionable human message.

## 12. CLI specification

| Command | Behaviour | Exit code |
|---|---|---|
| `whatsup` | TUI if stdout is a TTY, otherwise same as `list` | 0 |
| `whatsup list [--json] [--all]` | Table or JSON; alias `ls` | 0; 1 on error |
| `whatsup port <n\|:n>` | Detail view of whatever is on the port | 0 = in use, 1 = free (grep-style, script-friendly) |
| `whatsup kill <n> [-y] [-f] [--tree]` | Confirm, then stop (section 11) | 0 stopped / force-killed / nothing there, 1 failed, 3 still running or respawned |
| Global: `--all`, `--no-color` (+ respect `NO_COLOR`), `--version` | | |

Usage errors exit 2 (clap's default). Output never contains ANSI colour when piped.

`kill` exit codes by `StopResult` (section 11.5): `Stopped` / `ForceKilled` → 0, `Failed` → 1,
`StillRunning` → 3, `Respawned` → 3. A respawn exits 3 because the port is busy again, so
`whatsup kill 3000 && npm start` must not continue; its message names the parent that restarted
it. "Nothing listening on the port" is decided before `stop()` runs and exits 0.

The confirmation prompt (`kill` without `-y`) has three more outcomes, none of which stop anything:

| Situation | Message | Exit code |
|---|---|---|
| Declined at the prompt (anything but `y`/`yes`, including EOF) | `aborted, nothing stopped` | 1 |
| Ctrl-C at the prompt (also SIGTERM / SIGHUP: one handler, it can't tell them apart) | — | 130 (standard for SIGINT) |
| stdin is not a terminal and no `-y` (a usage error, not a decision) | `not a terminal: pass -y to confirm` | 2 (same as clap usage errors) |

When several PIDs own the port (e.g. one on `127.0.0.1` and another on `::1`, or `SO_REUSEPORT`
workers), one prompt lists them all and each is stopped in turn, one result line per owner. The
exit code is the worst: 1 if any `Failed`, else 3 if any `StillRunning` / `Respawned`, else 0.
The respawn check runs once, after all owners, and ignores every original PID. An owner that
exits while an earlier one is being stopped (a worker going down with its master) counts as
`Stopped`.

## 13. TUI specification

```text
 whatsup   3 listening (dev)   ⚠ 1 exposed to network                       ← header (1 line)
╭──────────────────────────────────────────────────────────────────────────╮
│ PORT     KIND       PROCESS                    PID     EXPOSED UP    MEM │ ← table (min 5 lines)
│▶ :3000   next.js    node                       41822   local    2h   412M│
│ :5432    docker     db (postgres:16)           1203    network 3d    18M │
╰──────────────────────────────────────────────────────────────────────────╯
cmd python -m uvicorn main:app --reload --host 0.0.0.0                       ← detail (4 lines)
cwd ~/code/api
 ↑↓ move · x stop · o open · / filter · a all/dev · r refresh · q quit       ← footer / status / prompt
```

| Key | Action |
|---|---|
| `↑↓` / `jk` | Move selection (wraps) |
| `x` / `Del` | Confirm prompt: `y` = stop, `Y` = force, any other key = cancel. Stops every owner of the selected port, like `kill <port>` (the prompt lists them) |
| `X` (v0.2) | Tree stop, with the tree shown in the prompt |
| `o` / `Enter` | Open in browser |
| `/` | Filter by port, process, kind or command; Enter keeps it, Esc clears it |
| `a` | Toggle dev-only / all |
| `r` | Manual refresh |
| `q` / `Esc` / `Ctrl-C` | Quit; the terminal is always restored, including on panic, SIGTERM and SIGHUP (closing the window). `Ctrl-C` quits from every mode (in the prompt it also cancels); `q` and `Esc` are text / clear in Filter and cancel in Confirm |

- State machine: Normal ⇄ Filter, Normal → Confirm → Normal. Auto-refresh pauses while
  confirming, so rows never shift under the prompt.
- Refresh every 2 s; keep the selection by `(port, pid)` across refreshes, not by row index.
- Status messages ('✓ stopped vite on :5173') stay for 4 s, then the key hints return.
- Minimum terminal size 60×12; below that, render 'terminal too small' instead of a broken
  layout.
- Colours by kind group (frontend green, Python yellow, DB magenta, docker cyan, other grey).
  Never rely on colour alone: the EXPOSED column carries the text too. Nginx and Caddy count as
  'other' (they serve HTTP; magenta is for databases).
- Scans run on a worker thread and each stop on its own thread (`actions::stop_port`, the same
  path as `kill`), so a slow `docker ps` or a 3 s grace period never freezes the keys. A scan
  that finishes while the prompt is open is dropped and redone when it closes.
- The detail pane is four labelled lines: `cmd`, `cwd` (`~` for home), `addr` (with the
  exposure in words) and `container` (`-` when none).

## 14. Cross-platform matrix

| Concern | Linux | macOS | Windows |
|---|---|---|---|
| Sockets | /proc/net + netlink | libproc | GetExtendedTcpTable |
| Other users' PIDs | Need root | Need root (hidden otherwise) | Need elevated terminal |
| cwd | Available | Available for own processes | Often unavailable: show '-' |
| Graceful stop | SIGTERM | SIGTERM | Not available: terminate |
| Docker port owner | docker-proxy | com.docker.backend / vpnkit | com.docker.backend / wslrelay |
| Open URL | xdg-open | open | cmd /C start |
| Known gotchas | Zombies in containers | lsof-style slowness avoided by native API | WSL2 ports appear via wslrelay |

On macOS without root, other users' listeners are not shown at all (libproc only lists the
sockets of processes we can inspect), rather than shown with a hidden PID. So `list --all` on macOS
as non-root prints one line under the table: "Some system listeners are hidden on macOS without
sudo; run `sudo whatsup list --all` to see everything." It doesn't appear in `--json` output.

**Testing reality:** macOS and Windows behaviour can only be verified in CI (section 17) or on real
machines. Treat a green CI matrix as a requirement for every release, not a nice-to-have.

## 15. Performance and quality budgets

| Metric | Budget | How to check |
|---|---|---|
| Startup to first frame | < 150 ms | `hyperfine 'whatsup list'` |
| One scan (100 listeners, no docker) | < 50 ms Linux, < 120 ms macOS | Criterion bench on Scanner with real sources |
| Idle CPU in TUI | < 1% | Activity Monitor / top over 60 s |
| Binary size (release, stripped, LTO) | < 5 MB | CI step that fails above budget |
| docker ps calls | ≤ 1 per 5 s | Cache in ContainerSource |

## 16. Security and safety

- whatsup can kill processes, so confirmation is the default; `-y` must be explicit.
- Re-verify PID ↔ port right before signalling (PID-reuse race).
- Never run with elevated privileges implicitly; never shell out with user-controlled strings
  (docker IDs come from docker itself; pass them as args, never through a shell).
- The JSON output may contain command lines with secrets (e.g. `--password=...`). Document
  this; v0.2 adds `--redact`, which masks values of flags matching `/pass|token|secret|key/`.
- Supply chain: pin dependencies (Cargo.lock committed), run `cargo audit` in CI, and publish
  release checksums (SHA-256) next to binaries.

## 17. Distribution, naming and testing

### 17.1 Distribution channels (in order of reach)

| Channel | Audience | Notes |
|---|---|---|
| GitHub Releases (5 targets) | Everyone | linux x86_64/aarch64 (glibc ≥ 2.35, built on Ubuntu 22.04), macOS arm64/x86_64, windows x86_64 (static CRT); tar.gz/zip, `SHA256SUMS`, a GitHub build attestation per archive |
| Homebrew tap | macOS / Linux devs | `brew install GeekyAsif786/tap/whatsup` (macOS arm64/x86_64, Linux x86_64); formula generated from `SHA256SUMS` by `packaging/homebrew/fill-formula.sh` and pushed to the tap by hand for v0.1.0 (automation later) |
| npm wrapper | JS devs (largest audience) | `npx whatsup-cli`; esbuild-style: a tiny JS launcher with `optionalDependencies` on five `@whatsup-cli/<os>-<cpu>` packages, each holding one binary and selected by npm via `os`/`cpu`/`libc`. No postinstall, no download at install time (works offline, behind proxies and with `--ignore-scripts`). Published with provenance via npm trusted publishing; the first publish of each package is manual |
| crates.io | Rust devs | Crate name 'whatsup' is taken; use 'whatsup-cli' with binary name 'whatsup'. `rust-version = 1.95` (sysinfo 0.40's floor) |
| Scoop / winget | Windows devs | v0.2 |

The release process (dry run, tag, Homebrew, npm, crates.io) is in `RELEASE.md`. M3 replaced
two details of prompt A4: the npm wrapper has no postinstall download (a postinstall that fetches
a binary breaks with `--ignore-scripts`, offline installs and strict proxies, and is a common
supply-chain red flag), and the Homebrew tap is updated by hand for v0.1.0 instead of by a
workflow step (that step needs a token that can write to another repository; it comes later).

**Naming check before launch:** crates.io 'whatsup' is taken (verified). Check npm, the Homebrew core
formula list, the GitHub repo/org name and a .dev domain before recording any marketing material. If
too many collide, rename now; it's free before launch and expensive after.

### 17.2 Testing strategy

| Layer | What | Where |
|---|---|---|
| Unit | `classify()` per rule, docker port parser, exposure, dev heuristic table, duration formatting, JSON schema snapshot | `cargo test`, every OS |
| Integration | Spawn a real listener (a tiny Rust listener on 127.0.0.1 and 0.0.0.0: the test binary re-run as a helper) → assert it appears with the right exposure → kill → assert it's gone. One test keeps a real `python -m http.server` to prove the 'python http' rule | CI on ubuntu, macos, windows |
| TUI snapshot | Render fixed fake entries with ratatui TestBackend; compare to stored snapshots (insta) | `cargo test` |
| State machine | Key sequences → expected mode/selection (no terminal needed) | `cargo test` |
| Manual pre-release | Real Docker, a reloader (`uvicorn --reload` / `next dev`), a sudo-owned listener | Checklist in RELEASE.md |

## 18. Roadmap

| Milestone | Scope | Exit criteria |
|---|---|---|
| M0: Skeleton | Repo, CI matrix, model, traits, classifier + tests | CI green on 3 OSes; no I/O in classify |
| M1: list / port / kill | Real sources, scanner, CLI, exit codes, JSON schema 1 | Integration test passes on 3 OSes |
| M2: TUI | State machine, view, auto-refresh, filter, open, confirm-stop | Snapshot + key-sequence tests; terminal always restored |
| M3: Ship v0.1.0 | Release workflow, checksums, Homebrew tap, npm wrapper, README + GIF (VHS) | Fresh-machine install on mac + Windows in < 60 s |
| v0.2 | Tree stop, respawn detection, rules in TOML, `--redact`, UDP, Scoop | Driven by first-week issues |
| v1.0 | `whatsup audit`: exposure + firewall check, 'who started this' (shell history / parent chain) | Security-angle launch post |

## 19. Decision log

| # | Decision | Recommended default | Status |
|---|---|---|---|
| D1 | Language | Rust (ratatui + sysinfo + netstat2): small static binary, systems/security résumé signal. Go (bubbletea + gopsutil) is equally valid if you prefer faster iteration. | Decided: Rust (stable, edition 2024) |
| D2 | Socket source | Native APIs via library; no lsof parsing | Decided |
| D3 | Docker integration | docker CLI with 5 s cache | Decided |
| D4 | Kill semantics | v0.1 listener-only + respawn warning; v0.2 tree mode | Decided: as recommended (section 11.5) |
| D5 | Default filter | Dev heuristic (section 9); `--all` to disable | Decided |
| D6 | Name | 'whatsup' binary; package names per channel after availability check | Decided: package `whatsup-cli`, binary `whatsup` |
| D7 | License | MIT or Apache-2.0 (dev tool: maximise adoption, packagers prefer permissive). Apache-2.0 adds a patent grant. | Decided: MIT |
| D8 | UDP | Excluded from v0.1 | Decided |
| D9 | npm packaging | Platform packages as optionalDependencies, no postinstall (section 17.1) | Decided (M3) |
| D10 | Release builds | Native runners per target (macOS x86_64 cross-built on arm64); Linux on Ubuntu 22.04 for glibc 2.35; static CRT on Windows; no build cache in release jobs | Decided (M3) |

---

## Appendix A: Claude Code prompts

Run one milestone per Claude Code session. After each one, paste the summary and diff into the
review chat. The prompts assume D1 = Rust; for Go, replace the crate names with bubbletea /
lipgloss / gopsutil and keep everything else.

### A0: Session preamble (paste first, every session)

```text
You are building `whatsup`, a cross-platform CLI/TUI that shows what is listening on
a developer's machine (ports, dev servers, databases, containers) and lets them stop it.
The design spec is in docs/DESIGN.md (I will paste it / it is in the repo) and is the
source of truth. If anything in my prompt conflicts with the spec, stop and ask.

Working rules:
- Small, reviewable commits with clear messages. No unrelated refactors.
- classify.rs and tui/view.rs must not perform I/O.
- No unwrap()/expect() outside tests; errors use anyhow with context.
- Every new behaviour gets a test in the same commit.
- Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` before
  saying you are done, and show me their output.
- End with: summary of changes, files touched, anything you were unsure about.
```

### A1: M0 Skeleton

```text
Milestone M0. Create the project skeleton exactly as in spec section 5:
1. `cargo new` with package name `whatsup-cli`, [[bin]] name `whatsup`, edition 2024,
   release profile: lto = true, strip = true, codegen-units = 1. Commit Cargo.lock.
2. model.rs: Entry, Exposure, Container, StopMode, StopResult per spec section 6,
   all serde::Serialize. JSON wrapper {"schema": 1, "entries": [...]}.
3. sources/mod.rs: traits SocketSource, ProcessSource, ContainerSource + in-memory
   fake implementations for tests.
4. classify.rs: ordered rule table from spec section 8, as data (a const slice of
   rules), plus classify(name, cmd) -> &'static str. One unit test per rule using
   realistic argv strings, plus tests proving order (vite beats node, uvicorn beats python).
5. filter.rs: dev heuristic (section 9) as a pure fn with a table-driven test covering
   every row of the section 9 table. exposure() (section 10) with tests.
6. sources/docker.rs: parse_docker_ports() only (no process spawning yet) with tests for
   single, IPv6 twin, range and unpublished cases.
7. CI: .github/workflows/ci.yml, matrix ubuntu/macos/windows: fmt check, clippy -D
   warnings, test, cargo audit.
Do NOT implement real OS sources, CLI or TUI yet.
```

### A2: M1 list / port / kill

```text
Milestone M1 (spec sections 7, 11, 12).
1. sources/sockets.rs: netstat2-backed SocketSource, TCP LISTEN only, IPv4 + IPv6.
2. sources/processes.rs: sysinfo-backed ProcessSource using a TARGETED refresh
   (memory, user, cmd, cwd, exe; cmd/cwd/exe with UpdateKind::OnlyIfNotSet).
   Add a test that a spawned child's cmd and cwd are non-empty on Linux/macOS.
3. sources/docker.rs: run `docker ps --format {{json .}}` with args (no shell),
   5-second cache, silently empty if docker is missing or fails.
4. scan.rs: group (port, pid) IPv4/IPv6 twins, join process + container, compute kind,
   exposure, dev; sort dev-first then by port.
5. actions.rs stop(): exactly the algorithm in spec 11.1, including Zombie == stopped,
   re-verify pid still owns the port before signalling, docker stop for containers.
   After a successful stop, re-scan after 1.5 s and report a respawn if the port is busy
   again with a different pid (spec 11.2, v0.1 behaviour).
6. main.rs + output.rs: commands, flags and EXIT CODES exactly per spec section 12;
   NO_COLOR / --no-color; no ANSI when piped.
7. tests/integration.rs: spawn `python3 -m http.server` on 127.0.0.1 and on 0.0.0.0,
   assert both rows appear with correct exposure, kill them, assert gone.
   Must pass in the CI matrix on all three OSes.
```

### A3: M2 TUI

```text
Milestone M2 (spec section 13).
1. tui/app.rs: state machine Normal / Filter / Confirm(Box<Entry>) with a pure
   `fn on_key(&mut App, KeyEvent) -> Effect` (Effect = None | Stop(..) | Open(port) | Quit).
   Side effects are executed by the loop, not inside on_key. Unit-test key sequences.
2. tui/view.rs: rendering only, layout exactly as the spec mock. Snapshot tests with
   ratatui TestBackend + insta for: normal list, empty list, filter mode, confirm prompt,
   exposed-warning header, terminal-too-small (< 60x12).
3. Auto-refresh every 2 s, paused while confirming; keep selection by (port, pid).
4. Terminal is restored on quit, on error AND on panic (install a panic hook).
5. Status line messages last 4 s.
```

### A4: M3 Ship v0.1.0

```text
Milestone M3 (spec section 17).
1. .github/workflows/release.yml on tag v*: build 5 targets (spec 17.1), package
   tar.gz/zip with README + LICENSE, attach SHA-256 checksums, generate release notes.
2. packaging/homebrew formula template + a workflow step that opens a PR to the tap repo
   with the new version and checksums.
3. npm wrapper package `whatsup-cli`: tiny JS shim, postinstall downloads the binary for
   process.platform/arch from the GitHub release and VERIFIES its SHA-256 before use.
4. docs/demo.tape for VHS; README per the outline I give you; CONTRIBUTING.md with the
   'add a framework rule' walkthrough; RELEASE.md manual checklist (spec 17.2).
5. CI step failing if the stripped Linux binary exceeds 5 MB.
```

## Appendix B: Review checklist

Every Claude Code PR is reviewed against this list. Items marked ★ block a merge.

| Area | Check |
|---|---|
| ★ Spec fidelity | Behaviour, flags, exit codes and JSON fields match the spec exactly; deviations are called out in the summary |
| ★ Purity | `classify.rs`, `filter.rs` and `tui/view.rs` contain no I/O, no process spawning, no clocks |
| ★ Safety | Kill requires confirmation unless `-y`; PID re-verified before signalling; no shell interpolation; never self-elevates |
| ★ Errors | No unwrap/expect in non-test code; permission errors give a human message and the right exit code |
| ★ Tests | New behaviour has tests in the same commit; the integration test runs on all 3 OSes; CI green |
| Terminal hygiene | Terminal restored on quit, error and panic; no colour when piped; `NO_COLOR` respected |
| Performance | Targeted sysinfo refresh (not full); docker cached; scan within budget (section 15) |
| Cross-platform | cfg-gated code compiles on all targets; Windows terminate path documented; `.exe` stripped in matching |
| Classifier | New rules are ordered correctly (framework before runtime) and each has a realistic argv test |
| UX | Messages are short and actionable ('run with sudo to see the PID'), never raw debug output |
| Supply chain | Cargo.lock committed; new dependencies justified in the PR; `cargo audit` clean |
| Docs | README / `--help` updated with any user-visible change; JSON schema bumped if fields renamed or removed |

**Review workflow:** paste the Claude Code summary, the output of `git diff --stat`, and any
files you want looked at. Each review returns findings ranked by severity with a concrete fix, plus
an approve or changes-requested verdict.
