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
- **Signed device list.** A certificate never expires, so on its own it can't take a lost device
  back. The identity key therefore also signs the user's **device list**: the active devices (ID and
  signing key) and every device revoked so far, with a sequence number that only grows. Each device of
  the user adds itself when it enrolls; revoking a device (from another device of the same user)
  publishes a list without it, then that device signs the device's removal from every group it shares
  with it. Other clients keep the newest list they verified per user and refuse older ones; they add a
  device to a group, accept a commit adding it, or seal an org recovery share to it only if the list
  names it with that key. A device removed from a group by a signed "remove device" never comes back
  into that group, whatever the server serves. Users who never published a list (older clients) are
  accepted as before. The server stores the list without reading it (`GET /users/{id}/device-list`,
  `PUT /me/device-list`, compare-and-swap on the sequence).
- **Removals need a signer.** A removal the server reports (an admin's deactivation, a revoked device)
  is only a hint; clients don't sign a removal on the server's word. The device that revokes signs
  (above); an admin removes people with a signed change any member commits, also in conversations the
  admin isn't in (admin.md §2.2).
- MLS ciphersuite: `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` (matches the device keys).
  The device key signs both MLS messages and TeamCraft structures; every TeamCraft signature and AEAD
  input is prefixed with a length-prefixed `TeamCraft-v1 …` label, so it can't collide with MLS's
  `"MLS 1.0 "` labels.

### 3.1 OIDC-attested identity keys

The OIDC provider (reference setup: Keycloak) runs on a separate server, not on the TeamCraft VPS.
That makes it a second, independent trust anchor:

- At enrollment, the client stores `"<TeamCraft user ID>:<identity key fingerprint>"` as a Keycloak
  user attribute (via the Keycloak account API, authenticated by the user's own login) and requests an
  ID token that contains it as claim `tc_identity_key`. Binding the user ID in the claim means the
  mapping from the token's subject to a TeamCraft user doesn't come from the server either.
- The client uploads this signed ID token to TeamCraft together with its identity key.
- Other clients verify the token's signature against Keycloak's JWKS, fetched **directly from
  Keycloak**, not through the VPS (the discovery document's `jwks_uri` must be on the issuer's host),
  and check issuer, audience and the claim before pinning a key. Implemented for ES256 and EdDSA ID
  tokens (`teamcraft-crypto::attestation`); the realm's ID token signature algorithm must be one of
  them. The token's expiry isn't checked: it attests a key, not a session.
- Configured per deployment (`ClientSettings::maybe_attestation`: issuer, client ID, required or
  not). When required, a key without a valid attestation isn't pinned at all; otherwise it is pinned
  as trust on first use and marked as not attested, so the UI can show it.
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
- After a removal (and after an addition that shares history only from the join on) the new key is
  the **epoch key**: every member derives it from the MLS exporter of the new epoch
  (`export_secret("TeamCraft-v1 epoch content key", conversation ID ‖ epoch, 48)`: 16 bytes key ID,
  32 bytes key) when it merges the commit, and switches to it. Nobody has to send it, so the server
  can't hold it back from some members and leave them on a key the removed person knows; the removed
  device never gets the epoch's secrets. (A member that never receives the removal commit stays in
  the old epoch and keeps using the old key; it can't tell.)
- Content key bundles (MLS application messages) only carry history for new members. A member takes a
  bundle's "current" key only if it has none yet, so an old or delayed bundle can't switch it back.
- Each message header (plaintext) carries `content_key_id`, so clients know which key to use.
- `content_key_id` is a random 128-bit value, not a sequence number: two members may rotate at the same
  time (e.g. both reach the message limit), and random IDs keep both keys usable without coordination.
- Content keys use XChaCha20-Poly1305 with random 192-bit nonces, so devices sharing a key need no nonce
  state.

### 4.3 New channel members

- When a member is added, the adding client (or any online member, if the adder is offline) sends the
  channel's past content keys to the new member through MLS. With history from the join on, it sends
  only the new epoch key, which the other members derive themselves.
- The choice is part of the signed change and settled before signing (guests: always from the join
  on); a client refuses to commit an admin's signed full-history add of a guest.
- Without an explicit choice the client uses the signed workspace default, never the server's
  per-channel setting, and shares full history only with users the signed roster lists: the server's
  "this user is not a guest" isn't signed, so a user the client can't vouch for gets history from the
  join on. (Signed per-channel history settings and a signed guest list would lift this; until then,
  sharing full history with others takes the explicit choice in the add dialog.)
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

**Sender signatures.** The content key is shared by all members, so on its own it only proves "some
member wrote this". Every message is therefore also signed by the sending device's Ed25519 key over
the associated data, nonce and ciphertext. Receivers check it against the signing key from the
device's verified certificate (recorded from the MLS group members and the user's signed device list);
a message whose device key isn't known yet waits until it is. Edits, deletes and reactions are
messages too, so nobody can change or delete someone else's message. A device that left a conversation
(removed, or its user removed) can't add messages dated after it left (five minutes of slack for
clocks). Messages from before signatures existed are refused unless the client setting
`accept_unsigned_messages` allows them.

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

