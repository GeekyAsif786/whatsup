# Contributing to whatsup

Thanks for helping. Bug reports, framework rules and fixes are all welcome. For anything bigger
than a fix, please open an issue first so we can agree on the approach; the design and the
reasoning behind it are in [docs/DESIGN.md](docs/DESIGN.md).

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Security problems go to
the [security policy](SECURITY.md), not to public issues.

## Set up

You need Rust (the version in `rust-toolchain.toml` installs itself through rustup) and, for the
packaging scripts only, Node 18 or newer.

```sh
git clone https://github.com/GeekyAsif786/whatsup && cd whatsup
cargo run              # the interactive view
cargo run -- list      # or any other command
```

Before you open a pull request, run what CI runs:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
node --test packages/whatsup-cli/test/launcher.test.js scripts/*.test.mjs   # only if you touched packages/ or scripts/
```

CI runs these on Linux, macOS and Windows; a PR needs all of them green.

## House rules

- Small commits with clear messages (`feat(classify): ...`, `fix(tui): ...`).
- Every behaviour change comes with a test in the same commit.
- `src/classify.rs`, `src/filter.rs` and `src/tui/view.rs` are pure: no I/O, no clocks, no
  spawning processes.
- No `unwrap()` / `expect()` outside tests; errors use `anyhow` with context.
- Never run anything through a shell; pass arguments to the program directly.
- User-visible changes update the README (and `--help` if needed). TUI layout changes update the
  insta snapshots: `cargo insta review` (or `INSTA_UPDATE=always cargo test`, then check the
  diff).

## Add a framework rule

whatsup recognises frameworks with an ordered table of rules in
[`src/classify.rs`](src/classify.rs). The first rule that matches wins, so a framework must come
before the runtime it runs on (`vite` before `node`, `uvicorn` before `python`). Teaching it a
new framework is one rule and one test. This walkthrough adds [Hugo](https://gohugo.io)'s dev
server (`hugo server`).

**0. Open an issue** with the "New framework rule" template, so nobody else does it twice. Note
the real command line: run the server, then `whatsup list --all --json` and copy the `process`
and `command` fields.

**1. Pick the kind.** If the framework is a new way of running something whatsup already knows
(for example another way to start Next.js), reuse its `Kind` and skip to step 2. Otherwise add
the kind in [`src/model.rs`](src/model.rs): a variant in its group, and its label (what users see
in the KIND column and in JSON):

```rust
// enum Kind, group "1. Frontend frameworks"
    Metro,
    Hugo,
```

```rust
// Kind::as_str
            Kind::Metro => "metro",
            Kind::Hugo => "hugo",
```

and give it a colour group in `kind_group()` in `src/classify.rs` (the match lists every kind, so
the compiler tells you):

```rust
        | Kind::Metro
        | Kind::Hugo => KindGroup::Frontend,
```

**2. Add the rule** to `RULES`, at the end of its group:

```rust
    argv(1, "metro", Kind::Metro),
    argv(1, "hugo server", Kind::Hugo),
```

There are three kinds of rule:

| Constructor | Matches when | Use for |
|---|---|---|
| `argv(group, "word ...", kind)` | the command line has these words as consecutive arguments, compared by file name without extension (`/x/.bin/vite.js` is `vite`; `jupyter` also matches `jupyter-lab`) | frameworks and tools started through a runtime |
| `name_is(group, "name", kind)` | the process name equals it (case-insensitive, `.exe` ignored) | databases and servers with their own binary |
| `name_starts(group, "prefix", kind)` | the process name starts with it | runtimes (`python3.12`, `node20`) |

Groups follow DESIGN.md section 8: 1 frontend, 2 backend frameworks, 3 tools, 4 databases and
infra, 5 container plumbing, 6 runtimes. A test fails if rules are out of group order. Patterns
are lowercase. Prefer two words (`hugo server`) over one when the single word could appear in
unrelated commands.

**3. Add the test**: one line in the `rule_cases!` table in the tests at the bottom of
`src/classify.rs`, with the realistic process name and command line you copied in step 0:

```rust
        rule_hugo: ("hugo", "hugo server --buildDrafts --port 1313") => Kind::Hugo;
```

`every_rule_is_the_first_match_for_some_case` fails if a rule has no case, or if an earlier rule
shadows yours. If your framework runs on a runtime, also make sure the runtime doesn't win; the
`vite_beats_node` test shows the pattern.

**4. Run** `cargo test classify`, then the full checks above, and open the PR. If you added a
kind, mention it in the README's list in "Why" if it's a well-known one.

## Pull requests

- Fill in the template; link the issue (`Closes #123`).
- Keep unrelated refactors out; they make review slow.
- New dependencies need a reason in the PR description (what it does, why not the standard
  library or an existing dependency, its size).

## Labels

Labels are defined in [`.github/labels.yml`](.github/labels.yml). Maintainers sync them with
`node scripts/sync-labels.mjs` (dry run) and `--apply`.
