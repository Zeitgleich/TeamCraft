# Upstream PR draft for Snazzah/davey

Not opened yet. Ready to send as-is. The change is `upstream.patch` in this directory, relative to
the upstream repository root (apply with `git apply upstream.patch` on commit `73bcec27`): the
dependency bumps in `davey/Cargo.toml` and the `process_commit` change in `davey/src/session.rs`,
without our `[workspace]` table and vendoring files.

---

**Title:** chore: update openmls to 0.9

**Body:**

This updates `openmls` to 0.9.1, `openmls_basic_credential` to 0.6.0 and `openmls_rust_crypto` to
0.6.0.

Motivation: `openmls_rust_crypto` 0.5.1 depends on `hpke-rs` 0.6.1, which pins `libcrux-sha3`
0.0.8 and `libcrux-secrets` 0.0.5. Those versions are affected by
[RUSTSEC-2026-0207](https://rustsec.org/advisories/RUSTSEC-2026-0207),
[RUSTSEC-2026-0208](https://rustsec.org/advisories/RUSTSEC-2026-0208) and
[RUSTSEC-2026-0212](https://rustsec.org/advisories/RUSTSEC-2026-0212); the fixed versions are only
reachable through `openmls_rust_crypto` 0.6 / `hpke-rs` 0.7. It also lets applications that use
`openmls` 0.9 elsewhere share one MLS stack with davey (with 0.8 the two don't resolve together,
`hax-lib` `=0.3.6` vs `^0.3.7`).

The only API change that affects davey: openmls 0.9 no longer returns
`ProcessMessageError::InvalidCommit(StageCommitError::OwnCommit)` for an own commit echoed back by
the delivery service. If it matches the pending commit, `process_message` returns `Ok` with
`ProcessedMessageContent::OwnPendingCommit`; if it doesn't, it fails with
`StageCommitError::OwnCommitMismatch`. `DaveSession::process_commit` now merges the pending commit
on `OwnPendingCommit` instead of in the error arm; behaviour for clients is unchanged.

- `cargo test` and `cargo clippy` pass.
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
decryptor and cryptor manager use `std::time::Instant`, which has no implementation on that target.

This switches those three files to `web_time::Instant`
([web-time](https://crates.io/crates/web-time), MIT OR Apache-2.0), which uses
`performance.now()` in browsers and Node and simply re-exports `std::time::Instant` on every other
target, so native behaviour and performance are unchanged.

- Verified with a two-member group and frame round trip under `wasm-bindgen-test` in Node 24 and
  Chromium; fails before the change, passes after.
- `cargo test` and `cargo clippy` pass natively.
- No public API changes.