**Implemented (client side):** the fragment is `version || flags || key material || name nonce ||
encrypted name`, base64url. Without a password the key material is the file key; with one it is
`salt || Argon2id parameters || nonce || XChaCha20-Poly1305(Argon2id(password, salt), file key)`,
using lighter parameters than the backup passphrase (32 MiB, 3 passes) because the download page
runs them in WebAssembly, and refusing parameters outside the backup's limits when opening, so a
hostile link can't make the browser allocate absurd memory. The file name is encrypted with the
file key. The server endpoint for creating links (`POST /attachments/{id}/share-links`) and the
download page are not built yet.

**Thumbnails:** the sender's client makes a small PNG of image attachments (decoding capped at
40 megapixels) and puts it next to the file key inside the encrypted message.

### 4.6 Who may change a group

Member clients only commit membership changes signed by someone allowed to make them: a member with
`manage_members`, a channel owner, a space admin or a server admin. Roles are anchored in a signed admin
roster rooted in the server owner's identity key, so the server can't add anyone (including itself) on
its own. See [admin.md](admin.md) §2.

- A membership change (add user with history choice, remove user, remove device) carries a random
  128-bit change ID, the conversation ID and the group epoch the signer saw, and is signed by the
  actor's device key; the device certificate travels with it.
- **Freshness.** A change is committed and accepted only in the epoch it was signed for or up to 32
  epochs later, and only if no other signed change about the same user was committed since it was
  signed and the device was already in the group when it was signed. Each device keeps the epoch it
  joined and the recent "user touched in epoch N" entries next to its MLS state. So the server can't
  replay an old "add Mallory" after Mallory's removal, not even to a device that joined later and
  never saw either. An admin who isn't a member signs for the epoch the server reports; if the epoch
  moved on too far before a member commits, the change is dropped and has to be made again.
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

### 4.7 Converting a channel between private and public

`PUT /api/v1/conversations/{id}/visibility` (permission `convert_channel_privacy`: channel owners, space
admins and instance admins by default; `PATCH` with `visibility` goes through the same code) turns a channel
public or private. Public channels are listed in the channel browser and any space member (never a guest)
may join them; private ones are invitation-only and invisible to non-members. What it means for the
encryption, and what does and does not hold:

- **The MLS group does not change.** Conversion neither adds nor removes anybody and doesn't rotate keys;
  everybody who is in stays in, in both directions. It only changes who the *server* lets join from now
  on. The server still can't add a device to the group (§3): a member who joined a public channel is
  brought into the group by an existing member's client, like any other addition (§4.3, §4.6), and reads
  nothing until then.
- **History is not signed per channel.** "Public" and the channel's `history_policy` are the server's
  word; no signed record says what a channel's newcomers may read. So clients decide on their own, and
  only ever lower what they share: when a member's client adds somebody **without an explicit choice**
  into a channel the server calls public, it shares history from the join on, whatever the signed
  workspace default says, because the newcomer may have joined by themselves and nobody chose to show
  them the past. **Full history in a public channel needs an explicit choice** by the member who adds
  the person ("share history" in the add dialog). A dishonest server can make a public channel look
  private; then the usual default for roster members applies, which is what a private add would give them.
