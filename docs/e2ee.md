# TeamCraft – End-to-end encryption design

Goal: **WhatsApp-level usability.** Users never see keys, passphrases or "verify this session" dialogs
during normal use. Everything is encrypted end to end anyway, because the server runs on a VPS we do
not trust.

Status: design draft.

## 1. What users experience

| Situation                         | What the user does                                                              | What happens underneath                                                                      |
|-----------------------------------|---------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------|
| First login ever                  | Log in (OIDC), then "Create passkey" (Face ID / Windows Hello / fingerprint)    | Identity keys generated, key backup created and wrapped with the passkey                     |
| Second device (laptop + phone)    | Log in on the new device → tap **"Approve"** on the old one (or scan a QR code) | Old device transfers identity + key bag over an encrypted rendezvous channel                 |
| Lost all devices                  | Log in → unlock with passkey                                                    | Key backup unwrapped with the passkey PRF; full history available again                      |
| Lost devices *and* passkey        | Ask IT                                                                          | Two admins approve the recovery; key bag unwrapped with the organization recovery key (§6.3) |
| Joining a channel                 | Click "Join"                                                                    | Existing member's client shares the channel's history keys automatically                     |
| Notification on the phone         | Nothing                                                                         | Encrypted wake-up push, app fetches and decrypts locally, shows sender + preview             |
| Colleague gets a new laptop       | Nothing                                                                         | New device is signed by the colleague's identity key; no warning                             |
| Someone's identity really changes | Sees a banner "Max's security key changed"                                      | Only happens on full reset without recovery                                                  |

There are no "unverified session" warnings for normal device additions, because every device is
cross-signed by the user's identity key (§3). Comparing verification codes is optional, like in
WhatsApp or Threema.

## 2. Keys

| Key                                                | Owner                         | Purpose                                                                |
|----------------------------------------------------|-------------------------------|------------------------------------------------------------------------|
| **Identity key** (Ed25519)                         | user, all their devices       | Signs device keys. Its fingerprint is "the user" for other clients     |
| **Device key** (Ed25519 + X25519)                  | one device                    | MLS credential, signs messages, receives key transfers                 |
| **MLS group state**                                | each device, per conversation | Key distribution (see §4)                                              |
| **Conversation content keys** (XChaCha20-Poly1305) | conversation members          | Encrypt message bodies and history; rotated on member removal          |
| **Index keys**                                     | conversation members          | Encrypt shared search index segments (§10)                             |
| **Key bag**                                        | user                          | Identity key, content and index keys, encrypted with the backup key    |
| **Backup key** (random 256 bit)                    | user                          | Encrypts the key bag; itself wrapped by passkey, devices, org recovery |
| **Push key** (random 256 bit)                      | one device                    | Shared with the server; hides push metadata from Apple/Google          |

## 3. Identity and devices

- On first login the client generates the identity key and one device key, signs the device key with
  the identity key and uploads the public parts.
- The server stores *public* identity and device keys. It cannot add a device on its own: a new device
  key is only trusted by other clients if it carries a valid signature from the user's identity key.
- Other clients pin the identity key (trust on first use). Adding devices never triggers a warning.
  An identity key change does, and that only happens on a full reset.
- **The OIDC login proves account ownership to the server, nothing more.** A freshly logged-in device
  has no identity key, so it can't read anything until it is approved (§5) or recovers (§6).
- The identity key signs a **device certificate**: user ID, device ID, identity public key, the device's
  Ed25519 signing key and X25519 agreement key. A device's **MLS credential is a `BasicCredential`
  whose identity is this signed certificate**, and its MLS signature key is the device's Ed25519 key.
  Every member checks every leaf on its own: decode the certificate, verify it against the pinned
  identity key of the claimed user, check that it vouches for the leaf's signature key.
- MLS ciphersuite: `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` (matches the device keys).
  The device key signs both MLS messages and TeamCraft structures; every TeamCraft signature and AEAD
  input is prefixed with a length-prefixed `TeamCraft-v1 …` label, so it can't collide with MLS's
  `"MLS 1.0 "` labels.

### 3.1 OIDC-attested identity keys

The OIDC provider (reference setup: Keycloak) runs on a separate server, not on the TeamCraft VPS.
That makes it a second, independent trust anchor:

