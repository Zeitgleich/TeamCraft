# teamcraft-crypto

All TeamCraft cryptography. No other crate implements or combines primitives
(`.claude/rules/security.md`). Builds for `wasm32-unknown-unknown`.

## Modules

| Module           | What                                                                        | Design                           |
|------------------|-----------------------------------------------------------------------------|----------------------------------|
| `secret`         | `SymmetricKey`, `SecretBytes`: zeroized on drop, redacted `Debug`           | security.md                      |
| `ids`            | Local ID newtypes; `ClientMessageId` (random 128 bit, bound in the AEAD)    | –                                |
| `identity`       | Identity key, device keys (Ed25519 + X25519), device certificates           | e2ee.md §2, §3                   |
| `content_key`    | Content key ring per conversation, random key IDs, rotation policy          | e2ee.md §4.1–§4.3                |
| `message`        | XChaCha20-Poly1305 message encryption bound to the plaintext metadata       | e2ee.md §4.1, §4.4               |
| `attachment`     | Per-file key, chunked STREAM encryption (XChaCha20-Poly1305, BE32)          | e2ee.md §4.5                     |
| `key_bag`        | Identity key + all content keys, serialized and encrypted with backup key   | e2ee.md §2, §6                   |
| `backup`         | Backup key, `BackupKeyWrapper` trait, passphrase (Argon2id), keystore, PRF  | e2ee.md §6.1–§6.3                |
| `recovery_key`   | Recovery kit key: 128 bit, Crockford Base32, checksum, `tcrecovery1:` form  | e2ee.md §6.5                     |
| `membership`     | Signed membership changes, `IdentityDirectory`, `MembershipAuthority`       | e2ee.md §4.6, admin.md §2.2–§2.3 |
| `mls`            | `openmls` groups, key packages, content key bundles, commit authorization   | e2ee.md §4.1, §4.6               |
| `push`           | Push wake-up sealed with the device push key; Web Push (RFC 8291) + VAPID   | e2ee.md §7                       |
| `workspace`      | Owner signatures on the admin roster and policy log entries                 | admin.md §2.3, e2ee.md §11       |
| `org_recovery`   | Org key (HPKE DeriveKeyPair), Shamir shares (vsss-rs GF(2⁸)), HPKE wrapping | e2ee.md §6.3.1                   |
| `rendezvous`     | Device linking handshake (AK auth keys, X25519, counter-nonce channel)      | e2ee.md §5                       |
| `local_store`    | IndexedDB record encryption: HKDF lookup IDs, XChaCha20-Poly1305 records    | e2ee.md §9.1                     |
| `source_token`   | Bridge source tokens (256 bit), their SHA-256, constant-time lookup         | e2ee.md §13.1, bridge.md         |
| `fingerprint`    | SHA-256 of a content fed piece by piece, compared in constant time          | bridge.md                        |
| `aead`, `domain` | Internal: AEAD helper with random nonces; domain separation labels          | –                                |

Dependencies are audited RustCrypto / dalek crates plus `openmls` with `openmls_rust_crypto`.
Versions follow what `openmls_rust_crypto` already pulls in, so the stack isn't duplicated more
than `openmls` itself does. Binary encodings use `tls_codec` (already an `openmls` dependency).

Chat and calls share this `openmls` 0.9 stack (ARCHITECTURE.md §6.2): DAVE's `davey` 0.1.5 needs
`openmls` 0.8, whose `hpke-rs`/`libcrux` versions carry RUSTSEC-2026-0207/-0208/-0212, so the
workspace uses a vendored `davey` ported to `openmls` 0.9 (`crates/vendor/davey/VENDORED.md`).
Keep `openmls` versions here and there in lockstep.

## How the pieces fit

- **Identity and devices.** The identity key signs a `DeviceCertificate` (user, device, identity
  key, device signing and agreement key). Clients only trust a device whose certificate verifies
  against the identity key they pinned for that user (`IdentityDirectory`).