- **Self-joins** sign `from_join` for themselves, and a member's client refuses to commit a signed self-add
  that asks for more (a modified client can't talk the group into showing it the past).
- **`history_policy` in the conversion request** is therefore a *hint* the server stores and shows (and
  that the add dialog can preselect); it is not what protects the past. What protects it is the rule
  above. Converting to public never gives anyone history they didn't have, and can't take anything back
  from people who already read it.
- **Going private again** keeps everybody who joined while it was public. Removing them is an explicit
  member removal, which rotates the keys (§4.2).
- Guests stay what they were: they can't join public channels on their own and are always `from_join`.
- Integration channels (bridge output) stay private; archived channels are converted after unarchiving.
- Members get a `privacy_changed` system event, the audit log records `conversation_visibility_changed`
  with the old and new value (metadata only).
- *Later:* a signed per-channel history record (like the roster) would let clients trust "this channel
  shares its past with newcomers" without the server's word.

### 4.8 Moving a channel between spaces

`PUT /api/v1/conversations/{id}/space` (`manage_space_properties` in the space the channel leaves and in the
one it enters) changes only the `space_id` and drops the category. Like a conversion (§4.7) it is the
server's word about *where a channel is listed*; **the MLS group, its members and the history stay as they
are**, and nobody is added or removed. Consequences:

- Every member of the channel must already be a member of the new space (guests: not expired);
  otherwise the move is refused (409) and the people have to be added to the space first. Channel
  members are always space members, so leaving the space, being removed from it or a guest's expiry
  still ends the channel membership, and guest restrictions keep following the space role. For
  newcomers of a public channel it is the new space's members who may join.
- The space's permission scheme and its retention defaults apply from the move on; a channel's own
  retention setting stays.
- A move is announced as a `moved` system event (metadata) and, by the client of the person who moved it,
  as an encrypted `moved` group message naming the new space.
- The new space's admins manage the channel from then on (its scheme applies), like any channel of that
  space.

## 5. Adding a device (Threema/WhatsApp style)

Two ways, the user picks whichever is convenient:

1. **Approve prompt:** the new device logs in via OIDC and shows a short code. Every other device of
   that user gets a prompt "New login on Firefox/Linux – code 471 902. Approve?". On approval the old
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
- `hello` (responder → initiator): challenge + a commitment `SHA-256(label ‖ ephemeral key)`,
  sealed with `HKDF(AK, "…rendezvous responder auth")`; `auth_hello` (initiator → responder): the
  challenge back, its own challenge and ephemeral X25519 key, sealed with the initiator auth key;
  `auth`: the challenge back and the committed ephemeral key, which the initiator checks. Handshake
  messages carry a random 96-bit nonce, so scanning a code twice never reuses one.
- Low-order keys are refused (the X25519 result must be contributory). Transport keys per direction
  and a 32-byte path hash come from `HKDF(salt = both ephemeral keys, AK ‖ X25519 shared secret)`;
  frames are ChaCha20-Poly1305 with a per-direction counter nonce. The 6-digit short code is the path
  hash mod 10⁶.
- Why the commitment: if `AK` reaches the old device through the server (approval prompt), the
  server can run a handshake with each side. It has to send its key to the old device before that
  device reveals its own, and commit to its key towards the new device before seeing the new device's
  key, so it can't search for keys that make both codes equal; it matches by chance with 10⁻⁶.
- Not a port of libthreema's `d2d_rendezvous`: its path nomination isn't needed with one relay, and
  the construction above uses only primitives already in the crate. It still needs the security
  review security.md asks for.
- The transfer carries the backup key and the encrypted key bag; the new device then registers and an
  existing member adds it to the groups.

## 6. Backup and recovery

The **key bag** (identity key, all content keys, settings) is encrypted with a random backup key and
stored on the server. Messages themselves are already on the server as ciphertext, so the key bag is
all that's needed to restore everything.

Every upload carries a sequence number the uploading device picks above any it has seen, and the
AEAD's associated data binds the bag to the user, that sequence and a hash of the wrapped backup keys
stored next to it. Devices keep the highest sequence they saw and refuse older bags, so the server
can't serve an old bag (with keys a removed device left behind, or without newer ones); and it can't
drop or swap a wrap (a passkey, the org wrap) without the bag failing to open. A device takes the
server's wraps only after the bag next to them opened. (A device with no history, such as one being
recovered, can't tell an old bag from the newest; it gets what the server serves.)

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
- A passphrase that wraps the backup key has at least 8 bytes (apps may ask for more). Only an
  enrollment with a passphrase creates a new identity: a linked or recovering device that the server
  tells "this account has no identity" stops, instead of creating one the server could unwrap.
- Changing the passphrase replaces only the passphrase wrap; the other wraps stay.

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
- **Recovery.** The recovering device creates a one-time X25519 *recovery request key* and registers
  it; the server assigns the request ID. The device shows the key's **fingerprint** (32 hex digits
  from a SHA-256 of the key); the user reads it to an admin by phone or in person, and the combining
  admin's client only seals to a request whose code the admin entered, so a request key the server
  swapped is refused. The request ID and user are bound into every seal of the recovery. `t` admins
  approve (server-side approval and audit log, shown to the user). Each approving admin seals their share to the combining admin's device for this request
  only (HPKE info binds request ID and key), after checking that the combiner is an admin of the
  signed roster. The combining device combines the shares into `s`, derives the org private key,
  opens the user's org-wrapped backup key, seals it to the confirmed request (bound the same way) and
  wipes `s` and the org key. The recovering device opens it, decrypts the key bag and continues like
  any recovered device.
