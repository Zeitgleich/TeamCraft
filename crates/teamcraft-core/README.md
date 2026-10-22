# teamcraft-core

Client logic shared by desktop, web and mobile (ARCHITECTURE.md §7–§8): the encrypted local store,
the server API, the sync engine and first-run enrollment. All cryptography is delegated to
`teamcraft-crypto`. Builds for `wasm32-unknown-unknown`.

## Modules

| Module        | What                                                                                              |
|---------------|---------------------------------------------------------------------------------------------------|
| `store`       | `LocalStore` trait; `SqlCipherStore` (native, e2ee.md §9), `MemoryStore`; database key sources     |
| `records`     | Typed records: account, sync cursors, outbox, pinned identities, openmls storage snapshot           |
| `api`         | `ServerApi` + `GatewayConnection`; `HttpServerApi` (reqwest, tokio-tungstenite), `fake::FakeServer` |
| `client`      | `Client`, the per-device sync engine; `PendingEnrollment` for the first run                         |
| `attestation` | Writes the identity key fingerprint to the Keycloak attribute `tc_identity_key` (e2ee.md §3.1)      |
| `ui_backend`  | `CoreChatBackend`: teamcraft-ui's `ChatBackend` over the store (feature `ui`)                       |
| `clock`       | Injectable wall clock (`SystemClock`, `ManualClock`)                                                |

## How it works

- **Enrollment** (`PendingEnrollment::complete`): new identity key → device registration → final
  device certificate (with the server-assigned device ID) → identity key upload (optional OIDC
  attestation) → key bag upload with the backup key wrapped by the recovery passphrase → key
  packages plus a last-resort package. If the account already has an identity key, the identity is
  recovered from the server's key bag with the passphrase instead.
- **Store**: the key bag (identity key + all content keys) is kept encrypted with the backup key, the
  same bytes that are uploaded. The openmls provider storage (group state, key package private
  keys) is snapshotted after every MLS operation, with the applied membership change IDs.
- **Conversations**: the client that creates a channel or opens a DM founds its MLS group and first
  content key. `add_member` / `remove_member` do the REST call, then commit a signed membership change
  (welcomes per added device), then share history keys (`full` / `from_join`) or rotate the content
  key. On `epoch_conflict` the client discards its commit, processes the backlog and retries; a lost
  race to found a DM switches to the winner's welcome.
- **Messages**: written to the store and the outbox with a random client message ID (bound in the
  AEAD, e2ee.md §4.4), encrypted with the current content key when sent, retried while offline.
  Incoming messages are decrypted into the store; messages whose key hasn't arrived are parked and
  retried when keys arrive. Reactions and thread replies are encrypted messages too.
- **Gateway**: resume with the stored session and sequence number; `session_invalid` → identify and
  a full REST sync (conversations, members, welcomes, MLS backlog, new messages, outbox, key
  packages, key bag). Commits are only merged if `teamcraft-crypto` accepts them.
- **UI**: `CoreChatBackend` reads the store synchronously; posting and reactions go into the outbox,
  other actions are `CoreCommand`s the app's sync task runs with `Client::run_ui_command`.

## Trust decisions (interim)

- Identity keys are pinned on first use. A different key from the server is recorded
  (`Client::changed_identities`) and not trusted. OIDC attestation tokens are stored on enrollment
  but other clients don't verify them yet (JWKS check, e2ee.md §3.1).
- Until the signed admin roster exists (admin.md §2.3), a membership change is authorized if its
  signer is a current member of the MLS group (`trust::GroupMemberAuthority`). Admins who aren't
  members can't add people yet.

## Stubbed / not yet done

- Browser: the gateway WebSocket (`HttpGateway` returns `Unsupported` on wasm) and a persistent store
  (IndexedDB with an encryption layer); the web build uses `MemoryStore`.
- Adding new devices of existing members is manual (`Client::add_missing_devices`); nothing triggers
  it yet (see server gaps).
- Content key rotation by message count / age (`RotationPolicy`) – only removal rotates.
- Attachments (presigned upload is on the server), mentions, edits/deletes, typing, followed threads,
  custom status, push registration, device linking via rendezvous (§5), passkey PRF and org recovery
  wrappers (only the passphrase wrapper is wired).
- The other UI traits (`CallBackend`, `AdminBackend`, `SecurityBackend`).
- The MLS storage is written as one snapshot per operation; fine for now, a per-entry table later.

## Server and environment gaps found

1. **Device ID vs. certificate:** the server assigns device IDs, but the device certificate signs the
   device ID. Registration therefore sends a provisional certificate (device ID 0) as
   `cross_signature`; the final one only lives in MLS credentials. Needs `PUT
   /devices/{id}/certificate` or client-chosen device IDs.
2. **Key package claims** take a package from *every* device of the user, including devices already
   in the group and the claimer's own device; those are wasted. A `device_ids` / exclusion filter on
   `POST /users/{id}/key-packages/claim` would fix it.
3. **No signal for new devices:** members aren't told when a colleague registers a device, so nobody
   adds it to existing groups. A gateway event (e.g. `device_add` to everyone sharing a conversation)
   would let one online member run `add_missing_devices`.
4. `epoch_conflict` carries the current epoch only in the message text, not as a field.
5. No custom status on the server (`CoreCommand::SetCustomStatus` is a no-op).
6. **Dev realm:** the public client's access token can't use Keycloak's account API (401) and the
   user profile doesn't declare `tc_identity_key`, so attestation is unavailable in dev; enrollment
   continues without it.
7. **Dev database:** the shared dev Postgres has migration 8 (`m9 admin`) from an unmerged branch, so
   a server built from `main` refuses to start against it. The integration test ran against a
   separate database (`TEAMCRAFT_DATABASE__URL=.../teamcraft_core_it`).

## UI wiring

`teamcraft-core` depends on `teamcraft-ui` (feature `ui`) to implement its trait, so the UI crate
can't depend on the core in turn. The app that wires both (the Tauri app or a web entry crate)
should own the composition, or the backend traits should move to a small shared crate.

## Checks

```sh
cargo test -p teamcraft-core --all-features
cargo clippy -p teamcraft-core --all-targets --all-features -- -D warnings
cargo check -p teamcraft-core --target wasm32-unknown-unknown
# with the dev stack and a server on 8081 (skipped without the variable):
TEAMCRAFT_TEST_SERVER_URL=http://127.0.0.1:8081 cargo test -p teamcraft-core --features integration --test dev_stack
```
