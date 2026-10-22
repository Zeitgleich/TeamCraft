# teamcraft-core

Client logic shared by desktop, web and mobile (ARCHITECTURE.md §7–§8): the encrypted local store,
the server API, the sync engine and first-run enrollment. All cryptography is delegated to
`teamcraft-crypto`. Builds for `wasm32-unknown-unknown` and runs there with a persistent store.

## Modules

| Module         | What                                                                                                                    |
|----------------|-------------------------------------------------------------------------------------------------------------------------|
| `store`        | `LocalStore` trait; `SqlCipherStore` (native), `IndexedDbStore` (browser, e2ee.md §9.1), `MemoryStore`; key sources     |
| `records`      | Typed records: account, sync cursors, outbox, pinned identities, openmls storage snapshot                               |
| `api`          | `ServerApi` + `GatewayConnection`; `HttpServerApi` (reqwest, tokio-tungstenite / browser WebSocket), `fake::FakeServer` |
| `client`       | `Client`, the per-device sync engine; `PendingEnrollment` for the first run                                             |
| `runner`       | `SyncRunner`: the app loop (reconnect, heartbeats, outbox, UI commands, due membership changes)                         |
| `platform`     | Timers on tokio or `setTimeout`, so the engine runs on a single-threaded browser executor                               |
| `push_preview` | What a notification shows, read-only from the store (+ optional fetch); `PushSession`                                   |
| `web_push`     | Browser: Web Push subscription; service worker entry points (feature `service-worker`)                                  |
| `read_state`   | Followed threads, unread counts, read receipts, acknowledgements as the server reports them                             |
| `attestation`  | Writes the identity key fingerprint to the Keycloak attribute `tc_identity_key` (e2ee.md §3.1)                          |
| `workspace`    | Verified admin roster and workspace policy (`WorkspaceState`, `RosterDocument`)                                         |
| `search`       | tantivy index of the store and shared encrypted segments (native only)                                                  |
| `ui_backend`   | `CoreChatBackend`: teamcraft-client-api's `ChatBackend` and `SearchBackend` over the store (feature `ui`)               |
| `clock`        | Injectable wall clock (`SystemClock`, `ManualClock`)                                                                    |

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
- **Runner**: `SyncRunner::new(client, settings, on_change)` returns the runner and a `RunnerHandle`;
  `run()` is a plain future (tokio task natively, `wasm_bindgen_futures::spawn_local` in the browser)
  that keeps the gateway connected and hands the client back on `RunnerCommand::Stop`.

## Browser

- **Transport:** the gateway and the device-linking relay use the browser's `WebSocket`; REST goes
  through reqwest's `fetch` backend. Nothing in the engine needs `Send` or threads.
- **Store:** `IndexedDbStore::open(name, &browser_database_key(name).await?)`. Every record is
  encrypted (`teamcraft-crypto::local_store`) under a pseudorandom ID; the database key is wrapped by
  a non-extractable Web Crypto key in the same database (why: e2ee.md §9.1). Reads come from a memory
  cache filled at open; writes go to IndexedDB immediately, `flush()` waits for them. Share the
  store with `Rc` (it is not `Send`).
- **Web Push:** `Client::register_web_push(vapid_public_key)` subscribes through the page's service
  worker and registers the subscription with a fresh push key. `web/service-worker.js` is the
  minimal glue (listeners must exist before wasm loads); the app builds its wasm with
  `--target no-modules` and `teamcraft-core/service-worker`, and registers the worker as
  `service-worker.js?db=<store name>`. The worker opens the store read-only, opens the wake-up, and
  shows conversation, sender and — if the store has the message or a `PushSession`
  (`push_preview::save_push_session`, refreshed by the app with its token) lets it fetch the message
  — the decrypted text. Titles and bodies are only names and message text, so nothing needs i18n.
  A click focuses an open window (`postMessage({type: "open_conversation", conversation_id})`) or
  opens `<scope>?conversation_id=<id>`.
