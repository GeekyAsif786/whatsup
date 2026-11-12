# Security policy

## Supported versions

Only the latest release of whatsup gets security fixes. Fixes are not backported.

## Reporting a vulnerability

Please report privately through GitHub: **Security → Report a vulnerability** on this repository
([private vulnerability reporting](https://github.com/GeekyAsif786/whatsup/security/advisories/new)).
Don't open a public issue for security problems.

Include what you found, the version (`whatsup --version`) and OS, how to reproduce it, and what an
attacker could do with it.

## Scope

whatsup lists listening sockets and can stop the processes behind them, so these are in scope:

- Stopping the wrong process: anything that gets whatsup to signal a PID that doesn't own the
  port it was asked about (for example a PID-reuse race the re-check misses).
- Command injection: any way for a process name, command line, container name or other scanned
  data to run a command, or to be interpreted by a shell.
- Privilege problems: whatsup elevating itself, or a `sudo whatsup` run doing more than asked.
- Terminal escape injection: scanned data (a crafted command line or container name) that
  rewrites the terminal when shown in the list or the interactive view.
- The release pipeline: the release and npm publish workflows, the archives, `SHA256SUMS`, the
  build attestations, the npm packages and the Homebrew formula.

Out of scope:

- Information whatsup shows by design to the user running it, such as other processes' command
  lines (which may contain secrets; see the README). If you can read it with `ps`, whatsup
  showing it is not a vulnerability.
- Problems that need an already compromised machine or account.
- Vulnerabilities in dependencies with no impact on whatsup (`cargo audit` runs in CI; Dependabot
  opens update PRs).

## What to expect

whatsup is maintained by one person in their spare time. Reports are handled on a best-effort
basis: I'll acknowledge as soon as I can, keep you informed, and credit you in the advisory and
release notes unless you prefer otherwise.
