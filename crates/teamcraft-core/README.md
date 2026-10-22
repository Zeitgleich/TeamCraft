# teamcraft-core

Client logic shared by desktop, web and mobile (ARCHITECTURE.md §7–§8): the encrypted local store,
the server API, the sync engine and first-run enrollment. All cryptography is delegated to
`teamcraft-crypto`. Builds for `wasm32-unknown-unknown`.

## Modules

| Module        | What                                                                                                |
|---------------|-----------------------------------------------------------------------------------------------------|
| `store`       | `LocalStore` trait; `SqlCipherStore` (native, e2ee.md §9), `MemoryStore`; database key sources      |
| `records`     | Typed records: account, sync cursors, outbox, pinned identities, openmls storage snapshot           |
| `api`         | `ServerApi` + `GatewayConnection`; `HttpServerApi` (reqwest, tokio-tungstenite), `fake::FakeServer` |
| `client`      | `Client`, the per-device sync engine; `PendingEnrollment` for the first run                         |
| `attestation` | Writes the identity key fingerprint to the Keycloak attribute `tc_identity_key` (e2ee.md §3.1)      |
| `workspace`   | Verified admin roster and workspace policy (`WorkspaceState`, `RosterDocument`)                     |
| `search`      | tantivy index of the store and shared encrypted segments (native only)                              |
| `ui_backend`  | `CoreChatBackend`: teamcraft-ui's `ChatBackend` and `SearchBackend` over the store (feature `ui`)   |
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

## Features beyond messaging

- **Workspace roster and policy** (admin.md §2.3): verified as a chain from the self-signed genesis
  roster (first owner = pinned root of trust); every later entry needs an owner of the roster so
  far. Owners publish both logs (`publish_roster`, `publish_policy`); the verified policy is
  `Client::workspace_policy`.
- **Admin actions** on groups the admin isn't in: the signed change goes into the group's queue as
  a proposal entry; a member's client verifies it against the roster and commits it.
  `admin_self_join` and history defaults (guests always `from_join`) are enforced by the committer.
- **Removals the server reports** (`device_remove`, `membership_remove` after admin removal or
  deactivation) are queued and committed by an online member, followed by key rotation.
- **Messages:** files (chunked encryption, presigned upload, hash checked before decrypting), edits
  and deletes of one's own messages, mentions (inside the message and as server metadata),
  reactions, typing indicators.
- **Push:** registration with a fresh push key; `Client::open_push` opens sealed wake-ups.
- **Device linking** (e2ee.md §5): `LinkOffer` (QR code), `NewDeviceLink` on the new device,
  `Client::start_link_approval` / `complete_link_approval` on the existing one; both show the same
  short code before anything is transferred.
- **Organization recovery** (e2ee.md §6.3.1): owner setup with shares sealed to admin devices,
  member opt-in, two admins recovering a user into a one-time request key.
- **Search** (e2ee.md §10.1): see `search`; `publish_index_segments`, `fetch_index_segments`,
  `fetch_search_hits`.

## Trust decisions

- Identity keys are pinned on first use, overridden by the identity keys the verified roster lists
  for owners and admins. A different key from the server is recorded
  (`Client::changed_identities`) and not trusted. OIDC attestation tokens are stored on enrollment
  but other clients don't verify them yet (JWKS check, e2ee.md §3.1).
- Membership changes are authorized for current group members and for roster admins
  (`trust::WorkspaceAuthority`). Per-channel roles (`manage_members`, channel owners) aren't in a
  signed log yet, so every member may add and remove; the server's permission check is the
  finer-grained control until then.
- Server-reported removals are accepted without a signature: they only ever remove access.
- `admin_self_join = owner_approval` refuses self-joins, since there is no approval flow yet.

## Stubbed / not yet done

- Browser: the gateway and rendezvous WebSockets return `Unsupported` on wasm, the store is
  `MemoryStore`, and search scans the store (tantivy doesn't target wasm32).
- Adding new devices of existing members is manual (`Client::add_missing_devices`).
- Content key rotation by message count / age (`RotationPolicy`) – only removals rotate.
- Org key rotation after a recovery, and the approval flow (both need server endpoints).
- An edit or delete that arrives before its target message isn't applied later.
- Followed threads, custom status, link previews, passkey PRF wrapper wiring.
- The other UI traits (`CallBackend`, `AdminBackend`, `SecurityBackend`).
- The MLS storage and the search index are rebuilt or rewritten whole; fine at today's sizes,
  incremental later.

## Server and environment gaps found

1. **Device ID vs. certificate:** registration sends a provisional certificate (device ID 0) as
   `cross_signature`; the final one only lives in MLS credentials (being fixed on
   `feat/server-gaps`).
2. **Key package claims** take a package from every device of the user, including ones already in
   the group (being fixed).
3. **No signal for new devices** to trigger `add_missing_devices` (being fixed: `device_add`).
4. `epoch_conflict` has the epoch only in the text; non-member admins parse it to post proposals
   (being fixed).
5. No custom status on the server (being fixed).
6. **Dev realm:** no account API access for the public client and no `tc_identity_key` user-profile
   attribute, so attestation is unavailable in dev (being fixed).
7. **Org recovery:** no endpoints for sealed shares, recovery requests with approvals, or handing a
   user's wrapped backup keys to the approving admins.
8. **Search segments:** stored through the attachment endpoints and announced in messages; a segment
   listing per conversation would let devices find them without reading the messages first.
9. **Dev environment:** the shared dev Postgres may carry migrations from unmerged branches; the
   integration tests ran against a throwaway database and a server on port 8083 (another session's
   server used 8081). They leave small encrypted test files in the dev Garage bucket.

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
# the server needs the [storage] section (dev Garage) for the attachment test
```