- **Custodians** are taken from the signed roster: setting up shares for a user who isn't an admin
  there fails, and shares are sealed only to devices in the admin's signed device list (§3).
- **New backup key.** The admins saw the user's backup key, so the recovered device does not keep
  it: it draws a new backup key, re-encrypts the key bag and wraps it with a new passphrase the user
  chooses and the current org key. Older wraps (passkeys) are dropped and enrolled again; the user's
  other devices, if any survived, link again.
- **Consequences.** The combining admin learns `s` (Shamir over GF(2⁸) can't do threshold decryption
  without reconstructing it), so an owner rotates the org key after each recovery: new `s`, new
  shares, a new key in the roster. Members' clients notice the changed key on their next sync and
  wrap their backup key to it; the old wrap is dropped. Until the owner rotates, the combining admin
  together with the server could open other members' wraps. Two
  colluding admins can recover anyone, as stated above; one admin alone cannot.
- **Server endpoints** (`/api/v1/org-recovery`). The server stores opaque sealed bytes, counts approvals,
  relays material and writes the audit trail; it never sees a share, the org key or a backup key.

  | Step                           | Call                                         | Who                 |
  |--------------------------------|----------------------------------------------|---------------------|
  | Store the sealed shares        | `PUT /shares` (key version only grows)       | owner               |
  | Fetch the shares of my devices | `GET /shares/mine`                           | admin               |
  | Ask for recovery               | `POST /requests` (request public key)        | the person, new device |
  | List / read requests           | `GET /requests`, `GET /requests/{id}`        | admins; the person  |
  | Approve on a device            | `POST /requests/{id}/approve`                | admin (not the person) |
  | Hand in a share                | `PUT /requests/{id}/share`                   | approver, sealed to the combining device |
  | Fetch shares and wrapped keys  | `GET /requests/{id}/material`                | combining admin only |
  | Deliver the sealed backup key  | `PUT /requests/{id}/result`                  | combining admin     |
  | Fetch it                       | `GET /requests/{id}/result`                  | the person          |
  | Turn down / withdraw           | `POST /requests/{id}/deny`, `DELETE`         | admin; the person   |

  The approval that reaches the policy's `admins_required` makes that admin's device the combining one;
  the other approvers see it in the request and seal their shares to it. The server computes the
  fingerprint of the request key (shown on the recovering device and compared by the admins). A person
  has one open request, which expires (`limits.org_recovery_request_ttl_secs`). Every step is an audit
  entry that targets the person, so it appears in their security log.

