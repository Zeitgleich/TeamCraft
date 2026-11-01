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

Not part of end-to-end encryption, listed so every key is in one place: the server's **settings key**
(`[secrets]`, random 256 bit) encrypts the SMTP and CalDAV passwords admins enter in the console
before the server stores them (XChaCha20-Poly1305 under an HKDF-derived key, the setting's name as
associated data; `teamcraft-crypto::server_secret`). The server holds both key and plaintext; the
key only keeps the passwords out of database dumps and backups ([admin.md](admin.md) §6.2).

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
  `PUT /me/device-list`, compare-and-swap on the sequence). Clients read a user's identity key and
  device list through `GET /users/{id}/enrollment`, which answers with whichever of the two the user
  has published (neither before their first device is enrolled) instead of a 404; against a server
  without it they fall back to the single endpoints.
- **Removals need a signer.** A removal the server reports (an admin's "remove from all chats", a
  revoked device) is only a hint; clients don't sign a removal on the server's word. The device that revokes signs
  (above); an admin removes people with a signed change any member commits, also in conversations the
  admin isn't in (admin.md §2.2).
- **Other devices finish a revocation.** If the revoking device is lost after it published the list
  but before it removed the device from every group, the user's other devices finish it. A
  `device_remove` the server reports for a device of the same user only makes them look: after
  `revocation_check_delay` (default 5 minutes, so the revoking device normally finishes first) they
  fetch the user's device list, verify it with the pinned identity key (never older than the one
  they stored), and only if it names the device revoked do they sign its removal from the groups
  they share with it. Without such a list nothing is signed; the check is repeated with the
  removal backoff, `removal_attempts` times, then dropped. A full resync (gateway events missed)
  does the same check once against the groups it holds. The revoked device's leaves carry the
  user's ID, so the removal is the user's own change, as from the revoking device.
