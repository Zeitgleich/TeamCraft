# Contributing to TeamCraft

## Developer Certificate of Origin

All commits must be signed off (`git commit -s`), certifying the
[Developer Certificate of Origin](https://developercertificate.org/).

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

## Guidelines

The rules are in [AGENTS.md](AGENTS.md) and [.claude/rules/](.claude/rules/). In short:

- Clean, typed, reusable code; no `unsafe`; no `unwrap()` outside tests.
- New dependencies need a short justification in the pull request.
- Parsers and protocol handlers need tests for malformed and hostile input.
- Changes to keys or protocols update [docs/e2ee.md](docs/e2ee.md) in the same pull request.
- Security issues are reported privately, never in public issues.

By contributing, you agree that your contributions are licensed under the AGPL-3.0-or-later.