### 6.4 Account data in the key bag (implemented)

Saved items, drafts, saved searches and trust decisions are personal and must follow the user to every device, but
the server must not learn them. They live in the key bag (version 2 adds an `account_data` field
next to the identity and content keys), so they are encrypted with the backup key like the keys and
restored together with them on a new device.

- Devices edit independently and upload the whole key bag with compare-and-swap on its version. On a
  conflict the device downloads the newer bag, merges and retries: content keys are a union; every
  account data entry carries the time of its last change, the newer side wins per entry, and
  removals stay as tombstones (dropped after 90 days) so a removal isn't undone by an older copy.
- Other devices pull the key bag when a gateway session starts and periodically while running
  (default every 60 s). A gateway event announcing a new key bag version would make this instant
  (server gap).
- The server sees that the key bag changed and its size, not what changed. Drafts make the key bag
  change while typing; clients upload at most once per sync round.
- **Trust decisions travel too.** The identity keys the user's devices pinned and the workspace root
  of trust are account data. A new or linked device starts with them instead of trusting the server
  on first use again: it verifies colleagues' credentials with the inherited pins and refuses a
  forged genesis roster. A key change the user accepted on one device is a newer entry and reaches
  the others. The root's fingerprint is available for comparing (admin.md §2.3).
- **The activity feed's read mark** (`activity_read`) is the sent time of the newest feed entry the
  user looked at. Merging takes the later one, so reading on any device counts everywhere and never
  moves back. The feed itself is computed from decrypted messages on each device (notifications.md).

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
- Apple/Google see: that *a* push was sent to *a* device at time T. Nothing else. (Through a push
  gateway, the gateway sees the same, plus an opaque ID of the sending server; §7.1.)
- The server decides *whether* to push using metadata it has anyway: membership, mute settings,
  presence (don't push to the phone while the desktop is active), DND, and the mention list (§4.4).
- Keyword notifications ("notify me on 'deploy'") are matched on the client after decryption.
- Browser (implemented): the service worker opens the encrypted local store (§9.1) read-only, opens
  the wake-up with the push key and shows conversation and sender names. The message text appears
  if the store already has the message, or if the app left a still valid access token
  (`PushSession`, in the encrypted store) so the worker can fetch and decrypt it; otherwise the
  notification has names only. Like the NSE, the worker never touches MLS state. The page subscribes
  with the server's VAPID key (`GET /api/v1/push/web-key`), registers the subscription with a fresh push
  key, keeps the access token in the `PushSession` fresh, and tells the worker which database to open;
  a click on a notification focuses an open window or opens `/?conversation_id=<id>`.
- iOS caveat: an NSE must show every notification it receives, unless the app has Apple's
  `com.apple.developer.usernotifications.filtering` entitlement. Threema has it (its NSE entitlements
  set it to `true`), so getting it for an E2EE messenger is realistic. We request it; until granted,
  the server pushes only what will definitely be shown (DMs, mentions, followed threads,
  "all messages" channels).

### 7.1 Push gateway

Apple and Google only deliver to the holder of an app's signing identity, so a self-hosted server can't
push to the store apps of its publisher on its own. A **push gateway** ([push-gateway.md](push-gateway.md))
holds the publisher's APNs and FCM credentials and forwards the sealed wake-ups of any server. Every
server uses it the same way, including the publisher's own; servers with their own apps send to
APNs and FCM directly. Web Push and UnifiedPush never go through it.

The wake-up is sealed with the device's push key *before* it leaves the server, so a gateway handles
exactly what Apple and Google handle: a device token and ciphertext. Threat model:

- A **malicious gateway** can drop or delay pushes, refuse a server, replay a wake-up it has seen
  (the device fetches the named message again: at worst a duplicate notification) and correlate device
  tokens with servers and timing. It **cannot read** a wake-up (no push key), **cannot forge** one (a
  ciphertext it makes up fails authentication and is discarded by the app), and does not learn user,
  device or conversation IDs. Messages are never lost by a gateway failure: apps catch up when they
  run or reconnect; only the prompt notification is.
- A **malicious server**, or an attacker with a copy of a server's database, **can't use a device token
  through any other server's identity**: the app never gives a server the token, it seals it to a key of
  the gateway together with the one server that may push to it and an expiry (**bound token**, HPKE,
  push-gateway design.md §4). The gateway opens it and refuses it for any other server, after
  expiry, or after the app revoked it (when the user leaves or removes the server). A server can only
  push to the devices of its own users, bounded by gateway-side limits per server and per device
  token, a fixed alert text and topic, and a block list; it learns nothing and can't make the app show
  content. The app takes the gateway's key from the gateway itself, not through the server, and a
  store app uses the gateway it was built for, so a server can't swap in a key of its own.
- The server authenticates to the gateway with an Ed25519 key pair of its own (not related to any
  user, device or MLS key); that key authenticates the *server* only and protects no content.

## 8. Calls

Calls use DAVE (see ARCHITECTURE.md §6): an MLS group per call (members are devices) and per-frame
encryption of media; the SFU only forwards encrypted frames. The SFU is DAVE's *external sender*: it
proposes adding and removing members, but members create the commits and the SFU never holds media
keys.

**Binding DAVE keys to devices.** DAVE v1 signs MLS with P-256 keys, while device keys are Ed25519
(§2), so a device can't use its device key as DAVE credential key. Instead:

1. For every call a device generates a **fresh P-256 key pair** (never reused across calls) for its
   DAVE MLS leaf.
2. It signs a **call key binding** with its device key: `{ call_id, user_id, device_id, DAVE public
   key, not_after }`, domain-separated with the label `TeamCraft-v1 call key binding` (validity at
   most one hour; it is only needed while the device joins). The binding travels to the SFU with the
   DAVE key package; the SFU hands it to the members together with the proposal to add the device,
   and to a joining device together with the welcome (the bindings of the existing members).
3. Members verify a binding against the device's certificate (§3, the same `VerifiedDevice` as in
   chat) and reject it if it is for another call, another device, expired, or valid for too long.
   Before committing proposals, and after every epoch change, each member checks that every other
   member's MLS leaf signature key equals the key in that member's verified binding. A mismatch or a
   member without a verified binding stops media for the call (no unencrypted fallback).