- **Verification state in the settings** (issue #409). Settings > Devices marks each of the user's
  devices from what this device verified, never from the server's list alone: *verified* when its
  certificate opens with the user's identity key and the newest signed device list names it with
  that key; *being set up* when the certificate is the user's but the list doesn't name it yet;
  *not verified* when nothing the identity key signed vouches for it (a device the server added on its
  own). Device names are the user's: the server shows them to the user and admins only, colleagues'
  clients get the keys with an empty name.
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

### 3.1.1 Sign-in through the server's provider (residual risk)

The apps learn the issuer and client ID from the server they were pointed at
(`GET /api/v1/server-info`). A lookalike server can announce another instance's real settings; an
existing SSO session then yields an access token for the real instance, which the app sends to the
lookalike as a bearer token, and the lookalike can use it at the real server until it expires. It gets
API access as that user (metadata, ciphertext, the ability to post as them), but no device or identity
keys: message content stays end-to-end encrypted, and new devices still need the existing device or
the recovery passphrase (§5, §6).

Mitigated by: showing the identity provider's host on the server confirmation, with a warning when it
is outside the server's domain; discovery documents must match the announced issuer (RFC 8414 §3.3)
and use https; ID tokens must carry the announced issuer and client (iOS, which reads them); refresh
tokens are kept per origin, issuer and client. Fixed by binding tokens to the device with DPoP
(RFC 9449), issue #120.

### 3.2 Enrollment order and interruptions

Enrollment is a series of requests, and the network can drop between any two of them. The order is
chosen so that every state the server can be left in is recoverable, and a retry (also after a
restart) finishes the job (`teamcraft-core::client::enroll`):

1. **Device key pair stored locally, then registered** (`POST /devices`). If the response is lost,
   the retry finds the registration by its public key (`GET /users/{id}/devices`) and keeps it. A
   registration certified by an identity that never reached the server (the attempt generated a new
   identity) is revoked, so interrupted attempts leave no orphan devices.
2. **A new identity key and its first key bag in one request** (`PUT /me/identity-key` with
   `key_bag`; the server stores both in one transaction and refuses an identity key without a bag).
   The bag is encrypted with a fresh backup key that is already wrapped with the recovery passphrase.
   So the server never holds an identity nobody can recover: a retry finds the identity and recovers
   it with the passphrase, like any further device. A server older than this protocol ignores the
   bag; the client then stores it with `PUT /me/key-bag` right away.
3. **An organization recovery** switches to a fresh backup key (§6.3.1) before anything is stored
   locally; interrupted, the device recovers again (with the new passphrase if the new bag reached
   the server).
4. **The account is stored locally** only now, marked as unfinished, with the wrapped backup keys
   it uploaded.
5. **Key packages and the signed device list** are published; an unfinished account does this
   first on its next sync, then clears the mark. The server picks device IDs, so a device refuses
   an ID that the signed list already gives another key or lists as revoked: signing the list
   would otherwise drop that device.

A device that finds no key bag on the server for an account it already uses refuses to upload one
with no wraps (`KeyBagMissing`): such a bag could never be recovered. The user sets a recovery
passphrase again instead.

**A setup nobody finishes (issue #415).** The key pair of step 1 waits for a retry. When the user
gives up instead, it is dropped (`teamcraft-core::client::abandoned`):

- **On sign-out** before the device is set up, at once, together with the keys of a link (§5).
- **After `unfinished_enrollment_expiry`** (a client setting, default one day from when the key pair
  was stored), checked when the app starts or shows the setup screen while the device is not set
  up. A record of an older version has no time, and a time in the future comes from a clock that
  was wrong: both count from the first check on. The key pair stays while the enrollment can still
  finish: an account is stored, the keys of a link wait to be enrolled (§5), or an enrollment or a
  link runs on this device right now.

In one group of writes (§9.5) the key pair is deleted, and its public key goes on a list of
registrations to revoke together with the identity key that certified it. The SQLite stores then
overwrite their freed pages and log (`purge_freed`), so the private key can't be read back from the
files. IndexedDB can't promise that: the deleted record's sealed blob may stay in the browser's
files until it compacts them, encrypted with the store's key. A crash leaves either the key pair or
the entry on the list, never neither.

The server then revokes the device the key pair registered, if any, so an abandoned setup doesn't
keep a slot of the user's device limit. A registration is revoked only when no enrolled device can
be using it: the identity that certified it isn't the account's (enrollment reuses only a
registration of its own identity, step 1), or the device list the account's identity signed doesn't
name it. The client checks that list against the identity key: its own once enrolled, before that
the server's only where it is the key the entry was certified with. Without a list that verifies,
nothing is revoked. So a copy of the store taken while the setup was unfinished (a restored backup)
never revokes the device the original finished enrolling with the same key pair; and the client
never revokes its own device. Offline, the entry stays and is revoked on the next start, the next
sign-out, or by the upkeep of the client the user enrolls later. A refusal for good gives the entry
up (the device stays in the user's device list, where the user can remove it); one that blocks the
whole list (reading the devices) waits an hour before the next try, doubling up to a day. Sign-out
waits at most a few seconds for the server. The key pair was never trusted anywhere (no signed
device list names it), so a late revocation is housekeeping, not a security boundary.

**The first owner.** The first roster entry makes its signer the workspace root of trust (admin.md
§2.3); an owner who can't recover their identity could never sign the roster or policy again. The
server therefore accepts the genesis roster only from a user who has both an identity key and a key
bag, in addition to the ordering above. The other owners and admins of the genesis come from the
identity provider's roles, which the server reports; the signer's client signs only the people the
signer confirmed one by one with user ID and pinned-key fingerprint (admin.md §2.3, "The first
roster"), so a hostile server can't slip a look-alike owner into the root roster.

**Never without an owner.** Every roster entry, the genesis included, must name at least one owner
with a valid 32-byte identity key. Clients check this before the chain and the signature and refuse
the entry otherwise (`NoOwner`), so a validly signed but ownerless roster can't lock the workspace;
owners' clients don't sign one (`LastOwner`), and the server refuses to store one (`last_owner`) as
a sanity check, not as a trust decision.

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
- Content key bundles (MLS application messages) to the whole group carry only a current key: for a
  new member or a further device of a member. History for a new member and the keys a device added
  back missed are sealed to the devices they are for (§4.3, §4.6). A member takes only the current
  key of a bundle sent to the whole group, and takes it as current only if it has none yet, so an
  old or delayed bundle can't switch it back; the one exception is a device that just joined and
  waits for its first one (§4.6).
- Each message header (plaintext) carries `content_key_id`, so clients know which key to use.
- `content_key_id` is a random 128-bit value, not a sequence number: two members may rotate at the same
  time (e.g. both reach the message limit), and random IDs keep both keys usable without coordination.
- Content keys use XChaCha20-Poly1305 with random 192-bit nonces, so devices sharing a key need no nonce
  state.

### 4.3 New channel members

- When a member is added, the adding client (or any online member, if the adder is offline) sends the
  channel's past content keys to the new member through MLS: the current key in a bundle to the
  group, every key of its ring sealed to each of the new member's devices (HPKE to the device's
  certified agreement key, as for a device added back, §4.6; at most `max_missed_keys` keys per
  message). The other members see that keys went to the new member, not which: a member who joined
  from the join on (a guest) never gets earlier history this way, and ignores history keys in a
  bundle sent to the whole group, which clients before this version send; only a device that just
  joined and waits for its first current key takes them, as the new member of such a client, and
  only from the member whose commit made its welcome, in the epoch it joined. A new member on a
  client before this version gets only the current key from an updated adder. A history share
  still waiting to be sent (the adder was offline) goes out only if the adder saw the member stay
  in the group since the addition: after a removal (and maybe an addition from the join on) it is
  dropped. With
  history from the join on, the adder sends only the new epoch key, which the other members derive
  themselves.
- The choice is part of the signed change and settled before signing (guests: always from the join
  on); a client refuses to commit an admin's signed full-history add of a guest. One exception: the
  import device, under the signed import policy, seals the keys of the import of a conversation to
  the guests the import's mapping named there, added from the join on like any guest (§4.10,
  "Guests in imported conversations"). No other device ever shares history with a guest.
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
- If the commit's answer is lost, the content keys the new member needs are sent once the commit is
  merged (§4.6, "Lost commit answers"); if sending fails, they are queued and sent later.
- **Nobody to add yet.** A person who hasn't set up the app has no device, so there is nothing to
  put in the group; a person whose devices ran out of key packages can't be added either. The
  conversation (a DM, or a channel the person is added to) is made anyway and the server lists
  them; the adding device keeps a pending addition with the history choice it settled. It costs no
  network while it waits: the person's first device (`device_add`) makes it due; a full sync (at
  start, and after the gateway's resume window expired, when that event may have gone by) asks for
  the device lists of up to 50 waiting people again, at most once per `rejoin_retry`. It is then
  committed like any add (the signed change is made when committed, so it is never stale). Without
  key packages it is retried after `device_add_stagger`, doubling up to `rejoin_retry_max`; when no
  device passes the checks (signed device list, credential) it isn't retried until the person's
  next device. It is dropped once the person is in the group or the server no longer lists them.
  A DM keeps its setup record (§4.6) so that a founding race with the person's first device is
  settled as there: the record counts as done while it only waits for a first device (no repair
  runs), the pending addition then resumes the DM's setup, and a missing key package is the
  setup's own retry. Only the adding device holds the pending addition (robustness audit, F2).
  The conversation shows "X hasn't set up the app yet" (or "… can't be added yet") meanwhile.
  The wait for a first device ends after `pending_addition_expiry` (default 90 days, noted from
  when the adding device began to wait): the pending addition turns into a small marker and the
  conversation says "X never set up the app" instead; a DM's setup record that only waited for the
  device is dropped. The marker costs nothing but a device-list check at a full sync (like any
  waiting addition), and X's first device, should it ever appear, still makes the addition due.

#### 4.3.1 Key package lifecycle

Others add a device with one of its MLS key packages. Packages expire, so a device that stays enrolled
for months must keep fresh ones on the server:

- **Lifetime.** A package is valid for 83 days from its creation, starting one day before it (a
  creator whose clock runs fast still makes packages others accept). That is the longest range
  openmls accepts. openmls checks lifetimes against the system clock, so they are made from it.
- **Server.** The server checks every uploaded package (encoding, signature, ciphersuite, lifetime)
  and records its expiry. It hands out only packages that stay valid for at least
  `key_package_min_validity_secs` (42 days), so members who process the add weeks later (a device
  that was offline) still accept it, and it deletes the others. Packages that expire within
  `key_package_renewal_window_secs` (56 days) count as "expiring soon", not as available, and an
  expiring last-resort package counts as missing.
- **Device.** A running client checks every 6 hours (and after every full sync) and uploads new
  packages when fewer than its target are available, and a new last-resort package (replacing the
  old one on the server) when it has none that stays valid. So regular packages are replaced after
  about 27 days and the last-resort package rotates on the same schedule.
- **Private parts.** openmls deletes a regular package's private part when a welcome uses it. At
  every check the device also deletes those of packages that expired, and of last-resort packages
  a newer one replaced on the server, each `key_package_retention` (client, 30 days) later, for
  welcomes still on the way (#148). A last-resort package counts as replaced only once the server
  confirmed its successor's upload: after a lost answer the server may still hand out either, and
  both stay. Nothing is deleted before the device read all its welcomes since its gateway last
  connected (also a resumed session: a desktop that slept for weeks may have welcomes waiting) or
  a full sync started, nor while it reads them (asking to be added back tops up the packages
  mid-sync); the 6-hour check reads the welcomes first (#395). A welcome for a package it no
  longer holds makes it ask to be added back (§4.6) instead of losing the conversation, unless the
  server no longer lists the user in the conversation (or refuses its member list): such a welcome
  (replayed, misrouted, or for a conversation left meanwhile) is dropped. A group keeps its own
  leaf key, so deleting the package it was joined with changes nothing there. Lifetimes and these
  times use the system clock openmls checks against.
- **Wiping.** A deleted package's stored bundle (private init and leaf keys) is overwritten in
  memory before it goes, and so is every old copy of the MLS storage in the store's cache. The
  local database zeroes freed pages (SQLite `secure_delete`), and the native stores (SQLCipher on
  the desktop, sealed SQLite on mobile, §9.4) truncate their write-ahead log after a deletion, and after saves of the MLS storage at most every 30 seconds
  (a checkpoint costs two or three syncs and the MLS storage is saved for every processed
  message; measured when the storage was one record: about 20 ms per save at 64 KiB, 320 ms at
  1 MiB; since #424 a save writes only the entries that changed, §9.5). What the
  interval keeps back is cleared when the app stops (goes to the background) or closes the store;
  a checkpoint that cannot finish (another reader holds the log) stays due and is retried, and never
  fails the write. Inside a group of writes (§9.5) the purge waits for the commit. So old storage
  records can stay in the log for up to 30 seconds while the app runs, and are not left in the files
  for somebody who later gets the database key (#391, #417). The first start after #391 rebuilt each
  database once (`VACUUM`), wiping free pages left from before; a rebuild that fails (full disk) is
  tried again on the next start. Copies the file system or the flash controller keep are
  out of reach; the browser's IndexedDB gives no such control. The listing and the wiping rely on
  how `openmls_memory_storage` 0.6 keys bundles; a test fails when a new version changes it.
- **History of a first add-back.** A user whose first device can't open the welcome that added
  them (its package deleted, or opened but not joined) is added back (§4.6). The content keys the
  adder shared for a full-history add went into the group the device never joined, so the member
  that adds it back seals that history to it again, checked against the signed addition it merged
  (§4.6, "Keys a device added back missed", #400). An addition from the join on brings nothing
  from before it, also after an add-back. Losing a welcome needs one that waited past the
  package's lifetime plus `key_package_retention` (about 72 days after the server handed it out),
  or a network error at the exact moment of joining.
- **One bad device.** The adder checks every claimed package (signature, lifetime, credential, the
  user's signed device list, not revoked from this group, the device and user the server claims)
  and leaves out bad ones with a warning, so a revoked or broken device doesn't keep its user's other
  devices out. Adding a user fails only if none of their devices can be added.
- **Joining.** A welcome is accepted without checking the lifetimes of the leaves already in the
  group: a member whose leaf still comes from its key package would otherwise make the group
  unjoinable once that package expired. A lifetime bounds when a package may be *added* (RFC 9420
  §7.3); the committer and every member check that then. Credentials are verified as always.

### 4.4 What the server sees

Plaintext metadata: conversation ID, sender user and device, timestamp, message ID (server Snowflake)
and client message ID, thread ID, `content_key_id`, size, **list of mentioned user IDs** (needed for notifications, §7), the "urgent"
flag. Reactions are encrypted messages, so not even reaction counts are visible.

Encrypted: text, attachments, attachment names, reactions, edits, polls, link previews, drafts.

Typing indicators are plaintext metadata too: a `typing` gateway frame tells the server that a user
types in a conversation, and for a reply in which thread (thread IDs are visible metadata anyway), never
what, and it forwards that to the other members (gateway.md). Clients
send at most one every 3 s while there is text in the composer, and none when the user turned "Show
when I'm typing" off (a preference of each device, on by default). Nothing about typing is stored, on
the server or the clients.

Directory data, not encrypted: the server has to show and route by it, so it holds it in plain text
like the identity provider does. Space names, descriptions, colors and **pictures**; channel names,
topics and **pictures**; people's profiles and avatars. A space or channel picture is uploaded to the
server itself, which refuses SVG and re-encodes PNG, JPEG and WebP uploads as PNG before storing
them, and serves it only to people who may see the space or channel (changing a channel's picture
needs `manage_channel_properties`). Anything put into these fields is visible to the server operator;
content that must stay confidential belongs in messages.

MLS: the commits, welcomes and application messages of each group (opaque), and the newest MLS
`GroupInfo` per group (§4.6, "A further device joins on its own"): the group's public ratchet tree,
i.e. its member devices' certificates, which the server knows anyway, and the epoch's external public
key. It holds no secrets; a joiner still needs a credential every member accepts.

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
a message whose device key isn't known yet waits until it is. A device that isn't in the group yet
(a new device of a member) reads the history its key bag opens right away: for each message it can't
verify yet it records that one sending device's key, if the sender's signed device list (identity
signature, never older than one seen before) names the device, and only if the sender is a member
per the server or a device whose leaving time it knows. Every device runs this for messages it can't
verify yet, not only new ones. Every device keeps, per conversation and device, when the device last
left and last came back (the server times of the removal commit and of the commit that added it
back) and refuses what it signed while out: after it left (with the slack) and before it came back.
Only the last absence is kept, so an earlier one is forgotten once the device leaves again. The
message time compared is the server's message timestamp, which the signature doesn't cover (review
A10): a server could re-date a message, but the check stops a removed device on its own. Push
previews apply the same check. These records travel in the account data (§6.4):
every device that processes a removal or a return records it there, shares what it has locally
(also from before this version) at startup, at each full sync and before handing its key bag to a
linked device, and takes the records of the others into its check for all conversations whenever it
loads or merges the account data, before it decrypts anything. A device can't know removals that
none of the user's devices processed (or that only a device not updated yet knows). Edits, deletes and reactions are
messages too, so nobody can change or delete someone else's message. A device that left a conversation
(removed, or its user removed) can't add messages dated after it left and before it was added
back, if it was (five minutes of slack for clocks). Messages from before signatures existed are refused unless the client setting
`accept_unsigned_messages` allows them.

### 4.5 Attachments

Client encrypts each file with a fresh random key (XChaCha20-Poly1305, chunked for streaming), uploads
the blob to S3 via a presigned URL, puts key + hash + file name + thumbnail into the encrypted message.

Files larger than one part (8 MiB) go up as an S3 multipart upload (issue #153): the blob is the same
STREAM of 64 KiB chunks, cut into parts of 128 whole chunks (the header goes into the first), and each
part is encrypted and uploaded on its own through a presigned part URL. Chunk `n` is encrypted with
nonce counter `n` whichever part it is in (`AttachmentSegmentEncryptor`), so the parts join into
exactly the blob the one-shot encryptor would have made, and receivers see no difference. To continue
after a restart, the sender's encrypted local store keeps the file's key, the header (its nonce
prefix), the parts' `ETag`s, the offset and the SHA-256 state over the uploaded ciphertext; the hash
in the message is that state finished after the last part. The server sees the same as for a single
upload: sizes, part numbers and `ETag`s of ciphertext. In the browser, a picked file waiting for its
upload is kept as a `File` in IndexedDB (plaintext, like the mobile staging copy) and deleted once the
upload is done or cancelled; the desktop app stages such files in its data directory the same way.

The message list shows the encrypted thumbnail at once. On web and desktop it then loads the full
picture inline (up to 16 MiB, a few at a time), and the lightbox does for any image or video: the
client core downloads the blob, checks its hash and decrypts it in memory (desktop: `core_attachment`,
raw bytes to the webview; web: the core in the page), up to `MAX_VIEWER_BYTES` (128 MiB) on every
platform; bigger files are only saved. The page holds the plaintext only as `blob:` URLs in a session
cache bounded by bytes (`teamcraft-ui` `media_cache`), revoked once evicted and no longer shown.
The full file is never written to disk unless the user saves it; only the inline rendition of a still
picture (at most 960 pixels a side) is also kept in the persistent media cache (§9.6), sealed like the
local store. "Copy image" draws that image into a canvas
in the page; "open in a new window" opens a blank window of the page itself (`about:blank`, the only
window the desktop app allows) that shows the same `blob:` URL. Neither creates a URL the server or
anyone else could load.

**Videos.** The sender's browser (the core has no video decoder) takes the poster frame, about a second in,
and the length from the file itself through a `blob:` URL it streams from, so even a large video is not read
into memory; the JPEG goes in as the thumbnail and the length as `duration_ms` of the attachment's details in
the encrypted message (optional; older clients ignore it). The message shows the poster with the length as a
badge. A click plays the video in the row (one at a time, paused when scrolled out of view) when it is at most
64 MiB, as the player gets the whole decrypted file as a `blob:` (`MAX_INLINE_VIDEO_BYTES`); bigger videos
open in the lightbox. The desktop app's CSP allows `media-src 'self' blob:` for this, as the web client's does.

**Audio** (#802). An audio file (`audio/*`, or by extension when the type is generic) is a card with the app's
own player. Nothing is downloaded until play is pressed; then the core decrypts the file in memory like a
video, and the app's one `<audio>` element (not part of the page's layout) plays it from a `blob:` URL. One
attachment plays at a time; the URL is revoked when another one plays, the conversation is left or no card
shows the file any more. Files above the user's limit (`AudioLimit`, 16 to 128 MiB, default 64 MiB) are only
saved. The length the sender's browser read from the file's metadata (`duration_ms`) shows before playing.

An attachment's file is untrusted content from another member. Only images, videos and known audio types get
a `blob:` of their own media type; everything else is opaque bytes. SVG is a document that can carry scripts,
and a `blob:` URL has the page's origin, so an SVG is never shown as itself: the page decodes it in an
`<img>` that is never attached to the document (no scripts, no network), draws it into a canvas and
shows only the resulting PNG. Thumbnails of SVG are drawn by the sender's browser the same way, or by
`resvg` on native platforms, with no DTD, no resources loaded and limits on size, element count and
`<use>` expansion (`teamcraft-core` `svg_thumbnail`).

#### 4.5.0 Previews of files

A received `.md`, `.html` or `.pdf` file, or a text file (`.txt`, `.log`, `.json`, `.csv`, `.yaml`, `.toml`,
`.xml`, `.ini`, `.conf`, any `text/*` type; by media type, or by extension when the type is generic), can be
opened as a preview in the app, up to a size the user chooses (`PreviewLimit`, 2 to 50 MiB, default 10 MiB;
a larger file shows its size and the download, and is not loaded). The preview works from the file the client
core already downloaded and decrypted, in memory; nothing leaves the client. The file is untrusted, so each
kind is shown in a way that can't run its code or reach the app:

| Kind     | Shown as                                                                                       |
|----------|------------------------------------------------------------------------------------------------|
| Markdown | the app's own renderer (the message subset: a tree of elements, no HTML string); "show source" |
| HTML     | a sandboxed frame, see below; "show source"                                                    |
| PDF      | the vendored PDF.js, see below                                                                 |
| Text     | plain text in the text viewer, see below                                                       |

**Text.** Text nodes only (`crates/teamcraft-ui/src/text_preview`): nothing in the file is read as HTML or
Markdown, and addresses in it stay text (no link is made, nothing is fetched). The bytes are read as UTF-8,
UTF-16 or, when clearly text in no Unicode encoding, Windows-1252; a file with NUL bytes or many control
characters is binary and not shown. Terminal escape sequences in logs are never acted on: SGR colours become
one of sixteen colours of the stylesheet, every other sequence (cursor moves, titles, OSC 8 hyperlinks,
device controls) is removed, and an unterminated one hides nothing beyond its line. JSON is formatted only on
request, after `serde_json` accepted it, by re-indenting its tokens, with nesting (128 levels) and the
formatted size (64 MiB) bounded so a small hostile file can't blow up. Only the rows in view
are in the page, at most a million lines are indexed, and finding is capped (10,000 matches), so a large
file can't exhaust the page.

**HTML.** An `<iframe sandbox="" srcdoc="…">` with the strictest sandbox (no `allow-scripts`, no
`allow-same-origin`: an opaque origin that reaches nothing of the app, its storage or its tokens; no forms,
popups, top navigation or downloads). The document starts with its own policy,
`default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'`
(also set as the frame's `csp` attribute): no script, no connection, no remote image or font, so opening a
file tells no one that it was opened. The page's own policy applies to the frame too (`srcdoc` inherits it),
which is stricter still: inline `<style>` blocks are blocked by `style-src 'self'`, so a previewed file shows
with the browser's default styles. Links in the frame do nothing; the file's `http(s)` links are listed under
the preview and open in the system browser (desktop) or a new tab (web), with the address as the label.
The list is read from an inert parsed copy of the file (`DOMParser`).

**PDF.** PDF.js, vendored and pinned (`crates/teamcraft-ui/vendor/pdfjs-<version>`, the npm package's
`pdf.min.mjs`, worker and the decoders' WebAssembly, unmodified, no CDN), loaded as a separate module when the first PDF opens, so the
first-visit bundle is untouched. It runs with `isEvalSupported: false`, no
scripting (the scripting sandbox is not shipped) and no annotation, form or link layer: pages are canvases
plus a text layer for selecting text and searching. Memory stays bounded: only pages near the viewport are
drawn. A document that can't be opened or drawn, or isn't open after 45 s, gives way to a message with the
download, and the preview closes in every state (#835).

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
file key. The server serves the links (`/api/v1/shares`); the download page is the web app's
`/f/<share-id>#<fragment>` route (`share_download.rs`), which needs no session: it reads the
fragment in the browser, asks `GET /api/v1/shares/{id}` for size and password need, asks for the
password if the link has one (a wrong one is told apart from a damaged link and doesn't count as a
download), counts the download with `POST /api/v1/shares/{id}/download`, and streams the ciphertext
through `AttachmentStreamDecryptor` into a `Blob` it offers to save. Neither the whole ciphertext
nor the whole plaintext is one buffer, the file is only offered once the end of the stream
authenticated, and files above 1 GiB are refused with a hint. The blob is always
`application/octet-stream` (a shared `.html` never renders in the page's origin), and the sender's
file name is shown and saved without bidirectional controls, control characters and path
separators. The signed-in app's chunks are not loaded for the page.

*A failure on the way doesn't burn a limited link.* The server counts when it hands out the URL and
returns a random **grant**; its hash is stored with a deadline (`shares.download_retry_minutes`,
default 10). A retry that presents the grant gets a fresh URL without counting; a second
downloader replaces the stored grant, whose retry then counts again (fail safe). Revoking or
expiry end a grant too. The page keeps the grant in memory only.

*The fragment stays in the address bar* so a reload still works; the trade-off is that it is in
the browser history and in what a person copies from the bar. The page sends no referrer
(`Referrer-Policy: no-referrer`, never cached) and the fragment never reaches the server or the
logs. Revoking links and the options dialog are separate (#501).

**Thumbnails:** the sender's client makes a preview of image attachments (decoding capped at
40 megapixels) and puts it next to the file key inside the encrypted message. It is turned upright
by the photo's EXIF orientation and is at most 640 pixels on its longer side: a JPEG (quality 80),
or a PNG when the picture has transparency. A preview larger than 96 KiB is made smaller or lower
in quality, since it counts against the message's size limit. Receivers read the format from the
bytes; clients before 1.0.2 sent 320-pixel PNGs, which still show.

**Decrypted files on the device:** downloads are decrypted into the app's private storage, one
folder per account, which doubles as the cache that reopens pictures without downloading them
again. The least recently used files go when the folder grows beyond 1 GiB (checked at start); all
of them are deleted at sign-out.

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
- **An admin outside the group lets no admin in against the policy** (admin.md §2.4, issue #816).
  The actor whose authority counts is the signer, not the committer. A signer who is in the group
  (and no guest) may add anybody: members' clients see who is in the group, but not who holds
  `manage_members` or owns the channel (channel roles aren't signed yet, issue #380), so that part
  is the server's word. A roster owner or admin who isn't in the group may sign any change for the
  members to commit, except bringing an owner or admin of the roster into a conversation that
  isn't an open public channel (as the conversation's visibility shows): themselves or another one
  alike, only under `admin_self_join = visible`. Clients before 1.0.9 checked only the admin's own
  self-join and still commit another admin's addition of an admin; a member on 1.0.9 or later
  refuses that commit, falls out and is added back into the group with the admin ("Falling out
  and coming back", below). This version never commits what an older one refuses, so the group
  doesn't split either way.
- **Deactivation suspends, removal rotates keys** (admin.md §3.2.1). Deactivating an account changes
  no group: the user's devices stay members and new messages are still encrypted to them, so a
  reactivation needs no recovery and no re-adding. The server refuses the account every request
  and sends it no pushes, but it is untrusted: a compromised server could keep delivering that
  ciphertext to a suspended user's devices, which can still decrypt it. That is the residual risk
  of a suspension. "Remove from all chats" closes it: the admin's client signs the user's removal
  from every channel, members commit it and the epoch keys rotate; then the server revokes the
  devices. It is not undone by a reactivation. The automatic removal after
  `remove_deactivated_after_days` is the server's alone and changes no group (A8); those users are
  listed for admins until one removes them. Admins should remove people who left the organization
  rather than only suspend them.
- **Exception: further devices of existing members.** A commit may add devices of users who already
  have a device in the group without a signed change, because each device is cross-signed by its
  user's identity key (§3). Removing anything always needs a signed change.
- **Exception: a further device joins on its own.** A new device of a user who already has a device
  in the group joins without waiting for anybody, with an MLS **external commit** from the group's
  `GroupInfo`. Every committer sends the `GroupInfo` of the epoch its commit starts (signed by the
  committer, with ratchet tree and external public key) along with the commit; the server keeps the
  newest one and serves it to members for the current epoch only (or a member publishes it for the
  current epoch when a device asks and none is there). The joiner verifies every leaf of that tree
  against the pinned identities (as for a welcome) and joins only if its user is in the tree and
  the device isn't. That the `GroupInfo` is current is the server's word, so it could hand out an
  old one whose tree still holds a member removed since, and fork the new device into a group with
  that member. The user's devices therefore record in their account data (§6.4) the epoch of every
  content key switch in each conversation (removals, "from join" additions, rejoins); the joiner
  compares the epoch inside the signed `GroupInfo` (not the server's envelope) with that floor and
  refuses an older one. The floor is only as fresh as what the server let the user's devices see:
  a removal commit withheld from all of them isn't in it, and a key bag the server serves (on
  recovery with the passphrase, a passkey or the organization) may be an old one. A device enrolled
  that way therefore never joins by external commit; only a device that got the key bag from
  another device of the user over the link (§5) does. The others wait for a member. Members merge an external commit only if it carries nothing but the
  external init (no removals, no authenticated data), its new leaf's certificate verifies against the
  pinned identity, the user's signed device list names the device (failing closed: a user without a
  verified list can't add devices this way), the user already has a device in the group and the
  device wasn't revoked from it: the rule of an unsigned device addition, stricter on the device
  list, so who may add whom doesn't change. The device's own credential is what authorizes it; nobody else
  can join this way. openmls merges an external commit at once, so the joiner forgets the group if
  the server refuses it; if the answer was lost and the server took it, the next try finds its
  device in the tree and asks to be added back (below). Without a `GroupInfo` (the last commit came
  from an older client, or the device's floor may be stale) the device posts a device-add request
  into the group's queue (`TCDA`, the user ID): members who see it, also later when they come
  online, check that the user's signed device list names the sending device and add it to that
  conversation's group like a colleague's new device. A member handles one device's request once
  per `rejoin_retry` in each conversation; repeats (a device asking too often, a server replaying
  entries) cost it no requests (#187). **The current key of a further device.**
  A further device reads history with what the user's key bag holds, but the bag's current key may
  be one from before a key switch the user's other devices hadn't uploaded yet, which a removed
  member knows. So the device doesn't write with it: when it joins (by external commit or from a
  welcome, unless it was added back) it stops using it (keeping it for reading) and shows that it
  waits, and the key bag can't make it current again. Every member that merges an unsigned addition
  of a further device (an external join, or a member's commit, its own included) queues sending the
  group its current key as a bundle, ranked like an addition (`device_add_stagger` per rank, online
  users first), so normally one member sends; the others drop theirs once a current key arrives, or
  when the group switches keys anyway. A member hands over only a current key it had in the group
  since its own join (derived or received there, see "Keys a device added back missed"). If its
  current key is one only its key bag knows, which may be stale in the same way (and so is every
  current key from before devices recorded this), it makes a fresh random key and hands over that
  one: it is new and reaches only the epoch's members over MLS. It switches to the fresh key only
  once the bundle went out, so a failed send never leaves it writing with a key nobody has. The
  same rule holds for the current key in the history it shares with a new member; a device that
  waits for a key itself makes a fresh one when it shares history (that is safe), never for a
  hand-over. The waiting device takes the first current key a member sends after its join (the
  adder's history share for a new user, the hand-over for a further device; only members of the
  epoch can send it, the server can delay it, not forge it), or the next epoch key it derives
  itself. So any one member decides which key a waiting device writes with; members that handed
  over at the same time each keep their own key, and every member has all of them. Members on clients before this version
  don't send the hand-over: until a member with this version comes online, the device reads but
  doesn't write.
  `GroupInfo` larger than
  `max_group_info_bytes` (client, 256 KiB) is not sent, and the server stores a commit without one
  that exceeds its payload limit; such groups use the request. A further device joins DMs and group
  DMs the same way, but only into a group that already holds every participant the server lists: a
  DM group of one may still lose its founding race (below), and a device that joined it would be
  left in a group nobody uses. Otherwise it asks with the request and waits for a member. A DM this
  device opened itself is finished by its setup. The server can still put a user who isn't in a group into a
  conversation so that the device asks with a signed self-add (as before this change). The app shows "waiting for another device" until
  then. Neither way announces a join: the user was a member all along.
- **Exception: a device added back.** A device that fell out of the group (see below) can ask to be
  added back with a request it signs with its device key: conversation, the group's epoch on the
  server, a random 128-bit request ID and the device. A member commits "remove the device's old leaf,
  add the same device from a fresh key package" with the signed request as the commit's
  authenticated data. Every member merges it only if the request is signed by that very device
  (certificate against the pinned identity, the same signing key as its leaf), is for this group,
  is at most 32 epochs old, wasn't answered before, and the commit removes exactly that device's leaf
  and adds exactly that device. Membership doesn't change, so no admin signature is needed; nobody
  but the device can make it give up its leaf. Every member then switches to the new epoch's content
  key, which the device derives from its welcome. Clients before this version refuse such commits;
  all clients have to be updated.
- **Keys a device added back missed.** Content keys introduced while the device couldn't follow the
  group (a removal, a "from join" addition in between) would otherwise stay unknown to it. The
  member that added it back sends it the keys of the epochs it was a member in, as an MLS
  application message whose keys are sealed to the device alone: HPKE (RFC 9180, base mode,
  X25519, HKDF-SHA256, ChaCha20-Poly1305) to the agreement key in the device's verified certificate,
  with the conversation, the epoch, the sending member and the device in the HPKE info
  (`TeamCraft-v1 sealed content keys`). Other members see that keys went to the device, not which:
  they may not have been members in those epochs (a guest added from the join on). A sealed bundle
  never names a current key; the device opens it only if it is for this device, from that member, in
  that epoch, and only adds its keys to the ring. Which keys: each device notes, per content key,
  the first epoch it had it in the group (derived as the epoch's key, or received from a member in
  an application message of that epoch, which every member of the epoch got too), and, per member
  device, the epoch its leaf was added in, when it saw that. It sends the keys noted for epochs since
  both its own join and the device's addition, newest first, at most `max_missed_keys` (client, 512).
  Keys from the user's key bag or sealed to it have no epoch and are never sent on. **A device a
  full-history addition put in** gets that history instead (#400): every key of the ring, as the
  addition shared it, in bundles of at most `max_missed_keys`, since it may never have opened that
  addition's welcome (§4.3.1). A member sends it only if it merged that addition itself (a signed
  change that shared the full history with the device's user and added the device's current leaf;
  each device notes this per member device and drops it with the leaf), the change wasn't signed
  by the user it added (only a member who chooses to share history adds somebody with it, §4.7),
  the user stayed a member since (as for a history share still pending), and the server doesn't
  show the user as a guest by now (its word can only lower what is shared); when the server
  refuses to say who the user is, the device gets the missed keys only. Nothing else unlocks
  it: the add-back only names the device, so neither the device nor the server can ask for more
  than the merged addition shared, and a replayed or old request (above) at most adds the device
  back again. Whether the device ever opened its welcome can't be told (a device that only reads
  leaves no trace in the group, and the signed request can't say so without older members
  refusing the add-back commit), so a device that did join and fell out later gets that history
  again, which it had. A device the addition didn't put in (a further device of the user, which
  gets the history through the user's key bag), a device added from the join on and a user
  removed since get nothing of it. A member that joined after the addition can't vouch for it and
  sends only the missed keys; the members that saw the addition follow up (below). Members that
  merged the addition on a client before this version noted nothing and send only the missed
  keys. A member that joined after the device fell out lacks the keys before its join; so when a
  member added back a device and the committer joined after this member (it saw the committer's
  leaf added, or the committer added back, which starts its own record afresh), this member
  sends its own keys too, ranked among the members whose record starts before the committer's
  (online users first, then the oldest record, `device_add_stagger` per rank), unless a member
  that joined no later than itself sent them first. Members can't see what a sealed bundle
  holds, so in a group with members on clients before #400 a follow-up with the history of an
  addition is also dropped when such a member sent only the missed keys first; the history then
  waits until somebody adds the user again with it. Every bundle of a sealed share (keys a
  device added back missed, the history of an addition or of a new member, §4.3) goes out only
  while it is still due: a member that has to catch up with other commits while sending (an
  epoch conflict) checks again that the device still has the leaf it had when the share was
  decided and is still owed what is sent, so a removal, maybe followed by a new addition from
  the join on, stops the rest. Each device
  keeps the epochs of at most `max_missed_keys` keys (the newest, and its current key), since no
  more are ever sent. Clients before this version ignore the sealed bundles.
- Commits with other proposal types (PSK, re-init, external init, group context changes), from
  external senders, or whose update changes a member's user or device are rejected.

**Lost commit answers.** A device keeps its commit pending until it knows the outcome. Only an epoch
conflict (another commit won) or a definite refusal discards it. If the answer is lost (timeout,
killed app), the commit and the request that posted it are kept with the MLS state; the next look at
the group's queue merges it when the queue shows the device's own commit with the same bytes,
discards it when another commit took the epoch, and sends the same request again when the server
never got it. No new commit is made while one is pending. So a lost answer no longer leaves the
device an epoch behind its own commit.

**Removal order.** Removing a member, leaving and revoking a device are one recoverable operation,
so a crash or a lost answer can't leave a removed person in the group while the server no longer
lists them:

1. The client asks the server whether the removal is allowed
   (`GET /conversations/{id}/members/{user}/removal`, the same checks as the removal, nothing
   changes), so the group never drops somebody the server then keeps (a moderator removing an admin,
   the last channel admin leaving).
2. The MLS side goes first. A member commits the signed removal; an admin outside the group posts it
   for the members to commit (admin.md §2.2); somebody leaving posts a signed removal of themselves,
   also from a device that isn't in the group (for the user's other devices), which the remaining
   members commit (MLS doesn't let a member commit its own removal; the leaver's own devices skip
   it). A leaving device that is in the group waits until a member's commit removed it (it watches
   its own group, no polling) and posts the request again when it got too old to be committed.
3. Only then is the server's membership dropped (`DELETE …/members/{user}`, idempotent: removing
   somebody who is gone succeeds, so a retry or a second admin finishes instead of failing).

The client records the removal and the step it reached in its pending queue before step 2 and
finishes it on the next round (after a restart, on the next sync), so a lost answer or a killed app
is repaired by the device that started it. If the server refuses step 3 after step 2 (the remover
lost the right meanwhile), the entry stays and is tried again after `removal_retry` (1 hour), twice
as long after each further refusal up to `removal_retry_max` (24 hours). After `removal_attempts`
(6) refusals it shows as failed in the conversation ("removal failed – retry") and waits for the
user; until then a removal or leave on its way shows as pending, also a leave that waits for the
members' commit.

The goodbye of somebody leaving and the announcement of a removal belong to the pending entry, with
the message ID chosen when the entry is recorded: the goodbye goes out before step 2 (the leaver
can't write afterwards, so step 2 waits until it reached the server), the announcement after the
group's key rotation. A second round, a restart or the user leaving again finds the message in the
store and doesn't write it again, and the server deduplicates on the ID: neither is sent twice, and
a removal finished on a later round still announces it.

When the remover commits (a member removing somebody, or the leaver's device that waits for the
commit), every state in between is safe: before step 2 nothing changed; after it the removed devices
have no secret of the new epoch, whose key every member switches to (§4.2), even while the server
still lists the user. Two cases rest on a signed change that a member still has to commit after the
server's membership is gone: an admin outside the group, who can't see the group's queue, and a
leaving device that isn't in the group. An admin outside the group keeps watching its change for
`removal_watch` (30 days from the first signing; signing again doesn't extend it): at growing intervals it learns the group's current epoch from the
epoch-conflict answer to a post for an epoch the group has left (nothing is stored), and signs the
removal again once the group is half as many epochs past it as members accept
(`MAX_CHANGE_AGE_EPOCHS`, see Freshness), at most `removal_attempts` times. Signing again is
harmless when the first change was committed: members find nobody to remove. Once the server lists
the user again (an instance admin may see the member list) it neither signs again nor forgets the
removal: a hostile server could list the user while holding the change back. The removal shows as
unconfirmed in the admin console and the conversation until the admin chooses "remove again" (signed
and sent afresh) or "keep". The same holds for its server step resumed later (after a refusal or a
restart): the admin can't see whether somebody added the user back since, so the step isn't sent on
its own. A leaver's other devices drop the group when the server's removal
reaches them.

A later addition wins over a pending removal: the remover's own addition of the user, or a signed
addition its device sees committed (which Freshness makes newer than the removal), drops the
pending entry, and a server step tried again finds the user back in the remover's group and is
dropped too, so a retry days later never undoes the new membership.

Removals backed by a signed change finish without anybody being asked: the remover's device, an
admin's signed change in the group's queue, or a leaver's signed request is committed in the
background by an authorised member's client, which rotates the keys (steps above). Nothing about
somebody left in the group is ever shown to members (issue #661). A client notes users who are in
the group but missing from its cached member list and whose removal isn't on its way, but the cache
may be stale (a member who joined minutes ago, a missed event), so that is only a suspicion: the
client fetches the member list afresh, drops everybody it lists and keeps the fresh list as its
cache. Only a user missing from two fresh lists in a row counts as left in the group. Such a removal
rests on the server's word alone (its signed change got lost, or the server removed the user
without one, like a deactivated user removed from all chats by the server alone), so no client signs it (security review A8): meeting guests among them leave with their own
signed leaves ("Meeting guests" below); everybody else is logged and listed for admins in the
admin console. There an admin decides (issue #734): "Remove from the group" makes the admin's own
device, a member of that group, sign and commit the removal, which rotates the keys like any member's
removal; the server's word only put the user on the list. Members' clients don't claim key packages
for a user the server lists but who has no device in the group.

Deleting an account (admin.md §3.2.1) follows the same rule: the admin's client signs the removal
from every channel first, then the server deletes. Direct messages with nobody left to talk to (1:1
DMs, group DMs that would keep one other person) are deleted on the server with their ciphertext,
attachments and MLS state; the remaining participants' devices drop them with their local history
on the gateway's `conversation_delete` or, offline, on the next full sync
(`deleted_conversation_ids`). A compromised server could claim such a deletion to make a device drop
a direct message's local history; it can already withhold or delete the ciphertext it stores, so
this adds no access to content.

A device that reads a commit late (it slept, or caught up after hours) verifies it against identities
pinned before processing: of whom the server lists in the conversation now, and of the user the
commit's signed change adds or removes and its signer, read from the commit's authenticated data
(which MLS doesn't encrypt). Somebody who was added and has left again since is no longer on the
server's list; without their pinned key the add commit was refused and the device fell out of the
group, with every later message unreadable until it was added back (issue #972). Reading the names
first trusts nothing: the change and every added leaf are verified as before. openmls spends the
commit's handshake key while it decrypts, so a refused commit can't be processed again; the
identities have to be pinned before.

A device that joins from a welcome made before somebody's removal pins the identities of everybody
the welcome's tree holds, not only of whom the server lists, and verifies every leaf as before; the
removal commit follows in the queue. openmls spends the device's key package while it decrypts the
welcome, before the tree can be read, so these identities can't be pinned first. If pinning them
fails for a network reason after that, the welcome can't be opened again; the device's leaf is in
the members' tree, so it asks to be added back like a device that fell out (below; signed for the
current epoch, which it learns from the conflict answer without a group), and asks again on a later
sync, after `rejoin_retry` doubled with every request up to `rejoin_retry_max` (24 hours), also
when posting the request failed.

The server refusing a member the group's queue (403/404) is quiet only when that is expected: the
device's own leave waits for the members' commit, or the server says the user is no longer in the
conversation at all (the conversation goes, as on the server's removal event, which the user sees).
Otherwise the group isn't marked caught up, the refusal is logged, and the conversation shows that
the server refuses this device the encrypted group and new messages may be missing: a server can't
make a member stop following a group unnoticed.

A revocation runs in the other order on purpose: the server first, so a lost device loses its
session at once, then the signed device list and the groups, again from the pending queue; a group
whose commit fails doesn't stop the others, and it is tried again.

**Falling out and coming back.** A device can still miss or refuse a commit the others merged (a bug,
a restored backup of the app's store, an old client). It notices when, with every earlier entry of the
queue handled, an entry from a later epoch comes. Then it asks to be added back (above) and keeps its
old group until the welcome arrives; it asks again after `rejoin_retry` (10 minutes) without one,
also when no later entry comes (its group's epoch is still below the server's). The
members queue the request like a new device of a colleague: ranked, so normally one commits. Each
answer costs a commit and a key package of the device, so a member ignores, with a warning, a
request for a device it saw added back less than `rejoin_cooldown` (client, 30 minutes) before
the request, by the server times of the commit and the request (#149); a member that joined after
that commit hasn't seen it and may answer once. A malicious member can thus delay a device's return
by up to one cooldown (30 minutes): an add-back that doesn't work for the device still starts it.
The
cursor stays before the later entries, so nothing after the welcome is lost. A device that joins
from a welcome reads the group's queue from the welcome on, also the entries it passed over while
it had no group (its own requests, others' entries it couldn't read), so a request it posted again
meanwhile doesn't make it skip keys sealed to it before that request. The keys of the epochs
it missed are sealed to it after the commit (above, "Keys a device added back missed"); the user's
key bag may hold them too.

**Notes to self.** Every user has a DM with themself (`POST /dms` with their own ID; the server
returns the same conversation every time). Its only participant is the user, so its group holds only
that user's devices: the device that finds none after a sync founds the group like any DM's, and
the user's further devices join it as further devices (#179). Nobody else can be added; the server
never lists it among anyone else's conversations (except the import account's, while it imports the
user's history into it, §4.10). The apps show it as "Notes" pinned above the
direct messages (`ClientSettings::creates_notes_to_self`). Two devices opening it at once still get
one conversation: the DM's participants key is unique, and the request that loses the insert
returns the winner's conversation. A conversation is the notes only when its *participants* (the
people the DM was opened for, kept by the server also once one is deactivated) are the user alone,
never because the user is its only member left: a DM whose other person was deactivated stays that
DM (issue #643). Showing it is display only; the group's membership is unaffected either way.

**Re-founding the notes** (issue #910). The notes' group holds only the user's own devices, so when
all of them are lost (a private window was the only device, and the user recovers, §6), nobody is
left to add the recovered device: it reads the history from the key bag, but new notes would stay
locked for good. "Dead" is decided by the client from signatures, never from the server's word
(last seen, an unsigned revocation, a member list): the device founds the group anew only when

- the conversation is the notes (§ above) and the server lists nobody but the user in it,
- this device has no group for it, and
- the user's newest signed device list (verified with the pinned identity key) names this device
  with its signing key and **no other active device**. Without a list nothing is founded.

While the list still names another device (the lost one was never revoked), the device waits and
asks to be added, like any further device; the banner says that removing the lost devices in the
settings sets the notes up again. Revoking them (§3) makes the next sync check at once.

The new group starts with this device alone. The server keeps one queue per conversation, with
first commit per epoch wins, so the device advances the new group, on its own and unseen,
with empty commits up to the epoch the server's queue is in, then posts one real commit (a self
update with the new group information) for that epoch. No server change is needed. If the server
refuses it with an epoch conflict, a device of the old group committed first and is alive: the
new group is dropped and that device adds this one. Once accepted, the device starts a fresh
content key (the lost devices know the old ones; those stay in the ring and the key bag for the
history), and devices linked later join the new group as usual. A device of the old group that
still runs can't read the new group: it falls out on that commit and asks to be added back
(§4.6, "Falling out and coming back"), which only works if the user's list names it. At most
`max_refound_epoch` (client, default 1 000) epochs are skipped this way, and an attempt that
fails (an epoch conflict, a lost answer) is tried again only after `rejoin_retry`, so a server
can't make a device spin on a made-up epoch. The group, its cursor and the fresh content key are
stored in one group of writes (§9.5): after a crash the device has either no group (and founds
again, one epoch on) or the group with the fresh key, never the group with the old key.

This never applies to a conversation with another person's devices: those are added back by the
other members (§4.6, rejoin), never replaced by one device alone. What a hostile server can still
do: claim a DM is the notes, or withhold a newer device list while the device enrolls, so that a
device founds a group holding only itself. Nothing becomes readable to anyone else that way; the
user's other devices (or the DM's other person) are only split off until added back, the same
denial of service the server can always cause.

**Founding a DM, and finishing it.** A DM or group DM has no creator who must found it: whichever
participant's device first commits at epoch 0 owns the group, and the server keeps only that commit.
The device that opened the DM keeps a setup record until its group holds every participant it asked
for (from the user's own request, never the server's member list), and finishes a setup cut short
(crash, lost network) on the next open, or on a sync at `rejoin_retry` doubled per attempt, up to
`rejoin_retry_max`. It founds only when the server shows no group (epoch 0) and it has none. A group
of one that this device founded and nobody joined is given up only for a welcome that checks out:
every leaf verified against the pinned identities and active by its user's newest signed device
list, the tree holding exactly this user and the DM's other participants, and made by one of those
participants or by another device of this user, which a signed device list must name (fails closed:
a revoked device can't hand out the group). The founded group goes only after the join succeeded. The
server's word alone (an epoch conflict, a later epoch in the queue) never makes it drop the group,
so a server can't make devices drop and found again in a loop; it can only delay the DM. On giving
up, the founding content key stops being current but stays in the ring (and the account's key bag,
which merges by union), and the winner's key becomes current when it arrives. While such a device
waits for the winner's welcome (or for anybody's, when someone else founded), the DM shows "setting
up encryption", and after `dm_welcome_timeout` (default 2 minutes) that the setup failed; "retry"
runs the setup again and, if the welcome is still missing, posts a signed rejoin request so a member
adds this device (§4.6, rejoin), and the wait starts anew. Retrying never founds or gives up a group
by itself; the rules above still decide.

**Meeting chats.** The server creates a meeting's chat together with the meeting (docs/meetings.md),
so no client founded its group. Who gets in is decided by the organizer's and hosts' own actions,
never by the server's member list:

- **Founding.** Only the device that scheduled or started the meeting founds the group, right after
  the server created it (`set_up_meeting_chat`), like a group DM's: a setup record holding the users
  invited in that request (noted before the first request, so a setup cut short by a lost network
  or a closed app is finished on a later sync), founded only when the server shows no group and has
  no group information for it. It publishes the group's information at once (and once more on a sync
  while the group is still at epoch 0, until the server is known to hold it; an archived meeting
  asks no more), so the organizer's other devices join by external commit
  even while the group holds nobody else (an instant meeting). Any other device, the organizer's
  included, only joins: as a further device, from a welcome, or by a device-add request. It never
  founds, so a server that hides a group can't make a second device fork it.
- **Old meetings.** A meeting made before the server stopped founding these groups has a chat
  without one. The organizer's device founds it the first time the organizer opens or joins the
  meeting (the same `set_up_meeting_chat`, with the attendees as that device knows the meeting; it
  does nothing when the group is there) and publishes the group's information at once. Another
  member's device only gets the usual "not let into the encryption yet" until then. If two of the
  organizer's devices found it at the same moment, the server keeps the first group's information
  at epoch 0 (a second, different one gets `409 epoch_conflict` while the group is still in epoch
  0), and the second device gives its group up and joins the first by external commit; the same
  goes when one device's first commit wins the epoch. Epoch 0 is the only epoch where the first
  information stays.
  Only the organizer's devices may publish a meeting chat's group information at epoch 0 (`403`
  for any other member), so a member can't take the slot the others join from. If the information
  at epoch 0 can't be joined (planted by the server, or not the organizer's group), the organizer's
  device founds the group anew and replaces it (`is_replacement`, allowed to the organizer while
  the group is in epoch 0).
- **Whose group.** The answer for a single meeting chat (`GET /conversations/{id}`) names the
  meeting's organizer. A device that joins the chat's group, from a welcome or by external commit,
  checks that the group's first member (the founder's leaf) is the organizer's device and refuses
  the group otherwise, so a member can't found the group and add the others. Remaining gap: the
  organizer is the server's word, not a signature, so a hostile server can name somebody else (a
  signed organizer in the invitation would close it); and a group whose first leaf was reused after
  the organizer's first device was removed is refused as well.
- **Later invitees.** When the organizer saves a changed invitee list (`PATCH /meetings/{id}`),
  the organizer's device adds the new users to the group with signed adds (`update_meeting_chat_members`,
  from the list the organizer saved, never the server's member list) and removes the users taken
  off the list with a signed removal commit, so they read nothing new. The removals are noted on
  the device first and finished on later syncs if a commit fails; admitted guests and members are
  not on that list and stay.
- **Admission.** A host's "admit" in the lobby is the host's own decision: the host's device adds the
  devices the admitted user has at that moment, with history from the join on (§4.7), signed like
  any addition. Nothing waits for a device that appears later, unattended, since the server could
  register one of its own. Being made a member by the server alone lets nobody in.
- **Calls.** A call needs the device in the group (§8). Starting one first takes the next step of
  this device's setup, picks up a waiting welcome or joins as a further device; without a group after
  that, the call fails at once with "not let into the encryption yet" and a retry, instead of
  waiting for keys.
- **A call that grows.** A DM or group DM call that somebody adds people to moves into a new meeting
  chat (docs/meetings.md, "A call becomes a meeting"); the DM's group never changes, so the added
  people get none of its history. The adding device founds the meeting chat's group as above, with
  the users who are in the call as the call shows them *and* in the DM's own group (what its members
  agreed on in signed commits: somebody the server lists gets nobody in), plus the users it added.
  Only once the group is founded does it ask the server to move the call, so the others find the
  welcome waiting when they switch over. A retry founds nothing anew and adds only who is missing.
  The server's `call_move` event is only a trigger: the adding device announces the move in the
  DM's group (an encrypted `CallMoved` event naming the room, the added people and the DM call's
  ID), and another device follows only that announcement, decrypted in the DM's group (so its
  sender is a member), after the user's tap that shows who moved the call and whom they added, and
  only if it is for the call this device is in now (the call ID of its own join token) and the
  room's group, once this device is in it, holds nobody but the DM's members and the named people.
  Only the device that added the people follows without the tap (a flag only it writes into its
  record); a followed announcement is deleted. A server that redirects the call, replays an earlier
  move in a later call, or a member who points it at an old room with other people in it, gets
  nobody's audio or video there.

- **Meeting guests** (#446). A guest has no account at the identity provider, but is a user of the
  server (kind `guest`) with a session token for one meeting (docs/meetings.md). Their browser page
  runs a normal client core with an **ephemeral device**:
  - *Identity and credential.* The page enrolls like any device (§3, §3.2): a fresh identity key,
    a device key, the signed device certificate (the MLS `BasicCredential`), key packages and a
    signed device list. The key bag the server requires with a new identity is wrapped with a random
    passphrase that is never shown or stored: there is no backup and no recovery, a guest who loses
    the device (cleared site data) loses the guest's place, and the host admits the same link-holder
    again as a new guest. The store is the web client's encrypted store (§9). "Leave the meeting" deletes its
    database (only a guest device accepts that command), and a guest page opened later deletes the
    database an earlier page left behind (closed without leaving), as far as no other tab holds it
    open. Until then the encrypted data stays in the browser; nothing reuses it, since the session
    token is gone and the server deactivates the account when access ends. No push, no search index, no account data sync.
  - *Link secrets.* The server signs the session tokens and decides admission, so by itself it
    could admit a guest of its own and hand members that guest's key. A guest link therefore carries
    two values in the `#fragment`, which no server sees: a random 256-bit **link secret** made by the
    host's device that makes the link (kept in its sync state; the shared guest link keeps its secret
    for as long as the link lives), and the **organizer's identity key fingerprint** as that host's
    device has it pinned (`#invite=<token>&k=<secret>&o=<fingerprint>`). The server never gets either.
  - *How members verify a guest without a directory account.* Nothing attests the guest's identity
    key (§3.1 needs the identity provider), so the guest proves it holds the link instead: after
    enrolling, the page sends `PUT /meetings/{id}/lobby/me/proof` with `HMAC-SHA256(link secret,
    "TeamCraft-v1 guest link proof" ‖ meeting ID ‖ user ID ‖ identity public key ‖ display name)`.
    The host's device that made the link fetches the guest's lobby entry
    (`GET /meetings/{id}/lobby/{user}`) and the guest's identity key, recomputes the MAC and adds the
    guest's device only if it matches, so the server can neither invent a guest nor swap a real
    guest's key or name: it never learns the secret. The same holds in meetings without a lobby
    (the server admits at once, but the device adds nobody without a valid proof). A host device
    that doesn't hold the secret (another device of a co-host) adds a guest only after the host
    compared the guest's **key code** (the first 64 bits of the identity key fingerprint, as
    `1a2b-3c4d-5e6f-7a8b`, shown on the lobby entry and on the guest's screen) and confirmed it. A valid proof is a bearer proof of
    the link: anybody with the link can be the guest, which is what a link is; the shared link's guest
    limit and the lobby bound that. Members pin the identity key on first use, mark the user "Guest
    (unverified)" in the chat, the member list and the call, and verify the certificate chain
    (identity key → device key → MLS leaf) exactly as for members. An identity key change for a
    pinned guest is refused like for anybody else (the server also refuses `replace` for guests).
  - *How the guest verifies members.* The guest's client treats the members' identity keys like a
    member's client does: attested ones (§3.1; the issuer and client come from the server info, the
    JWKS from the identity provider directly) are pinned as attested, the rest on first use. The
    guest's device is marked as a guest device when it enrolls, and a guest device accepts a group only from a
    welcome sent by a device the organizer's identity key signed (the fingerprint in the link), so
    only the organizer's devices make guest links and let guests in; a co-host's app refuses
    ("only the organizer's devices can let guests in"). A guest device with no fingerprint (a link
    of an older client) or offered a group by external commit joins nothing: it fails closed. The
    check is about who added the guest, not about who is in the group (a group the server made
    with devices of its own can hold the organizer's published key package) and not about where a
    leaf sits in the tree, so a hostile server can't put keys of its own in for the people the
    guest talks to. Remaining gap: the guest still pins the other members' keys on first use
    where there is no attestation, and the member path (§4.6, "Whose group") still trusts the
    server's naming of the organizer.
  - *Admission.* A host's "admit" makes the guest a member on the server, and the host's device
    then adds the guest's device as above (`let_guest_into_meeting_chat`: the proof or the
    confirmed key code is checked first, then the key is pinned and the add is signed like any
    addition), always with history from the join on. The guest's page enrolls its device and sends
    its proof while it waits in the lobby, so the host's app offers "admit" once the lobby entry
    carries the proof (or the key code is shown); a guest admitted without a device is not added
    later unattended (§4.6, "Admission", above). The welcome reaches the guest through its normal
    welcome queue, and the current key follows from a member as for any new member.
  - *Removal.* A guest's session ends when the guest leaves (`POST /meetings/{id}/leave`), is
    turned away, or the meeting's access ends: the server then deactivates the guest and reports
    the removal (`membership_remove`, `device_remove`). The sweep that ends sessions of guests whose
    meeting is over or cancelled runs on the server at least every ten minutes and acts at `ends_at`
    plus `guest_grace_secs`, so no guest stays a member on the server. Clients still sign removals
    themselves (§3): a report is only a hint. Every online device of a group member that sees
    `membership_remove` for a user it knows as a guest of that meeting (admitted by a host's
    device, which is recorded in the group's signed adds as the guest's add) commits the signed
    removal of the guest's leaves, ranked as for device removals so normally one commits; a device
    that was offline does it on its next sync, and the device that admitted a guest also removes them when the meeting's access ends by its own
    clock (the end the host's app showed it at the admission), whether or not the server reports
    anything. The guest's
    page removes itself first where it can (a signed leave before wiping the store). Until the
    commit, a guest device that is gone for good stays a leaf and could decrypt what the group sends
    in that time; the commit moves the group to an epoch it has no keys for.
  - *What a guest may commit, and who counts as a guest.* Members merge a commit made by a
    guest's device only if it concerns that guest's own devices, and a guest's signed change is
    authorized only for their own removal. One more commit is merged from a guest: somebody's
    verified signed leave (the signer removes themselves, and only them), so the import account can
    leave somebody's notes or a DM of guests, where nobody else could commit it (#1074). A guest's
    own device leaves every other signed change of somebody else (an admin's removal) to the
    members, and a signed leave too while a member who isn't a guest is in the group: members
    before this version refuse that commit, and the group would split.
    Who is a guest is not the server's word alone: in a
    meeting chat everyone the signed roster doesn't vouch for is treated as a guest for signed
    changes; a device that admitted a guest records it and treats the user as a guest from then
    on; and a roster member or an organizer is never treated as a guest, whatever kind the server
    shows. Remaining gap: a hostile server that relabels a guest as a member to the devices that
    did not admit them can't give them signed authority (the roster rule), but can let them add
    further devices of users who are already members, which those users' identity keys have
    signed anyway.
  - *Proof scope.* The proof binds meeting, user, identity key and display name, not the device or
    an expiry: the host checks the guest's device against the signed device list, and the link is
    only good for the meeting's lifetime plus the grace time. Binding the device ID would only
    matter if a second guest device were allowed, which a guest session does not need.
  - *Calls.* The guest's call key binding (§8) is signed by the guest's device key; members verify
    it like any other and show the guest as unverified.
  - *Scope.* A guest session token is accepted only on the routes a guest needs (chat, key
    material for the guest's own device, the meeting's lobby state and leave, calls). Everything else
    answers 403, so a guest can't search the directory, open DMs, join spaces or schedule
    meetings, whatever the roles say.

Not covered yet: users invited later by editing the meeting (#475).

**Server time for merge decisions.** Whether every member merges a commit must not depend on its own
clock, or a skewed clock would split the group. The roster freshness (admin.md §2.3) is judged at the
server's time of the commit (its queue ID), and a device's removal time for refusing its later
messages is the removal commit's time, compared with the server's time of each message. A server can
stretch a stale roster's freshness only by back-dating its IDs, which every client also shows as
message times. A device's own decision to commit still uses its own clock.

**Outbox after catch-up.** After every new gateway connection the outbox sends into a conversation
only after its MLS queue was caught up, so an offline message never goes out under a key that a member
removed meanwhile still holds.

### 4.6.1 Signed channel roles

A channel's roles (admin = channel owner, moderator, member; admin.md §1) are changed on the server
like any setting, but members don't take the server's word for them (issue #380). Each channel has
an append-only **role log** next to its messages (`/conversations/{id}/role-log`), built like the
workspace roster (admin.md §2.3):

- **Entries.** Each entry is a full snapshot of the channel's admins and moderators (members are
  implicit), with the identity key of each as the signer's device pinned it, the channel's ID, the
  entry's version, the hash of the last entry the signer's device accepted (`prev_hash`, zeros
  before the first) and the signing time. It is signed by a device key like a roster entry, under
  its own log kind (`WorkspaceLog::ChannelRoles`), so it can't be moved to another log, version or
  channel. The server checks who may append (`manage_channel_roles` in an open channel, the
  caller's own active device), that the document names this channel and the next version, and
  orders the entries (`409 version_conflict` for a stale version); it never verifies signatures.
- **Who may sign** follows the role rules of `teamcraft_proto::role_rules` (the code the UI uses;
  the server checks the same rules with `is_allowed`), fed only with signed facts and the built-in
  permission scheme:
  - an owner or admin of the verified workspace roster, fresh at the entry's server time, may sign
    any snapshot, also one built on another head than this device's (a checkpoint that re-anchors
    a device that skipped an entry);
  - an admin of the snapshot this device accepted last, verified against the key that snapshot
    names for them, may sign the next snapshot built on exactly it;
  - **the founder**: while nothing was accepted yet, an entry whose signer names themselves admin,
    with the identity key this device pinned for them (trust on first use, like the workspace root
    of trust). A channel's creator signs it when creating the channel; a channel from before signed
    roles is founded by the first admin the server lists who changes or confirms a role.

  Every change against the accepted snapshot must pass `check_channel_role_change`: nobody changes
  their own role (the founder's claim aside), a snapshot keeps an admin, only `admin` and
  `moderator` are listed. Moderators, members and anybody else sign nothing members accept. Space
  admins and custom permission schemes aren't signed facts, so space admins' and scheme-granted
  role changes stay the server's word (shown as not signed).
- **Verifying.** Each device reads the log after its cursor and verifies every entry in version
  order; an entry is accepted or skipped, never fatal. Signers build on the last entry they
  accepted, so a junk entry (a hostile server, a modified client) can't block the log. The cursor
  and the accepted version only move forward: a replayed entry is below the cursor, and moved to a
  new position its signed version doesn't match. The accepted head (version and entry hash) travels
  inside every encrypted text message of the channel; a member with another entry for the same
  version raises an alarm, a newer one makes the device read the log again and raise an alarm if
  it still isn't shown it, as for the roster.
- **Using a role.** The role that counts is the signed one, capped by the server's: the server can
  take a role away (it can withhold anything anyway) but never grant one, and an instance guest is
  a member whatever is signed. Members' apps show a role only the server lists as "not signed";
  admins who can sign confirm it with "Sign role". Decisions every member must make alike (issue
  #776, the owner's approval of an admin joining a private channel) use the signed role alone
  (`Client::verified_channel_role`) together with the MLS group's membership.
- **Admin joining under `owner_approval` (issue #776, stage 2 of #380).** An admin of the signed
  roster who is not in a private channel asks its owners (admin.md §2.4.1). The approving owner's
  app reads the role log (`sync_channel_roles`) and approves only if `verified_channel_role` makes
  the owner an `admin`; otherwise it shows "your owner role isn't signed" and offers Decline only.
  Members' apps judge the commit the same way (`WorkspaceAuthority`): in a private channel whose
  role log has an accepted entry, while the verified policy says `owner_approval`, an
  `AddUser` of a fresh-roster admin is authorized only if the committer is in the signed snapshot as
  `admin`. The committer's signed role comes from the device's own verified state, never from the
  server's member list; the device reads the log before judging a commit under this policy (one
  request per commit, nothing otherwise). Removals, other additions and every other policy are
  unchanged.
- **Migration (compatibility step).** The rule is a stricter merge rule, so it is switched on per
  channel by data, not by version: only a channel with an accepted role entry uses it. A channel
  nobody signed yet keeps "every member may commit" (§4.6), so no approval is refused for lack of
  a signature and clients without this rule in the group keep merging what newer clients merge.
  A channel becomes signed when it is created by this version, or when an admin it lists changes
  or confirms a role (founding, above); its owners then sign their own role once. Residual: an
  owner of a signed channel whose role is the server's word only (for example a co-owner who uses
  an older client, which can't sign) who approves from an older client makes newer clients refuse
  that one commit, and the admin stays out until a signed admin signs the co-owner's role or the
  co-owner updates; the older client never gets a wrong view of the group beyond that commit.
- **Not yet decided by signed roles.** Other commits still follow §4.6: every member of the group
  may sign membership changes (security review A11), since custom permission schemes and space
  admins are not signed facts and would be locked out. Integration sources still need a workspace
  admin (§13.1). Both are tracked as follow-ups of #380 (restricting commits further, signed
  channel admins as source admins; a withheld demotion is tracked there as well).
- **Workspace admins and channel admins.** `role_rules` lets a workspace admin of the fresh roster
  sign any snapshot, so a workspace admin can appoint a channel admin who then approves a workspace
  admin's join. This is kept on purpose: orphaned channels need it (admin.md §2.5); a policy switch for it is tracked as a follow-up.

Residual risks: the founding entry of a channel is trust on first use, so a hostile server together
with a member of a channel from before signed roles can make that member its first signed admin
before a real admin does (a workspace admin's checkpoint fixes it, and the real admins see the
roles they hold shown as not signed). A demotion the server withholds is noticed only through the
heads members exchange; channel roles have no freshness window like the roster's. A device that
verifies a log from scratch judges a workspace admin's old entry by the roster it has now, so an
entry by an admin who lost the role since is skipped there (and the entries built on it) until a
workspace admin signs a checkpoint.

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
  that asks for more (a modified client can't talk the group into showing it the past). The joiner
  never hears of a refusal: until a member lets it in, its device shows "waiting to be let into this
  channel" and posts the self-add again after `self_add_retry` (30 minutes, doubling up to a day),
  so a self-add refused by one version or policy goes through once the members accept it (issue
  #907).
- **`history_policy` in the conversion request** is therefore a *hint* the server stores and shows (and
  that the add dialog can preselect); it is not what protects the past. What protects it is the rule
  above. Converting to public never gives anyone history they didn't have, and can't take anything back
  from people who already read it.
- **Going private again** keeps everybody who joined while it was public. Removing them is an explicit
  member removal, which rotates the keys (§4.2).
- Guests stay what they were: they can't join public channels on their own and are always `from_join`.
- Integration channels (bridge output) stay private; archived channels are converted after unarchiving.
- Members get a `privacy_changed` system event, the audit log records `conversation_visibility_changed`
  with the old and new value (metadata only). The line members' apps show ("This channel was made
  public") comes from the server's record of the channel, not from a member's message (§4.9).
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
  as an encrypted `moved` group message naming the new space. Members' apps show the line from the
  server's record of the channel; the message only names the mover (§4.9).
- The new space's admins manage the channel from then on (its scheme applies), like any channel of that
  space.

### 4.9 Lines about channel changes (issue #818)

Whether a channel is archived, public or listed in a space is the **server's record**: no MLS commit
carries it, and nothing signs it until channel properties are signed (a follow-up of the signed
channel roles, issues #380, #801). The lines "… archived this channel", "… made this channel
private / public" and "… moved this channel to …" used to come from the encrypted message of whoever
made the change. Its sender is authenticated, its content wasn't checked against anything: a member
could post "Carol archived this channel" without anything happening, and a member who did it could
leave the message out. So the line now comes from where the change is, and says whose word it is:

- **The line is the server's statement.** Every member's device compares the server's record of a
  channel with the one it held whenever it stores a new one: the refetch after a gateway
  `conversation_update`, a sync's conversation list, the server's answer to its own change. Each
  difference in `is_archived`, public or not, and space is a line, in that order, worded as what the
  server says: "This channel was archived", "This channel was moved to Project B" (the space's name
  as this device knows it). Only channels; a record the device sees for the first time tells
  nothing (nothing it saw changed), so a device that joins later, or was away while a change was
  undone again, shows no line for it.
- **Who made it, only when that member says so and it fits.** Whoever makes the change still sends
  the encrypted `archived` / `unarchived` / `made_private` / `made_public` / `moved` message (older
  versions show it). This version keeps every such message hidden; one names who made the change
  ("Carol archived this channel") on the line it matches: the same kind (for a move, the same space
  unless one side doesn't know its name), sent after every message the device held when it saw the
  change, and no later than `change_announcement_window` (10 minutes by default, on the server's
  clock) after the server's newest message then (the sync's head for the conversation, else the
  newest the device held). The earliest line still unnamed takes it, and moves back to the
  message's place if that is earlier (a change caught up after a while away), never later. A second
  member's message that fits a named line leaves it naming nobody: two members claimed the change.
  A user's own change names them on the device that made it ("You archived
  this channel"), whatever others claim, and only for what their request asked for: a difference
  in the server's answer that somebody else made meanwhile names nobody, and the device announces
  only what it changed. A message that matches nothing (a made-up change, a different kind, too
  early or too late) shows nothing at all, rather than a neutral "Carol says …": it would still
  read as a system line, and the change, if there was one, is on the record's line already.
- **Where and how often.** The line is stored like a message, marked as derived from the server's
  record (`server_change`, only ever set by the device itself), with no server ID. It sits after
  everything the device knew of the conversation when it saw the change (the server's newest
  message for a sync, else the newest message held) at the last ID of that millisecond, which no
  message gets unless one server instance hands out 4096 IDs in one millisecond, and a second line
  of the same record at the next millisecond's; once a message names who made it and is older, at
  that message. Its ID comes from that first place. The record and its lines are written in one
  group of writes, so a crash keeps both (the change was told) or neither (the next refetch or sync
  sees it again): refetches, syncs and reconnects tell a change once. An announcement and the name it puts on a
  line reach the disk together too. As for the admin-join line (admin.md §2.4.2) no message
  replaces, edits, deletes or pins it, and a message the server gives the same ID doesn't take its
  row. The time shown is when the device learned of it (at least its place's); once named, the
  message's.
- **Unread** like the admin-join line: like a message at its place, from anybody but the user
  (their own change isn't unread to them); a line newer than every message is read when the user
  reads the conversation on this device.
- **Older versions.** 1.0.8 clients send the same messages, which name them here; this version still
  sends them, so 1.0.8 shows its changes as before. Messages stored before the update are hidden
  after it, and their changes have no line (they happened before this device compared records).

What still rests on the server: whether and when a channel changed, and the space's name, are its
word. A server can report a change that didn't happen or hold one back, and nothing a member sends
makes a line or keeps one away. A name on a line is always a member's own encrypted claim of that
kind of change; a server that numbers messages could at most hand a device such a claim it never
saw before (an older one, re-timed into the window), or drop the real one so that a member's
competing claim is the only one that fits. A member racing the real announcement with their own
only makes the line name nobody. Signed channel properties would close this.

### 4.10 Imported history (issue #402)

History from another chat system (Synology Chat first; Slack, Teams and Mattermost later, #209) comes
in through a client, never through the server: the server can't write into an encrypted
conversation. The **importer** is `teamcraft-import` (`crates/teamcraft-import`), a headless client on
`teamcraft-core` with a device of its own, run by an operator next to the export (admin.md §3.8, runbook
[import-synology.md](import-synology.md)). It signs in as a dedicated **import account**: an ordinary
member of the identity provider with the admin role, so it is a workspace admin of the signed roster
(admin.md §2.3). Bots can't import: they have a guest's rights.

**The import policy.** Who imports is the owners' signed decision, not the server's: the workspace
policy (admin.md §2.3) names the import account and how long it may join conversations
(`import: { importer, joins_until_ms }`, off when absent). Members' clients act on an imported message
or an importer joining only for that account, under the verified policy. Clearing `joins_until_ms`
(or letting it pass) ends the joins; the importer stays named so that what it imported keeps showing
its origin. Naming another account makes the first one's messages show as its own again.

**An imported message** is an ordinary encrypted message of the importer's device: the same content
key, associated data and sender signature (§4.4). What it says about its origin is inside the
ciphertext, in `MessageMeta::imported` (`ImportedOrigin`):

| Field          | Meaning                                                                                                                                                                                  |
|----------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `source`       | The system it came from (`synology`), forward compatible                                                                                                                                 |
| `source_id`    | The message's ID there (opaque), for idempotence                                                                                                                                         |
| `author`       | `user` (a TeamCraft account the mapping names), `deleted` (unmapped, deleted or left people: shown as "Deleted user") or `named` (a bot or webhook of the old system, by its name there) |
| `sent_at_ms`   | The original time                                                                                                                                                                        |
| `edited_at_ms` | Set when the original was edited                                                                                                                                                         |
| `reactions`    | Emoji with the authors who reacted, as above                                                                                                                                             |
| `missing`      | Names of files the export didn't have                                                                                                                                                    |

- **Whose word it is.** The signature proves the importer's device wrote the message; author and time
  are the importer's statement. Clients show them only when the sender is the policy's import
  account: the author's name and picture (or "Deleted user", or the bot's name with a "bot" mark),
  the original time; the time's tooltip says "Imported from Synology Chat", and one line marks where the imported history ends and the conversation's own messages begin.
  From any other sender `imported` is ignored, so a member can't post in somebody else's name.
  Clients before this version show the message from the import account.
- **Fixed.** The original authors can't edit or delete an imported message, and its reactions can't
  be taken back (they are part of the message); everybody can react anew. Moderators delete it like
  any message. Mentions stay in the text (`@username`), but the server isn't told (no
  `mentioned_user_ids`): nobody is notified of old mentions.
- **IDs.** The client message ID is derived (`SHA-256("TeamCraft-v1 imported message" ‖ conversation ‖
  source ‖ source_id)`, first 16 bytes), so a second run sends the same IDs and the server keeps the
  first (§4.4, deduplication). Threads, reply quotes and pins point at the imported messages as usual.
- **Order.** Server IDs decide the order. The server numbers imported messages (admin.md §3.8) from
  one sequence of its own (a reserved worker number, starting at the epoch), in the order the
  importer sends them: within a conversation oldest first, and below every message of the
  conversation that wasn't imported, so the history goes before what the conversation already
  holds. The ID's time says nothing about an imported message; clients show `sent_at_ms`.
- **No unread, no pushes.** The server counts no imported message as unread and sends no push and no
  `message_create` for it; when a conversation's import is finished it announces a
  `conversation_update` (with `imported_through`). Imported messages lie before everything a client
  holds of the conversation, so paging back loads them like older history; a client that holds the
  conversation loads one page of them on that update, or on its next sync when it missed the update.
  Clients count no imported message as unread and don't show the import account adding people, nor
  a member letting one of them in once they set up the app: they were in the conversation in the
  old system. Read positions come from the old system (admin.md §3.8).
- **Files** are attachments of the importer's message (§4.5), encrypted with a fresh key each and
  uploaded from the export as a stream (multipart above 8 MiB), with thumbnails for pictures.
  Stickers of the old system are pictures.
- **Search** indexes an imported message under its original month (`sent_at_ms`).

**Into which group.** The importer posts with the conversation's current content key, as a member of
its group, and leaves once it is done:

- **A new channel**: it creates the channel, posts the history while it is the only member, then adds
  the members with the full history (§4.3) and makes the most active mapped member channel admin
  (§4.6.1) before it leaves. Somebody without a device yet is a pending addition (§4.3, "Nobody to add
  yet"), so the importer keeps running until everybody joined (below).
- **An existing channel**: it joins as a workspace admin (admin.md §2.4: from the join on, and every
  member sees "… joined this channel as a workspace admin"), posts, and leaves. Members are not
  changed; a member added later with the full history gets the imported history with the rest.
  A channel that shares history only from the join on would hide the import from everybody who joined
  after the original dates, so the importer refuses it.
- **A DM, or somebody's notes** (participants: the two people, or the user alone; §4.6, "Notes to
  self"): the server makes the conversation for the participants and lists the import account as a
  member that isn't a participant (admin.md §3.8). Without a group yet the importer founds it and
  adds the participants with the full history; a participant's device joins from that welcome like
  any DM's (the server lists the import account, so its identity is pinned and its leaf verified).
  A participant who wrote in the DM before, while the other had no device yet, holds a group of one
  that was never committed; their device takes the importer's welcome in its place when it comes
  from the import account of the signed policy and the tree holds nobody but that account and the
  DM's participants (a participant without a device yet is added later, by the importer's pending
  addition). Any other welcome still has to hold exactly the participants.
  Into an existing group the importer posts its signed self-addition as a workspace admin (admin.md
  §2.4: from the join on, only under `admin_self_join = visible`), which a participant's device
  commits. Once every participant is in the group, the importer posts its signed leave, a
  participant's device commits it (the keys rotate, §4.2), and the import account leaves the
  conversation on the server without a line. Group DMs of the old system would be handled the same
  way.
- **The import account in a DM is no person of it.** Clients leave the policy's import account out
  of a DM's name and member list, and show no "joined" or "left" line for it there. Whoever is a
  member of a DM or the notes without being a participant is shown: for the import account the
  banner "History is being imported. The import account can read this conversation until the import
  is finished.", for anybody else a warning that somebody who isn't a participant can read it. That
  is the trade-off: until everybody is in, an admin's device can read what the participants write
  there, which the banner says and the signed policy bounds in time.

**Waiting for people.** A person who never signed in has no device: they are listed by the server
(pre-provisioned from the identity provider, admin.md §3.8) and their additions wait on the import
device, which alone holds them (§4.3). The importer therefore keeps running after the import
(`teamcraft-import serve`) with `pending_addition_expiry` off, adds each person's first device with the
full history, and leaves each conversation once nobody is missing. If somebody's first device joins
on its own first (a member committed the device's signed self-add, which shares from the join on),
the importer, which holds that person's pending full-history addition, seals the history to the new
devices like the history of a new member (§4.3). Its report lists who is still missing, where.

**Guests in imported conversations (issue #1074).** A guest gets history from the join on (§4.3),
which would hide the import from a guest of the old system who signs in only afterwards. So the
import device makes one exception: a guest is still added from the join on (the signed change says
`FromJoin`, as for every guest, and the content key rotates to the epoch key, §4.2), and the import
device then seals **the import's keys**, and no other, to the guest's devices. The keys of the
import are those its ring held when the conversation's import was finished; in a new channel the
import device then switches to a fresh key while it is still alone in the group, so what members
write afterwards is never under a key of the import. A guest so gets the imported history and
everything from their join on, not what members wrote between the import and the guest's join.

The import device seals the import's keys when it commits the guest's addition, when a waiting
addition of its own finds the guest's device let in by somebody else meanwhile, and to a device of
the guest added back, and only when all of this holds at that moment:

- **The policy.** This account is the import account of the verified workspace policy and the
  policy's `joins_until_ms` hasn't passed; the verified workspace state raised no alarm (a fork or
  withheld entries, admin.md §2.3) and its roster is younger than `workspace_max_age`, so a server
  that withholds an early "End import" can't keep the exception alive for longer than that. Times
  are the later of the import device's own clock and the newest server time it saw (a server that
  holds its clock back gains nothing).
- **The conversation and the person.** The import tool recorded on the import device, from its
  mapping, that the person was a member or participant of the conversation in the old system (the
  *import grant*), for a conversation the import made or took over: a new channel, or the DM or
  notes the server answered for the participants (`POST /admin/import/dms` may hand back an existing
  one, which the import device then founds or joins, §4.10 above; whether it existed is the server's
  word, which only decides which group the import's keys belong to). A channel merged into gets no
  grant: there the import changes no members. The grant names the policy's `joins_until_ms` it was
  recorded under; under another date (also a renewed policy) it counts only once the tool records it
  again.
- **The keys were noted.** The import's keys are noted when the tool finishes a conversation's
  import, or for a conversation an earlier version finished, on the first pass of this version (then
  they are the keys the import device holds at that time, which may include what members wrote
  before under the last of them: a limit of imports run before this version).

Nothing on the wire is new: the addition is an ordinary "from the join on" addition, and sealed keys
are an ordinary sealed share (§4.3), which every client version takes. Members' devices never share
history with a guest; only the import device seals the import's keys.

The server keeps the same line for the ciphertext it hands out: a guest lists messages,
attachments and lines from before their join only in a conversation that shares its full history
and only when the import account added them, during the import period, to a conversation whose
import was finished (a membership flag that goes with the membership, admin.md §3.8); everybody
else as before. It can't check the mapping; the import device decides what can be read.

What the exception can and can't give away, with a compromised server:

- The server's word decides who is a guest (§4.3), as before: claiming a guest is a member already
  lifts the rule for any full-history addition, with or without this exception. The exception adds
  no new reliance on the server.
- Who may get the keys is the mapping's statement, kept on the import device; the server can't add
  a name to it. Adding somebody else to the conversation gets them nothing from the import device.
- What they may get is the import's keys of that conversation's group, which the import device
  holds only as a member of it: another conversation's history can't be routed to a guest by
  swapping IDs, and nothing members wrote after the import (and, from this version on, before the
  guest's join) is under those keys.
- When: only while the owners' signed policy names the account, its date runs and the workspace
  state is fresh; the server can't extend the date. Once the import account left a conversation it
  holds nothing more to share.

The import account leaves with a signed leave that a member commits (§4.10 above). In somebody's
notes or a DM of guests nobody but guests is left to commit it; there a guest's device commits the
signed leave, which members accept from a guest (§4.6, "What a guest may commit").

People made members for the import and switched back to guests afterwards keep what they got: the
keys are in their key bag, a member who becomes a guest isn't removed from a conversation, and no
device re-adds or re-shares because of the switch. A waiting addition of such a person is committed
from the join on once they are a guest, and gets the import's keys if the import grant names them.

## 5. Adding a device (Threema/WhatsApp style)

Two ways, the user picks whichever is convenient:

1. **Approve prompt:** the new device logs in via OIDC and shows a short code. Every other device of
   that user gets a prompt "New login on Firefox/Linux – code 471 902. Approve?". On approval the old
   device transfers the identity key and the key bag.
2. **QR code:** the device that can't scan shows a QR code, the phone scans it (like WhatsApp Web).
   A new computer shows its code to a signed-in phone; a new phone scans the code a signed-in
   computer shows for it (*Link a phone*). A new phone never shows a QR code (issue #998).

Transport for the transfer, modeled on Threema's connection rendezvous protocol. Threema implements it
in Rust (`libthreema`, module `d2d_rendezvous`, AGPL-3.0, compiled to WASM and exposed via UniFFI –
the same setup we use). Since TeamCraft is AGPL as well, we can reuse or port it:

- The QR code (or approval) carries a random 32-byte authentication key `AK`.
- Both devices run an X25519 handshake, keys derived from `AK` + ECDH (BLAKE2b / HKDF), traffic
  encrypted with ChaCha20-Poly1305, relayed by the server, which only sees ciphertext.
- In the code variant, the short code is checked by the user, so a server injecting itself is detected.

Implemented as (`teamcraft-crypto::rendezvous`, `teamcraft-core` linking):

- The new device draws `AK` and a relay ID and shows both (`tclink2:<id>:<AK>`; the digit is the
  protocol version, and a device refuses a code of another version when it is scanned, before any
  key leaves the existing device). It is the
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
- **Prompt on every device (the user's choice).** A scanned or typed code never involves the
  server beyond the relay: `AK` stays between the two devices. The new device offers a second way
  next to the QR code, a button "Ask my other devices". Only when the user taps it, the new device
  announces itself (`POST /device-links`, after it joined the relay) with name, platform and `AK`,
  which the server passes to the user's other devices as it would in the approve-prompt variant
  above. **This is the one step that gives the server `AK`**, and the screen says so. The gateway
  sends the request to every connected device of the user (unnumbered, never in the replay buffer,
  because it holds `AK`); a device that connects reads the open ones (`GET /device-links`). Every
  signed-in device (desktop, web, phones) shows a corner prompt. Name and platform are the new
  device's own claim and the prompt says so ("says it is …, not verified"). **The prompt never
  approves and never shows the verification code**: the user takes the request up (*Review*), which
  joins the relay as the responder, and types the code the *new* device shows; the core checks it
  (three wrong codes end the attempt), as for a scanned code. A failed join leaves the request
  listed, so the user can try again. The first device that joins the relay owns the request: the
  server closes it (`device_link_closed`, reason `claimed`) and the other prompts go, so approving on
  one device completes it everywhere. *Deny* closes the request and tells the waiting device
  (`rejected`), which stops. The request ends with the relay (the new device gave up or its
  lifetime, `rendezvous.ttl_secs`, ran out); prompts count the time left from when they arrived, so
  a wrong local clock changes nothing the server enforces. A server that invents a request or
  changes `AK` cannot complete a link: the handshake with the real new device would not produce the
  code the user types. Not yet: a notification on a backgrounded phone (needs a push for the
  request).
- **The code goes on the screen that can't scan (issue #998).** A phone is the device with the
  camera, so a new phone shows no QR code: it offers the prompt on the other devices (above, which
  works from a signed-in phone too) and *Scan code*. For the latter a signed-in computer or browser
  (*Link a new device*) draws `AK` and a relay ID and shows them as `tcjoin2:<id>:<AK>`: same
  payload and version, another prefix. Then the roles in the relay swap: the signed-in device joins
  first and is the *initiator*, the new phone that scans it is the *responder*; the handshake, the
  commitment and the short code are the same. The keys still go from the signed-in device to the new
  one, and the user still types the code the *new* device shows into the signed-in one, which checks
  it as for a scanned code. Each kind of code is refused by the side that must not scan it, before
  anything joins the relay (`LinkCodeForNewDevice` on a signed-in device, `LinkCodeForSignedInDevice`
  on a new one), so two devices never meet that both wait to send, or to receive, the keys. `AK` of
  a `tcjoin` code never reaches the server. A phone without camera access uses the prompt, which
  needs none; a computer that can't show a code to scan types the new computer's code. The server's
  `joined` answer names the relay's lifetime (`expires_in_secs`, absent from servers before 1.0.15);
  the device that shows a code counts it down and offers a fresh code when it ran out. A new phone
  whose wait ran out unasked starts a fresh one by itself and says so.
- **Failures are visible, never a silent wait (issue #645).** The relay belongs to whoever joins
  first. A device that scans a code after the new device left its relay (the code's lifetime ran
  out, the new device slept or went offline) would otherwise open an empty relay and wait for nobody.
  So the responder looks at the server's `joined` answer: without the new device present it fails at
  once (`LinkPeerGone`, "show a new code and scan again"). It also gives the new device 30 s to answer
  each handshake step (`LinkTimedOut`). Text that is no link code (another QR code) is
  `LinkCodeInvalid`, and a code of another protocol version is `LinkVersionMismatch` ("update
  TeamCraft"). A new device whose relay ends unused reports `LinkCodeExpired` and offers "Try again"
  with a fresh code. The apps word these as typed failures (`FailureKind::Link`), and the new
  device's screen says to scan in TeamCraft, since the camera app alone can't use the code. Without
  another signed-in device, the screen points to the recovery passphrase or organization recovery
  (§6). None of this changes what crosses the relay or the server.
- The transfer carries the backup key and the encrypted key bag (and the protocol version). The new
  device checks that the bag opens with the backup key and answers with a sealed **receipt**
  (`received` or `rejected`); the existing device reports success only on `received`. A lost
  transfer frame, a closed relay or 60 seconds without a receipt fail on the existing device, and the
  new device fails because the transfer never arrives (the relay closes when the existing device
  gives up). A lost receipt makes the existing device report a failure although the new device has
  the keys; linking again is harmless. The wait for the receipt holds the relay, not the client
  (`LinkReceipt`, issue #415): the apps send the keys from their core loop and wait on a task of
  their own (web, desktop and the mobile engine alike), so syncing, sending and reading go on
  meanwhile, and the user's confirmation call answers with the outcome. Content keys the existing
  device learns in that window (up to 60 s after the key bag was snapshotted for the transfer)
  reach the new device like any other change: through the next key bag upload, which the new device
  merges when it syncs. Closing the dialog cancels the wait (the relay closes; the keys were sent
  already), and so does stopping the app. Before it
  sends `received`, the new device stores the backup key and the encrypted key bag in its local
  store, as a record next to the account protected the same way (§9; on the web the write is issued
  at once instead of after the batching delay). If the
  app stops before the account is stored, the next start finishes the enrollment with them, so the
  existing device's success is never a lie the user has to undo by linking again. The record goes
  with the account write that completes enrollment (one group of writes, §9.5), when the user
  cancels linking or signs out before the device is set up (with the pending device key pair, whose
  registration is then revoked, §3.2), or when it can't work any more (the key bag no longer opens,
  the record is unreadable, another account signed in). While it is there, the pending device key
  pair is never dropped for its age. The new device then enrolls (§3.2) and joins the user's
  groups on its own (§4.6, "A further device joins on its own"); where it can't yet, an existing
  member adds it when it comes online. It reads the history the key bag opens at once.

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
Secret Service). This is the everyday path; nobody types anything. The desktop app keeps it in the
keychain next to the database key, so the device can be set up again without the recovery kit
when its local data is lost (§6.5.6).

### 6.2 Wrapped by a passkey (primary recovery, like WhatsApp since 2025)

- The user registers a passkey; the client uses the WebAuthn **PRF extension** to derive a wrapping key.
- The passkey syncs through iCloud Keychain, Google Password Manager, 1Password, etc., so losing a phone
  does not lose the passkey.
- Recovery = log in + unlock with Face ID / fingerprint. No password to remember.
- Fallback for platforms without PRF support: a generated **recovery kit** (§6.5) since issue #738.
  Before that, users invented a recovery passphrase, stretched with Argon2id (128 MiB, ~1 s), with
  rate limiting on the server; existing passphrases keep working (legacy, §6.5.7), and the
  workspace policy decides whether new ones may be set (§6.5.8).
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
- **Group epoch floors.** Per conversation, the newest MLS epoch in which one of the user's devices
  switched the content key; merging keeps the higher one. A new device of the user doesn't join a
  group from information older than that (§4.6).
- **Removed senders.** When devices last left conversations and last came back (the removal commit
  and the commit that added them back, server times; only the last absence), so every device of the
  user refuses what they signed while out, by the server's message timestamp (§4.4).
  Merging keeps the later time of each, so a device's clock never decides. Each full sync drops
  records that can't matter any more (#186): absences that ended (the device came back) before
  anything the workspace's retention limit keeps, counted back from the newest server time the
  device saw (its MLS queue cursors) with `removed_sender_retention_leeway` (client, 1 day), and
  only when pinned messages don't survive retention. A device that is still out keeps its record,
  and so does every absence of a conversation the user left: a re-join may share its history
  again, and what a removed device signed while out must still be refused. A dropped record
  leaves a tombstone with its left time for the usual 90 days, so the same absence isn't taken
  back from another device's local records or an older key bag (possibly without its return); a
  later removal of the device counts as before.
- **When the recovery passphrase was last set** (issue #410): the device that sets it (enrollment,
  a change, a recovery) records its clock's time; merging keeps the later one. Settings > Security
  shows it next to the backup's state, which each device reads from what it already knows: whether a
  key bag is stored, uploads still pending, and which wraps exist (passphrase, passkey,
  organization). The server learns nothing new. When the recovery kit was last made is kept the
  same way (`recovery_key_set_at`, §6.5).
- **The activity feed has no read mark of its own** (issue #534): an entry is read when the read
  position of its target (conversation or thread) is at or after it, and those positions sync
  through the server like any other. The older `activity_read` field is still merged (the later one
  wins) so old devices keep working, but nothing reads it any more. The feed itself is computed
  from decrypted messages on each device (notifications.md).

### 6.5 Recovery kit (issue #738, design for review)

Inventing and remembering a recovery passphrase is unfriendly, and the passphrases people invent
are weak or get lost. Like Apple's recovery key or Passbolt's recovery kit, the client instead
**generates** a high-entropy recovery key and hands it to the user as a file to keep (print it, put
it in a password manager or a safe). Nobody has to invent or remember anything; a further device
is linked from an existing one (§5) and never needs the kit.

Nothing here is a new primitive: random bytes, SHA-256, HKDF-SHA256 and the XChaCha20-Poly1305
wrap of §6 (`teamcraft-crypto::recovery_key`, `teamcraft-crypto::backup`).

#### 6.5.1 Key format

```text
secret    = 16 random bytes from the OS RNG                         (128 bit)
checksum  = SHA-256(labeled("TeamCraft-v1 recovery key checksum", 0x01 ‖ secret))[0..4]
key bytes = secret ‖ checksum                                        (20 bytes = 160 bit)
shown     = Crockford Base32(key bytes): 32 characters, 8 groups of 4 joined with "-"
            e.g. 7K2Q-M9XD-4TRB-0HZE-VWAP-36NC-JF8G-1SYK (made up)
machine   = "tcrecovery1:" ‖ the 32 characters (QR code and kit file)
```

- **128 bits** of entropy: a search of the whole space is out of reach, so the key needs no
  stretching (§6.5.2). The issue asked for at least 128 bits; more would only make the key longer
  to type.
- **Crockford Base32** (alphabet `0123456789ABCDEFGHJKMNPQRSTVWXYZ`, most significant bit first, no
  padding): no `I`, `L`, `O`, `U`, so nothing looks alike in print. Reading is forgiving: case is
  ignored, spaces and hyphens are dropped, `O` reads as `0` and `I`/`L` as `1`. Any other character,
  or anything but 32 characters, is refused.
- **Checksum.** 32 bits that are a function of the secret, so a typo is caught locally (with
  probability 1 − 2⁻³²) and the app says "a character is wrong" instead of "the key doesn't open
  your backup" after a server round trip. The format version (`0x01`) is inside the hash, so a
  future version's keys never pass as version 1. The checksum adds no information to the full key;
  for someone holding *part* of a key it filters guesses of the missing characters offline (the
  wrap on the server still has to confirm a guess), which only matters for leaks of most of a key.
- **Versioning.** The machine form names the version (`tcrecovery1`, like `tclink2:` of §5); the
  shown form is recognized by its length and checksum. A future format gets a new version digit, a
  new checksum input and, if it needs one, a new KDF identifier (§6.5.2).
- The secret lives in a zeroizing type with a redacted `Debug` (`RecoveryKey`); its text forms are
  zeroizing strings.

#### 6.5.2 Wrapping the backup key

A new wrap method next to §6.1–6.3, `WrapMethod::RecoveryKey = 5`, in the existing envelope
(`WrappedBackupKey` version 1):

```text
salt       = 16 random bytes, new for every wrap
KEK        = HKDF-SHA256(salt = salt, IKM = secret, info = "TeamCraft-v1 recovery key backup key wrap")   32 bytes
parameters = TLS { kdf: u8 = 1 (HKDF-SHA256), salt: [u8; 16] }
wrap       = XChaCha20-Poly1305(KEK, random 24-byte nonce, backup key,
                                AAD = labeled("TeamCraft-v1 backup key wrap", TLS { 1, user ID, method 5, parameters }))
```

- **HKDF, no Argon2id.** Stretching makes guessing a low-entropy passphrase expensive; a uniformly
  random 128-bit key can't be guessed, so a key derivation that only separates domains is enough.
  It also keeps recovery instant on phones and in the browser.
- The **salt** makes the KEK differ per wrap, so even two wraps made with the same key share no
  KEK. The user binding, the method and the parameters are authenticated in the AAD, as for every
  wrap: a wrap moved to another user or relabelled as another method fails to open.
- **KDF identifier.** An unknown `kdf` value is refused (`UnsupportedKdf`); a later key format or
  KDF gets a new identifier and old wraps keep opening.
- The wrap list next to the key bag holds at most one recovery-key wrap. Making a new kit replaces
  it, so the old kit stops opening the backup (as far as the server serves the new list, see
  §6.5.9).
- The list's own encoding (a count byte, then each wrap as a 32-bit length and its bytes) moves
  from `teamcraft-core` into `teamcraft-crypto::backup`, unchanged on the wire, so the server can
  read which methods a list holds (§6.5.8).

#### 6.5.3 The kit file

A single self-contained **HTML** file (no scripts, no external resources, one printed A4 page),
built in the client and saved through the browser's or the webview's download. It is never sent
to the server: on the web it is a `Blob` in the page, on the desktop it goes from the webview to the
downloads folder. It contains:

- what it is ("TeamCraft recovery kit"), the account (name, e-mail) and the server address it
  belongs to, and the day it was made, so a user with several accounts picks the right one;
- the key in large monospace groups, and the same key as a QR code (inline SVG of the machine
  form) for phones;
- instructions: keep it offline or in a password manager, don't send it by e-mail or chat; it is
  only asked for when setting up a device without another one at hand; TeamCraft (or IT) never
  asks for it otherwise; if it is lost or seen by others, make a new kit in Settings > Security &
  privacy (the old one stops working).

PDF was considered: a PDF writer in the web bundle costs size for no gain the browser's own
"Print / Save as PDF" of the HTML doesn't already give. The app reads the key back from the file
(§6.5.5) by looking for the machine form, so the file stays plain HTML a person can read.

The file name carries the server's host name, never the key (`teamcraft-recovery-kit-<host>.html`).

#### 6.5.4 First sign-in

1. The client generates the key (`RecoveryKey::generate`, in the UI's WebAssembly: the browser
   build and the desktop webview alike) and shows it with **Download recovery kit**.
2. The user must show they kept it before continuing: type one group of the key, chosen at random
   (the key itself is hidden at that step), or open the saved file. This is about not clicking
   through, not a security boundary.
3. Only then the device enrolls (§3.2) with `EnrollmentSecret::RecoveryKey`: the fresh backup key is
   wrapped with the recovery key (and later the org key, §6.3), and the identity and its first key
   bag go to the server in one request as before. If the enrollment is interrupted after that
   request, the account has an identity, and the next start recovers it with the kit the user
   just confirmed.
4. The device records when the kit was made in the account data (`recovery_key_set_at`, merged
   like `recovery_passphrase_set_at`, §6.4).

The browser and the desktop no longer offer inventing a passphrase at first sign-in. The phone apps
keep doing so until they get the kit (follow-up), and the bridge's bot keeps its configured
passphrase (docs/bridge.md).

#### 6.5.5 Recovery (all devices lost)

The setup screen of an account that has keys offers, besides linking (§5):

- **Open recovery kit…**: the client reads the chosen file locally and finds the machine form in it;
- a field **Recovery key or passphrase**: text that reads as a recovery key (32 characters, valid
  checksum) is used as one, anything else as a legacy passphrase. A passphrase can't be mistaken
  for a key: it would have to be exactly 32 Crockford characters with a matching 32-bit checksum.
- the desktop app, if its keychain still holds this account's backup key: **Restore on this
  computer** (§6.5.6).

The core tries the recovery-key wraps of the server's list with the key, then opens the key bag
as for every recovery (identity check, sequence binding, §6). For apps that only have a
passphrase field (the phone apps until they get the kit), the core treats a "passphrase" that
opens no passphrase wrap but reads as a recovery key as that key, so typing the key works there
too. Scanning the kit's QR code is a phone feature and comes with the phone apps (follow-up);
desktop and web have no camera scanner and take the file or the typed key.

#### 6.5.6 The OS keychain (desktop)

The desktop app keeps the account's **backup key** in the OS keychain through the maintained
`keyring` crate, which it already uses for the database key: macOS Keychain, Windows Credential
Manager, Linux Secret Service (GNOME Keyring, KWallet). This is §6.1's "wrapped by devices": the
keychain is the wrap, nothing is uploaded.

- Entry: service `de.zeitgleich.teamcraft.backup-key`, account `<server URL>|<user ID>`, the same
  scope as the database key; the value is the 32-byte backup key.
- Written when the device is set up (enrolled, linked, recovered) and on every start when it is
  missing or differs (a backup key switched by an org recovery, §6.3.1); written through
  `run_outside_runtime` like the database key (§9).
- Used when this computer must set up the same account again (the local database was lost, or set
  aside as unreadable, §9.5): the core opens the server's key bag with it as with a recovery key
  and checks the identity. A key that no longer opens the bag (the account's backup key changed)
  fails the restore, and the user takes the kit instead.
- **Fallback.** Without a usable keychain (Linux without a Secret Service, a denied or locked
  keychain) nothing is kept and the failure is logged; setting the device up again needs the kit or
  another device. (The database key itself has no fallback yet: the app doesn't open its store
  without a keychain. "Ask once and keep the store key wrapped by the kit" is a follow-up.)
- Not on the web: the browser has no OS keychain (§9.1); the kit is the only way back there.
- Phones keep their database key in the iOS Keychain / Android Keystore already (§9.2); keeping the
  backup key there and the kit screens are part of the phone follow-up.

What it costs: the entry is as strong as the OS account's keychain. Software running as the user
that may read the keychain gets the backup key without the database file (today it needs both the
database key from the keychain and the file). Windows may roam Credential Manager entries with a
domain profile; there the entry reaches other computers of the profile, which is the "known
platform" convenience of the issue, but also what a domain administrator can read. The server's
key bag is still needed to read anything.

#### 6.5.7 Existing users (migration)

- A passphrase wrap keeps working for recovery, Argon2id unchanged, for as long as it exists.
- Settings > Security & privacy offers **Replace passphrase with a recovery kit**: the same kit
  steps as at first sign-in, then one key bag upload with the new wrap list: the new recovery-key
  wrap in, the passphrase wrap and any earlier recovery-key wrap out, the other factors (org,
  passkey) kept. The backup key and the key bag's contents don't change, so the user's other
  devices carry on without noticing (they adopt the new list with the next bag, §6).
- **Make a new recovery kit** (kit lost or seen by others) does the same with a fresh key.
- Changing the passphrase stays possible where the policy allows it (§6.5.8), for users who still
  have one; users with a kit aren't offered a passphrase.
- **Older apps** decode the wrap list strictly and don't know method 5: an app older than this
  change can't change the passphrase of, or recover with a passphrase into, an account whose list
  holds a recovery-key wrap. The web client is always current, desktop apps update themselves, and
  phone apps of the same release decode the new method; they just don't offer the kit yet.

#### 6.5.8 Admin policy

A new group in the owner-signed workspace policy (admin.md §2.3, §6.3):
`recovery.is_passphrase_allowed`, default `true`, so a workspace that never set it keeps today's
behaviour. With `false`, for members (guests' bags are wrapped with a random secret nobody keeps,
bots' passphrase comes from the bridge's configuration; both are exempt):

- **Clients** (from the policy they verified): Settings offers no passphrase, only the kit, and a
  member who still has only a passphrase sees "Replace with a recovery kit" as the next step. The
  core refuses to set a passphrase (`PassphraseNotAllowed`). Existing passphrase wraps keep opening:
  switching the policy never locks anyone out.
- **Server** (sanity check, as for the rest of the policy): it refuses (403
  `recovery_passphrase_not_allowed`) a member's `PUT /me/identity-key` whose first key bag holds a
  passphrase wrap, and a `PUT /me/key-bag` whose list holds a passphrase wrap that is not
  byte-identical to one in the stored list. Devices that upload a bag with the user's unchanged
  passphrase wrap keep working; only new or changed passphrases are refused. The server reads the
  method byte of each list entry and nothing else; a list it can't split under this policy is
  refused as malformed. This stops older apps and today's phone apps, which only know passphrases,
  from setting new ones; the clients' own check rests on the signed policy.
- Until the phone apps have the kit, a member whose only device is a phone can't set up a new
  account with passphrases switched off (linking and recovering with a typed key work). The admin
  console says so next to the switch.

#### 6.5.9 Threat model notes

- **The key never leaves the client unwrapped.** It is made in the client, saved to a local file,
  and on the desktop crosses only the webview–core IPC of the app itself. The server stores the
  wrap list and the encrypted key bag as before; it never sees the key, the KEK or the backup key.
- **Offline attacks by the server** on a recovery-key wrap need 2¹²⁸ guesses; this is what a
  passphrase wrap can't promise (a weak passphrase falls to Argon2id guessing). It is also why the
  policy can switch passphrases off.
- **The kit is the backup's key.** Whoever has the file and the encrypted key bag (the server, or
  someone who can sign in as the user) can read the user's history. Hence the instructions in the
  kit and the confirmation step. The identity provider's sign-in still stands between a stolen kit
  and the key bag for anyone but the server.
- **Replacing a factor doesn't rotate the backup key.** The server could have kept an older wrap
  list. After *Replace passphrase with a recovery kit* such a server can still attack the old
  passphrase wrap offline (Argon2id-stretched), and after *Make a new recovery kit* whoever holds the
  old kit and an old wrap can still unwrap the backup key. Rotating the backup key would stop that,
  but the user's other devices would then have to link again (§6.3.1 does this after an org
  recovery). Offering "new backup key, link your devices again" for a kit that was seen by others is
  a follow-up.
- **Phishing.** The app asks for the key only on the setup screen of a device that has no keys,
  and the kit says so. It is never typed into a page of the server's choosing other than the web
  client itself, whose integrity is §9.3's.
- **Keychain** trade-offs: §6.5.6.
- **Out of scope:** additionally wrapping the backup key with a passkey (WebAuthn PRF, §6.2) for
  recovery without the file; tracked as a follow-up of issue #738.

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
  commits are left for the main app: the NSE opens pushes with `notification_preview_for_push`,
  which never catches a conversation up (§7.2, §9.4).
- App and NSE still share the local database (App Group container + keychain access group). Like
  Threema's `ProcessCoordinator`, a cross-process lock ensures only one process syncs with the server at
  a time; Darwin notifications tell the other process to reload.
- Apple/Google see: that *a* push was sent to *a* device at time T. Nothing else. (Through a push
  gateway, the gateway sees the same, plus an opaque ID of the sending server; §7.1.)
- The server decides *whether* to push using metadata it has anyway: membership, mute settings,
  attention (don't push to the phone while the user looks at a desktop), DND, and the mention list
  (§4.4). Details: notifications.md, "Push routing" and "Mobile push delivery".
- **What this shows the server, beyond content-free metadata it had before:**
  - *Attention:* desktop and web sessions tell the server whether the user looks at the app (focused,
    recent input, not idle, screen unlocked; never which window, what input or any content). They
    report transitions only, and "away" only once it lasted a minute, so short glances away never
    reach the server; the gateway heartbeats keep an attentive report alive as a lease. The server
    learns coarse "at the desktop / not at the desktop" periods, held in memory only, never stored or
    logged. Users turn it off in their notification settings ("Use desktop activity to hold phone
    notifications"), admins for the whole instance (`[notifications] is_desktop_attention_enabled`);
    without it a connected, active desktop holds phone pushes back as presence already did.
    Organisations with works councils or employee-monitoring rules should decide on it.
  - *DND:* the weekly schedule, the allow list of people who break through, whether urgent messages
    do, and the IANA time zone (sent only while the schedule has ranges) are stored on the server, so
    that routing works with every device offline. The time zone says roughly where the user is.
  - *Urgent breaks DND* (opt-in): with the option on, an urgent message reaches the user during DND, by
    default only in DMs and group DMs (`urgent_scope: direct`), so not everybody in a big channel can
    wake them; `everywhere` widens it. People on the allow list break through anyway.
- A forged `dismiss` (the server knows the push key) can only remove notifications and set the badge;
  it never shows anything.
- The push key is chosen when the device first registers with a push service and kept while it
  stays with that service: the apps register their token again on every start and whenever the
  service hands it out again (a UnifiedPush distributor re-sends its endpoint after reconnecting),
  under the same key (`register_push_keeping_key`). A fresh key each time made wake-ups sealed in the
  meantime unreadable, and a registration whose answer was lost left server and device with
  different keys, so nothing could be opened until the next start (issue #826). Sign-out deletes the
  key; the next registration chooses a new one.
- Wake-up kinds: `message`, `mention`, `call`, `dismiss` (a notification was read on another device, or
  a ring ended: remove it; silent, APNs background push / FCM normal priority, never to Web Push) and
  `test` (diagnostics). Besides conversation, message, sender, thread and call IDs the sealed wake-up
  carries the badge count and the send time; all of it is inside the ciphertext. What push services
  and a gateway additionally learn is the push type and priority: that a push was a silent withdrawal
  rather than an alert.
- iOS calls go as mutable alerts on the app's topic, never as VoIP pushes: those need the separate
  PushKit token, and a VoIP push to the alert token is refused as a dead token. Forwarding to CallKit
  from the NSE needs the filtering entitlement below; until it is granted the NSE shows an
  "incoming call" alert instead.
- Keyword notifications ("notify me on 'deploy'") are matched on the client after decryption.
- Browser (implemented): the service worker opens the encrypted local store (§9.1) read-only, opens
  the wake-up with the push key and shows conversation and sender names. The message text appears
  if the store already has the message, or if the app left a still valid access token
  (`PushSession`, in the encrypted store) so the worker can fetch and decrypt it; otherwise the
  notification has names only. Like the NSE, the worker never touches MLS state. The page subscribes
  with the server's VAPID key (`GET /api/v1/push/web-key`), registers the subscription with a fresh push
  key, keeps the access token in the `PushSession` fresh, and tells the worker which database to open
  together with the notification texts in the user's language (`NotificationLabels`: "New message",
  "New message from {name}", "Photo", "Incoming call from {name}", "Mark as read", …), which the
  worker keeps in its cache: it has no translations, and no page may be open when a push arrives.
  These texts are UI strings, not secrets. Title and body follow the native apps' rules
  (`push_text::notification_text`, iOS `NotificationText`); the apps' subtitle (the sender in a
  channel) leads the body, as web notifications have none. A notification never goes out empty:
  when policy, device setting or a failure leaves no names or text, it still says "New message" in
  that language (issue #644). The server sends only the
  sealed wake-up; any "from <site>" line under the notification is the browser naming the app.
  A click on a notification focuses an open window or opens `/?conversation_id=<id>`.
  Browsers rotate or drop subscriptions on their own. The worker handles `pushsubscriptionchange`
  (subscribing again with the old subscription's key where the browser did not) and tells open pages;
  the page that owns the store (§9.4), at every start and on that message, compares
  `pushManager.getSubscription()` with what it registered and registers a changed or new one under
  the same push key (`Client::check_web_push`).
  So that pushes don't go to the old subscription until a page opens, the worker also tells the
  server itself (`POST /api/v1/push/web/subscription-change`, issue #145). It can't use the page's
  credentials (the access token in the `PushSession` has usually expired by then, and no long-lived
  token is kept for it), so the request proves knowledge of the **push key**, which the server holds
  anyway: `HMAC-SHA-256(HKDF(push key, "TeamCraft-v1 push subscription change key"), label ||
  device ID || SHA-256(old subscription) || SHA-256(new subscription))`. The server verifies it in
  constant time and swaps the subscription only if the old one is what it stores (compare-and-swap),
  so a captured request can't be replayed once the subscription moved on, and it can't change the
  provider, the push key or anything else. Every refusal is the same 401, counted by the per-address
  limits like a wrong token. The worker only reads the store (§9.4); the page re-registers as before,
  which is then a no-op on the server. A second rotation before a page opened finds the server
  holding the subscription the worker sent before (409); the worker then retries from the browser's
  `oldSubscription`. Browsers that don't hand one over leave that case to the page.
  *What the push key now also allows:* whoever holds it (the device's store, the server) can point
  the device's Web Push at another endpoint. That redirects wake-ups, which the push key opens anyway
  (conversation, message and sender IDs, never content), and can make the device miss pushes; it
  gives no access to messages, keys or the account.
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

### 7.2 iOS: catching a conversation up for a notification (design, not implemented)

The Android app catches a conversation up itself when a push names a sender the store does not know
(`Client::catch_up_for_push`, §7 above). The iOS notification extension must not do that on its own:
catching up advances MLS state (a welcome spends its key package; a commit moves the epoch), and an
extension that is killed by the system at an arbitrary point, next to an app that may start at any time,
could leave the shared store half-advanced or fork it from the app's copy. So today the extension stops
after fetching and decrypting the one message and shows "New message from <name>" when that fails.
Two options, in order of preference:

1. **The app catches up, the notification is updated.** The extension keeps the sealed wake-up in the
   delivered notification's `userInfo`. When the app next runs (foreground, background refresh, or the
   next push) it takes the cross-process lock, runs the normal sync, finds the pending wake-ups of
   its delivered notifications, reopens them with the core and replaces the notification content with
   the text. No new rights for the extension; the cost is that the text appears when the app runs.
2. **The extension catches up under the lock.** The extension takes the cross-process lock for the whole
   catch-up and the store records an "advance in progress" marker that a started app finishes or rolls
   back before it syncs. Needs a crash-safe store protocol (each step idempotent, the welcome's key
   package spent only after the join is stored), a time budget below the extension's 30 s with
   `serviceExtensionTimeWillExpire` handled, and a memory check against the extension's limit. This
   handles decryption keys in the extension, so it needs a security review before it ships.

Both keep the push content-free and the keys in the shared Keychain access group; neither changes what
the server sees.

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

**Meeting links** (docs/meetings.md, "Links"). The meeting link `<base>/meeting/<id>` is no
credential: meeting IDs are not secret. It lets in members already in the meeting chat; any other
member who opens it knocks and waits in the lobby until a host admits them, also in meetings
without a lobby. Guest links (personal one-time links and the meeting's shared guest link) carry a
token signed by the server in the URL fragment, which browsers never send, so it stays out of server
and proxy logs. A guest link only gets its holder into the lobby; the chat and the call are
encrypted to them only after a host admitted them, like to any member added to a conversation
(guests always from the join on, §4.3).
Since the server signs the tokens and decides admission, it could let in a guest of its own; a host's
device therefore adds a guest only with the proof of the link secret from the link's fragment (§4.6,
"Meeting guests"), and guests stay "Guest (unverified)" for everybody.

**Who signs in the browser.** The web client's call engine is a wasm module of its own
(`teamcraft-web-calls`, loaded by the page; `teamcraft_client_api::page_calls`). It never holds the
device key: the page prepares the join with its client core and keeps a `DeviceCallKeyAuthenticator`
(the core's signer and the conversation's verified devices); the module asks the page through
JavaScript callbacks to sign its fresh DAVE key (step 2) and to verify the others' bindings (step 3),
and gets back only the signed binding or the verified DAVE key. Both run in the same page and origin,
so this adds no party: whoever can run script in the page could use the core's signer anyway (§9.3).
A rejoin prepares again, so the trusted roster follows membership changes; so does a device added
to the conversation during the call (§8.2, "Membership"): the module asks the page's `roster`
callback, and the page refreshes its authenticator with the core's devices.

**The SFU's certificate.** Clients reach the SFU over TLS (QUIC). Normally its certificate chains to
a trusted root. For test and development setups with a short-lived self-signed certificate, the
server can hand out its SHA-256 with the join token (`[calls] sfu_certificate_sha256`, several while
one replaces another): browsers pass them as `serverCertificateHashes`, the native clients accept
the same ones. Every rejoin uses the hashes of its own fresh token, so an SFU restarted with a new
certificate is reached again. A compromised server could point clients at an SFU of its choosing this way, but it can do
that through `sfu_url` already, and the SFU only ever sees DAVE-encrypted media and verified
bindings: the hash decides reachability, not confidentiality.

**Lost connections** (ARCHITECTURE.md §6.5). A QUIC connection that migrates to another network
keeps its DAVE state; nothing changes for the keys. A connection the SFU loses (idle timeout) keeps
the device's call slot for a grace period, but not its DAVE leaf: the SFU proposes removing the leaf
at once, so the others move to an epoch the absent device has no keys for (it may be offline for
good, or the loss may be someone cutting it off). When the device rejoins on a new connection, it
discards its DAVE state and joins like a new member: a fresh P-256 key pair (step 1), a new call key
binding (step 2), verification by the others (step 3) and a welcome. The same happens when a
device's new connection replaces one the SFU still had: the old leaf is removed before the new one is
added. Media from and to the device pauses until the welcome; there is no unencrypted interval. RTP
sequence numbers and timestamps continue across the rejoin – they are unencrypted RTP header fields
the SFU sees anyway, and DAVE's per-frame nonces come from the new epoch's ratchets, so nothing
repeats.

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

### 8.2 Notetaker: a participant that sends call audio out (issue #975)

The notetaker is an AI participant that transcribes a call and posts a summary into the call's
conversation. It **ends end-to-end encryption for the calls it is in, by design**: it can only hear a
call as a member with DAVE keys of its own, and what it hears leaves for a transcription service. The
product decision is to offer it anyway, off by default, and to make every consequence visible. This
section says what it is, what leaves, and what protects participants.

**What it is and where it runs.** `teamcraft-notetaker` is a headless client like the bridge (§13): a
bot account (`POST /api/v1/admin/bots`) with a device of its own, identity key, MLS leaves and
encrypted local store (`teamcraft-core`), and the native call driver (`teamcraft-media`) for calls. It
is a separate process, not part of the server or the SFU, and runs on a host the organization trusts
(next to the bridge, or on a machine of its own). It can run on the server's host, but whoever
controls that host can then read the audio and transcripts the notetaker handles, which is what the
rest of this document keeps the server from. In a call it is an ordinary participant: its device must
be in the conversation's MLS group, its call key binding is verified like everybody's (§8), it gets
the media keys through the normal welcome, and it is in everybody's participant list.

**What leaves, and where to.** The notetaker decrypts each participant's audio frames (the SFU sends
one stream per speaker, so the speaker of every word is known without guessing), decodes them, cuts
each speaker's speech into pieces of at most 30 seconds at pauses, and sends every piece as a 16 kHz
mono WAV over TLS to OpenRouter's transcription endpoint (`POST /api/v1/audio/transcriptions`), which
passes it to the model's provider. At the end the transcript (speaker names, times, text) goes to a
chat model on OpenRouter (`POST /api/v1/chat/completions`) for the summary. Nothing else of the call
leaves: no video, no screen share, no chat. The SFU forwards the three loudest speakers
(ARCHITECTURE.md §6.3), so that is also what the notetaker hears.

- **Audio is not kept.** A piece stays in the notetaker's memory until it is transcribed or given up
  after its retries; it is never written to disk. The transcript so far is kept in the encrypted
  local store, so a restart of the notetaker doesn't lose it, and deleted once the result is posted.
- **Provider data policy.** With `zero_data_retention` (default on) every request carries
  OpenRouter's provider preferences `zdr: true` and `data_collection: "deny"`, so only endpoints that
  keep no data and don't train on it serve it. What OpenRouter and the provider do with the data is
  in the end a contract (admin.md §3.7, data processing).

**Policy (signed).** Everything that decides whether audio leaves and where it goes is a group of the
owner-signed workspace policy (`notetaker`, admin.md §2.3), so a compromised server can neither turn
the notetaker on, nor point it at another model, prompt or account. Clients and the notetaker read it
from the verified policy only.

| Field                 | Default                                 | Meaning                                                                        |
|-----------------------|-----------------------------------------|--------------------------------------------------------------------------------|
| `is_enabled`          | `false`                                 | The notetaker may be asked into calls                                          |
| `bot`                 | none                                    | User ID of the notetaker's bot account; clients show this account as notetaker |
| `inviters`            | `members`                               | Who may ask it in: `members` (not guests) or `admins` (of the verified roster) |
| `can_anyone_stop`     | `true`                                  | Anyone in the conversation, guests included, can send it away                  |
| `transcription_model` | `mistralai/voxtral-mini-transcribe-3-0` | OpenRouter model of the transcription                                          |
| `summary_model`       | `mistralai/mistral-large-2512`          | OpenRouter model of the summary                                                |
| `language`            | `auto`                                  | Language of the transcript and the summary (`auto`, or an ISO 639-1 code)      |
| `prompt`              | built in                                | Instructions for the summary                                                   |
| `zero_data_retention` | `true`                                  | Only zero-data-retention endpoints that don't train on the data                |
| `api_key_sha256`      | none                                    | SHA-256 (hex) of the OpenRouter API key the owner entered                      |

Old owner clients that re-sign the policy without knowing `notetaker` drop the group, which turns the
notetaker off: the failure is the safe one.

**The API key.** An admin enters the OpenRouter key in the console. The server stores it encrypted
with its settings key (admin.md §6.2), never sends it to member clients, and hands it only to the bot
account the policy names, over that account's authenticated connection
(`GET /api/v1/notetaker/credentials`). The owner's client puts the key's SHA-256 into the signed
policy, and the notetaker uses a key only if it matches: a compromised server can't swap in the key of
an account of its own, whose request logs it could read. It can withhold the key, which only stops the
notetaker. The notetaker's configuration may hold the key instead (`openrouter.api_key`); then it
never reaches the server, and its hash in the policy must match all the same.

**Asking it in.** The notetaker never decides on its own to listen. A request is an encrypted control
message in the conversation (`MessageContent::Notetaker`, hidden from the timeline like the bridge's
source announcements, §13.1):

| Request             | Sent when                                               | The notetaker                                                                      |
|---------------------|---------------------------------------------------------|------------------------------------------------------------------------------------|
| `join` `this_call`  | "Add notetaker" in a running call                       | joins the conversation's call running now, or one starting within 10 minutes       |
| `join` `every_call` | a meeting is scheduled with "Notetaker joins the calls" | joins every call of the meeting chat until a `stop`                                |
| `stop`              | "Remove notetaker" in the call                          | leaves the call, transcribes what it heard, posts it; ends a standing `every_call` |

It takes a `join` only from a sender the verified policy allows (`inviters`; the server's account kind
can only take rights away, as for sources), and a `stop` from the member who asked it in, anybody
allowed to ask it in, an admin of the verified roster or, with `can_anyone_stop`, any member of the
conversation. A meeting host's "Remove" (meetings.md, host controls) stops it as well. It also leaves a
call where nobody else has been for a minute, and after `max_call_minutes` (default 240).

**Membership.** The notetaker hears only calls of conversations it is a member of: invited with the
meeting ("Notetaker joins the calls", the first entry of the people to invite), added to a channel
like an integration bot, or added **during the call**. "Add notetaker" in a running call is offered
to whoever the policy lets ask it in, where it is a member or where that user can add it: a channel
of a space where the user manages members (the bot goes in from its join on, into the space as a
guest first where needed, as from the channel's integrations), or a meeting the user organizes (the
bot becomes an invitee, which puts it into the meeting chat's group). A DM takes nobody more, so
there it is offered only where the notetaker is a member. The button first adds the bot through
that normal member-add path, and sends the `join` only once the addition is through: the request
then goes out after the commit that put the bot into the group, in an epoch it can read.

Members only accept DAVE keys of devices that are on their trusted roster (§8, step 4), which they
took from the conversation's group when they prepared their join. A device added since (the
notetaker added during the call, a new channel member) brings a key binding the roster doesn't
know: the call engine then holds that DAVE change, asks the client core for the conversation's
verified devices after catching up its group (`Client::refresh_call_devices`, which processes the
commit that added the device), takes them as the new roster and authenticator, and goes on with
what it held. A device that is still unknown then is rejected as before, so the server can't get a
device of its own choosing in this way: only a member-add commit of the group, which members
verify, puts one on the roster. Desktop, web (through the page's `roster` callback,
`teamcraft_client_api::page_calls`) and the mobile apps' core do this; an app from before this
change ends the call when such a device joins, and the notetaker's join notice asks its people to
join again.

**Everybody sees it.**

- It is a participant: its tile reads "Notetaker – transcribing", and it counts in the participant
  list, on every client, including clients without this feature, which show the bot account under the
  name the admin gave it.
- While it is in the call, the call screen shows a banner that says where the audio goes and who may
  remove it, to everyone who joins later as well, and the pre-join screen shows it when the running
  call has it. The header's encryption badge changes from "End-to-end encrypted" to "Notetaker in the
  call – audio goes to OpenRouter". Clients derive both from the participant list and the verified
  policy, never from what the notetaker or the server says about itself: the server can't hide them
  while the notetaker is in the call, and can't make an ordinary participant look like it.
- When it joins it posts a message into the conversation: that it transcribes, that the audio goes to
  OpenRouter and to which model, and how to remove it. Clients that don't know the feature yet (the
  mobile apps) see that message and the participant.
- The notices bind compliant clients only: anybody in a call can record it by other means (#216).

**The result.** One message in the call's conversation, encrypted for its members like any other: the
summary (short summary, decisions, action items with owners, open questions) as text and the
transcript as a Markdown attachment (§4.5). Everyone in the conversation can read it, which for a
meeting chat is the people of the meeting; retention applies as to any message. Long transcripts are
summarized in parts first, then the parts together.

**What else the notetaker sees.** It is a member device, so it receives the messages of the
conversations it is in, from its join on, and keeps them in its encrypted local store like any client.
Add it to meeting chats and to the channels where it is used, not everywhere.

## 9. Local storage

- Local database: SQLite with SQLCipher; key in the OS keystore (desktop).
- Android: plain SQLite with records sealed like the browser store (§9.1), because SQLCipher needs
  a libcrypto the NDK doesn't ship. The database key is random, wrapped by an AES-GCM key in the
  Android Keystore (hardware-backed where available); see §9.2.
- Decrypted search index (`tantivy`) also stored encrypted.
- Attachments cached encrypted.
- Pictures shown again and again (avatars, space and channel pictures, inline renditions of image
  attachments) are kept in a persistent media cache, sealed (§9.6).

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

**Sign-in tokens across reloads:** the web client keeps its OIDC tokens (access and refresh token)
in a separate IndexedDB database, `teamcraft-session`, sealed with AES-256-GCM under a
non-extractable Web Crypto key in the same way as the database key; nothing token-like is written
to `localStorage` or `sessionStorage`. A reload restores the session with a refresh-token grant
instead of a redirect round trip to the identity provider. The record names the issuer, client and
account (`sub` of the ID token) it belongs to (a provider switch discards it) and is deleted at
sign-out, as soon as the provider refuses the refresh token, and when the server answers 401 (a
back-channel logout ended the session). Tabs refresh one at a time under the Web Lock
`teamcraft-auth-refresh` and take over tokens another tab of the same account got meanwhile, so a
rotated refresh token is never used twice; sign-out deletes under the same lock. Tokens that name
no account are neither saved nor taken over.
The protection is the store's (table above): a copy of the profile or script on the origin can use
the tokens. A server-side session cookie (backend for frontend) was not chosen: it would hand the
refresh token to the server, which the threat model does not trust (§2 of ARCHITECTURE.md), and
needs CSRF protection and server endpoints for a gain the client can have alone.

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

**App lock (Android and iOS, optional).** A biometric or device-passcode gate in front of the app, and
the app switcher shows a cover instead of the chats while it is on. It is a gate in the user interface
only: the database key is protected the same way with or without it (it must open for push wake-ups
while the phone is locked), so it protects against someone holding the unlocked phone, not against
anyone who can read the app's storage. On iOS, while it is on, notifications say only that something
arrived (no sender, no text; calls still ring); Android always shows a generic version on the lock
screen. Switching it off or lengthening its delay asks again (iOS).

### 9.3 Web client integrity (implemented)

The server serves the web client's code, so a compromised server could serve a client that leaks
everything (§9.1). Signing the releases narrows this to what the operator controls:

- **Release manifest.** `scripts/sign-web-release.sh` (run on the machine that holds the release key,
  not on the server) writes `release.json`: every file of the bundle with its SHA-256, and an Ed25519
  signature over `teamcraft-web-release-v1\n` + the canonical JSON of `{version, files}` (sorted keys).
  It also pins the release public key into `sw.js`. The files include the Rust half of the service
  worker (`teamcraft-web-sw_bg.wasm`). The release workflow runs it on the published web bundle when
  the secret `TEAMCRAFT_WEB_RELEASE_KEY` exists (release.md); without it the bundle is unsigned and
  the UI says so.
- **Verifying service worker.** On install and on every update `sw.js` fetches `release.json`,
  checks the signature against the pinned key, fetches every listed file, compares the hashes and only
  then stores the files in a versioned cache and activates. Files may come from the browser's HTTP
  cache or, unchanged, from the previous verified version's cache, so an update downloads only what
  changed; the hash check is the same for every source, and a mismatch is fetched once more from the
  network before the update is dropped. Precompressed responses (`.br`/`.gz` next to the files) arrive
  decoded, so the hashes are those of the files as built; the variants are not part of the manifest.
  The app is served from that cache, never from the network.
- **Parts loaded when first used.** The app is split into wasm chunks that load when their part of
  the app first opens (#334). `release.json` lists them in `lazy` (the wasm files `index.html` and
  `sw.js` don't name, including the signed-in app's), outside the signature so that older workers
  still verify the manifest. The worker installs without them, and fetches a chunk the first time a
  page asks for it, or in the background about 15 s after a page other than a guest's meeting page
  loaded (so guests never download the signed-in app), checks it against the signed hash in the
  version's verified file list and only then stores and serves it. Files only some people use are
  also listed in `on_demand` (the background effects' segmentation worker and model, #439): the
  worker skips them in the background fetch. A server that edits `lazy` or `on_demand` only
  changes when a file is downloaded, never what runs. A chunk that doesn't match its hash isn't
  served, and the pages warn as for a release that doesn't verify. A page of a release whose chunks
  the server no longer has (a deploy since it loaded, before the background fetch) gets an error
  for that part of the app with a retry and a reload, which brings the new release.
- **Failed updates.** An update that does not verify is dropped: the previous verified version keeps
  running and the pages are told, so the UI warns (and shows the key's fingerprint on the About
  screen). A worker the browser stopped restores what it verified from a cache before answering.
- **Updates.** A verified update installs in the background but does not take over open pages: it
  waits until the user reloads (the page offers "a new version is ready") or every tab closed. Each
  page is served from the release it was loaded with (the worker remembers the release per page),
  and a release's cache is deleted only once no page uses it, so a page never loads files of two
  releases. Pages call `registration.update()` when they become visible, come back online and
  hourly, at most every 15 minutes, and ask `GET /api/v1/client-compatibility`; a version the server
  no longer supports asks for the reload too. "Reload now" asks the waiting worker to take over and
  reloads only once it did (`controllerchange`), never under the old worker: the browser activates
  the new worker only after the running one finished its events, so the running worker stops its
  background fetch of lazily loaded files as soon as a newer release waits (#1045). If the switch
  doesn't happen within a minute the banner says so and the button tries again.
- **Call module.** The browser's call engine (`teamcraft-web-calls`, §8) is a separate ES module and
  wasm the page imports after sign-in. Its files are part of the release like any other (listed in
  the manifest, served from the verified cache); the build gives them content-hashed names and writes
  them into `index.html` (`<meta name="teamcraft-calls">`), so a page never imports another build's
  module, also without a service worker.
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

### 9.4 One owner per store, durable writes (implemented)

Every store is loaded into memory, worked on there and written back. Two cores on the same store
would each keep their own copy of the MLS state, the outbox and the key packages; whoever writes last
wins per record. MLS epochs fork (messages become undecryptable) and queued messages go out twice.
So exactly one core owns a device's store at a time:

- **Web.** The tab holding the Web Lock `teamcraft-core-<user>` (`navigator.locks`, taken with
  `ifAvailable` before IndexedDB is opened) owns the store, runs the core and is the only tab that
  shows the app (#744). Other tabs of the same account are inactive: they show "TeamCraft is
  already open in another tab" with "Use here", and their few `core_*` calls (the session check)
  go over the `BroadcastChannel` of the same name to the owner. "Use here" asks the owner over that
  channel to hand over and waits for the lock. The owner refuses while it is in a call (the user can
  then end the call and ask again); otherwise it has the composers store their drafts, waits for
  IndexedDB to hold everything written (the outbox included, at most 5 s), stops its core, which
  releases the lock, and shows "TeamCraft was opened in another tab". Every inactive tab waits for
  the lock, so when the active tab closes, crashes or is discarded one of them takes over by
  itself: the tab that gets the lock holds a roll call on the channel (150 ms; only live tabs
  answer) and keeps it only if it should run the app — the tab where "Use here" was pressed, else
  the one focused or shown last. Otherwise it passes the lock on and queues again after a short
  jitter; after five passes in a row it keeps it. A reload of the active tab finds its own account
  in the tab's session storage, claims the lock like "Use here" and waits up to 3 s for it instead
  of asking. Browsers without Web Locks run one core per tab, as before. The service worker only reads
  the store (push previews, §7) and opens it without upgrading it (§9.5).
- **Desktop.** One running instance on every OS (Tauri's single-instance plugin: a named mutex on
  Windows, D-Bus on Linux, a Unix socket on macOS); a second launch shows the first instance's window
  and exits. Behind that, an OS file lock (`<store>.lock`, `StoreLock`) keeps any second process off
  the store, for example on Linux without a session bus. The OS releases it when the process ends.
- **Mobile.** One app process. The iOS notification service extension opens the same store under a
  cross-process lock and never advances the account's state: it opens a push with
  `notification_preview_for_push`, which shows what the store and the one fetched message give and
  never catches the conversation up (§7.2). It writes only its push bookkeeping (received pushes,
  what it showed) and, on its first open after an update, the store's migrations. Until 1.0.9 its
  core did catch a conversation up when the app wasn't syncing, writing MLS state next to the app's
  copy (issue #424 review). Should another process write MLS state anyway, the app's next MLS save
  sees its commit and writes the whole state (§9.5).

**Writes that fail (web).** IndexedDB refuses writes when the origin's quota is exhausted. A failed
write is kept and issued again on every flush, until it went through; the flush reports the reason
(quota exceeded, or the database closed for a newer version) and the app shows that changes are not
saved on this device. Without that, memory and disk would diverge silently and the next reload would
roll the MLS state back. A newer version of the app that upgrades the database (`versionchange`) gets
it: the old page closes its connection, stops writing and asks for a reload.

**Journal (native).** Both SQLite stores write ahead (`journal_mode = WAL`) with
`synchronous = NORMAL`: a commit appends its pages to the log (encrypted with the database key by
SQLCipher, sealed records on mobile) without syncing the disk; the log is synced and written back
at checkpoints. A commit stays atomic and survives a crash of the app; only a power loss or an OS
crash can take back the last commits, and then whole groups of writes (§9.5), never half of one.
The client repairs that like a lost write: the sync reads again what came after its stored cursors,
the outbox sends again under the same client message ID (the server deduplicates), and an own
commit the server took but the device lost shows the device an epoch behind, which asks to be
added back (§4.6). Until 1.0.11 the desktop store used SQLite's rollback journal with full syncs:
every single write cost about four syncs (create and sync the journal, sync the database, delete
the journal), and a catch-up after a long absence spent minutes waiting for the disk (#972). The
log holds old page versions until it is purged (§4.3.1, "Wiping"); a store whose database is set
aside (§9.5) takes its log with it.

**Eviction (web).** The core asks for persistent storage (`navigator.storage.persist()`) when it
opens the store; browsers grant it by their own rules (installed or often used sites, or the user
agrees). Without it the browser may delete the origin's data under storage pressure, and Safari
deletes it after weeks without a visit. Deleted data can't be restored from the device, but the keys
can: the enrollment screen comes up again, and when this browser had the account set up before (a
marker in local storage), it says that the browser deleted the data and offers the recovery
passphrase or linking from another device. If the browser deleted the marker as well, it looks like a
first visit, which offers the same two ways.

### 9.5 Integrity: groups of writes, layout versions, recovery (implemented)

**Groups of writes.** Records that must agree are written as one group (`LocalStore::begin_writes` /
`end_writes`, `write_group`, `Client::atomically`): after a crash, or when one of the group's writes
fails, the store holds all of them or none. SQLite stores (SQLCipher, sealed SQLite) run a group as
one transaction; IndexedDB never splits a group between two batches, and a batch is one
transaction (§9.4). Groups are synchronous, so none stays open across a network request. Grouped:
a new message and its outbox entry, a sent message and the removal of its outbox entry, the MLS
state after a processed entry, a merged own commit, a joined welcome or a join as a further device
together with the cursor past it, the applied changes and pending commit next to the MLS state, the
key bag and its "upload me" flag, the account with its key bag and the end of a pending enrollment.
If SQLite rolls a group's transaction back by itself (a full disk), the group's later writes are
refused instead of being committed one by one. Where order alone is enough (content keys are written before
the MLS state that consumed the epoch), reprocessing an entry after a crash is idempotent.

**MLS state (issue #424).** The openmls provider storage (every group's tree, context, epoch and
message secrets; key package private keys) is a map from storage key to value; each entry is one
record of the `mls_storage` table (`entry/` and the storage key in base64url). A save writes the
entries that changed since the last save and deletes the ones openmls dropped, as one group of
writes, inside the caller's group where there is one (the cursor past a processed entry, the
applied changes): after a crash the store holds the state before or after the save, never a group
half old, half new (kill-point tests in `store_integrity.rs`). Joining a group or processing a
message writes that group's few changed entries, whatever the number of groups; before, the whole
storage was one record rewritten after every change, which made a catch-up of many new groups
quadratic (0.8 GB written for 300 groups, docs/performance.md). The client remembers what the
store holds (`SavedMlsStorage`) to find the changes without reading the store. The SQLite stores
count what may make the disk differ from what they accepted (`LocalStore::write_tally`): groups
of writes rolled back (a full disk) and commits of another connection to the same file (`PRAGMA
data_version`, another process). Once a count moved, the next save writes every entry again and
deletes every key that may still be on disk, the deletes the disk lost included: the sealed store's
cache keeps a rolled-back group, so it remembers the entries such a group deleted
(`LocalStore::lost_deletes`) until a later group that reached the disk wrote them again. So a second
writer, should there be one, makes the whole state of whoever saves last win, as with the one
record, instead of mixing two epochs of a group per entry. A client loaded again on a store that
already lost writes (the mobile engine reopens it after a panic, on the same store) starts with
such a full save. The browser store never loses an accepted write while the page runs and has one
owner (§9.4). A deleted group leaves no entry, the previous epoch's leaf keys included, which
openmls 0.9 leaves behind after a commit that removed the device.

Versions before 1.0.9 kept the whole storage as one record (`provider`). Opening the store moves it
to entries in one group of writes that puts a marker in the old record's place last: until that
group is on disk the old record is what a start reads. The marker is a length prefix far beyond the
record, so every earlier version (1.0.0 to 1.0.8 read the record the same way) reports the store as
damaged instead of reading no MLS state, which would lose every group and later save a near-empty
record. A store without the marker (set up by this version) gets it when it opens. An old record
next to entries can only come from an older version that wrote after the move: the entries are the
device's groups and stay, the record is set aside (`set_aside/provider-<n>`, never read) and logged.

**Layout versions.** The SQLite stores keep their layout version in `PRAGMA user_version`. Each store
has a list of numbered steps (forward-only, never edited once released); opening runs the missing
ones, each in its own transaction together with setting its version, so a crash or a failed step
leaves the database at the last step that completed. Upkeep that can't run in a transaction (the
one-time wipe of free pages, version 2, issue #391) runs after the earlier steps committed and
counts as done only once it succeeded; a failure (a full disk) is tried again on the next start.
Released 1.0 databases carry version 0 and the layout of version 1. A newer layout is refused
without touching it. Version 3 changes no table but marks the MLS state stored per entry, so
1.0.3 to 1.0.8 refuse the database ("a newer version") before they would read it; 1.0.0 to 1.0.2
check no version and fail on the record's marker instead. The marker never waits for upkeep: a wipe
that failed before it is noted as due (`upkeep_due`), the version moves on, and the wipe runs again
on every open until it succeeds.
IndexedDB versions its object stores itself and refuses a newer database (`VersionError`); version 2
is the same marker. Only a page that owns the store (it holds the Web Lock, §9.4) opens it at its
version and so upgrades it; an older page that still holds the database closes it on
`versionchange` and asks for a reload. The service worker opens it at whatever version it has and
never upgrades it, so a push can't close the store under an older page that goes on running.
Databases
written by every release are kept as fixtures (`crates/teamcraft-core/tests/fixtures/store/`) and
opened by every later version in CI. Records inside the store stay JSON with defaults for new
fields and an `Unknown` catch-all, so newer records are read or skipped, not misread.

**Unreadable data.** A store that is damaged (SQLite's corruption codes; a sealed store whose records
only partly open), whose key no longer opens it (the keystore lost or replaced the key), or whose
layout is newer is not a generic failure: the app says what happened and offers to set the device
up again (recovery passphrase or linking, §5, §6), after a confirmation. The desktop sets the old
file and its journal or log aside (`<user>.db.unreadable-<time>`, never deleted), the mobile apps likewise with their
database and its log (`<account>.teamcraft.db.unreadable-<time>`, issue #414); the browser deletes
the database, because its key wrapping lives in the same database (if another tab doesn't let go
of it within seconds, the reset fails with that reason). Data from a newer version is never reset
in the browser: a newer bundle reads it, so the page offers to reload; an old cached page must not
wipe it. A keystore that is just unavailable (locked, no keyring service) stays a "try again"
failure, since the data is intact.

**Concurrent edits.** Two devices editing the same message converge: the edit with the highest
server ID wins on every device, whatever order the edits arrive in (history loads newest first). An
own edit shows at once and competes with the others once the server numbered it.

### 9.6 Persistent media cache (implemented)

Avatars, space and channel pictures and the inline renditions of image attachments (§4.5) stay on the
device across restarts (issue #749, `teamcraft-core` `media_cache`), so they show at once after a
start and remain visible while the server or its object storage is unavailable.

- **Sealed like the local store, with keys of its own.** HKDF-SHA256 derives a media cache root key
  from the local database key (info `TeamCraft-v1 media cache root key`), and from that root
  `teamcraft-crypto::local_store` derives the lookup and record keys
  (`LocalStoreKeys::derive_media_cache`), so the database key itself is never the HKDF input of
  both the store and the cache. Each entry is sealed (XChaCha20-Poly1305) under the lookup ID of its
  name in the namespace `media_cache`, with that ID as associated data. The name inside
  (`avatar/{user}`, `space_image/{space}`, `channel_image/{channel}`,
  `rendition/{conversation}/{message}/{attachment}/{ciphertext SHA-256}`) is checked again on
  opening, so an entry can't be moved to another picture unnoticed. Neither the pictures nor whose
  they are can be read from the cache: files and records are named by the pseudorandom ID.
  Renditions are end-to-end encrypted content; avatars and space or channel pictures are directory
  data the server holds in plain text (§4.4), but follow the same rule.
- **Renditions are bound to their message.** A cached rendition is only found for the message that
  shows it and the ciphertext hash that message names. A cached picture comes back without
  `fetch_decrypted`'s hash and key check, so a message of someone else that references the same
  attachment ID gets another key, misses, and downloads: the core then checks the file against the
  hash and key of that message.
- **Renditions go with their message.** When a message with pictures is deleted (by its sender or
  locally) or its conversation is no longer listed for the user, the store notes its image
  attachments (`TrackedStore::take_removed_renditions`, bounded) and the host drops their renditions
  from the cache.
- **Desktop:** one sealed file per entry under the app's data directory, a folder per server and
  user beside the encrypted database, with keys derived from the SQLCipher key from the OS keystore.
  The shell opens it together with the store; the webview reads and fills it through the shell,
  which clamps the limits the page passes and refuses pictures larger than an entry. A file larger
  than any entry is deleted without being read.
- **Web:** an IndexedDB database per account (`teamcraft-media-{user}`), apart from the store, with a
  database key of its own wrapped by a non-extractable Web Crypto key exactly like the store's (§9.1).
  Deleting it can't touch the store, and two tabs racing to create its key only lose cached entries.
- **Versions.** An entry holds the version it was made from: a space or channel picture's `version`,
  an avatar's `avatar_version` (absent from older servers: the cached avatar is shown and refreshed
  once per session), or what a rendition was made with. The app shows a cached older version while
  it downloads the one the server names, and keeps showing it when that download fails.
- **Bounded in size and age.** The least recently used entries go once the cache is larger than its
  limit, a setting on the device (200 MB by default, at most 1 GB); one picture is at most 8 MiB. An
  entry is kept at most as long as the age setting says after it was written (30 days by default,
  at most a year), however often it is shown, and entries nobody used for that long are swept with
  the next write.
- **Sign-out deletes it.** Desktop: the caches of every server and user; one that can't be deleted
  now is deleted at the next start (a marker remembers it). Web: the session account's database by
  name, and every `teamcraft-media-*` database the browser lists (`indexedDB.databases()`), also at
  a start while signed out.
- An entry that doesn't open (tampered with, another key) is deleted and counts as missing.
- **Notification pictures (#543).** System notifications show the sender's avatar or the channel's
  or space's picture from this cache, never a rendition. The OS shows them in other processes, so
  they leave the seal there: Linux gets the pixels over D-Bus (`image-data`, no file); Windows and
  macOS get a small PNG (128 px) in the app's cache directory (mode 0600 where the system has modes),
  one per picture on Windows (the toast reads it again from the Action Center) and a copy per
  notification on macOS (the notification center moves it into its own store). These files are
  deleted at sign-out, at start and, on Windows, when the picture is no longer among the last 32
  shown. The browser's service worker reads the cache like the page, with the account's wrapped key,
  and hands the picture to the browser as a `data:` URL. Pictures go only with names: a
  notification the workspace policy or the device setting keeps to "New message" has none
  (notifications.md, "Desktop notifications").
- The mobile apps don't use it: Android keeps avatars in its own file cache and opens pictures from
  the decrypted downloads (§4.5).

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

**Links to messages of the same server** (permalinks `teamcraft:/c/{c}/m/{m}[/t/{t}]` and the app's
own `https://<server>/c/{c}/m/{m}` and `/c/{c}/t/{t}` URLs) get an embedded preview of the linked
message instead. The sender's client embeds nothing but the link. Each **receiving client resolves
it from its own decrypted store**, and only for conversations it is a member of: author, time,
source conversation, the text and the attachment names come from the copy that device already
holds. A viewer who is not a member sees "Message from a conversation you can't see" and learns
nothing, not even whether the message exists; the server is never asked. Edits and deletes show in
the preview as soon as the device has them. Links to other servers stay plain links. The OpenGraph
card of such a URL, if the sender's client made one, is not shown.

## 13. Bots and integrations

E2EE means a bot can only read a channel if it is a member with its own device keys. Bots therefore
run as clients (TeamCraft bot SDK, Rust) on a machine we trust, not on the VPS. Incoming webhooks and
logs are encrypted by `teamcraft-bridge`, which runs next to the source (ARCHITECTURE.md §4.5).

### 13.1 Bridge sources announced in the channel

A workspace admin adds a webhook or log source in the app (docs/bridge.md, "Sources") without
touching the bridge's configuration. The app generates the source token (256 random bits,
`teamcraft-crypto::source_token`), shows the URL once and sends an encrypted control message into
the channel: `MessageContent::IntegrationSource` with a `SourceChange` (`teamcraft-proto`):

| Change    | Carries                                                                                                                                                |
|-----------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| `created` | random source ID, name, kind (`webhook`, `log`), SHA-256 of the token, options (danger attachments alert, alert levels, rate limit, files per message) |
| `renamed` | source ID, new name                                                                                                                                    |
| `revoked` | source ID                                                                                                                                              |

The message is hidden from the timeline; older clients that don't know the kind keep it as
`unknown` (also hidden), and unknown changes or kinds are ignored. The token itself never travels;
the bridge hashes the token of each request and compares it with every known hash in constant time.

**Trust rule.** The server sees ciphertext only and can't write in the channel's group, so it can
neither create nor alter a source. Who may manage sources is decided from signed data only: the
verified workspace roster (admin.md §2.3). Channel roles are signed now (§4.6.1), but they don't
count here yet: the bridge and every member would have to read each channel's role log before
taking in an announcement, which is a follow-up of issue #380. So a server can't promote a channel
member into a source admin. Each member device (the bridge included) keeps a bounded ledger per channel
(`teamcraft-core::integration_sources`):

- **Taking in.** A `created` or `renamed` reaches the ledger only from an admin of the verified
  roster, fresh at the message's server time; a `revoked` only from such an admin or from the
  source's creator. Changes from anybody else are dropped before the ledger, so a member (or a
  guest) can't fill its 1024 entries and crowd out later sources.
- **In force.** A source counts only if its creator is, at that moment, an admin of the fresh
  roster and a member of the channel's MLS group (the bridge checks its own group), and not a guest
  or bot (the server's account kind can only take rights away). At most 50 per channel, the oldest.
  A creator who loses the role or leaves the channel takes their sources out of force. A source
  always posts into the channel whose group announced it, never elsewhere.
- A revocation is final: a revoked source never comes back, whatever arrives later or in which
  order. A rename's newest by server order wins. Of two announcements with the same source ID the
  first the server numbered wins.

**What the server can still do.** It can withhold or drop messages, as for any content: hold back
an announcement (the source doesn't work), or, for a bridge whose database is new, leave a
`revoked` out of the history it serves, so that bridge serves a revoked source again. A bridge that
once read a revocation keeps it in its store; the server can't take it back. Revoke and add a new
source when in doubt, and keep the bridge's database volume (docs/bridge.md, "Container").

The bridge reads the ledgers from its encrypted store once a second and looks back once through older history (bounded) when its database is
new. A bot added after a source was announced can't read the announcement (history from its join
only), so the app offers "Add source" only once a bot is in the channel.

**Files of webhooks** (docs/bridge.md, "Files") are attachments of the bridge's message, encrypted
and uploaded exactly like a client's (§4.5, `teamcraft-core` `direct_uploads`): a fresh key per
file, the key and hash only inside the message. The bridge sends all of a message's files and completes
their uploads only then, all together, before it sends the message: a failed file, or a producer
that hangs up, gives every upload of the message up, so there is neither a message whose files are
missing nor a file without its message (only a failure while completing, a few requests at the end,
can leave completed ciphertext whose key nobody has, until retention removes it). Before their
upload the files wait as plaintext in the bridge's spool directory, on the bridge's own host,
readable by its user only (`0700`, checked at start) and deleted once the request is answered. The
bridge decodes pictures for their thumbnails itself, off its loop and within a pixel limit
(`ClientSettings::makes_thumbnails` off for the core).

## 14. Open questions

- Key transparency: when?
- Segment size and mobile prefetch defaults (per month? size cap on phones?)
