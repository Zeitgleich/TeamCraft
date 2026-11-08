# Vendored: davey

| Item         | Value                                                                             |
|--------------|-----------------------------------------------------------------------------------|
| Upstream     | https://github.com/Snazzah/davey (crate `davey` on crates.io)                     |
| Version      | 0.2.0, tag `rs-0.2.0`, upstream commit `72d1c6d081f4993d618806990f1d3a1f6f53ed67` |
| License      | MIT (`LICENSE`, copied from the upstream repository at that commit)               |
| Used by      | `teamcraft-media` (DAVE, ARCHITECTURE.md §6), via `[patch.crates-io]`             |
| Why vendored | Move DAVE to `openmls` 0.9 so chat and calls share one MLS stack                  |
| Also         | `web-time` instead of `std::time::Instant`, for DAVE in the browser               |

## Why

`davey` 0.2.0 still requires `openmls` ^0.8.1 and `openmls_rust_crypto` ^0.5.1. That stack pins
`hpke-rs` 0.6.1 and with it `libcrux-sha3` 0.0.8 / `libcrux-secrets` 0.0.5, which carry
RUSTSEC-2026-0207, RUSTSEC-2026-0208 and RUSTSEC-2026-0212. The fixes are only reachable through
`openmls` 0.9 (`openmls_rust_crypto` 0.6 → `hpke-rs` 0.7). `teamcraft-crypto` uses `openmls` 0.9,
and ARCHITECTURE.md §6.2 wants a single MLS stack, so `davey` is patched to `openmls` 0.9.

## History

- 0.1.5 (upstream commit `73bcec27`), vendored with the same changes (#124, #165).
- 0.2.0 (#1541): security fixes (no nonce reuse when the current epoch's key is installed again,
  stricter welcome/commit/proposal checks, duplicate user IDs, LEB128 bounds, missing-nonce
  tracking) and the transition model (`prepare_transition` / `execute_transition`); see
  `CHANGELOG.md`. Re-vendored in the same way: one commit with the unmodified `davey/` directory of
  the tag, then one commit per change below.

## Changes against upstream 0.2.0

The first commit of the 0.2.0 copy is the unmodified crate (`davey/` of the tag plus the repository's
`LICENSE`); `git log -p -- crates/vendor/davey` shows every change since.

1. `Cargo.toml`: `openmls` 0.8.1 → 0.9.1, `openmls_basic_credential`, `openmls_rust_crypto` and
   `openmls_traits` → 0.6.0. Added an empty `[workspace]` table so the copy builds and tests on
   its own (our workspace excludes `crates/vendor`), and the dev profile overrides of the upstream
   workspace root (`scrypt`/`salsa20` optimised, `tls_codec` without debug assertions, which panic
   on truncated input instead of returning an error); they only apply to that standalone build.
2. `src/session.rs`, `process_commit`: openmls 0.9 no longer fails an echoed own commit with
   `StageCommitError::OwnCommit`. It returns `Ok` with `ProcessedMessageContent::OwnPendingCommit`
   when the confirmation tag matches the pending commit, fails with
   `StageCommitError::OwnCommitMismatch` when it doesn't and the commit has an update path, and
   *stages* a mismatching own commit without an update path (DAVE's add-only commits) like anyone
   else's. 0.2.0 accepts an own commit only if it is the pending commit byte for byte, so: the
   pending commit is merged on `OwnPendingCommit` only when the bytes match; a
   `StagedCommitMessage` from our own leaf is refused with `ProcessCommitError::OwnCommitMismatch`
   (without this, libdave's `own_commit_must_match_prepared_commit` fails: the stale own commit
   would be merged); openmls' `OwnCommitMismatch` maps to the same error.
3. `src/cryptor/{encryptor,decryptor,clock}.rs`: `std::time::Instant` → `web_time::Instant`
   (dependency `web-time` 1.1, MIT OR Apache-2.0, already in our tree). `std::time::Instant`
   compiles for `wasm32-unknown-unknown` but panics there on first use ("time not implemented on
   this platform"), and the encryptor, the decryptor and the cryptor managers' shared `Clock` (new
   in 0.2.0, replaces their own `Instant`s) read it for every frame. On native targets
   `web_time::Instant` *is* `std::time::Instant`, so nothing changes there; the frozen test clock
   is untouched. Upstream draft: second section of `UPSTREAM_PR.md`, patch
   `upstream-web-time.patch`.
4. `Cargo.toml`: `sha2` 0.10 → 0.11 and `hmac` 0.12 → 0.13 together (issue #124; neither builds
   alone, both moved to `digest` 0.11). `src/cryptor/mlspp_crypto.rs` imports `hmac::KeyInit`, where
   `new_from_slice` lives now. `src/cryptor/aead_cipher.rs` takes `InvalidLength` and the `U8`/`U12`
   sizes from the `cipher` crate its AES-GCM is built on instead of from `sha2::digest` (the old
   import only worked while both shared `crypto-common` 0.1). Same HMAC-SHA256, same bytes: the
   known-answer tests of change 5 and libdave's ratchet vector pass unchanged. `aes` 0.9 /
   `cipher` 0.5 are not taken: the AES-GCM in `src/aes_gcm.rs` is a copy of `aes-gcm` 0.10
   internals, which stays in our tree through `openmls_rust_crypto` 0.6; it moves together with
   `aes-gcm` 0.11.
5. Tests. 0.2.0 moved upstream's tests to `tests/unit/` (ported from libdave, pinned to libdave
   `8de72b1`), included from `src/lib.rs`. Added:
   - `tests/unit/teamcraft_kat.rs` (issue #124, values captured with sha2 0.10 / hmac 0.12 before
     change 4): the truncated-tag AES-128-GCM against fixed values (they match a standard
     AES-128-GCM with the tag cut to 8 bytes) and a tampered tag rejected; hash ratchet generation
     1, which runs the 32-byte secret derivation of `mlspp_crypto.rs` (libdave's vector only checks
     generation 0).
   - The `teamcraft_*` tests at the end of `tests/unit/session.rs`: preparing the current epoch
     again continues the nonce sequence (the 0.2.0 nonce-reuse fix, at session level); a prepared
     transition installs decryption keys at once and switches the encryption key only on
     `execute_transition`, once; a downgrade to protocol version 0 stops encryption when executed
     while older frames still decrypt; a proposal adding a second leaf for a user already in the
     group is refused (`RosterError::DuplicateUser`).
6. Added `VENDORED.md`, `UPSTREAM_PR.md` with `upstream.patch` (changes 1 and 2 without the
   `[workspace]` table and profiles) and `upstream-web-time.patch` (change 3), both relative to the
   upstream repository, and `.gitignore` (standalone `Cargo.lock` and `target/`).

No other source changes. `cargo test` and `cargo clippy --all-targets -- -D warnings` pass on the
copy (CI: `davey` job in `ci.yml`).

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
- **Upstream releases a newer version without `openmls` 0.9:** re-vendor it the same way
  (unmodified copy, then the changes above one commit each) so the diff stays reviewable. Check
  change 2 against upstream's `process_commit` each time: it is the one with security weight.
- Keep the version in `Cargo.toml` equal to upstream's, otherwise `[patch.crates-io]` no longer
  matches `teamcraft-media`'s requirement.