4. Members only accept proposed devices that are on the call roster they trust (from the server,
   verified like chat membership); the SFU's own list of expected members is not trusted.

This way the SFU can neither add a device of its own choosing nor swap a member's DAVE key: the
calls are verified with no extra prompts, and the DAVE privacy code and per-member verification codes
remain available for out-of-band comparison. Meeting guests without an account get an ephemeral
key and are shown as "Guest (unverified)".

### 8.1 Call backgrounds

Backgrounds are composited on the sending device before encoding, so the SFU and other participants
only ever see the result. The pictures themselves:

- **Own pictures** (per user): encrypted like attachments (§4.5), with a fresh key each, and stored at
  `/me/backgrounds`. The key, hash, name and size are account data in the key bag (§6.4), so only the
  user's devices can open them, and they sync to those devices; a `key_bag_update` gateway event makes
  the other devices sync right away. Before upload the app re-encodes the picture as a JPEG, which drops
  metadata such as the place a photo was taken.
- **Organization pictures** (admin.md §3.5): stored unencrypted, because they aren't secret from the
  people who would see them in calls, and an organization-wide key would need its own distribution and
  rotation for no gain. Integrity is what matters: the owner-signed workspace policy (§11, admin.md
  §2.3) lists each picture with its SHA-256, and clients use a picture only after its hash matches the
  policy they verified, so a compromised server can't swap a picture for another. The server checks
  that listed pictures exist with the stated size, so owners can't publish one nobody can fetch.
- **Built-in pictures** are drawn by code (`teamcraft-media`), so they need neither files nor downloads.

Old owner clients that re-sign the policy (after a roster change) without knowing `call_backgrounds`
drop the section, which means `free` until an up-to-date owner client signs again; update owner devices
before using organization backgrounds.

## 9. Local storage