- **Search:** scans the store (tantivy doesn't target wasm32).

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
- **New devices of members** (`device_add`): every member that hears it queues the addition per
  shared group, delayed by its rank (devices of online users first, ID-mixed order, capped at
  `device_add_max_rank` × `device_add_stagger`), so normally one member commits and the rest find
  the device present. Retried while the device has no key packages yet (the server announces it
  before they are uploaded).
- **Read state:** `thread_update` refetches `/threads` and `/unread` (last replies are decrypted into
  the store), `read_receipt` and `message_ack` are stored; read them with `read_state`.
- **Messages:** files (chunked encryption, presigned upload, hash checked before decrypting), edits
  and deletes of one's own messages, mentions (inside the message and as server metadata),
  reactions, typing indicators.
- **Push:** registration with a fresh push key; `Client::open_push` opens sealed wake-ups, and
  `push_preview::push_preview` builds what a notification shows without a `Client`.
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

- Browser: no persistent search index (tantivy doesn't target wasm32); a stricter store mode with
  a passkey-PRF database key is possible (`IndexedDbStore::open` takes any key) but not wired.
- `apps/desktop` still has its own sync loop; it could switch to `SyncRunner`.
- Content key rotation by message count / age (`RotationPolicy`) – only removals rotate.
- Org key rotation after a recovery, and the approval flow (both need server endpoints).
- An edit or delete that arrives before its target message isn't applied later.
- Custom status, link previews, passkey PRF wrapper wiring; sending receipts and acknowledgements.
- The other UI traits (`CallBackend`, `AdminBackend`, `SecurityBackend`).
- The MLS storage and the search index are rebuilt or rewritten whole; fine at today's sizes,
  incremental later.

## Server and environment gaps found

1. **Device ID vs. certificate:** registration sends a provisional certificate (device ID 0) as
   `cross_signature`; the final one only lives in MLS credentials (being fixed on
   `feat/server-gaps`).
2. **Key package claims** take a package from every device of the user, including ones already in
   the group (being fixed).
3. **No signal for new devices** to trigger `add_missing_devices`: fixed (`device_add`), handled by
   the core.
4. `epoch_conflict` has the epoch only in the text; non-member admins parse it to post proposals
   (being fixed).
5. No custom status on the server (being fixed).
6. **Dev realm:** no account API access for the public client and no `tc_identity_key` user-profile
   attribute, so attestation is unavailable in dev (being fixed).
7. **Org recovery:** no endpoints for sealed shares, recovery requests with approvals, or handing a
   user's wrapped backup keys to the approving admins.
8. **Search segments:** stored through the attachment endpoints and announced in messages; a segment
   listing per conversation would let devices find them without reading the messages first.
9. **VAPID public key:** the server logs its application server key at start but has no endpoint
   for it; the web app has to be configured with it. A `GET /api/v1/push/vapid-key` (or a field in
   an instance-info response) would remove that step.
10. **Push previews without a session:** the service worker can only fetch the pushed message while
    a stored access token is valid. If the server put the message ciphertext into the wake-up when
    it fits (Web Push allows ~4 KB), previews would work without any token; it stays opaque to the
    push service either way.
11. **Stale devices accumulate:** devices that never come back stay registered with key packages, so
    `add_member` claims and welcomes all of them. In the dev database, repeated integration runs
    (each registers new devices for the same users) made a welcome exceed the server's body limit.
    Real deployments need device expiry (and the client a way to skip stale devices).
12. **Dev environment:** the shared dev Postgres may carry migrations from unmerged branches; the
    integration tests ran against a throwaway database and a server on port 8083 (another session's
    server used 8081). They leave small encrypted test files in the dev Garage bucket.

## UI wiring

The UI's backend traits and view models live in `teamcraft-client-api`, which both `teamcraft-ui` and
`teamcraft-core` (feature `ui`) depend on. `apps/desktop` composes the two (login, enrollment, sync loop).

## Checks

```sh
cargo test -p teamcraft-core --all-features
cargo clippy -p teamcraft-core --all-targets --all-features -- -D warnings
cargo check -p teamcraft-core --target wasm32-unknown-unknown
# with the dev stack and a server on 8081 (skipped without the variable):
TEAMCRAFT_TEST_SERVER_URL=http://127.0.0.1:8081 cargo test -p teamcraft-core --features integration --test dev_stack
# the server needs the [storage] section (dev Garage) for the attachment test
```

### Browser tests

`tests/web.rs` and `tests/web_service_worker.rs` run in headless Chromium (wasm-bindgen-test). The
image in `dev/wasm-test` has Chromium, chromedriver and the matching `wasm-bindgen-cli`:

```sh
docker build -t teamcraft-wasm-test:local dev/wasm-test
docker run --rm -v "$PWD":/src:ro -v teamcraft-wasm-target:/target -e CARGO_TARGET_DIR=/target -w /src \
  teamcraft-wasm-test:local cargo test --target wasm32-unknown-unknown -p teamcraft-core \
  --features service-worker --test web --test web_service_worker
```

`webdriver.json` passes `--no-sandbox` to Chromium for the container.
