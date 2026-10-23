# Vendored: davey

| Item         | Value                                                                 |
|--------------|-----------------------------------------------------------------------|
| Upstream     | https://github.com/Snazzah/davey (crate `davey` on crates.io)         |
| Version      | 0.1.5, upstream commit `73bcec27b7190ce47088c831e0aa72600cc657e7`     |
| License      | MIT (`LICENSE`, copied from the upstream repository at that commit)   |
| Used by      | `teamcraft-media` (DAVE, ARCHITECTURE.md §6), via `[patch.crates-io]` |
| Why vendored | Move DAVE to `openmls` 0.9 so chat and calls share one MLS stack      |
| Also         | `web-time` instead of `std::time::Instant`, for DAVE in the browser    |

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
3. `src/cryptor/{encryptor,decryptor,cryptor_manager}.rs`: `std::time::Instant` → `web_time::Instant`
   (new dependency `web-time` 1.1, MIT OR Apache-2.0, already in our tree). `std::time::Instant`
   compiles for `wasm32-unknown-unknown` but panics there on first use ("time not implemented on
   this platform"), and the encryptor and decryptor take a timestamp for every frame. On native
   targets `web_time::Instant` *is* `std::time::Instant`, so nothing changes there. Upstream draft:
   second section of `UPSTREAM_PR.md`, patch `upstream-web-time.patch`.
4. Added `VENDORED.md`, `UPSTREAM_PR.md` with `upstream.patch` and `upstream-web-time.patch` (changes 1 and 2 without the
   `[workspace]` table, relative to the upstream repository), `.gitignore` (standalone `Cargo.lock`
   and `target/`).
5. Known-answer tests (issue #124, captured before the dependency upgrade in change 6):
   `src/cryptor/aead_cipher.rs` checks the truncated-tag AES-128-GCM against fixed values (they
   match a standard AES-128-GCM with the tag cut to 8 bytes) and rejects a tampered tag;
   `src/cryptor/hash_ratchet.rs` also checks ratchet generation 1, which runs the 32-byte secret
   derivation of `mlspp_crypto.rs`.

No other source changes. `cargo test` and `cargo clippy` pass on the copy.

## Browser verification

`crates/teamcraft-media/tests/wasm_dave.rs` sets up a two-member DAVE group and round-trips frames
on `wasm32-unknown-unknown` (`wasm-bindgen-test`). Without change 3 it fails with "time not
implemented on this platform"; with it, it passes in Node 24 and in Chromium (checked 2026-10 with
`wasm-bindgen-cli` 0.2.129, matching `Cargo.lock`):

```sh
cargo install wasm-bindgen-cli --version 0.2.129 --locked
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
cargo test -p teamcraft-media --target wasm32-unknown-unknown --test wasm_dave          # Node
WASM_BINDGEN_USE_BROWSER=1 NO_HEADLESS=1 \
  cargo test -p teamcraft-media --target wasm32-unknown-unknown --test wasm_dave        # open the printed URL
```

Firefox and Safari still need the same run (the code path is plain `performance.now()`).

## Updating or dropping the vendor copy

- **Upstream releases `openmls` 0.9 support** (e.g. after the PR in `UPSTREAM_PR.md`): delete
  `crates/vendor/davey` and the `[patch.crates-io] davey` entry in the root `Cargo.toml`, bump the
  `davey` requirement in `teamcraft-media` to that release, remove `exclude = ["crates/vendor"]` if
  nothing else is vendored, then `cargo update -p davey` and run the workspace checks.
- **Upstream releases a newer version without `openmls` 0.9:** re-vendor it in two commits
  (unmodified copy, then the changes above) so the diff stays reviewable.
- Keep the version in `Cargo.toml` equal to upstream's, otherwise `[patch.crates-io]` no longer
  matches `teamcraft-media`'s requirement.
