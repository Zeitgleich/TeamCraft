# Upstream PR draft for Snazzah/davey

Not opened yet. Ready to send as-is. The change is `upstream.patch` in this directory, relative to
the upstream repository root (apply with `git apply upstream.patch` on tag `rs-0.2.0`, commit
`72d1c6d0`): the dependency bumps in `davey/Cargo.toml` and the `process_commit` change in
`davey/src/session.rs`, without our `[workspace]` table, dev profiles and vendoring files.

---

**Title:** chore: update openmls to 0.9

**Body:**

This updates `openmls` to 0.9.1, `openmls_basic_credential`, `openmls_rust_crypto` and
`openmls_traits` to 0.6.0.

Motivation: `openmls_rust_crypto` 0.5.1 depends on `hpke-rs` 0.6.1, which pins `libcrux-sha3`
0.0.8 and `libcrux-secrets` 0.0.5. Those versions are affected by
[RUSTSEC-2026-0207](https://rustsec.org/advisories/RUSTSEC-2026-0207),
[RUSTSEC-2026-0208](https://rustsec.org/advisories/RUSTSEC-2026-0208) and
[RUSTSEC-2026-0212](https://rustsec.org/advisories/RUSTSEC-2026-0212); the fixed versions are only
reachable through `openmls_rust_crypto` 0.6 / `hpke-rs` 0.7. It also lets applications that use
`openmls` 0.9 elsewhere share one MLS stack with davey (with 0.8 the two don't resolve together,
`hax-lib` `=0.3.6` vs `^0.3.7`).

The only API change that affects davey is how openmls treats a commit from our own leaf. 0.8
failed every one with `StageCommitError::OwnCommit`. 0.9:

- returns `Ok` with `ProcessedMessageContent::OwnPendingCommit` when the commit's confirmation tag
  matches the pending commit,
- fails with `StageCommitError::OwnCommitMismatch` when it doesn't and the commit has an update
  path,
- stages a mismatching own commit *without* an update path like anyone else's (DAVE's add-only
  commits have none).

`process_commit` keeps 0.2.0's rule that an own commit is accepted only if it is the pending
commit byte for byte: it merges the pending commit on `OwnPendingCommit` only when the bytes
match, refuses a `StagedCommitMessage` whose sender is our own leaf, and maps openmls'
`OwnCommitMismatch` to `ProcessCommitError::OwnCommitMismatch`. Without the second point,
`own_commit_must_match_prepared_commit` would merge a stale own commit.

- `cargo test` (all ported libdave tests) and `cargo clippy` pass.
- No public API changes in davey.

---

# Second upstream PR draft: browser support

Not opened yet. Independent of the openmls PR above; `upstream-web-time.patch` applies on top of it
(the `Cargo.toml` context includes the openmls 0.9 lines). Without the first PR, add the `web-time`
line to `davey/Cargo.toml` by hand.

---

**Title:** fix: use web-time so encryption works on wasm32-unknown-unknown

**Body:**

`davey` builds for `wasm32-unknown-unknown` (openmls has the `js` feature enabled), but every
`encrypt`/`decrypt` panics there with "time not implemented on this platform": the encryptor,
the decryptor and the cryptor managers' `Clock` use `std::time::Instant`, which has no
implementation on that target.

This switches those three files to `web_time::Instant`
([web-time](https://crates.io/crates/web-time), MIT OR Apache-2.0), which uses
`performance.now()` in browsers and Node and simply re-exports `std::time::Instant` on every other
target, so native behaviour and performance are unchanged. The test clock is untouched.

- Verified with a two-member group and frame round trip under `wasm-bindgen-test` in Node 24 and
  Chromium; fails before the change, passes after.
- `cargo test` and `cargo clippy` pass natively.
- No public API changes.
