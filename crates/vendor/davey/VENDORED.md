# Vendored: davey

| Item         | Value                                                                 |
|--------------|-----------------------------------------------------------------------|
| Upstream     | https://github.com/Snazzah/davey (crate `davey` on crates.io)         |
| Version      | 0.1.5, upstream commit `73bcec27b7190ce47088c831e0aa72600cc657e7`     |
| License      | MIT (`LICENSE`, copied from the upstream repository at that commit)   |
| Used by      | `teamcraft-media` (DAVE, ARCHITECTURE.md §6), via `[patch.crates-io]` |
| Why vendored | Move DAVE to `openmls` 0.9 so chat and calls share one MLS stack      |

## Why

`davey` 0.1.5 requires `openmls` ^0.8.1 and `openmls_rust_crypto` ^0.5.1. That stack pins `hpke-rs`
0.6.1 and with it `libcrux-sha3` 0.0.8 / `libcrux-secrets` 0.0.5, which carry RUSTSEC-2026-0207,
RUSTSEC-2026-0208 and RUSTSEC-2026-0212. The fixes are only reachable through `openmls` 0.9
(`openmls_rust_crypto` 0.6 → `hpke-rs` 0.7). `teamcraft-crypto` uses `openmls` 0.9, and
ARCHITECTURE.md §6.2 wants a single MLS stack, so `davey` is patched to `openmls` 0.9.

## Changes against upstream 0.1.5

The first commit adding this directory is the unmodified crate (its `Cargo.toml.orig` as
`Cargo.toml`); `git log -p -- crates/vendor/davey` shows every change since.

1. `Cargo.toml`: `openmls` 0.8.1 → 0.9.1, `openmls_basic_credential` 0.5.0 → 0.6.0,
   `openmls_rust_crypto` 0.5.1 → 0.6.0. Added an empty `[workspace]` table so the copy builds and
   tests on its own (our workspace excludes `crates/vendor`).
2. `src/session.rs`, `process_commit`: openmls 0.9 no longer reports a commit this client created
   and that the delivery service echoed back as `ProcessMessageError::InvalidCommit(
   StageCommitError::OwnCommit)`. It returns `Ok` with `ProcessedMessageContent::OwnPendingCommit`
   when the commit matches the pending one (and `StageCommitError::OwnCommitMismatch` when it
   doesn't). The own-commit branch moved from the error arm to that content variant; the now unused
   `ProcessMessageError` import was dropped.
3. Added `VENDORED.md`, `UPSTREAM_PR.md` with `upstream.patch` (changes 1 and 2 without the
   `[workspace]` table, relative to the upstream repository), `.gitignore` (standalone `Cargo.lock`
   and `target/`).

No other source changes. `cargo test` and `cargo clippy` pass on the copy.

## Updating or dropping the vendor copy

- **Upstream releases `openmls` 0.9 support** (e.g. after the PR in `UPSTREAM_PR.md`): delete
  `crates/vendor/davey` and the `[patch.crates-io] davey` entry in the root `Cargo.toml`, bump the
  `davey` requirement in `teamcraft-media` to that release, remove `exclude = ["crates/vendor"]` if
  nothing else is vendored, then `cargo update -p davey` and run the workspace checks.
- **Upstream releases a newer version without `openmls` 0.9:** re-vendor it in two commits
  (unmodified copy, then the changes above) so the diff stays reviewable.
- Keep the version in `Cargo.toml` equal to upstream's, otherwise `[patch.crates-io]` no longer
  matches `teamcraft-media`'s requirement.