- **MLS credentials.** A device's MLS credential is a `BasicCredential` whose identity *is* its
  signed device certificate, and its MLS signature key is the device's Ed25519 key. Every member can
  verify every leaf on its own; the server can't introduce a device the user didn't sign.
- **Membership changes.** Adds and removes are `MembershipChange`s signed by the actor's device key
  and checked by a `MembershipAuthority` (to be backed by the signed admin roster). The signed
  change travels in the MLS commit's authenticated data. Members only merge a commit whose
  add/remove proposals match its verified change exactly, whose change ID wasn't applied before,
  and whose new or updated leaves all have valid credentials. Commits with other proposal types,
  from external senders, or that change a member's identity in an update are rejected.
- **Content keys.** MLS application messages carry only `ContentKeyBundle`s. Message bodies are
  encrypted with the current content key; the AEAD associated data binds conversation, sender user,
  sender device, client message ID (random 128 bit, generated before encrypting) and content key ID.
- **Backup.** The key bag is encrypted with a random backup key; the backup key is stored wrapped by
  each recovery factor. The AAD binds the wrapped key to its user, method and public parameters.
- **Domain separation.** Every signature and AEAD input is `len(label) || label || body` with a
  `TeamCraft-v1 …` label, which can't collide with MLS's `"MLS 1.0 "` labels, so the device key can
  sign both safely.

## Design decisions recorded in the docs

The choices that went beyond the original design draft are now part of `docs/e2ee.md` (§2, §3, §4.2,
§4.4, §4.6) and `docs/admin.md` §2.2: signed admin actions in the commit's authenticated data
instead of MLS external proposals for v1, unsigned additions of further cross-signed devices of
existing members, random 128-bit content key IDs, XChaCha20-Poly1305 only, the MLS credential as
signed device certificate, the ciphersuite, and client-generated 128-bit message IDs bound in the
AEAD. Still open in the docs: how the reserved `system:archive` credential (§11) maps onto the
certificate-based credential.

## Open questions

- **Old-epoch application messages.** Groups keep no past epochs (openmls default), so a key bundle
  sent in epoch *n* is rejected by members already in *n+1*. The server must deliver group messages
  in order, and the sender must re-send its bundle if its epoch was superseded. Alternatively allow
  a small `max_past_epochs`.
- **Persistent state.** Applied change IDs (replay protection) and the MLS group state live in
  memory; both must be persisted together in the client database. The provider is generic over
  `OpenMlsProvider`, so a persistent storage provider can be plugged in.
- **Failed joins consume the key package.** openmls deletes the key package's private key while
  processing a welcome, even if our credential check then rejects it. Clients need enough key
  packages (and a last-resort one) uploaded.
- **Organization recovery** follows e2ee.md §6.3.1; the server endpoints for shares and recovery
  requests are still missing (see teamcraft-core's README).
- **Not yet implemented:** share link password wrapping (§4.5.1), OIDC attestation verification
  (§3.1).
- **Review needed:** the rendezvous handshake (§5), org recovery (§6.3.1) and the recovery kit
  (§6.5) combine primitives in new ways; security.md asks for a review before relying on them.
- **Argon2id cost.** Default 128 MiB, 3 iterations, 1 lane (configurable). "~1 s" (§6.2) still needs
  measuring on phones and in the browser (single-threaded wasm).
- **IDs.** `ids` holds local newtypes; switch to `teamcraft-proto`'s IDs once this crate may depend
  on it, and move `ClientMessageId` there as a wire type. The big-endian `u64` encoding is part of
  signed data and must not change.
- **`cargo deny`:** `openmls_rust_crypto` → `hpke-rs` → `libcrux-*` → `hax-lib-macros` pulls in
  `proc-macro-error2`, flagged unmaintained (RUSTSEC-2026-0173). It's a build-time proc macro, not
  a runtime vulnerability. Needs an `ignore` entry with reason in `deny.toml`, or an upstream fix.

## Checks

```sh
cargo test -p teamcraft-crypto
cargo clippy -p teamcraft-crypto --all-targets --all-features -- -D warnings
cargo check -p teamcraft-crypto --target wasm32-unknown-unknown
```