- At enrollment, the client stores the fingerprint of the user's identity key as a Keycloak user
  attribute (via the Keycloak account API, authenticated by the user's own login) and requests an ID
  token that contains it as claim `tc_identity_key`.
- The client uploads this signed ID token to TeamCraft together with its identity key.
- Other clients verify the token's signature against Keycloak's JWKS, fetched **directly from
  Keycloak**, not through the VPS.
- To inject a fake identity key, an attacker would have to compromise both the TeamCraft VPS and
  Keycloak. Users get this without ever seeing a verification code.
- Requires an OIDC provider that lets users write a profile attribute and emits it as a claim
  (Keycloak does). Without that, and for optional local accounts and meeting guests, clients fall
  back to trust on first use and optional verification codes. Guests in the company realm get the
  attestation like members.

Later option: a key transparency log (append-only, auditable) so that a malicious server can't show
different identity keys to different users.

## 4. Messages

### 4.1 Why not "MLS only"

MLS (RFC 9420) gives strong forward secrecy, but a new device or a new member cannot read anything sent
before it joined. For a company chat, history on every device and for new channel members is a hard
requirement. So TeamCraft splits the job:

- **MLS (`openmls`) distributes keys.** Every conversation is an MLS group; every device is a member.
- **Content keys encrypt messages.** Each conversation has a chain of symmetric content keys. The
  current key is sent to all members through MLS. Messages are encrypted with it and stored on the
  server as ciphertext.

Result:

- Any device holding a conversation's content keys can download and decrypt its history from the server.
  No need to store everything locally, no history transfer between devices.
- Removing a member: MLS commit removes their devices, then a new content key is distributed. The
  removed person keeps what they already had but sees nothing new.
- Trade-off: history is protected by content keys, not per-message ratchets. A stolen key bag exposes
  that history. This is the same trade-off WhatsApp makes with encrypted backups.

### 4.2 Content key rotation

- Rotate on: member removal, device removal, every N messages or every 7 days.
- New key is generated by the client that commits the MLS change and sent as an MLS application message.
- Each message header (plaintext) carries `content_key_id`, so clients know which key to use.
- `content_key_id` is a random 128-bit value, not a sequence number: two members may rotate at the same
  time (e.g. both reach the message limit), and random IDs keep both keys usable without coordination.
- Content keys use XChaCha20-Poly1305 with random 192-bit nonces, so devices sharing a key need no nonce
  state.

### 4.3 New channel members

- When a member is added, the adding client (or any online member, if the adder is offline) sends the
  channel's past content keys to the new member through MLS.
- The person adding someone chooses **"Share history"** (on/off) in the add dialog:
  - members: default **on** (full history); the default is an admin setting,
  - guests: always `from_join`.
- Admins can additionally lock the choice per space/channel (e.g. "never share history here").
- Server membership change and MLS commit happen in one transaction from the server's point of view;
  if no member is online, the server queues the add and the next member who comes online completes it.

### 4.4 What the server sees

Plaintext metadata: conversation ID, sender user and device, timestamp, message ID (server Snowflake)
and client message ID, thread ID, `content_key_id`, size, **list of mentioned user IDs** (needed for notifications, §7), the "urgent"
flag. Reactions are encrypted messages, so not even reaction counts are visible.

Encrypted: text, attachments, attachment names, reactions, edits, polls, link previews, drafts.

**Message IDs.** The sending client generates a random 128-bit **client message ID** before encrypting.
The AEAD associated data binds conversation ID, sender user and device, client message ID and
`content_key_id`, so the server can't move a ciphertext to another conversation, attribute it to another
sender or replay it as a different message. The server still assigns its Snowflake message ID on
receipt; that ID is for ordering, pagination and references, and is not part of the encryption. The
client message ID is stored next to it, unique per conversation; clients drop a second message with an
already seen client message ID (replay or duplicate delivery). Sending needs no extra round trip.

### 4.5 Attachments

Client encrypts each file with a fresh random key (XChaCha20-Poly1305, chunked for streaming), uploads
the blob to S3 via a presigned URL, puts key + hash + file name + thumbnail into the encrypted message.

#### 4.5.1 Share links for external download

A file shared in a chat can get a link that anyone can open in a browser, without an account:

```
https://chat.example.com/f/<share-id>#<file-key>
```

- The part after `#` (the file key + encrypted file name) never leaves the browser – browsers don't send
  the fragment to the server. The server only serves the ciphertext; a small download page decrypts it
  in the browser (WebCrypto) and saves the file.
- Every file already has its own random key (§4.5), so the link exposes exactly that one file.
- Options: expiry date, max downloads, optional password (key wrapped with Argon2id(password) on top),
  revoke at any time (server stops serving the ciphertext).
- Server sees: that a link exists, for which blob, download count and times, downloader IPs.
- Limitation, stated in the UI: the download page's JavaScript comes from the server. A compromised
  server could serve a page that leaks the key from the fragment. That's the known weakness of all
  browser-based E2EE downloads (e.g. Firefox Send) and acceptable for files people deliberately share
  with outsiders. Mitigation: the page is a small, versioned static bundle with Subresource Integrity,
  and the desktop/mobile app can verify its hash.
- Admin controls: on/off, max expiry, which roles may create links (default: members, not guests).

### 4.6 Who may change a group

Member clients only commit membership changes signed by someone allowed to make them: a member with
`manage_members`, a channel owner, a space admin or a server admin. Roles are anchored in a signed admin
roster rooted in the server owner's identity key, so the server can't add anyone (including itself) on
its own. See [admin.md](admin.md) §2.

- A membership change (add user with history choice, remove user, remove device) carries a random
  128-bit change ID and the conversation ID and is signed by the actor's device key; the device
  certificate travels with it.
- **v1: the signed change travels in the authenticated data of the MLS commit**, made by an online
  member (for admins who aren't members: the next member client that comes online, admin.md §2.2).
  Every member merges a commit only if the signed change verifies, the actor is authorized, its change
  ID wasn't applied before, and the commit's add/remove proposals do exactly what the change says.
  MLS external proposals from a registered admin key are a later option; the signed change format
  stays the same.
- **Exception: further devices of existing members.** A commit may add devices of users who already
  have a device in the group without a signed change, because each device is cross-signed by its
  user's identity key (§3). Removing anything always needs a signed change.
- Commits with other proposal types (PSK, re-init, external init, group context changes), from
  external senders, or whose update changes a member's user or device are rejected.

## 5. Adding a device (Threema/WhatsApp style)

Two ways, the user picks whichever is convenient:

1. **Approve prompt:** the new device logs in via OIDC and shows a short code. Every other device of
   that user gets a prompt "New login on Firefox/Linux – code 4 7 1 9. Approve?". On approval the old
   device transfers the identity key and the key bag.
2. **QR code:** the new device shows a QR code, the phone scans it (like WhatsApp Web).

Transport for the transfer, modeled on Threema's connection rendezvous protocol. Threema implements it
in Rust (`libthreema`, module `d2d_rendezvous`, AGPL-3.0, compiled to WASM and exposed via UniFFI –
the same setup we use). Since TeamCraft is AGPL as well, we can reuse or port it:

- The QR code (or approval) carries a random 32-byte authentication key `AK`.
- Both devices run an X25519 handshake, keys derived from `AK` + ECDH (BLAKE2b / HKDF), traffic
  encrypted with ChaCha20-Poly1305, relayed by the server, which only sees ciphertext.
- In the code variant, the short code is checked by the user, so a server injecting itself is detected.

Implemented as (`teamcraft-crypto::rendezvous`, `teamcraft-core` linking):

- The new device draws `AK` and a relay ID and shows both (`tclink1:<id>:<AK>`). It is the
  *initiator*; the existing device that scans is the *responder*.
- `hello` (responder → initiator): challenge + ephemeral X25519 key, sealed with
  `HKDF(AK, "…rendezvous responder auth")`; `auth_hello` (initiator → responder): the challenge back,
  its own challenge and ephemeral key, sealed with the initiator auth key; `auth`: the challenge back.
- Transport keys per direction and a 32-byte path hash come from `HKDF(AK ‖ X25519 shared secret)`;
  frames are ChaCha20-Poly1305 with a per-direction counter nonce. The 4-digit short code is the path
  hash mod 10⁴.
- Not a port of libthreema's `d2d_rendezvous`: its path nomination isn't needed with one relay, and
  the construction above uses only primitives already in the crate. It still needs the security
  review security.md asks for.
- The transfer carries the backup key and the encrypted key bag; the new device then registers and an
  existing member adds it to the groups.

## 6. Backup and recovery

The **key bag** (identity key, all content keys, settings) is encrypted with a random backup key and
stored on the server. Messages themselves are already on the server as ciphertext, so the key bag is
all that's needed to restore everything.

The backup key is stored *wrapped* in up to three ways:

### 6.1 Wrapped by devices

Each device holds the backup key in the OS keystore (iOS/macOS Keychain, Android Keystore, Linux
Secret Service). This is the everyday path; nobody types anything.

### 6.2 Wrapped by a passkey (primary recovery, like WhatsApp since 2025)

- The user registers a passkey; the client uses the WebAuthn **PRF extension** to derive a wrapping key.
- The passkey syncs through iCloud Keychain, Google Password Manager, 1Password, etc., so losing a phone
  does not lose the passkey.
- Recovery = log in + unlock with Face ID / fingerprint. No password to remember.
- Fallback for platforms without PRF support: a recovery passphrase, stretched with Argon2id
  (128 MiB, ~1 s), with rate limiting on the server.

### 6.3 Wrapped by the organization recovery key (optional, per workspace)

For a company it is usually unacceptable that an employee's chat history disappears when they lose
their phone and passkey. Optional organization recovery:

- At setup, the workspace generates an **org recovery key pair**. The private key is split
  2-of-N with Shamir's secret sharing among admins and stored on their devices (or hardware keys).
- Each member's backup key is additionally encrypted to the org recovery public key.
- Recovery needs **two admins** to approve. Every recovery is logged and shown to the affected user.
- The VPS never has the org recovery private key.
- Decided: planned for v1, configurable in the admin panel (on/off, separately for members and guests,
  number of admins required). Default: on for members, off for guests.

This means company admins could technically read a user's chats if two of them collude. That's a
policy decision, not a technical one, and it has to be documented for employees.

#### 6.3.1 Construction

All primitives come from audited crates (`hpke-rs`, as used by openmls; `vsss-rs` for secret
sharing); nothing below is a new primitive.

- **Org key.** An owner's client draws a random 32-byte secret `s` and derives the org key pair with
  HPKE `DeriveKeyPair(s)` (RFC 9180 §7.1.3) for DHKEM(X25519, HKDF-SHA256). The public key goes into
  the signed admin roster (`org_recovery_key`, admin.md §2.3), so the server can't swap it.
- **Shares.** `s` is split with Shamir's secret sharing over GF(2⁸) (byte-wise, `vsss-rs`), threshold
  `t` = the policy's `org_recovery.admins_required` (default 2), one share per admin (at most 255).
  Each share is sealed with HPKE (base mode, ChaCha20-Poly1305) to the X25519 agreement key of every
  device of that admin; `s` is wiped afterwards. Admin devices keep their opened share in their
  encrypted local store.
- **Wrapping.** Each member's client seals its backup key with HPKE to the org public key. Info and
  AAD bind the user ID and the wrap method; the org public key is stored next to the ciphertext so a
  later key rotation can tell old wraps apart. The result is one more entry in the user's wrapped
  backup keys (§6), uploaded with the key bag.
- **Recovery.** The recovering device creates a one-time X25519 *recovery request key* and shows its
  fingerprint. `t` admins approve (server-side approval and audit log, shown to the user). Each
  approving admin seals their share to the last approver's device; that device combines the shares
  into `s`, derives the org private key, opens the user's org-wrapped backup key, seals the backup key
  to the recovery request key and wipes `s` and the org key. The recovering device opens it, decrypts
  the key bag and continues like any recovered device.
- **Consequences.** The combining admin's device holds `s` for the duration of one recovery, so the
  owner should rotate the org key (new `s`, new shares, members re-wrap) after each recovery. Two
  colluding admins can recover anyone, as stated above; one admin alone cannot.
- **Server endpoints needed** (not yet there): storing sealed shares per admin device, a recovery
  request with approvals, and handing the user's wrapped backup keys to the approving admins.

## 7. Push notifications (Threema model, extended)

Threema's approach (from its cryptography whitepaper): the APNs payload is a secretbox ciphertext with
a key the client chose and sent to the server; Apple can't read it. The app decrypts it, fetches the
actual message from the server, decrypts it and shows a local notification with preview. On Android
the FCM message is empty and only wakes the app.

TeamCraft does the same, per platform:

| Platform                | Mechanism                                                                                                                                                                                                                                                                                                       |
|-------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **iOS**                 | APNs with `mutable-content`, payload = XChaCha20-Poly1305(push key, {conversation, message ID, sender}). A **Notification Service Extension** decrypts it, fetches the message ciphertext, decrypts it with the content key from the shared App Group keychain, rewrites the notification with sender + preview |
| **iOS calls**           | Call offer arrives as a normal encrypted push; the NSE decrypts it and calls `CXProvider.reportNewIncomingVoIPPushPayload` (iOS 14.5+), which wakes the app into CallKit. This is how Threema does it, and it avoids PushKit's rule that every VoIP push must ring                                              |
| **Android (Google)**    | FCM high-priority data message, empty or encrypted payload → app wakes, fetches, decrypts, shows local notification                                                                                                                                                                                             |
| **Android (no Google)** | **UnifiedPush** (e.g. self-hosted ntfy), or a foreground service that keeps the gateway connection open (Threema's libre build does the latter)                                                                                                                                                                 |
| **Desktop**             | Persistent gateway connection; decrypt locally; native OS notification                                                                                                                                                                                                                                          |
| **Browser**             | Web Push (VAPID, RFC 8291 payload encryption) → service worker fetches + decrypts                                                                                                                                                                                                                               |

Details:

- The NSE only needs content keys (simple symmetric decryption), not MLS state. This avoids the classic
  problem of the extension and the main app both advancing the same MLS ratchet. Incoming MLS
  commits are left for the main app.
- App and NSE still share the local database (App Group container + keychain access group). Like
  Threema's `ProcessCoordinator`, a cross-process lock ensures only one process syncs with the server at
  a time; Darwin notifications tell the other process to reload.
- Apple/Google see: that *a* push was sent to *a* device at time T. Nothing else.
- The server decides *whether* to push using metadata it has anyway: membership, mute settings,
  presence (don't push to the phone while the desktop is active), DND, and the mention list (§4.4).
- Keyword notifications ("notify me on 'deploy'") are matched on the client after decryption.
- iOS caveat: an NSE must show every notification it receives, unless the app has Apple's
  `com.apple.developer.usernotifications.filtering` entitlement. Threema has it (its NSE entitlements
  set it to `true`), so getting it for an E2EE messenger is realistic. We request it; until granted,
  the server pushes only what will definitely be shown (DMs, mentions, followed threads,
  "all messages" channels).

## 8. Calls

Calls use DAVE (see ARCHITECTURE.md §6). The DAVE MLS group for a call is derived from the call
participants' existing device keys, so calls are verified automatically with no extra prompts.
Meeting guests without an account get an ephemeral key and are shown as "Guest (unverified)".

## 9. Local storage

- Local database: SQLite with SQLCipher; key in the OS keystore.
- Decrypted search index (`tantivy`) also stored encrypted.
- Attachments cached encrypted.

## 10. Search

Users expect search to be fast and complete on every device right after login – a common weak point
of self-hosted chats. For comparison, Element X (Matrix) only searches a local index that contains messages
received after search was enabled; older history isn't found, and encrypted search on the web is a
prototype.

TeamCraft can do better because the full history is already on the server as ciphertext and every
member has the content keys (§4.1).

### 10.1 Encrypted shared index segments (decided)

- Search uses `tantivy` on the client: ranking, snippets, stemming (de/en), fuzzy matching, filters.
- Index data is built in **segments per conversation and time range** (e.g. one segment per
  conversation per month). Tantivy segments are immutable, which makes them easy to share and sync.
- Any member client that has the messages (usually a desktop) builds the segment, encrypts it with the
  conversation's **index key** (distributed via MLS like content keys) and uploads it to S3.
- Other members' devices – including a fresh phone – download and decrypt the segments of their
  conversations and search them locally. Full history is searchable minutes after login, without
  re-indexing on every device.
- Current month: each client indexes new messages itself; when the month closes, one client publishes
  the segment (first one wins, others reuse it).
- Edits/deletes: tombstones in a small per-conversation delete list; segments are rebuilt when they
  accumulate too many changes.
- Hits are always verified by fetching and decrypting the actual messages, so a manipulated segment
  can't show fake content – at most a wrong hit that disappears.
- Permissions come for free: you can only decrypt segments of conversations you hold keys for. History
  policy applies (a guest with `from_join` gets keys and segments only from their join onward).
- Mobile: segments are prefetched on Wi-Fi/charging, with an optional size cap (oldest conversations
  fetched on demand when a search needs them).

What the server sees: segment sizes and upload times per conversation, and which segments a device
downloads. Not words, not queries, not which messages match.

Implemented (`teamcraft-core::search`, first version):

- Instead of a separate index key per conversation, every segment gets a fresh random key and is
  encrypted and uploaded exactly like an attachment (§4.5). The key, hash and month travel in an
  encrypted, hidden `index_segment` message of the conversation. Access then follows the history
  policy automatically: whoever can read the announcement can read the segment, and a `from_join`
  member never sees announcements from before they joined.
- A segment is a tantivy index (English and German stemming, ASCII folding) exported as a bundle of
  its files. The client publishes closed months it has messages for; when two clients publish the
  same month, readers use the earliest announcement.
- The local index is built in memory from the encrypted store when searching and never written to
  disk. The browser has no tantivy (it doesn't target wasm32) and scans the store instead.
- Server gap: segments use the attachment endpoints; a dedicated segment listing would let devices
  find segments without reading the conversation's messages first.

### 10.2 Optional: vault node for compliance and file contents

A **vault node** (`teamcraft-vault`) on company hardware (e.g. a NAS or on-prem server) is only needed for the
compliance archive (§11) and, optionally, for extracting text from PDFs/Office files into index
segments. It's not required for normal search.

### 10.3 Rejected: blind index on the server

Keyed search tokens (`HMAC(search_key, term)`) would let the VPS match queries, but it would learn which
messages share words and which match a search; frequency analysis can then reveal common words. With
§10.1 available, this isn't worth the leakage.

## 11. Compliance archive (planned in the protocol from day one)

Not needed for the current team size, but designed in so it can be switched on later without a
protocol change:

- The signed workspace policy has a field `compliance_archive` (`off` by default). When on, it names
  the archive device (the vault node, §10.2).
- Member clients then **must** add the archive device to every conversation covered by the policy and
  share history keys with it. Clients refuse to send into a covered conversation without it, so the
  archive is complete.
- Every covered conversation shows a banner "Messages are archived by the company".
- Policy changes are signed by owners (admin roster, [admin.md](admin.md) §2.3), so the server can't
  switch archiving on silently.
- The archive node provides export (e.g. EML/JSON per conversation), legal hold (suspends retention
  deletion for selected users/conversations) and retention enforcement on its copy.
- Credential type `system:archive` is reserved in the MLS credentials from the start.

## 12. Link previews

- The **sender's client** generates the preview (title, description, image), so pages behind a login
  the sender has access to still get a useful preview. The preview travels encrypted inside the message.
- The **domain allow/block list** is part of the signed workspace policy. The server can't enforce it
  itself, because it can't see the URL inside the ciphertext (and making the domain visible would leak
  which sites people share). So clients enforce it on both sides:
  - the sender's client only generates previews for allowed domains,
  - receiving clients drop previews for disallowed domains before rendering.
- Same effect as server-side filtering, without leaking URLs.

## 13. Bots and integrations

E2EE means a bot can only read a channel if it is a member with its own device keys. Bots therefore
run as clients (TeamCraft bot SDK, Rust) on a machine we trust, not on the VPS. Incoming webhooks and
logs are encrypted by `teamcraft-bridge`, which runs next to the source (ARCHITECTURE.md §4.5).

## 14. Open questions

- Key transparency: when?
- Segment size and mobile prefetch defaults (per month? size cap on phones?)
