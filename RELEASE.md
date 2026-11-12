# Releasing whatsup

The maintainer's checklist. Pushing a `v*` tag builds and publishes the GitHub Release
automatically; Homebrew, npm and crates.io are separate steps after it. For v0.1.0, every
step after the tag is done by hand.

What ships where:

| Channel | Made by | Published by |
|---|---|---|
| GitHub Release: 5 archives, `SHA256SUMS`, build attestations | `.github/workflows/release.yml` on the tag | the same workflow |
| Homebrew: `GeekyAsif786/homebrew-tap`, `Formula/whatsup.rb` | `packaging/homebrew/fill-formula.sh` | you, by hand (automation later) |
| npm: `whatsup-cli` + 5 `@whatsup-cli/*` packages | `scripts/npm-assemble.mjs` | first time: you, by hand; later: `.github/workflows/npm-publish.yml` |
| crates.io: `whatsup-cli` | `cargo publish` | you, by hand |

## 1. Before the release

- [ ] `main` is green on CI (all four checks).
- [ ] Bump the version everywhere (they must match; `scripts/npm-assemble.test.mjs` fails in CI
      otherwise):
  - `Cargo.toml` `version`, then `cargo check` to update `Cargo.lock`;
  - `packages/*/package.json` `version` (6 files) and the five `optionalDependencies` versions
    in `packages/whatsup-cli/package.json`.
- [ ] README is true of the release (install commands, keys, exit codes).
- [ ] Manual checks (DESIGN.md section 17.2), on a real machine:
  - [ ] Real Docker: a published container (`docker run -d -p 5432:5432 -e POSTGRES_PASSWORD=x postgres:16`)
        shows as `docker` with its name and image; `whatsup kill 5432` runs `docker stop`.
  - [ ] A reloader: `uvicorn main:app --reload` or `next dev`; `whatsup kill <port>` reports the
        respawn and names the parent, exit code 3.
  - [ ] A listener owned by another user (`sudo python3 -m http.server 8080`): without sudo it
        shows "PID hidden" (Linux) / is hidden with the `list --all` note (macOS), and stopping it
        fails with the sudo hint; with `sudo whatsup kill 8080` it stops.
  - [ ] The TUI: stop with `x`/`y`, filter, open, resize below 60×12, quit; close the terminal
        window while it runs and check the next shell is not left in raw mode.
- [ ] Dry run of the release workflow: Actions → **Release** → **Run workflow** on `main`. It
      builds all five targets, writes `SHA256SUMS`, fills the Homebrew formula and assembles the
      npm packages, but publishes nothing. Check the run summary (sizes, checksums).
- [ ] `cargo publish --dry-run --locked` passes.

## 2. Tag: creates the GitHub Release

```sh
git switch main && git pull
git tag -a v0.1.0 -m "whatsup v0.1.0"
git push origin v0.1.0
```

The **Release** workflow checks the tag equals `Cargo.toml`'s version, builds, attests every
archive (`actions/attest-build-provenance`) and creates the release with generated notes
(categories from `.github/release.yml`). A tag with a `-` (`v0.2.0-rc.1`) becomes a pre-release.

Check the release:

```sh
mkdir /tmp/whatsup-check && cd /tmp/whatsup-check
gh release download v0.1.0 --repo GeekyAsif786/whatsup
sha256sum --check SHA256SUMS                     # macOS: shasum -a 256 -c SHA256SUMS
for f in whatsup-v0.1.0-*; do gh attestation verify "$f" --repo GeekyAsif786/whatsup; done
```

- [ ] Edit the generated notes if needed (highlights at the top).
- [ ] Fresh-machine check (M3 exit criterion): on a clean macOS and a clean Windows machine,
      install from the release and run `whatsup` in under 60 s.

## 3. Homebrew (manual for v0.1.0)

The tap repository `GeekyAsif786/homebrew-tap` is empty today. Users install with
`brew install GeekyAsif786/tap/whatsup` (Homebrew maps `tap` to the `homebrew-tap` repo).

