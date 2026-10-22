# teamcraft-crypto

All TeamCraft cryptography. No other crate implements or combines primitives
(`.claude/rules/security.md`). Builds for `wasm32-unknown-unknown`.

## Modules

| Module           | What                                                                       | Design                           |
|------------------|----------------------------------------------------------------------------|----------------------------------|
| `secret`         | `SymmetricKey`, `SecretBytes`: zeroized on drop, redacted `Debug`          | security.md                      |
| `ids`            | Local `UserId`, `DeviceId`, `ConversationId`, `MessageId` newtypes         | –                                |
| `identity`       | Identity key, device keys (Ed25519 + X25519), device certificates          | e2ee.md §2, §3                   |
| `content_key`    | Content key ring per conversation, random key IDs, rotation policy         | e2ee.md §4.1–§4.3                |
| `message`        | XChaCha20-Poly1305 message encryption bound to the plaintext metadata      | e2ee.md §4.1, §4.4               |
| `attachment`     | Per-file key, chunked STREAM encryption (XChaCha20-Poly1305, BE32)         | e2ee.md §4.5                     |
| `key_bag`        | Identity key + all content keys, serialized and encrypted with backup key  | e2ee.md §2, §6                   |
| `backup`         | Backup key, `BackupKeyWrapper` trait, passphrase (Argon2id), keystore, PRF | e2ee.md §6.1–§6.3                |
| `membership`     | Signed membership changes, `IdentityDirectory`, `MembershipAuthority`      | e2ee.md §4.6, admin.md §2.2–§2.3 |
| `mls`            | `openmls` groups, key packages, content key bundles, commit authorization  | e2ee.md §4.1, §4.6               |
| `aead`, `domain` | Internal: AEAD helper with random nonces; domain separation labels         | –                                |

Dependencies are audited RustCrypto / dalek crates plus `openmls` with `openmls_rust_crypto`.
Versions follow what `openmls_rust_crypto` already pulls in, so the stack isn't duplicated more
than `openmls` itself does. Binary encodings use `tls_codec` (already an `openmls` dependency).

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
  sender device, message ID and content key ID.
- **Backup.** The key bag is encrypted with a random backup key; the backup key is stored wrapped by
  each recovery factor. The AAD binds the wrapped key to its user, method and public parameters.
- **Domain separation.** Every signature and AEAD input is `len(label) || label || body` with a
  `TeamCraft-v1 …` label, which can't collide with MLS's `"MLS 1.0 "` labels, so the device key can
  sign both safely.

## Deviations from the design docs and proposed doc changes

These are deliberate and should be reflected in `docs/e2ee.md` / `docs/admin.md`:

1. **Admin actions without MLS external proposals (admin.md §2.2).** v1 carries the signed admin
   action in the authenticated data of a commit made by an online member, instead of registering
   the workspace admin key as an `external_senders` entry and sending an external proposal. The
   security property is the same (members verify the signature and role before committing); the
   external-proposal path can be added later without changing the signed action format.
2. **Device additions need no admin signature (e2ee.md §3, §4.6).** A commit may add further
   devices of users who are already members without a signed change, because each device is
   cross-signed by its user's identity key. §4.6 should state this exception.
3. **Content key IDs are random 128-bit values (e2ee.md §4.2),** not a sequence: two members may
   rotate concurrently, and random IDs keep both keys usable.
4. **Content keys use XChaCha20-Poly1305 only.** e2ee.md §2 lists "AES-256-GCM /
   XChaCha20-Poly1305"; the table should name one algorithm.
5. **MLS credential format (e2ee.md §3).** The doc should say that the MLS credential carries the
   signed device certificate (see above), and how the reserved `system:archive` credential (§11)
   maps onto it.
6. **Ciphersuite.** `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`, matching the Ed25519
   device keys. Not stated in the docs yet.

## Open questions

- **Message ID before encryption.** The AEAD binds the message ID, so the sender must know it before
  encrypting. With server-generated Snowflake IDs this costs a round trip per message. Options:
  client-generated message IDs (random 128 bit, server assigns its Snowflake separately), or bind a
  client nonce instead of the server ID.
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
- **Organization recovery (e2ee.md §6.3)** is not implemented: it needs a decision on the public-key
  scheme (HPKE to the org key) and the Shamir secret sharing crate. The `BackupKeyWrapper` trait
  and `WrapMethod::OrgRecovery` are in place.
- **Not yet implemented:** rendezvous device transfer (§5), share link password wrapping
  (§4.5.1), index keys in the key bag (§10.1), OIDC attestation of identity keys (§3.1), push key
  encryption (§7), the signed admin roster behind `MembershipAuthority`.
- **Argon2id cost.** Default 128 MiB, 3 iterations, 1 lane (configurable). "~1 s" (§6.2) still needs
  measuring on phones and in the browser (single-threaded wasm).
- **IDs.** `ids` holds local newtypes; switch to `teamcraft-proto`'s IDs once this crate may depend
  on it. The big-endian `u64` encoding is part of signed data and must not change.
- **`cargo deny`:** `openmls_rust_crypto` → `hpke-rs` → `libcrux-*` → `hax-lib-macros` pulls in
  `proc-macro-error2`, flagged unmaintained (RUSTSEC-2026-0173). It's a build-time proc macro, not
  a runtime vulnerability. Needs an `ignore` entry with reason in `deny.toml`, or an upstream fix.

## Checks

```sh
cargo test -p teamcraft-crypto
cargo clippy -p teamcraft-crypto --all-targets --all-features -- -D warnings
cargo check -p teamcraft-crypto --target wasm32-unknown-unknown
```
