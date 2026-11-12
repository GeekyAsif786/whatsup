<!-- Thanks for contributing! Delete any section that doesn't apply. -->

## What and why

<!-- One or two sentences. Link the issue: "Closes #123". -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked` pass
- [ ] Behaviour changes come with tests in the same commit
- [ ] User-visible changes are in the README (and `--help`)
- [ ] New dependencies are justified below (what, why, size)

## Adding a framework rule

- [ ] The rule is in its group in `RULES`, before any runtime it runs on
- [ ] One `rule_cases!` line with a real process name and command line (copied from `whatsup list --all --json`)
- [ ] New kind only: variant and label in `src/model.rs`, colour group in `kind_group()`