- Local database: SQLite with SQLCipher; key in the OS keystore (desktop).
- Android: plain SQLite with records sealed like the browser store (§9.1), because SQLCipher needs
  a libcrypto the NDK doesn't ship. The database key is random, wrapped by an AES-GCM key in the
  Android Keystore (hardware-backed where available); see §9.2.
- Decrypted search index (`tantivy`) also stored encrypted.
- Attachments cached encrypted.

### 9.1 Browser (implemented)

The browser has neither SQLCipher nor an OS keystore. The web client keeps the same store
(`LocalStore`) in IndexedDB and encrypts it record by record (`teamcraft-crypto::local_store`):

1. **Database key:** 256 random bits. HKDF-SHA256 derives a *lookup key* and a *record key* from it.
2. **Record IDs:** each record is stored under `HKDF-Expand(lookup key, labeled(namespace, name))`, a
   pseudorandom 32-byte ID. Table names, conversation, user and message IDs never appear in
   IndexedDB.
3. **Records:** the plaintext name and value are sealed with XChaCha20-Poly1305 under the record key,
   with the record ID as associated data, so records can't be swapped or moved unnoticed. Opening the
   database with another key fails on the first record.
4. **Where the database key lives:** wrapped (AES-256-GCM) by a **non-extractable Web Crypto key**,
   both stored in the same IndexedDB database. The wrapping key's own bytes can never be read by a
   script, but any script of the origin can *use* it: unwrapping the database key is one
   `crypto.subtle.decrypt` call that returns the raw key. The wrapping only protects the key at rest
   (a copy of the IndexedDB files without the browser's key store); it does not hide it from code
   running in the page.

**Why a non-extractable key and not the passkey PRF or a passphrase:** the service worker must open
the store without a user gesture to decrypt a push and show a notification with a preview (§7). A key
derived from the passkey PRF or a passphrase needs the user every time the browser restarts, which
would make push previews impossible and is a poor fit for a chat app that is opened many times a
day.

What this protects, and what it doesn't:

| Attacker                                      | Protected                                                                               |
|-----------------------------------------------|-----------------------------------------------------------------------------------------|
| Copy of the whole browser profile             | No: the browser keeps the wrapping key's bytes in the profile                           |
| Injected script on the origin (XSS)           | No: it can unwrap the database key, copy it with all records and decrypt them offline, for good. Same for the wasm memory (decrypted cache, MLS state, device key) |
| Other origins, extensions without host access | Yes (same-origin policy)                                                                |
| Server                                        | Only as far as the web client's code is trustworthy: the server serves that code (§9.3) |

The store is therefore as strong as the browser profile *and* as the code running in the page. The
mitigations are about keeping foreign script out (§9.3 and the policy below), not about hiding the
key from script that got in. For people who need more, the desktop and mobile apps do not take their
code from the server.

**What keeps script out of the web client:** no `innerHTML` for user content (messages render from a
typed tree, links only for `http(s)`); a Content-Security-Policy without inline scripts or styles
(the loader is an external file and dynamic styles go through the CSSOM) and with `connect-src`
limited to the app's own origin, the identity provider and the SFU, so injected script cannot send
data elsewhere (`teamcraft-server` builds the policy from its configuration and refuses an invalid
one at startup); `frame-ancestors 'none'`, `object-src 'none'`, `base-uri 'none'`. Trusted Types
(`require-trusted-types-for 'script'`) would stop DOM XSS regressions as well, but the UI framework
builds static markup through `innerHTML` and the service worker registers from a string URL, so the
client does not start under it yet; a deployment can add the directive to `web.content_security_policy`
once the client has a policy.

The design leaves room for a stricter mode: `IndexedDbStore::open` takes any 256-bit key, so a
deployment can derive it from the passkey PRF (§6.2) instead, at the cost of push previews. Choosing
that per workspace is a policy setting for later (PLAN.md).

### 9.2 Android (implemented)

The same sealed-record construction as §9.1, over plain SQLite (`SealedSqliteStore`): the file
holds only pseudorandom lookup IDs and XChaCha20-Poly1305 ciphertext. The 256-bit database key is
generated by the app, wrapped with a non-exportable AES-256-GCM key in the Android Keystore
(StrongBox or TEE where the device has one) and stored wrapped in the app's private files. Unlike the
browser, the Keystore key can't be read even with a copy of the app's data directory: unwrapping
needs the device. No user authentication is required for the key, so push wake-ups (§7) can open
the store while the screen is locked; the wrapped key is excluded from backups, so a restored
backup can't be opened elsewhere.

Plaintext outside the sealed store, and how long it lives:

- Files the user opens are decrypted to a folder per account (`downloads/<account>`), so another
  app can open them; they are deleted at sign-out (a file that is gone is shown as not downloaded).
- Files to send are copied out of the picking app into `outgoing/<account>`; the core deletes each
  copy once its upload finished or failed for good, and the folder is wiped at sign-out.
- Notifications show what the workspace policy allows (§7) and are removed at sign-out.

### 9.3 Web client integrity (implemented)

The server serves the web client's code, so a compromised server could serve a client that leaks
everything (§9.1). Signing the releases narrows this to what the operator controls:

- **Release manifest.** `scripts/sign-web-release.sh` (run on the machine that holds the release key,
  not on the server) writes `release.json`: every file of the bundle with its SHA-256, and an Ed25519
  signature over `teamcraft-web-release-v1\n` + the canonical JSON of `{version, files}` (sorted keys).
  It also pins the release public key into `sw.js`. The files include the Rust half of the service
  worker (`teamcraft-web-sw_bg.wasm`).
- **Verifying service worker.** On install and on every update `sw.js` fetches `release.json`,
  checks the signature against the pinned key, fetches every listed file, compares the hashes and only
  then stores the files in a versioned cache and activates. The app is served from that cache, never
  from the network. An update that does not verify is dropped: the previous verified version keeps
  running and the pages are told, so the UI warns (and shows the key's fingerprint on the About
  screen). A worker the browser stopped restores what it verified from a cache before answering.
- **Push code.** The service worker needs Rust for push (§7: open the sealed wake-up, read the store,
  build the notification). A worker cannot verify scripts it loads with `importScripts`, so
  `scripts/finish-sw.sh` (a Trunk hook) inlines the JS glue into `sw.js`, and the worker reads the wasm
  only from the verified cache, where the manifest pins its hash. A build without a pinned key (development)
  loads it from the network and says it is unsigned.
- **Limits.** The browser fetches `sw.js` itself on update checks, outside the worker's control, so a
  server that replaces `sw.js` defeats the check, and so does the very first visit (trust on first
  use). The check protects returning users against a changed or partly replaced bundle, and gives
  auditors a signed list of what a release is. People who need more use the desktop and mobile apps,
  which do not take their code from the server.

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
- Prefetch: the mobile core has a setting (`teamcraft_core::prefetch::SegmentPrefetch`: off, unmetered
  networks only (default), any network) in the sealed store, and `Client::fetch_index_segments` does the
  download. The platform decides when it can run: iOS schedules a `BGProcessingTask` (external power and
  network), the app asks the core with the network's metered flag (`TeamCraft.prefetch_segments`). No size
  cap yet.

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

**Implemented:** the first http(s) link of a message is previewed. The message waits in the outbox
while the sender's client reads the page (5 s timeout, at most 512 KiB of it, 3 redirects; a redirect
to a disallowed domain gets no preview), takes OpenGraph title, description, site name and image
(falling back to `<title>` and `description`), fetches the image (at most 2 MiB) and turns it into a
small PNG. All of it goes into the encrypted message. The fetch comes from the sender's network,
so the linked site sees the sender's IP address, as it would on opening the link. Browsers can't
read cross-origin pages, so the web client sends links without previews.

## 13. Bots and integrations

E2EE means a bot can only read a channel if it is a member with its own device keys. Bots therefore
run as clients (TeamCraft bot SDK, Rust) on a machine we trust, not on the VPS. Incoming webhooks and
logs are encrypted by `teamcraft-bridge`, which runs next to the source (ARCHITECTURE.md §4.5).

## 14. Open questions

- Key transparency: when?
- Segment size and mobile prefetch defaults (per month? size cap on phones?)