```sh
gh release download v0.1.0 --repo GeekyAsif786/whatsup --pattern SHA256SUMS --dir /tmp/whatsup-sums
git clone https://github.com/GeekyAsif786/homebrew-tap && cd homebrew-tap
mkdir -p Formula
/path/to/whatsup/packaging/homebrew/fill-formula.sh 0.1.0 /tmp/whatsup-sums/SHA256SUMS > Formula/whatsup.rb
git add Formula/whatsup.rb
git commit -m "whatsup 0.1.0"
git push
```

Then test it from the tap:

```sh
brew update
brew install GeekyAsif786/tap/whatsup
brew test whatsup
brew audit --strict --online GeekyAsif786/tap/whatsup
whatsup --version
```

The first time, also add a short `README.md` to the tap ("`brew install GeekyAsif786/tap/whatsup`").
The formula covers macOS arm64 / x86_64 and Linux x86_64. Later releases: the same three
commands with the new version. (Automation later: a release-workflow job that opens a PR on the
tap with a fine-grained token limited to that repo.)

## 4. npm

Six packages: `whatsup-cli` (the launcher, `bin: whatsup`) and `@whatsup-cli/darwin-arm64`,
`darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64` (one binary each, selected by npm through
their `os` / `cpu` / `libc` fields). No install scripts.

### First publish of each package: by hand

npm's trusted publishing can only be configured for a package that already exists, so v0.1.0
goes up from your machine:

- [ ] Make sure you own the npm org `whatsup-cli` (it already exists on npm; the `@whatsup-cli/*`
      packages need it). If it isn't yours, pick another scope and rename it in
      `packages/*/package.json`, `packages/whatsup-cli/bin/whatsup.js` and its test.
- [ ] `npm login` (with 2FA).
- [ ] Assemble from the published release, check, then publish platform packages first:

```sh
cd /path/to/whatsup
gh release download v0.1.0 --dir /tmp/whatsup-rel --pattern 'whatsup-v*' --pattern SHA256SUMS
node scripts/npm-assemble.mjs --version 0.1.0 --archives /tmp/whatsup-rel --out /tmp/whatsup-npm
for p in darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-x64 whatsup-cli; do
  (cd /tmp/whatsup-npm/$p && npm publish --access public --dry-run)
done
# Looks right? Run the loop again without --dry-run.
npx whatsup-cli@0.1.0 --version
```

(Packages published from your machine have no provenance statement; later versions published by
the workflow do.)

### After that: the workflow

- [ ] On npmjs.com, for each of the six packages: **Settings → Trusted publishing → GitHub
      Actions**, with owner `GeekyAsif786`, repository `whatsup`, workflow `npm-publish.yml`,
      environment `npm`.
- [ ] In GitHub: **Settings → Environments → New environment** `npm`, with yourself as a
      required reviewer.
- [ ] On npmjs.com, for each package: **Settings → Publishing access → Require two-factor
      authentication and disallow tokens**.

Each later release: Actions → **npm publish** → Run workflow with the tag (e.g. `v0.2.0`), first
with **dry-run** ticked, then without. It downloads the release, checks `SHA256SUMS` and the
attestations, assembles and publishes with provenance. Versions already on npm are skipped, so a
failed run can be re-run.

## 5. crates.io

```sh
cargo login            # once, with a token scoped to publish-new / publish-update
git switch --detach v0.1.0
cargo publish --locked
```

The package holds `src/`, `tests/`, `README.md` and `LICENSE` only (`include` in `Cargo.toml`).
After the first publish, consider crates.io trusted publishing for later releases.

## 6. After

- [ ] README demo GIF: `vhs docs/demo.tape`, upload `docs/demo.gif` to a GitHub comment, put the
      URL at `<!-- DEMO -->`.
- [ ] Labels: `node scripts/sync-labels.mjs`, then `--apply`.
- [ ] Watch issues for the first week; the v0.2 scope is driven by them (DESIGN.md section 18).
