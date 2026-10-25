# Security review

Focused security review of `main` at commit `9667ef3`, against the threat model in
ARCHITECTURE.md §2 and docs/e2ee.md: the VPS server and S3 are untrusted, the OIDC provider runs
elsewhere and attests identity keys. Read-only review: no code was changed. Every finding was checked
against the cited lines; findings that rest on inference rather than a traced code path are marked
**uncertain**. No exploit was run against a live deployment.

Finding IDs are the part letter plus the number inside that part (A1 = part A, finding 1).

| Part | Scope                                                                                                  |
|------|--------------------------------------------------------------------------------------------------------|
| A    | Trust anchors, identity pinning, signed membership/roster/policy, content-key rotation, history sharing |
| B    | Device-linking rendezvous, key bag and backup wrapping, org recovery, push crypto and service worker    |
| C    | Browser store and web hardening, desktop shell, DAVE call key bindings, SFU auth and parsers           |
| D    | Server authz (guests, admin API, internal endpoints), rate limits and DoS, logs, zeroization, SQL      |
| E    | Supply chain: vendored `davey`, `deny.toml` ignores                                                    |

## Top findings

| ID  | Severity      | Finding                                                                                                     | Location                                                                         |
|-----|---------------|-------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------|
| A1  | High          | OIDC attestation of identity keys is never verified (`TODO(attestation)`); identity keys are TOFU from the server | crates/teamcraft-core/src/client/mod.rs:257                                      |
| B1  | High          | Linked-device enrollment ignores the transferred secrets on a server 404 and uploads a new identity wrapped under an empty / fixed passphrase | crates/teamcraft-core/src/client/enroll.rs:86-99,154-157; apps/desktop/src/core/runtime.rs:594; crates/teamcraft-ffi/src/engine.rs:43,218 |
| A3  | High          | Old signed membership changes can be replayed (no epoch/time binding; replay set per device) to re-add a removed user | crates/teamcraft-crypto/src/mls/group.rs:117,326-333,504-506; crates/teamcraft-crypto/src/membership.rs:69-74 |
| A4  | High          | Device revocation not cryptographically enforced: devices of existing members are added without a signed change, certificates never expire | crates/teamcraft-crypto/src/mls/group.rs:219-239,531-543; crates/teamcraft-crypto/src/identity.rs:59-66 |
| A5  | High          | Content-key rotation after removal is a separate, suppressible message, and a full-history bundle can roll the current key back | crates/teamcraft-core/src/client/groups.rs:401-421,483-512; crates/teamcraft-crypto/src/mls/content_keys.rs:54-63,80-87 |
| A2  | High          | Identity pins and the workspace root of trust are per device; every new device/reinstall re-trusts the server, incl. a forged genesis roster | crates/teamcraft-core/src/records.rs:272-284; crates/teamcraft-core/src/workspace.rs:157-175 |
| C1  | High          | Web client bundle is served (no-cache) by the untrusted server; a compromised VPS can ship a key-stealing bundle | crates/teamcraft-server/src/web.rs:35-58; docs/e2ee.md:396                       |
| B3  | High (design) | Org recovery seals the recovered backup key to an unauthenticated request key the server can swap      | crates/teamcraft-core/src/client/org_recovery.rs:132-158                         |
| B2  | High (latent) | 4-digit rendezvous short code does not protect against a relay that knows AK (low-order points, no commitment); only the planned approval-prompt flow is affected | crates/teamcraft-crypto/src/rendezvous.rs:35,128-133,200-205,244-265             |
| C3  | Medium        | Desktop: `withGlobalTauri` lets any webview script approve device linking (short-code check only in UI) | apps/desktop/tauri.conf.json:19; apps/desktop/src/core/commands.rs:139-146       |
| C2  | Medium        | XSS can decrypt the wrapped database key with the non-extractable key handle; docs claim otherwise       | crates/teamcraft-core/src/store/browser/browser_key.rs:5-6,89-108                |
| C4  | Medium        | Call key bindings expire after 600 s but are relayed to late joiners, who reject them and drop out (uncertain, inferred) | crates/teamcraft-media/src/call_auth.rs:22; crates/teamcraft-crypto/src/call_binding.rs:152 |
| A6  | Medium        | Roster/policy logs have no freshness or fork detection; server can withhold demotions and policy changes | crates/teamcraft-core/src/workspace.rs:120-198                                   |
| A9  | Medium        | History-share level and guest status come from unsigned server data                                    | crates/teamcraft-core/src/client/groups.rs:117-129,610-629                       |
| A10 | Medium        | Message sender authenticity rests on the shared content key only; members can forge each other         | crates/teamcraft-crypto/src/message.rs:89-103                                    |
| D1  | Medium        | Web Push SSRF: endpoint host is checked textually, never resolved (same as B7)                         | crates/teamcraft-server/src/push/web_push.rs:40-73                               |
| D2  | Medium        | `ManageUsers` admins can deactivate owners (the only roster/policy signers)                            | crates/teamcraft-server/src/services/admin.rs:149-176                            |

The full lists, including the further Medium, Low and Info findings and the "done correctly" notes,
are in parts A to E below.

**Fix status (branch `fix/security-crypto-core`):** A1–A10, A12, B1–B6, B10 and B11 are fixed or
mitigated; each finding below has a status line naming the commit and what remains open.

### Suggested order of fixes

1. **Close the server-injection paths before anything else** (A1, A2, A4, B1): verify the attestation
   against a pinned IdP JWKS; sync pins and the workspace root through the key bag and show the root
   fingerprint; add a signed, versioned device list per user and require it for device adds; make
   linked enrollment fail closed when the server claims there is no identity.
2. **Make removal really revoke** (A3, A5, A10): bind signed membership changes to group ID + epoch;
   rotate the content key inside the removal commit (or derive it from the MLS exporter) and refuse
   bundles that move `current` backwards; add per-sender signatures or MLS application messages.
3. **Recovery and linking hardening** (B2, B3, B4, B5, C3): authenticate recovery request keys against the
   user's pinned identity, force backup-key and org-key rotation after recovery, use a SAS with
   commitment (or a longer code) and reject low-order points before shipping the approval-prompt flow,
   move the linking confirmation behind a Rust-side check.
4. **Document and mitigate the web-delivery trust gap** (C1, C2): state it in ARCHITECTURE.md §2 and
   e2ee.md §9.1, consider a separately hosted / signed bundle (SRI-pinned loader, desktop as the
   recommended client).
5. Server hardening (D1–D16) and DAVE availability (C4–C6).


**Fix status (branch `fix/security-server`):** D1–D16, B7, C7, C8, C10 are fixed (D13 mitigated), C9 is mitigated, and the server side of A2, A4 and A8 is in place. Not fixed here: C6 (needs the DAVE flow rework), the client-side findings (C1–C5, C11–C13, A-series) and the Keycloak realm audience split (A15). Each finding below has a status line with its commit.

## Part A: trust anchors, membership and key rotation

Scope: `teamcraft-core` (trust.rs, attestation.rs, workspace.rs, client/{mod,enroll,groups,workspace,sync,gateway,messaging,linking,org_recovery}.rs) and `teamcraft-crypto` (identity.rs, membership.rs, workspace.rs, message.rs, content_key.rs, mls/*). Threat model: the VPS server is malicious; Keycloak (separate host) is the second trust anchor.

All paths are relative to the worktree root. Line numbers were checked against the current tree (commit 9667ef3).

### Summary

| #   | Severity        | Title                                                                                           | Location                                                                                  |
|-----|-----------------|-------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------|
| 1   | High            | OIDC attestation is never checked: identity keys are pure TOFU from the server                  | core/src/client/mod.rs:249-277                                                            |
| 2   | High            | Pins and the workspace root are per device: every new device or reinstall is a fresh TOFU window | core/src/records.rs:272-284, core/src/client/workspace.rs:68-70, core/src/workspace.rs:157-175 |
| 3   | High            | Replay of old signed membership changes through devices that joined later (removed user re-added with full history) | crypto/src/mls/group.rs:31,117,326-333,504-506; core/src/client/groups.rs:251-258   |
| 4   | High            | Device revocation is not enforced by cryptography: a removed device can be added back without a signed change | crypto/src/mls/group.rs:219-239,531-543; core/src/client/workspace.rs:248-289; core/src/client/groups.rs:157-203 |
| 5   | High            | Content key rotation after a removal relies on a separate message the server can suppress, and later bundles can roll it back | core/src/client/groups.rs:401-421,483-512; crypto/src/mls/content_keys.rs:54-63,80-87 |
| 6   | Medium          | Roster and policy logs have no freshness and no fork detection (server can withhold demotions and policy changes) | core/src/workspace.rs:120-198, crypto/src/workspace.rs:34-39                              |
| 7   | Medium          | The server sets the order of entries across the roster and policy logs                          | core/src/workspace.rs:122-128,194                                                         |
| 8   | Medium          | The server can make honest clients sign removals of anyone                                      | core/src/client/gateway.rs:163-178,187; core/src/client/workspace.rs:337-356              |
| 9   | Medium          | History share level and guest status come from the server, not from signed data                  | core/src/client/groups.rs:117-129,610-629                                                 |
| 10  | Medium          | Message sender authenticity rests only on a shared symmetric key, and old keys never expire      | crypto/src/message.rs:89-103,130-140                                                      |
| 11  | Low             | Any MLS group member may add or remove anyone (no per-channel roles yet)                        | core/src/trust.rs:78-80                                                                   |
| 12  | Low             | Org recovery shares are sealed to device keys whose device ID is not bound                      | core/src/client/org_recovery.rs:168-191; core/src/client/enroll.rs:342-357               |
| 13  | Low             | Server-controlled DoS levers (epoch metadata, self-removal event, 404 at enrollment)            | core/src/client/groups.rs:276-282; gateway.rs:166-173; enroll.rs:86-100                   |
| 14  | Info            | Time- and count-based content key rotation is not implemented                                    | crypto/src/content_key.rs:17-59 (RotationPolicy unused in core)                           |
| 15  | Info, uncertain | Attestation design: the claim is user-writable and the sub to UserId mapping comes from the server | core/src/attestation.rs:1-79; docs/e2ee.md §3.1                                       |

### Findings

#### 1. High: OIDC attestation is never checked, so identity keys are pure TOFU from the server

**Status: fixed** in `80cb2df` (`fix/security-crypto-core`). `teamcraft-crypto::attestation` verifies ES256/EdDSA ID tokens against the issuer's JWKS (fetched from the issuer's own discovery document, never through the server), checks issuer and audience, and requires the claim `"<user id>:<fingerprint>"`. Configured with `ClientSettings::maybe_attestation`; with `is_required` unattested keys aren't pinned, otherwise they are pinned as not attested. Open: RS256 realms (no RSA implementation), and showing attested vs. TOFU pins in the UI.

**Location:** `crates/teamcraft-core/src/client/mod.rs:249-277`. The TODO at lines 257-258 reads `// TODO(attestation): verify response.maybe_attestation ... before pinning`.

**What happens.** `pin_identities` takes `response.public_key` from `GET identity_key(user)` and pins it the first time it sees the user (lines 259-263). `maybe_attestation` is ignored. Nothing else in the code verifies an ID token: no JWKS fetch and no check of the `tc_identity_key` claim. `attestation.rs` only *writes* the Keycloak attribute. Every caller of enrollment in the repo also passes `maybe_attestation: None` (ui_backend.rs:607, ffi/engine.rs:537, bridge).

**Exploit.** The first time Alice's client needs Bob (when it opens a DM, adds Bob, processes a commit or joins from a welcome), a malicious server returns a key it controls as Bob's identity key, with fake device certificates. Alice's client pins that key, accepts the server's key packages for "Bob", adds the device to the group and sends it content keys. For `AddUser` with `HistoryShare::Full` that is every past key (groups.rs:403-411). No warning is shown, because this is the first sighting. docs/e2ee.md §3.1 promises that an attacker would need to break into both the VPS and Keycloak. The code does not deliver that.

**Fix.** Verify the ID token before pinning: fetch JWKS directly from the configured issuer (pinned issuer URL, not taken from the server), check `iss`/`aud`/signature, and check that `tc_identity_key == fingerprint(public_key)`. Ignore `exp`, or use a dedicated long-lived attestation. Bind the token's `sub` to the TeamCraft user (see #15). Record whether a pin was attested or TOFU, and show TOFU pins in the UI.

#### 2. High: pins and the workspace root are per device, so every new device or reinstall is a fresh TOFU window

**Status: fixed** in `a85a688`. Pins and the workspace root are account data in the encrypted key bag; linked and recovered devices start with them, `sync_workspace` seeds the root (a forged genesis fails with `RootChanged`), `workspace_root_fingerprint` exposes it. Open: requiring the genesis owner's key to be attested, and the UI for comparing the root fingerprint.

**Server status:** `05f3e57` (`fix/security-server`). Server side of the fix: a `KeyBagUpdate` gateway event tells a user's other devices to fetch a changed key bag (which carries the pins and the workspace root).

**Location:** pinned identities live in the local store, `crates/teamcraft-core/src/records.rs:272-284` (`Table::PinnedIdentities`). The workspace state, including `maybe_root`, also lives in the local store: `client/workspace.rs:68-70`, `Table::Workspace`. The key bag (`crypto/src/key_bag.rs`) holds only the identity key and content key rings. Genesis acceptance is at `crates/teamcraft-core/src/workspace.rs:157-175`: the genesis roster vouches for itself (`owner_directory(&document)`), and `RootChanged` is only checked against a root this same device pinned earlier.

**Exploit.**
- A user's new laptop arrives linked through §5, so it holds the full key bag and gets added to every group. It starts with no pins and no workspace root.
- The server shows it a fabricated genesis roster in which a server-controlled "owner" or "admin" has a server-chosen identity key. It also serves substituted identity keys for any colleague.
- That device then does the following:
  - It commits server-forged `Delegated` admin changes, such as "add ghost user", to its groups. The signer is an admin in its fake roster (trust.rs:81-91). It then ships full-history bundles to the ghost.
  - Through `add_missing_devices` (groups.rs:157-203) it adds fake devices of real colleagues. Device additions need no signed change.

Honest members reject these commits, and the group forks. The forked device and the attacker still get all content keys from the device's key bag and its later bundles. The root of trust is never shown to the user: `maybe_root` is not read anywhere outside workspace.rs. That contradicts admin.md §2.3 ("shown once, can be verified").

**Fix.** Sync pins and the workspace root across a user's devices by storing them in the key bag, or in a signed per-user "trusted contacts" blob. Linking (§5) should transfer them. Show the root fingerprint and let the user verify it. With #1 fixed, require that the genesis owner's key is OIDC-attested.

#### 3. High: replay of old signed membership changes through devices that joined later

**Status: fixed** in `13df103`. Signed changes (version 2) carry the group epoch; committers and receivers accept them only up to 32 epochs old, not older than the device's join, and not older than a committed change about the same user (history persisted next to the MLS state).

**Location:**
- `crates/teamcraft-crypto/src/mls/group.rs:31` (`applied_changes`), `:117` and `:326-333`. `join` builds the group via `from_group`, which starts with an empty `BTreeSet`.
- The replay check at `:504-506` is purely local.
- `crates/teamcraft-crypto/src/membership.rs:69-74`: a `MembershipChange` has a random ID, a conversation and an action. Nothing binds it to an epoch, a time or a membership state.
- The server feeds changes in at `crates/teamcraft-core/src/client/groups.rs:251-258`: any `Proposal` entry with the `TCAC` prefix is queued as `Delegated`. They are committed at `client/workspace.rs:328-336` and `groups.rs:370-399`.

**Exploit.**
1. Earlier, Alice (a member) added Mallory. The signed change travels in the commit's authenticated data, which is plaintext even in a PrivateMessage, so the server has it.
2. Bob removes Mallory.
3. The server reposts Alice's old signed `AddUser{Mallory, history: Full}` as a `TCAC` proposal.
   - Members that saw the original commit drop it as `ReplayedChange`.
   - A device whose group came from a welcome after that commit has an empty `applied_changes` set. That covers any newer member or new device. This device verifies the change: Alice is still a member, so `WorkspaceAuthority` allows it. It claims Mallory's key packages, which the server serves, and commits.
4. After the commit is posted, `ContentKeyBundle::for_new_member(.., Full)` sends Mallory every content key, including keys rotated after her removal.

Old members reject the commit and stall in their epoch. The new members and Mallory move forward. A replayed `RemoveUser` can also be used to kick people.

**Fix.** Bind a change to group state. Include the epoch (or the group's tree hash or confirmed transcript hash) in the signed change and reject it unless it equals the current epoch. Alternatively, put an expiry or timestamp in it and require it to be fresh. Also carry the applied-change set, or a hash chain of it, in a GroupContext extension so that joiners inherit it. At a minimum, refuse an `AddUser` for a user whose most recent change in the group was a removal signed after it.

#### 4. High: device revocation is not enforced by cryptography

**Status: fixed** in `153d020` (client side). Signed per-user device lists (identity key, monotonic sequence, revoked set) gate device additions, commits adding devices and org recovery sealing; groups never re-admit a device removed by a signed `RemoveDevice`; `Client::revoke_device` revokes, republishes the list and signs the removals. Needs the server endpoints `GET /users/{id}/device-list` and `PUT /me/device-list` (core README, server gaps); until then clients fall back to certificates. Open: certificate validity periods.

**Server status:** `54bc175` (`fix/security-server`). Server side of the fix: `GET /api/v1/users/{user_id}/device-list` and `PUT /api/v1/me/device-list` store the signed list as an opaque blob with a compare-and-swap on a sequence number that only goes up.

**Location:**
- `DeviceCertificate` (`crypto/src/identity.rs:59-66`) has no expiry, no serial and no revocation list.
- Unsigned device additions are accepted for any device whose certificate verifies, if its user is already a member: `crypto/src/mls/group.rs:219-239` (commit) and `:531-543` (accept).
- Clients add devices because the server says so: `gateway.rs:195` → `client/workspace.rs:248-289` (`DeviceAdd` event) → `add_missing_devices` in `client/groups.rs:157-203`. That function uses the server's `list_user_devices` and `claim_key_packages`.

**Exploit.**
1. A user loses a phone, and the device is removed through a signed `RemoveDevice` plus a key rotation.
2. The thief holds the phone's device keys and still-valid uploaded key packages, including the reusable last-resort package. The thief works with the server, or is the server.
3. The server emits `DeviceAdd{user, lost_device}` and serves the old key package.
4. Every online member auto-commits the addition with no signed change. Other members accept the commit, because the certificate is still signed by the user's identity key.
5. The lost device gets every later content key rotation.

`verified_agreement_keys` in org_recovery.rs:168-191 has the same problem: shares can be sealed to revoked devices.

**Fix.** Add a signed, versioned per-user device list: the identity key signs the set of active device keys. Require that a device is in the latest list before it is added or used for sealing. Keep removed devices in a signed revocation set so they cannot come back. Give certificates a validity period. Check the device list for rollback with a monotonic version, as in #6.

#### 5. High: content key rotation after a removal can be suppressed or rolled back

**Status: fixed** in `f4099d8`. After a removal (and a from-join addition) every member derives the epoch's content key from the MLS exporter at merge; no separate message to suppress. Bundles only set a current key on rings without one, so nothing rolls back. Residual: a member that never receives the removal commit stays in the old epoch.

**Location:**
- `crates/teamcraft-core/src/client/groups.rs:401-421`: the removal commit is posted and confirmed first. Only after that does `send_bundle` send the new key as a separate MLS application message.
- `:483-512`: after `max_commit_attempts` epoch conflicts it returns `GaveUp`. The committer has already switched to the new key locally, through `ContentKeyBundle::rotate`.
- Receivers of a removal commit (`groups.rs:299-304`) do not rotate. They do not block sending until a fresh key arrives, and they do not detect a missing rotation.
- `crypto/src/mls/content_keys.rs:54-63`: a `Full` bundle for a new member re-asserts the *sender's* current key as `maybe_current`. `apply_to` (`:80-87`) switches every receiver's current key to it without checking whether it is older.

**Exploit.**
- **Suppression.** After Bob removes Mallory, the server answers Bob's bundle post with `epoch_conflict` repeatedly, or drops it. Every other member keeps encrypting with the old current key, which Mallory holds. With the server's help, Mallory reads all new messages except Bob's.
- **Rollback.** The server delays Bob's rotation bundle to Carol. Carol adds a new member with `Full` history. Her bundle marks the old key as current, and every receiver switches back to the key Mallory knows.

The design doc (§4.1: "The removed person ... sees nothing new") is not met against a malicious server.

**Fix.**
- Derive the post-removal content key from the MLS epoch, for example with `export_secret("teamcraft content key", epoch)`, or put the new key in the same commit through a SafeExportSecret or a GroupContext extension. Every member then has it the moment it merges the commit.
- Failing that, refuse to send with a key that is older than the latest removal commit this device merged.
- Never move `current` back to a key that was already superseded. Track an epoch or sequence per key and only accept a newer current.

#### 6. Medium: roster and policy logs have no freshness and no fork detection

**Status: fixed** in `8391fca`. Documents name the previous entry's hash and their signing time; owners re-sign the roster when half of `workspace_max_age` (default 30 days) has passed, and a stale roster grants no admin authority; members gossip their verified heads in encrypted messages, and a fork or withheld entries set an alarm.

**Location:** `crates/teamcraft-core/src/workspace.rs:120-198`, `client/workspace.rs:79-93`. The signed input is `{log, version, payload}` (`crypto/src/workspace.rs:34-39`): it has no previous-entry hash and no timestamp.

**Exploit.**
- **Withholding.** An owner demotes admin Mallory in roster v5, or sets `admin_self_join = disabled`. The server never serves v5, or serves a garbage v5 that is rejected. Per `apply_entries`, the chain then stops for good and the last state stays in force. Clients keep treating Mallory as admin. Mallory adds herself to any private channel (trust.rs:84-89, with the default `Visible`) and receives full history keys.
- **Split view.** Owners A and B both sign v6, or an ex-owner colludes. The server can show different v6 entries to different clients. There is no hash chain, and clients never compare heads.

Rollback below an applied version is prevented (lines 145-148 and 182-185). That part is done correctly.

**Fix.** Include `prev_hash` in the signed input. Have owners re-sign the head regularly (a signed timestamp), and have clients refuse admin authority when the head is older than N days. Gossip the head hash inside MLS groups, for example as a GroupContext extension or in content key bundles, so that forks are detected. Treat a rejected entry as an alarm surfaced to the user, not only a log line.

#### 7. Medium: the server sets the order across the roster and policy logs

**Status: fixed** in `8391fca`. Rosters are applied before policies by version; a policy names its roster version, is verified against that roster's owners and may not name an older one than the previous policy; owners re-sign the policy after each roster change.

**Location:** `crates/teamcraft-core/src/workspace.rs:122-128` sorts by the server-supplied `created_at`. A policy entry is verified against whichever roster is current at that point (`:194`). Policy entries are not bound to a roster version.

**Exploit.** Owner A is removed in roster v3. Later, A (an insider working with the server) signs policy v(n+1). The server back-dates its `created_at` to before roster v3. Any client that syncs from scratch, or has not applied v3 yet, accepts A's policy. Examples: flipping `admin_self_join` to `visible`, or changing `history_default`.

**Fix.** Put the roster version, or the roster head hash, into the signed policy document. Verify it against exactly that roster version, and require the roster version to be the latest.

#### 8. Medium: the server can make honest clients sign removals of anyone

**Status: fixed** in `153d020`. Server-reported removals are hints only. Removals are signed by the remover, by the user's own device that revokes (`revoke_device`) or by an admin (`admin_remove_member`, a delegated change). Server gap: deactivation needs the admin client to remove the user per conversation.

**Server status:** `05f3e57` (`fix/security-server`). Server side of the fix: `GET /api/v1/admin/conversations?user_id=` lists the channels a user is in, so the admin client can sign the removals itself.

**Location:** `gateway.rs:163-178` (`MembershipRemove`) → `queue_user_removal`, and `gateway.rs:187` (`DeviceRemove`) → `queue_device_removal`. Both are committed as `ChangeSource::Own` in `client/workspace.rs:337-356`. The client signs `RemoveUser`/`RemoveDevice` with its own device key because the server said so.

**Exploit.** The server sends a `MembershipRemove{conv, victim}` event to one online member. That client creates a validly signed "Alice removed victim" and commits it. Every member accepts it, because Alice is a member (trust.rs:78-80). Membership integrity and the audit attribution are both forged. This does not directly break confidentiality, but it gives the server unilateral kick power. It can also clear a target's legitimate devices before a fake-device attack.

**Fix.** Only commit removals the user started on this device, or removals that come with a signed admin change (the `Delegated` path). Treat server-reported removals as hints: hide the conversation in the UI, but don't sign on the server's behalf.

#### 9. Medium: history share level and guest status come from the server

**Status: fixed (interim)** in `c335ada` and `f4099d8`. Without an explicit choice the signed workspace default applies, and full history only for users the signed roster lists; the choice is settled before signing, and an admin's full-history add of a guest isn't committed. Open: signed per-channel history settings and a signed guest list.

**Location:** `crates/teamcraft-core/src/client/groups.rs:117-129`. `history_policy` comes from the server's `ConversationResponse` (not signed); only `Unknown` falls back to the signed workspace default. At `:610-629`, guest status (`UserKind::Guest`) is taken from the server's `UserSummary`.

**Exploit.** For a channel configured as `FromJoin`, the server reports `Full`, or reports a guest as a regular user. When a member adds someone legitimately, the client sends the newcomer every historical content key. This breaks e2ee.md §4.3 ("Guests only ever see history from their join on").

**Fix.** Make per-channel history policy and guest status signed data: owner or admin signed, like the roster. Until then, default to `FromJoin` unless the signed workspace policy says `Full`. Treat users the client can't prove to be members as guests.

#### 10. Medium: message sender authenticity rests only on a shared symmetric key

**Status: fixed** in `12166d6`. Messages (version 2) are signed by the sender's device key and checked against verified device keys; devices can't add messages dated after they left a conversation; unsigned version 1 messages only with `accept_unsigned_messages`.

**Location:** `crates/teamcraft-crypto/src/message.rs:89-103` and `:130-140`. The sender user and device are only AEAD-bound to the shared content key. `decrypt_message` accepts any key in the ring, however old (`:98-100`).

**Exploit.** Any current member, or a removed member holding old keys with the server delivering, can make up messages, edits and deletes in another member's name. The `apply_control` author check (messaging.rs:299) uses the same unauthenticated `sender_user_id`. A removed member can keep injecting messages under a pre-removal key, attributed to anyone, indefinitely. The doc comment at message.rs:3-6 claims protection against the server only, which is accurate. Against members it is not.

**Fix.** Sign each message with the sender's device key (Ed25519 over the AAD and ciphertext), and verify it against the MLS-verified member credential. Reject messages under a key superseded before the message's server timestamp, plus a grace window, or messages from senders who weren't members when that key was current.

#### 11. Low: any MLS group member may add or remove anyone

**Location:** `crates/teamcraft-core/src/trust.rs:78-80`. This is documented at lines 23-24 as a gap: "signed per-channel roles aren't in the roster yet". Any single member can add arbitrary users with full history, or remove anyone, which amplifies #3 and #8. It matches the stated interim design. Track it so it is closed before release.

#### 12. Low: org recovery shares are sealed to device keys whose device ID is not bound

**Status: fixed** in `153d020`. Shares go only to devices the signed device list names with that key.

**Location:** `crates/teamcraft-core/src/client/org_recovery.rs:168-191`. Registration certificates carry `PROVISIONAL_DEVICE_ID = 0` (`enroll.rs:28-30,342-357`). The server's `device.id` is not bound to the certificate, so the server can relabel devices or present revoked ones. The share is still sealed only to a key the identity signed, so the server itself cannot open it. With #4, though, a revoked or stolen device can receive admin shares.

**Fix.** Use the signed device list from #4, and seal only to devices in the latest list.

#### 13. Low: server-controlled DoS levers

- `groups.rs:276-282`: the outer `message.epoch` is server metadata. The server can label a real commit with a lower epoch so that the client skips it, and the client then drifts out of sync.
- `gateway.rs:166-173`: a `MembershipRemove` for self makes the client delete its MLS group state (`drop_group`) on the server's word.
- `enroll.rs:86-100`: a 404 for an existing user's identity key makes the new device create a fresh identity. Peers then see a "key changed" warning, so the effect is availability and confusion only.

All of these affect availability, not confidentiality. Consider only dropping a group after a merged, authorized self-removal commit.

#### 14. Info: time- and count-based content key rotation is not implemented

`RotationPolicy` and `RotationReason` (`crypto/src/content_key.rs:17-59`) are not used anywhere in `teamcraft-core`. Keys rotate only on adds with `FromJoin` and on removals. e2ee.md §4.2's "every N messages or every 7 days" is missing. That widens the exposure if a key leaks.

#### 15. Info (uncertain): attestation design points to settle before implementing #1

**Status: partly addressed** in `80cb2df`: the claim binds the TeamCraft user ID, so the subject mapping doesn't come from the server. Open: separate tokens or audiences for the TeamCraft API and the Keycloak account API (server and app side).

**Server status:** not changed (`fix/security-server`). The server no longer relies on `aud` (D12); rejecting tokens that carry the Keycloak `account` audience would refuse the stock tokens of a default realm, so separating the audiences stays a realm configuration task.

- The claim is a user attribute that anyone holding the user's access token with the `account` audience can write (`attestation.rs:163-207`). If the access token the client presents to the TeamCraft server carries that audience, the server could rewrite the attribute. It could not mint an ID token itself, but the next legitimate token refresh would carry the attacker's fingerprint. Use separate tokens or audiences for the TeamCraft API and the Keycloak account API, and make the server reject tokens with the `account` audience. I could not confirm which token the apps send to the server.
- The verifier must map the token's `sub` to the TeamCraft `UserId`. That mapping is server data, so put the `UserId` (or `sub`) into the signed claim, or verify `sub` against a value inside the identity-key upload signed by the identity key.

### Done correctly

- Device certificates are verified with `verify_strict` against the pinned key. The key comparison is constant-time, the encoding is versioned and domain separated (identity.rs:379-395), and the leaf signature key must match the certificate (credential.rs:27-42).
- MLS commits:
  - Only `Add`/`Remove`/`Update` proposals are accepted, and only from members.
  - External senders and external commits are rejected (group.rs:299-301, 438-448).
  - Updates may not change the user or device (`verify_same_identity`, 477-495).
  - Every removal needs a signed change that matches the proposals exactly (411-421, 498-529).
  - The signed change is bound to the conversation (`ensure_conversation`, 503).
- Welcomes are checked for group ID or conversation, ciphersuite, and every member credential (group.rs:91-118).
- A pinned identity-key change is not trusted silently: the old key stays in use and the change is recorded (mod.rs:268-274). There is no UI or API yet to accept a change, and `changed_identities` only lists them.
- Roster entries need an owner of the *previous* roster, so a removed owner can't sign the next roster version (core/workspace.rs:159-163). Log entries are bound to log kind and version, and versions must be strictly consecutive (crypto/workspace.rs:34-39, core/workspace.rs:266-276).
- `admin_self_join` is enforced client-side from the signed policy (trust.rs:84-89).
- Content key rings refuse to replace existing key material under a known ID (content_key.rs:101-110). Bundles are bound to their conversation (group.rs:307).
- Message AAD binds conversation, sender user and device, client message ID and key ID (message.rs:130-140), so the server alone can't re-attribute or move ciphertexts.
- Device linking authenticates the rendezvous with a key from the QR code and shows a short code before sending the key bag (linking.rs).

## Part B: device linking, key bag/backup, org recovery, push crypto

Scope: read-only review of the worktree `claude/security-review-b62c0a` (HEAD 9667ef3).
Threat model: the VPS server and S3 are untrusted (ARCHITECTURE.md §2, docs/e2ee.md).
Paths are relative to the repo root. Line numbers were checked against the files.

### Summary

| #   | Severity        | Title                                                                                          | Location                                                                                       |
|-----|-----------------|------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------|
| B1  | High            | Linked-device enrollment falls back to a new identity wrapped with an empty or fixed passphrase | `teamcraft-core/src/client/enroll.rs:86-99,154-157`; `apps/desktop/src/core/runtime.rs:594`; `teamcraft-ffi/src/engine.rs:43,218` |
| B2  | High (latent)   | Rendezvous short code does not stop a relay that knows AK (low-order keys, 4-digit SAS with no commitment) | `teamcraft-crypto/src/rendezvous.rs:35,128-133,200-205,244-265`                                 |
| B3  | High (design)   | Org recovery seals the backup key to an unauthenticated request key                           | `teamcraft-core/src/client/org_recovery.rs:132-158`; `teamcraft-crypto/src/org_recovery.rs:351-368` |
| B4  | Medium          | Combining admin keeps the org secret and t shares, and nothing rotates afterwards              | `teamcraft-core/src/client/org_recovery.rs:112-158`                                             |
| B5  | Medium          | A recovered user keeps the backup key that admins have seen                                    | `teamcraft-core/src/client/enroll.rs:298-306`                                                  |
| B6  | Medium          | Custodians are not checked against the signed roster, and old device certificates still get shares | `teamcraft-core/src/client/org_recovery.rs:39-62,168-193`                                       |
| B7  | Medium          | Web Push SSRF check covers IP literals only, not DNS names that resolve to private addresses   | `teamcraft-server/src/push/web_push.rs:41-75`                                                  |
| B8  | Low             | Push notification sender and conversation names come from the server's wake-up, not from the decrypted message | `teamcraft-core/src/push_preview.rs:79-90,172-179`                                              |
| B9  | Low             | Push payload is not padded, so the length (and Urgency header) shows the push kind to the push service | `teamcraft-crypto/src/push.rs:148-178`; `teamcraft-server/src/push/routing.rs:228-232`          |
| B10 | Low             | Rendezvous handshake nonce is reused when the same link code is scanned twice                   | `teamcraft-crypto/src/rendezvous.rs:325-331`; `teamcraft-core/src/client/linking.rs:143-152`    |
| B11 | Low             | Key bag has no version or rollback binding, and the server's wrapped keys are adopted unchecked | `teamcraft-crypto/src/key_bag.rs:65-70,224-233`; `teamcraft-core/src/client/enroll.rs:213-223` |
| B12 | Info            | Shamir shares are not verifiable, and sealed shares do not authenticate the sender             | `teamcraft-crypto/src/org_recovery.rs:118-132,166-190`                                          |
| B13 | Info            | Browser store key can be used by any same-origin script; the service worker holds an access token | `teamcraft-core/src/store/browser/browser_key.rs:27-53`; `teamcraft-core/src/push_preview.rs:50-59` |

---

### B1: Linked-device enrollment falls back to a new identity wrapped with an empty or fixed passphrase (High)

**Status: fixed** in `a8e25c2`. `EnrollmentSecret` (passphrase, linked, org recovery); only a passphrase enrollment creates an identity, others fail with `IdentityMissing`; wrapping refuses passphrases under 8 bytes.

**Location**
- `crates/teamcraft-core/src/client/enroll.rs:86-99`: if `api.identity_key(me.id)` returns *not found*, the
  client creates a new identity and backup key. It does this even when `options.maybe_linked` or
  `maybe_org_recovery` is set.
- `enroll.rs:154-157`: for a new identity it calls `upload_key_bag_with(&options.passphrase, …)`.
- `apps/desktop/src/core/runtime.rs:594`: the linked flow passes `passphrase: SecretBytes::new(Vec::new())`
  (an empty passphrase).
- `crates/teamcraft-ffi/src/engine.rs:43,218`: the linked flow passes the fixed public string
  `LINKED_DEVICE_PASSPHRASE = "unused: keys come from the linked device"`.
- `PassphraseWrapper::new` (`teamcraft-crypto/src/backup.rs:193-196`) only checks the Argon2 settings,
  not the passphrase. Argon2 accepts an empty password.

**Exploit.** A user links a new desktop or mobile device. The malicious server answers
`GET identity_key(me)` with 404 and accepts `put_identity_key` (`replace: false` is only enforced
by the server). The client then:
1. generates a new identity key,
2. wraps a new backup key with `""` or the public constant,
3. uploads the key bag.

The server unwraps it offline with one Argon2 call. It now holds the identity private key of the
new identity, and later every content key that device adds to its key bag (uploads happen on every
change). With that key it can cross-sign server-controlled devices and impersonate the user to any
peer that TOFU-pins the key: `pin_identities` takes the server's key on first contact, and
attestation is a TODO (`client/mod.rs:257`). Peers that already pinned the old key, or that get the
key from the signed roster, see an identity change. That limits the damage but does not prevent it.

**Fix**
- In `complete()`, if `maybe_linked` (or `maybe_org_recovery`) is set and the server reports no
  identity, fail with an error. For linked devices, take the identity from the transferred key bag
  and never create one.
- Refuse empty or short passphrases in `PassphraseWrapper::new` (or in `upload_key_bag_with`).
- Replace the "unused passphrase" sentinel with an enum (`EnrollmentSecret::Passphrase | Linked | OrgRecovery`)
  so this state can't be represented.

---

### B2: Rendezvous short code does not stop a relay that knows AK (High, latent)

**Status: fixed** in `adc0837`. Responder commits to its ephemeral key, non-contributory DH results are refused, keys and path hash are bound to both ephemeral keys, the code has 6 digits.

**Location:** `crates/teamcraft-crypto/src/rendezvous.rs`
- `:35`: `SHORT_CODE_DIGITS = 4` (about 13.3 bits).
- `:128-133` and `:200-205`: the X25519 result is used without `SharedSecret::was_contributory()`.
- `:259-265`: `path_hash = HKDF(AK ‖ shared)`, with no transcript hash and no commitment.
- `:244-257`: the short code is the path hash mod 10⁴.

**Context.** In the QR flow that exists today, AK goes only through the screen or QR code. The
server never sees it, and the AK-sealed handshake (32-byte random AK, AEAD-authenticated `hello`
and `auth_hello`) already stops a MITM. **Not exploitable today.**

docs/e2ee.md §5 (lines 192-204) also describes an **approval-prompt** variant in which AK reaches
the old device through the server. The module docs (`:14-16`) and the test
`server_in_the_middle_shows_different_codes` (`:387-394`) claim the short code then detects the
server. It does not:

1. **Low-order point.** The server knows AK, so it can forge `hello` and `auth_hello` with a
   low-order X25519 public key. Both honest sides then compute `shared = 0^32`, both channels derive
   the same `HKDF(AK ‖ 0)`, and the short codes **match**. The server derives the transport keys and
   reads the backup key and key bag (`linking.rs:169-176`).
2. **Grinding.** Even with a contributory check, the server finishes the handshake with the new
   device first (it learns code A), then picks its ephemeral key toward the old device after seeing
   the old device's `hello`. On average it needs about 10⁴ X25519 and HKDF operations to hit code A.
   That takes milliseconds.

**Fix.** Before the approval variant ships:
- reject non-contributory DH results;
- add a commitment: the side that sends its key last commits to it (hash) before seeing the peer's
  key, as in ZRTP / Signal SAS;
- derive the SAS from a hash over the full transcript (both ephemeral keys, rendezvous ID, roles);
- use at least 6 digits or emoji (about 20+ bits);
- or keep AK out of the server's reach entirely (QR or typed code only).

Until then, remove or rewrite the misleading test and the doc comment.

---

### B3: Org recovery seals the backup key to an unauthenticated request key (High, design)

**Status: fixed** in `52c3c82`. `RecoveryRequest` (the server's request ID, user, key); its verification code is the request key's fingerprint, and only a `ConfirmedRecoveryRequest` (code entered by the admin) receives the backup key; HPKE info binds request ID, user and key.

**Location:** `crates/teamcraft-core/src/client/org_recovery.rs:132-158`.
`recover_backup_key_for(…, request: [u8; 32])` seals the recovered backup key to raw caller- or
server-supplied bytes (`RecoveryRequestPublicKey::from_bytes(request)`, `:155-156`;
`teamcraft-crypto/src/org_recovery.rs:361-368`). Nothing binds the request key to the recovering
device, to the approvals, or to a fingerprint the admin compared. `RecoveryRequestPublicKey` has no
fingerprint or display helper at all. e2ee.md §6.3.1 relies on the request key's fingerprint being
shown and compared, but no such check exists in code.

**Exploit.** Once the planned server endpoints exist, the server replaces the recovering device's
request key with its own key. t honest admins approve the user's legitimate recovery. The combiner
seals the backup key to the server, which decrypts the user's key bag (identity key and all content
keys): a full compromise of that user.

**Fix**
- Make the API take a verified request object. The recovering device signs the request (user ID,
  request key, request ID, timestamp) with an IdP-attested ephemeral key, or the admin UI must show
  the request-key fingerprint and require an out-of-band match (phone or in person).
- Include the request ID and request key in the HPKE info or AAD (`seal_info`, `:390-394`).

---

### B4: Combining admin keeps the org secret and t shares, and nothing rotates afterwards (Medium)

**Status: mitigated** in `52c3c82`. Shares for the combiner are bound to one request and only go to roster admins; `rotate_org_recovery` replaces the org key and members re-wrap automatically. Open: threshold decryption (no reconstruction of `s`) and the server telling owners' clients that a recovery finished so they rotate on their own.

**Location:** `crates/teamcraft-core/src/client/org_recovery.rs:112-127` and `:132-158`.
- `share_for_combiner` seals the admin's long-term share itself, not anything tied to one request,
  to whichever admin and device the caller names. It does not check that a recovery request exists
  or that `combiner` is an admin.
- The combiner therefore receives t-1 durable shares and reconstructs `s` (`:146`).
- Nothing enforces "rotate after each recovery". The comment at `:131` and e2ee.md §6.3.1 only say
  it should happen. Old wraps remain valid on the server.

**Exploit.** With the default t = 2, a single malicious admin (or a compromised admin device) asks
for one plausible recovery. The share they receive, plus their own, gives them `s` permanently. The
server stores every member's org-wrapped backup key, so admin + server can decrypt everyone's key
bag until the org key is rotated and all members re-wrap. Threshold enforcement is only
cryptographic. It holds for the first t-1 admins, but one approval hands over a lasting capability.

**Fix**
- Use threshold decryption instead of reconstructing the secret. Each approver computes its partial
  DH with `s_i` on the wrap's encapsulated key. Shamir over GF(2⁸) bytes can't do this; switch to
  Shamir over the X25519 or ristretto scalar field.
- Failing that, force rotation in code: after a recovery, the owner client creates a new org key
  automatically, and clients drop and re-wrap old-key wraps.
- Bind approvals to a specific request and check that the combiner is a roster admin.

---

### B5: A recovered user keeps the backup key that admins have seen (Medium)

**Status: fixed** in `ab95d28`. The recovered device switches to a new backup key, wrapped with a new passphrase and the current org key.

**Location:** `crates/teamcraft-core/src/client/enroll.rs:298-306`. After org recovery, the
recovered `backup_key` is kept as the account backup key, and later key bag uploads use it
(`enroll.rs:226-229`). The identity key also stays the same.

**Exploit.** The combining admin saw the backup key (B4), and the server stores every future key
bag version. The admin, together with the server, can keep decrypting the user's key bag after the
recovery, including content keys of conversations joined later.

**Fix.** After any org (or passphrase) recovery, generate a new backup key, re-encrypt the key bag,
and re-wrap with every factor (passphrase, passkey, new org key). Consider rotating device keys and
telling the user that admins performed a recovery.

---

### B6: Custodians are not checked against the signed roster, and old device certificates still get shares (Medium)

**Status: fixed** in `52c3c82` (custodians must be roster admins) and `153d020` (devices must be in the signed device list).

**Location:** `crates/teamcraft-core/src/client/org_recovery.rs:39-62` and `:168-193`.
- `set_up_org_recovery(admins, …)` takes any `UserId` list. It doesn't check that each user is an
  admin in the signed roster. For users missing from the roster, the identity key comes from
  `pin_identities`, which is TOFU on the server's answer with attestation still a TODO. Roster
  entries do override pins (`trust.rs:33-48`).
- The device list comes from `list_user_devices`. Any certificate that verifies against the
  identity key is accepted. There is no revocation or removed-device check, so the server can
  include an old certificate of a lost or removed admin device and get a share sealed to it.

**Exploit (uncertain; depends on caller UI).** If the owner's UI passes a user who is not yet in the
roster, the server substitutes the identity key and receives that share. Separately, an attacker
holding a stolen old admin phone gets a share sealed to it.

**Fix.** Take custodians from the verified roster's admin role. Fail if any custodian lacks a
roster identity key. Check devices against a signed device list or revocation log.

---

### B7: Web Push SSRF check covers IP literals only (Medium; server-side, outside the E2EE threat model)

**Status: fixed** in `9548566` (`fix/security-server`), same change as D1.

**Location:** `crates/teamcraft-server/src/push/web_push.rs:41-75`. `check_endpoint` blocks private
IP literals and a few name suffixes. A DNS name that resolves (or rebinds) to `10.x`, `127.x`,
`169.254.169.254`, etc. passes, and `reqwest` connects to it (`:146-157`). Redirects are disabled,
which is good. The IPv6 list also misses NAT64 `64:ff9b::/96` and site-local addresses.

**Exploit.** Any authenticated client registers a subscription with `endpoint = https://internal.attacker-dns.example/…`.
The server then sends blind POSTs to internal HTTPS services, or probes ports and hosts through the
timing and status mapping.

**Fix.** Resolve the name, check every address, and connect to the checked IP. Use a custom
`reqwest` DNS resolver that filters private ranges, or a proxy. Alternatively, allowlist the known
push service domains, configurable per deployment.

---

### B8: Push notification names come from the server's wake-up (Low)

**Location:** `crates/teamcraft-core/src/push_preview.rs:79-90` and `:172-179`.
- The title and sender come from the server-authored `wake_up.conversation_id` and
  `wake_up.maybe_sender_id`. The server holds the push key: it seals the wake-up, so the wake-up
  AEAD only protects against the push service.
- The fetched message is found by ID only. Its `conversation_id` and `sender_user_id` are not
  compared with the wake-up.

**Exploit.** The server shows real decrypted text from Bob in conversation X as
"Alice in #board", which helps social engineering.

**Fix.** After decryption, take the sender and conversation from the decrypted message's
authenticated context. Drop the preview if they differ from the wake-up.

---

### B9: Push payload length and Urgency show the push kind (Low)

**Location:** `crates/teamcraft-crypto/src/push.rs:148-178`. The record has no padding. The
plaintext is variable-length JSON (`push/crypto.rs:17`: `kind` string, optional
`message_id` / `sender_id`, `is_urgent`). The `Urgency: high` header is set for urgent pushes
(`routing.rs:228-232`).

**Impact.** Google, Mozilla or Apple can tell DM, mention, call or urgent apart, and whether a
message ID is present. e2ee.md §7 promises they see "nothing else".

**Fix.** Pad the wake-up plaintext to a fixed size (for example 256 bytes) before sealing. RFC 8291
padding (zeros before the 0x02 delimiter) can also be used. Consider a constant `Urgency`, or
document that it leaks.

---

### B10: Rendezvous handshake nonce is reused when the same code is scanned twice (Low)

**Status: fixed** in `adc0837`. Handshake messages carry a random nonce.

**Location:** `crates/teamcraft-crypto/src/rendezvous.rs:325-331`. Handshake messages use fixed
nonces 0 and 1 under keys derived only from AK. `start_link_approval`
(`client/linking.rs:143-152`) builds a new `Responder` with fresh random contents every time it is
called with the same code. If the first attempt fails (for example, the server drops frames) and
the user scans or pastes the same code again while the new device is still waiting,
`hello` #1 and `hello` #2 are encrypted under the same (key, nonce). That leaks the XOR of the two
plaintexts and the Poly1305 key for nonce 0, which allows forging under that key. No practical
exploit is evident (the plaintexts are random challenges and keys), but it breaks the AEAD's
contract.

**Fix.** Add a random per-handshake nonce, or mix the challenge into the key. Alternatively, mark
the code as single-use on the responder and refuse a second use.

---

### B11: Key bag rollback and server-supplied wrapped keys (Low)

**Status: fixed** in `0be8c92`. Encrypted key bags carry a sequence and a hash of the wrapped keys in the AAD; devices refuse older bags and take the server's wraps only after the bag opened with them.

- `crates/teamcraft-crypto/src/key_bag.rs:65-70,224-233`: the AAD binds only the format version and
  user ID, not a monotonic key bag version. The server can serve an older bag to a recovering
  device: that device misses content keys (availability), and a removed device's leftovers could
  come back. `merge_remote_key_bag` (`enroll.rs:264-283`) only adds keys, so live devices are not
  hurt.
- `crates/teamcraft-core/src/client/enroll.rs:213-223`: after a recovery, the first upload adopts
  the server's `wrapped_backup_keys` blob and re-uploads it. The server can drop wrappers (for
  example the passkey) without the client noticing. This is a DoS or recovery-loss vector, not
  confidentiality.

**Fix.** Put the key bag version (and a hash of the wrapped-key set) in the AAD, keep the highest
version seen locally, and reject lower versions.

---

### B12: Shamir shares are not verifiable, and sealed shares are unauthenticated (Info)

`combine_shares` (`teamcraft-crypto/src/org_recovery.rs:118-132`) has no Feldman/VSS commitments.
`open_on_device` (`:181-190`) uses HPKE base mode with `info = (purpose, recipient user)`, so the
server can inject a garbage "share" and abort a recovery without the culprit being identified.
Confidentiality holds: a wrong `s` fails the org-public-key check (`:284`) and the HPKE open.

**Fix.** HPKE auth mode, or a signature by the approving admin's device key over (request ID,
share). Publish per-share commitments in the roster.

### B13: Browser store key, and the service worker holding a token (Info)

`browser_database_key` (`store/browser/browser_key.rs:27-53`) unwraps the IndexedDB key with a
non-extractable AES key that is stored next to it. Any same-origin script (XSS) or anyone with
profile access can call `subtle.decrypt` and read the whole store: identity key, backup secret,
push key, and the `PushSession` access token (`push_preview.rs:50-59`). That is inherent to web
clients. Keep a strict CSP, and treat the browser as a weaker device class in docs. Minor: the
"read-only" service worker path calls `load_or_create`, which *creates and writes* a key if none
exists (`browser_key.rs:48-51`).

---

### What is done correctly

- **QR linking:** the 256-bit AK is drawn on the new device and never sent to the server
  (`linking.rs:58-75`). Handshake messages are AEAD-sealed with AK-derived, direction-specific keys,
  and challenges are compared in constant time with `subtle` (`rendezvous.rs:153,195`). Transport
  frames use per-direction keys with counter nonces, so replay, reordering and reflection fail
  (tests at `:396-414`). The relay requires both peers to be the same user and caps frame and byte
  quotas (`server/rendezvous/hub.rs:89-133`). Secrets go over only after the user confirms on the
  existing device (`linking.rs:154-179`). The new device checks the transferred identity against
  the account's identity key (`enroll.rs:102-105`).
- **Backup wrapping:** XChaCha20-Poly1305 with random 192-bit nonces (`aead.rs`). The AAD binds
  the version, user ID, method and the full parameter blob, so the server can't downgrade Argon2
  parameters: changing them breaks the tag (`backup.rs:349-410`). Server-supplied Argon2 parameters
  have upper bounds (`backup.rs:25-27,264-276`). Defaults are Argon2id 128 MiB, t = 3. The passkey
  PRF output goes through HKDF with its own label. Domain labels are distinct, length-prefixed and
  can't collide with MLS labels (`domain.rs`). Key bag plaintext is zeroized, and the user ID is
  checked in both the AAD and the plaintext.
- **Org recovery:** the threshold is at least 2 (`org_recovery.rs:91`). The org public key comes
  from the signed roster (`client/org_recovery.rs:66-75`). Unwrapping checks that the derived
  public key equals the wrap's key (`:284`). HPKE info separates purpose and user. Shares go only to
  certificate-verified devices.
- **Push:** RFC 8291/8188 are implemented as specified, with a known-answer test. Each push uses a
  fresh ephemeral key and salt. VAPID tokens live 12 h, and the audience is the endpoint origin. The
  inner wake-up is AEAD-sealed, so the push service sees only ciphertext. The service worker JS is
  minimal glue (`teamcraft-core/web/service-worker.js`). `teamcraft-ui/sw.js` shows a generic
  notification and holds no keys. The push key is client-generated and stored in the encrypted
  store.

## Part C: browser store, web client hardening, DAVE call bindings, SFU auth and parsers

Scope: read-only review of the worktree `security-review-b62c0a`. Threat model: the server on the
VPS is untrusted (ARCHITECTURE.md §2), and so is the SFU. I checked every finding against the code
and cite the lines I read. Findings marked "(uncertain)" depend on behaviour I could not fully
confirm.

### Summary

| #  | Severity | Title                                                                                     | Location                                                                                   |
|----|----------|-------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| 1  | High     | The web client is served by the untrusted server, which breaks the E2EE claim for web users | `crates/teamcraft-server/src/web.rs:35-58`, ARCHITECTURE.md §2                            |
| 2  | Medium   | Docs and code claim XSS "can't take the key away"; one `decrypt` call exports the DB key  | `docs/e2ee.md:382,396`, `store/browser/browser_key.rs:1-8,89-108`                         |
| 3  | Medium   | Desktop: webview JS can approve device linking (backup key + key bag) without the SAS check | `apps/desktop/src/core/commands.rs:139-146`, `tauri.conf.json` `withGlobalTauri: true`    |
| 4  | Medium   | Call key bindings expire after 10 min but are relayed to later joiners, so late joins fail | `teamcraft-media/src/call_auth.rs:22,82`, `teamcraft-sfu/src/dave_coordinator.rs:104-105,280-285`, `call_binding.rs:152` |
| 5  | Low/Med  | Any bad relayed binding ends the call for every existing member (fail-stop, not reject)   | `teamcraft-media/src/call_engine.rs:443-449,541-556`, `call_native.rs:349-350`            |
| 6  | Low      | The DAVE coordinator has no commit timeout, so a silent or garbage-committing member stalls or breaks the call | `teamcraft-sfu/src/dave_coordinator.rs:112-136,170-183`                        |
| 7  | Low      | Unbounded per-participant signaling queue and no cap on published video tracks (SFU memory DoS) | `teamcraft-sfu/src/server.rs:187,342-349`, `room.rs:172-186`, `forwarding.rs:186-226` |
| 8  | Low      | Web CSP `connect-src 'self' wss: https:` allows exfiltration anywhere; no Trusted Types; a bad CSP config drops silently | `teamcraft-server/src/config.rs:342-344`, `web.rs:43,77-79`                 |
| 9  | Low      | Join tokens can be replayed within their TTL; removed members are not evicted from running calls | `teamcraft-sfu/src/auth.rs:92-118`, `services/calls.rs` (create_join_token)          |
| 10 | Low      | Dev mode on the SFU accepts unsigned tokens when the public key is missing (fail-open by env) | `teamcraft-sfu/src/main.rs:120-141`                                                  |
| 11 | Info     | The client never checks the SFU's `Joined.call_id` against the server's call id           | `teamcraft-media/src/call_engine.rs:492-515`                                               |
| 12 | Info     | What the SFU learns (metadata)                                                            | see section                                                                                |
| 13 | Info     | The OIDC `authorization_endpoint` scheme is not checked before `location.href`            | `teamcraft-ui/src/auth/browser.rs:203-221,373-379`                                         |

---

### (a) Browser store and web client

**Status of the UI-side fixes (feat/ui-security-chat):**

| # | Status | What changed |
|---|--------|--------------|
| 1 | Mitigated, limit documented | Signed release manifest checked by the service worker (`crates/teamcraft-ui/sw.js`, `scripts/sign-web-release.sh`, e2ee.md §9.3); the UI warns about unsigned or rejected bundles and shows the release key. A server that replaces `sw.js` still defeats it; the doc says so |
| 2 | Fixed (docs and comments) | e2ee.md §9.1 and the `browser_key.rs` comment now say XSS gets the database key; the table lists what keeps script out |
| 3 | Fixed | `withGlobalTauri` is off; every command is listed in `apps/desktop/build.rs` and allowed per command in `capabilities/default.json`; the core checks the short code of a device link itself (typed from the new device, three tries), so a script in the webview cannot approve a link |
| 8 | Fixed, one item open | The CSP is built from the configuration (`connect-src` limited to self, issuer, SFU), an invalid CSP stops the server at startup, `sw.js` and `init.js` are revalidated. Trusted Types is an opt-in: the UI framework writes markup with `innerHTML` and the worker registers from a string URL, so the client does not start under it yet (the finding's assumption "no HTML sinks" does not hold for the framework's static templates) |
| 13 | Open | The OIDC endpoint scheme check is not part of this change |

#### 1. High: the web client is served by the untrusted server

`web.rs:35-58` serves `index.html`, `init.js`, the wasm and `sw.js` from the same server the threat
model treats as compromised. `index.html` is `no-cache` (`web.rs:89-90`), so a compromised server
can ship a modified bundle on the next load. That bundle can read the IndexedDB store, which it can
unwrap like the real app (see #2), plus the in-memory MLS state, the device key and the tokens. It
can exfiltrate all of this through its own origin. SRI does not help, because the server also
serves the HTML that carries the hashes.

The share-link page has the same issue, and the docs note it there (`docs/e2ee.md:153-162`). For
the main web client neither ARCHITECTURE.md §2 nor e2ee.md §9.1 mentions it, and the table at
`docs/e2ee.md:396` lists "Server: Yes (nothing leaves the browser)".

- Exploit: an attacker on the VPS edits `dist/index.html` or the wasm. Every web user who next
  opens the app hands over its keys and history.
- Fix: document that the web client trusts whoever serves it. Better options: serve the web client
  from a separate trusted origin or CDN, distribute it as a signed, installable PWA bundle, or pin
  it with a code-verify browser extension. Recommend the desktop and mobile apps for E2EE-critical
  users.

#### 2. Medium: XSS can export the database key, contrary to the docs

`browser_key.rs:55-67` creates a non-extractable AES-GCM key, which is good. That key only wraps
the real 256-bit database key, and `unwrap()` (`browser_key.rs:89-108`) decrypts the database key
into a plain `Uint8Array` with `subtle.decrypt`. Any script on the origin can repeat this:

1. read `wrapping_key` (a CryptoKey handle) and `wrapped_database_key` from the `keys` object store;
2. call `crypto.subtle.decrypt({name:"AES-GCM", iv}, handle, ct)` to get the raw 32-byte database key;
3. send it off together with the IndexedDB records and decrypt them offline indefinitely.

Non-extractability therefore protects nothing: the key is "used" once to reveal a secret that is
fully extractable. Even so, the module comment (`browser_key.rs:5-6`, "an injected script can't
take the key away for offline use") and e2ee.md (`docs/e2ee.md:396`) claim the opposite. Inside the
wasm, XSS can also read linear memory, since `WebAssembly.Memory` is reachable from JS through
`wasm_bindgen` exports. That exposes the decrypted cache (`SealedStore` holds a plaintext
`MemoryStore`, `sealed.rs:22-26,64-66`), MLS secrets and the device key.

Also, the doc row "Copy of the whole browser profile: No" is correct: Chromium stores
non-extractable CryptoKeys in the profile in a recoverable form.

- Fix: correct the docs to "XSS gets the database key and all data permanently". If this matters,
  use the PRF or passphrase mode mentioned in §9.1 as a per-workspace policy. With WebCrypto, the
  only way to keep the key non-exportable is to never unwrap a raw key: do the record AEAD with the
  non-extractable CryptoKey itself (AES-GCM via SubtleCrypto). Even then XSS can decrypt on demand
  while it runs, but it can't take the key offline.

#### 3. Medium (defense in depth): desktop webview can approve device links without SAS confirmation

`tauri.conf.json` sets `"withGlobalTauri": true`, so `window.__TAURI__` is available to any script
in the webview. The capability `core:default` allows `invoke` of all app commands registered with
`generate_handler!`. These include `core_approval_start(code)` and `core_approval_confirm()`
(`apps/desktop/src/core/commands.rs:139-146`). According to `client/linking.rs:1-8`,
`complete_link_approval` transfers the backup key and the key bag to the new device. Only the UI
enforces that the user compares the short code. A script calls `confirm` directly.

So any XSS in the desktop UI turns into a persistent account takeover: a linked attacker device
with full history. The same applies to `api_request` with the user's bearer token
(`auth/commands.rs:61-68`; `validate_api_path` limits it to the API, which is fine).

- Current mitigations: the UI renders messages through a typed Markdown tree with no `inner_html`
  (see "Done correctly"). The desktop CSP is strict for scripts.
- Fix: show the SAS confirmation in a native dialog (`tauri-plugin-dialog`) owned by the Rust side
  instead of trusting a webview call. Set `withGlobalTauri: false` and import `@tauri-apps/api`
  only where needed (for a Leptos app use `wasm-bindgen` bindings). Consider per-command
  capabilities.

#### 8. Low: web CSP and header gaps

**Status: fixed** in `8a121e6` (`fix/security-server`). `connect-src` is generated from the configuration (`web.connect_origins`, issuer, SFU and storage origins) instead of allowing any HTTPS host, an invalid configured CSP is a startup error (and the fallback is `default-src 'none'`), and `init.js` and `sw.js` are served `no-cache`. Trusted Types are not in the default CSP; operators can add `require-trusted-types-for 'script'` through `content_security_policy`.

Default CSP (`config.rs:342-344`): `script-src 'self' 'wasm-unsafe-eval'`, no inline scripts (the
loader is `init.js`, written by `scripts/build-app.sh`), plus `frame-ancestors 'none'`,
`base-uri 'none'`, `form-action 'self'`. That is good. The gaps:

- `connect-src 'self' wss: https:` lets injected code `fetch` any HTTPS host, which makes #2 an
  easy exfiltration. Restrict it to `'self'`, the configured issuer origin and the SFU origin
  (generate the CSP from config).
- No `require-trusted-types-for 'script'`. The app has no HTML sinks, so enabling it is cheap and
  stops future regressions.
- `web.rs:43`: `HeaderValue::from_str(&config.content_security_policy).ok()` silently serves no CSP
  if the configured value is invalid (for example, it contains a newline). Fail at startup instead.
- `sw.js` and `init.js` are unhashed and cached for 1 h (`SHORT_LIVED`, `web.rs:30,93-94`). This
  is not a security issue in itself, but a stale `init.js` after a deploy can point to removed
  wasm.

#### 13. Info: OIDC endpoints are not scheme-checked

`browser.rs:203-221` takes `authorization_endpoint` from the issuer's discovery document and
`navigate()` sets `location.href` to it (`browser.rs:373-379`). `oauth2::AuthUrl::new` accepts any
URL, including `javascript:`. A malicious identity provider, or whoever controls the issuer
configuration, could run script on the app origin. Because of #1 this adds nothing for the VPS
operator. It matters only if Keycloak is operated separately from the app server. Fix: require
`https:` (and the issuer origin) for the endpoints.

#### Done correctly (a)

- Markdown (`teamcraft-ui/src/markdown.rs`) builds a typed tree. Links come only from
  `http://`/`https://` prefixes (`markdown.rs:133-140`), and `javascript:` is tested
  (`markdown.rs:186-191`). They render as `<a href target=_blank rel="noopener noreferrer">`
  (`components/rich_text.rs:46-49`). Recursion depth is bounded because each nesting level drops a
  delimiter.
- `grep` finds no `inner_html`, `innerHTML` or `insert_adjacent_html` anywhere in `teamcraft-ui`.
- Records are sealed with XChaCha20-Poly1305 under HKDF-derived keys, with the lookup ID as AAD
  (`local_store.rs:45-101`). Table names and IDs are pseudonymized.
- API responses get `default-src 'none'; frame-ancestors 'none'`, `no-store`, nosniff and optional
  HSTS (`security.rs:28,207-220`). `/f/{id}` is `no-store` with `no-referrer` (`web.rs:87-88`).
- Desktop CSP blocks remote scripts and connections (`connect-src 'self' ipc:`), and the opener
  capability is only the default.

---

### (b) DAVE call key bindings

#### Design check: can the SFU add members, impersonate participants or see media?

No, as long as the desktop/native path is used, and only the desktop/native path makes calls: the
web UI has no call code, and `teamcraft-ui` does not depend on `teamcraft-media`.

- Each device generates a fresh P-256 DAVE key per call (`dave.rs:129-139`). It signs a
  `CallKeyBinding {call_id, user_id, device_id, dave_public_key, not_after}` with its Ed25519
  device key under a domain label (`call_binding.rs:91-107`).
- Bindings are verified against the device certificates that `verified_call_devices` returns
  (`core/client/calls.rs:57-69`): members of the conversation's MLS group, verified against pinned
  identities. Verification checks the signature, user and device match, call ID, expiry and
  maximum validity (`call_binding.rs:125-162`).
- The trusted roster passed to `process_proposals` is those same devices
  (`apps/desktop/src/calls/mod.rs:584-617`; `call_engine.rs:613-628`). The SFU's
  `expected_participants` is used only without a roster, and then raises `UntrustedRoster`.
- Before sending a commit, `verify_pending_additions` (`call_engine.rs:582-597`) checks that every
  added leaf's signature key equals the verified bound key. After each commit or welcome,
  `verify_members` (`call_engine.rs:560-579`) checks every member. On a mismatch, `is_compromised`
  stops both sending (`call_engine.rs:309,365`) and decrypting (`call_engine.rs:650`). This fails
  closed.
- The SFU acts only as the external sender. It can propose Add with real key packages, which it
  can't forge without the P-256 private key. Welcomes are HPKE-encrypted to the key package's init
  key, so the SFU never gets epoch secrets. It can propose Remove (a DoS only).
- Comparison is constant time (`call_binding.rs:175-181`). Note: `ensure_matches` itself is not
  used. The engine compares `Vec`s with `!=`, which is fine because the values are public keys.
- The privacy code is reported on every rekey (`call_engine.rs:748-759`), and pairwise
  `verification_code` is available (`call_engine.rs:255-266`).

#### 4. Medium (availability): bindings expire after 10 minutes but are replayed to later joiners

A device signs its binding once, when the SFU sends the external sender
(`call_engine.rs:528-544`), with `not_after = now + 600 s` (`call_auth.rs:22,82`). The SFU stores
it (`dave_coordinator.rs:104-105`) and hands the stored bindings of all existing members to each
new joiner with the Welcome (`dave_coordinator.rs:278-286`). The joiner verifies them with the
current time, and `call_binding.rs:152` returns `Expired`. The error ends the joiner's call
(`call_engine.rs:457-460`, then `call_native.rs:349-350`).

As a result, a device can't join a call whose earliest current member joined more than about
10 minutes earlier. This is not a confidentiality issue, but it is a likely user-visible failure,
and it creates pressure to weaken the check.

- Fix: check expiry only at the time the binding is first presented, or have members refresh their
  binding (re-sign the same DAVE key) on each epoch or periodically and send it with a new
  message. Alternatively, bind to the MLS epoch instead of wall-clock time. Add a test for a joiner
  arriving after `BINDING_VALIDITY`.

#### 5. Low/Medium (availability): any bad relayed binding ends the call for everyone

`verify_bindings` runs before `on_proposals` (`call_engine.rs:443-449`) and returns the first
error (`call_engine.rs:552-553`): `UnknownDevice`, `Expired`, a bad signature or a decode error.
The native loop ends the call on any `handle_signal` error (`call_native.rs:349-350`). A malicious
SFU, a joiner with a skewed clock, or a device added to the conversation after others started the
call (not in their roster snapshot) therefore terminates the call for every existing member, not
just itself.

- Fix: on a bad binding for a joiner, skip that joiner (don't commit) and report it. Don't tear
  down the session.

#### 11. Info: SFU-chosen call ID is not checked

`on_joined` accepts the `call_id` from the SFU's `Joined` event (`call_engine.rs:492-515`) and uses
it for the DAVE group ID and binding verification. The client knows the server's
`CallJoinTokenResponse.call_id` but never compares the two. I found no confidentiality impact,
because the roster and keys are still per conversation, but checking it is cheap and rules out
cross-call confusion.

#### 12. Info: what the SFU learns

Device and user IDs and join/leave times (it also reports them to the server,
`reporting.rs:90-108`). Also: per-packet audio level (the RTP header extension is unencrypted, by
design, for speaker detection, `forwarding.rs:304`), video frame numbers, fragment counts and
keyframe flags (`fragment.rs` extension), frame sizes and timing, codec, simulcast layers, and
unencrypted codec headers per DAVE (H.264 NAL headers, Opus silence frames). Bindings expose the
per-call DAVE public keys. All of this matches ARCHITECTURE.md §2. A participant can also fake its
audio level to occupy a speaker slot (minor).

---

### (c) Server-SFU authentication, join tokens, SFU parsers

#### Done correctly (c)

- Join tokens are EdDSA JWTs. The SFU pins the algorithm to `EdDSA` and requires
  `aud = CALL_JOIN_TOKEN_AUDIENCE`, `iss` and `exp`/`iat` (`auth.rs:78-81`). It checks expiry with
  leeway, rejects future `iat` and enforces a maximum lifetime of 300 s (`auth.rs:103-112`). The
  server TTL is 60 s (`config.rs:724-726`).
- The server issues a token only to conversation members, for the caller's own active device, and
  not for archived conversations (`services/calls.rs`, `create_join_token`).
- SFU-to-server: a bearer registration token, compared in constant time with `subtle::ct_eq`
  (`services/calls.rs:173-180`). Without a token, the endpoints answer 503. The token is held as
  `SecretString` in config. Failed attempts count toward the auth-failure rate limit
  (`security.rs:193-195`). The SFU keeps the token out of `Debug` (`reporting.rs:121-127`).
- Duplicate device joins are refused (`room.rs:263-266`).
- Parsers: signaling checks the length prefix against `max_signaling_message_bytes` (256 KiB)
  before allocating (`signaling.rs:52-68`). The datagram header uses `get(..)` bounds checks
  (`datagram.rs:101-128`). RTP checks every length before indexing (`rtp.rs:154-203`), and
  padding and one-byte extensions are bounds-checked (`rtp.rs:107-115`, `parse_one_byte_extensions`).
  The client-side frame assembler is limited to 8 frames x 255 fragments (`fragment.rs`). I saw no
  panicking index operations.
- Pre-join timeouts apply (`server.rs:164-174`).

#### 6. Low: no commit timeout in the DAVE coordinator

**Status: not fixed** (`fix/security-server`). The coordinator has no clock; a commit timeout needs a timer per transition and a decision on which committer to drop, which touches the DAVE flow that the media/SFU work is changing. Left for that work.

`on_commit` accepts only commits from the current transition's committers
(`dave_coordinator.rs:119-129`), and `start_transition` returns early while a transition is in
flight (`dave_coordinator.rs:171-173`). No timer exists. A committer that never commits blocks all
later joins and leaves until it disconnects. A committer that sends garbage wins the race, gets the
commit announced to everyone (`dave_coordinator.rs:133`, `apply`), and every member's
`process_commit` fails, which ends their calls (see #5). Both require a legitimate participant, so
this is insider DoS only.

- Fix: add a transition timeout that re-proposes with the remaining committers. Optionally drop the
  offending device.

#### 7. Low: SFU memory DoS by an authenticated participant

**Status: fixed** in `f139904` (`fix/security-server`). Signaling queues are bounded (`signaling_queue_len`, default 256) and a participant that overflows is disconnected; a participant may publish at most four video tracks; one source address may hold 16 unjoined sessions (`max_prejoin_sessions_per_ip`).

- `signal_tx` is `mpsc::unbounded_channel` (`server.rs:187`). The writer blocks on QUIC flow
  control (`server.rs:342-349`). A client that never reads its signaling stream accumulates
  `TransportFeedback` every 100 ms (`server.rs:241-252`) plus broadcasts without bound. Use a
  bounded channel and disconnect on overflow.
- `publish_video` has no per-participant track limit (`room.rs:172-186`, `forwarding.rs:186-226`).
  Each call allocates a track and broadcasts `VideoPublished` to everyone, so a loop amplifies.
  Limit tracks per participant (for example 2: camera and screen).
- Any number of unauthenticated QUIC sessions can each hold resources for up to 2 x `join_timeout`
  (20 s) before the token is checked (`server.rs:164-185`). Consider a per-IP connection limit.

#### 9. Low: token replay and no revocation

**Status: mitigated** in `d2afd85` (`fix/security-server`). Removing a member from a channel or space, or deactivating a user, now tells the SFU to close that user's session in the running call. Join tokens are still not one-time: replay within the token lifetime (default 60 s) is accepted, because a one-time `jti` would break client reconnects without a client change.

Tokens have no `jti` or nonce, and the SFU keeps no replay cache (`auth.rs:92-118`). Within the
60 s TTL (or up to the 300 s SFU cap), a leaked token can be used again after the legitimate
session ends. Duplicate concurrent use is blocked by `AlreadyJoined`. More relevant: once a member
is removed from the conversation, neither the server nor the SFU evicts them from a running call,
and the other clients' trusted roster is a snapshot taken at join time
(`apps/desktop/src/calls/mod.rs:584-617`), so a removed member keeps receiving media until they
leave. (uncertain: I didn't find any roster refresh path; there may be UI-level handling I missed.)

- Fix: one-time `jti` with a short replay cache on the SFU. Have clients refresh the trusted roster
  on MLS membership changes and propose removal of devices that are no longer members. Optionally
  add an authenticated server-to-SFU kick.

#### 10. Low: dev mode can fail open

**Status: fixed** in `3a71381` (`fix/security-server`). Unsigned dev tokens need `TEAMCRAFT_SFU_INSECURE_DEV_TOKENS=1` together with loopback `TEAMCRAFT_SFU_BIND` and dev HTTP addresses; otherwise all joins are rejected and the reason is logged.

`main.rs:120-141`: when `TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY` is unset and `TEAMCRAFT_SFU_DEV_HTTP`
is set, the SFU accepts unsigned `dev:<call>:<user>:<device>` tokens. Anyone can then join any
call as anyone at SFU level. DAVE still protects media, but this exposes metadata and allows
spoofed join reports (`server.rs:200`). A leftover dev variable in production is plausible, since
the dev HTTP page also listens on any given address. Gate this behind a compile-time feature or an
explicit `TEAMCRAFT_SFU_INSECURE_DEV_TOKENS=1`, and refuse it when the bind address is not
loopback. Related points: the SFU falls back to a self-signed certificate when none is configured
(`main.rs:150-154`), and `HttpReporter` accepts `http://` server URLs, which sends the
registration token in cleartext (`reporting.rs:82-96`). Warn or refuse in both cases.

## Part D: teamcraft-server / proto permissions / db / bridge

Scope: `crates/teamcraft-server` (auth, extractors, router, handlers, services, gateway, meetings, push),
`crates/teamcraft-proto/src/permission.rs`, `crates/teamcraft-db` (SQL construction), `crates/teamcraft-bridge`
(webhook auth). Read-only review on branch `claude/security-review-b62c0a` (HEAD 9667ef3). Paths below are
relative to the repo root.

Severity scale: critical / high / medium / low / info. "Uncertain" marks findings whose exploitability
depends on deployment configuration or on behaviour I could not confirm end to end.

### Summary

| #  | Severity           | Title                                                                                     | Location                                                                                              |
|----|--------------------|-------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------|
| 1  | medium             | Web Push SSRF: endpoint check is hostname-suffix only, DNS can resolve to internal IPs    | `crates/teamcraft-server/src/push/web_push.rs:40-73, 152-163`                                          |
| 2  | medium             | Admins (non-owners) can deactivate owners/admins and revoke their devices                 | `crates/teamcraft-server/src/services/admin.rs:149-176, 254-262`                                       |
| 3  | medium (uncertain) | ID tokens accepted as access tokens (no `typ` check); raw ID tokens are published as identity-key attestations | `crates/teamcraft-server/src/auth/validator.rs:190-242`, `crates/teamcraft-server/src/services/identity.rs:18-79` |
| 4  | medium             | Gateway client frames are not rate-limited; `last_typing` map grows without bound         | `crates/teamcraft-server/src/gateway/connection.rs:196-262`                                            |
| 5  | low-medium         | Unbounded gateway sessions per user (each with a 1000-event resume buffer)                | `crates/teamcraft-server/src/gateway/hub.rs:102-124, 184-216`                                          |
| 6  | low-medium         | iMIP REPLY: any recipient can set RSVPs for all e-mail attendees; one request fans out to N CalDAV writes | `crates/teamcraft-server/src/meetings/ical.rs:148-196`, `crates/teamcraft-server/src/services/meetings.rs:328-375` |
| 7  | low-medium         | No per-user caps on devices, attachments/storage, meeting mail volume                     | `crates/teamcraft-server/src/services/devices.rs:17-59`, `services/attachments.rs:22-57`, `services/meetings.rs:193-265` |
| 8  | low                | Denied meeting guests keep a valid session token until meeting end + grace                | `crates/teamcraft-server/src/services/meeting_guests.rs:185-208`, `services/accounts.rs:21-29`         |
| 9  | low                | Back-channel logout token: `exp` not required, `iat` age unbounded, `jti` not tracked (replayable) | `crates/teamcraft-server/src/auth/validator.rs:150-187`                                                |
| 10 | low                | Gateway / rendezvous auth errors bypass `public_message()` and leak internal error text; gateway 401s are not counted by the auth-failure limiter | `crates/teamcraft-server/src/gateway/connection.rs:145-151`                                            |
| 11 | low                | Server does not enforce `admin_self_join` / admin non-member channel management (client-only enforcement) | `crates/teamcraft-server/src/services/members.rs:68-127`, `crates/teamcraft-proto/src/permission.rs:336-344` |
| 12 | low                | Access-token client binding accepts `aud` alone (cross-client token substitution within the realm) | `crates/teamcraft-server/src/auth/validator.rs:202-220, 246-255`                                        |
| 13 | low                | Presigned upload URL is reusable until expiry (comment claims "exactly one upload")       | `crates/teamcraft-server/src/storage.rs:64-72`                                                         |
| 14 | low                | Secret-bearing types derive `Debug` / are not zeroized (push key, push token, bridge source token) | `crates/teamcraft-proto/src/api/users.rs:68-73`, `crates/teamcraft-server/src/push/sender.rs:13-22`, `crates/teamcraft-bridge/src/http.rs:47-73` |
| 15 | info               | Revocations keyed by subject only (not issuer)                                            | `crates/teamcraft-server/src/auth/revocations.rs:22-44`, `services/oidc.rs:25-60`                      |
| 16 | info               | Default config behind a reverse proxy collapses all clients into one rate-limit / auth-failure bucket | `crates/teamcraft-server/src/security.rs:170-197, 225-240`, `config.rs:306-308`                        |

No critical or high findings. No SQL injection found.

---

### Findings

#### 1. Web Push SSRF via DNS (medium)

**Status: fixed** in `9548566` (`fix/security-server`). Hostnames are resolved through a resolver that drops non-public addresses, the connection uses the checked address, redirects are off, and the private-range list covers NAT64, 6to4, Teredo, site-local, multicast and documentation ranges. Developer mode can still allow private targets.

**Location:** `crates/teamcraft-server/src/push/web_push.rs:40-73` (`check_endpoint`), `:152-163` (POST),
called at registration from `crates/teamcraft-server/src/services/devices.rs:163-167`.

`check_endpoint` blocks `localhost`, `*.localhost`, `*.local`, `*.internal` and literal private IPs, but
for any other hostname (`Host::Domain`) it never resolves the name. The sender later POSTs to the
endpoint with `reqwest` (redirects disabled, which is good).

**Exploit:** any authenticated user (guests and bots too) registers a Web Push subscription whose
endpoint is `https://internal.attacker.example/…`, with an A/AAAA record pointing to `10.0.0.5`,
`169.254.169.254`, `127.0.0.1` or a sidecar on the VPS. Every message notification then makes the
server POST to that address. This is a blind SSRF: the response body is not returned, but the status
code is a side channel (404/410 cause the registration to be deleted, see `push/routing.rs:238-248`),
and POST-reachable internal admin APIs can be triggered. DNS rebinding between registration and send
also works because the name is resolved again at send time. `is_private_ip` also misses some ranges
(IPv4-compatible `::a.b.c.d`, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`, `0.0.0.0/8`, `198.18.0.0/15`).

**Fix:** resolve the host at send time and refuse private/reserved addresses, then connect to the
vetted IP (custom `reqwest` resolver / `dns::Resolve` that filters results, so there is no TOCTOU).
Alternatively allowlist known push-service domains (FCM, Mozilla autopush, Apple, Windows) as browsers
only hand out those. Extend `is_private_ip` with the ranges above.

#### 2. Admin can deactivate owners and other admins (medium)

**Status: fixed** in `f3c437e` (`fix/security-server`). Deactivating, reactivating and revoking devices need a rank at least as high as the target's (owner above admin above everyone else); only an owner may act on an owner.

**Location:** `crates/teamcraft-server/src/services/admin.rs:149-176` (`deactivate_user_as_admin`),
`:254-262` (`revoke_user_device`), `crates/teamcraft-server/src/services/devices.rs:108-140`.

Both require only `ManageUsers` at instance scope, which the default scheme grants to `admin`
(`permission.rs:204-209`: admin = everything except `SignWorkspace`). The target user's role is never
compared with the caller's. The only guard is "not yourself" (`admin.rs:156`).

**Exploit:** a rogue or compromised admin account deactivates every owner (`POST
/api/v1/admin/users/{owner}/deactivate`) and removes their devices. Owners are the only role that can
sign roster/policy (`SignWorkspace`); deactivated owners can't log in (`accounts.rs:94-96`), so the
workspace root of trust is locked out until someone reactivates them. It also lets an admin remove
another admin's devices to force re-enrollment. Audit entries exist, but the action is not prevented.

**Fix:** forbid acting on users whose stored OIDC role (`maybe_oidc_role`) is >= the caller's role
(admins may only deactivate members/guests/bots; owners may act on admins). Apply the same rule to
`revoke_user_device` and `reactivate_user`. Add tests for the role × target-role matrix.

#### 3. ID tokens accepted as access tokens; raw ID tokens published (medium, uncertain)

**Status: fixed** in `a73700f` (`fix/security-server`). The `typ` claim must be `Bearer` (or absent), so ID tokens no longer pass as access tokens.

**Location:** `crates/teamcraft-server/src/auth/validator.rs:190-242` (`verify`), `:245-255`
(`is_issued_to`); `crates/teamcraft-server/src/services/identity.rs:84-90, 114-131`;
`crates/teamcraft-core/src/attestation.rs:1-9` / `client/enroll.rs:1-6` (attestation = the refreshed
OIDC ID token).

`verify` checks signature, `iss`, `exp`, `nbf`, `sub`, client binding (`azp == client_id` or
`client_id ∈ aud`) and client roles, but not the token type. Keycloak ID tokens have `aud = <client_id>`
and `azp = <client_id>`, so they pass `is_issued_to`. The client uploads its ID token as
`maybe_attestation` with `PUT /me/identity-key`, and `get_identity_key` returns it verbatim to every
user who can see the owner (all members; guests for co-members).

**Exploit:** if the realm's client-role mapper is configured with "Add to ID token" (or a custom
mapper puts `resource_access` in the ID token), any member who fetches `GET
/users/{id}/identity-key` shortly after the victim enrolled gets a token TeamCraft accepts as the
victim's bearer token (REST and gateway) until it expires (`accessTokenLifespan`, 300 s in the dev
realm). Keycloak's default is to not put client roles into ID tokens, so with a default realm the
token fails with `NoTeamcraftRole`; hence "uncertain". Independently, publishing raw ID tokens hands
everyone the victim's `sid`, email and other claims.

**Fix:** reject tokens whose `typ` claim is not `Bearer` (Keycloak sets `typ: "ID"` for ID tokens and
`"Logout"` for logout tokens) and optionally require `azp == client_id`. On the attestation side, only
accept it if expired-or-short-lived is acceptable, or better: let clients verify an attestation whose
audience is *not* the API (e.g. a dedicated client/scope), or strip it to a signed claim set that is
not a usable bearer token.

#### 4. Gateway frames are not rate-limited; unbounded `last_typing` (medium)

**Status: fixed** in `d30c1d9` (`fix/security-server`). A token bucket limits client frames per connection (`gateway.max_client_frames_per_minute`, `client_frame_burst`), activity changes are limited to one per second, and the typing map is capped and only filled after a successful check.

**Location:** `crates/teamcraft-server/src/gateway/connection.rs:196-262`.

After `Identify`, every frame is processed without any rate limit (the HTTP `RateLimits` only sees the
upgrade request):

- `SetActivity` (`:206-209`) calls `presence::refresh_presence` each time: two DB queries, and on
  change a `users_sharing_conversation` query plus a broadcast to everyone sharing a conversation
  (`services/presence.rs:46-73`). Toggling `active`/`idle` makes every frame a broadcast.
- `Typing` (`:210-226`) inserts the conversation ID into `last_typing` *before* validating it, so a
  client sending `Typing` for many distinct (non-existent) conversation IDs grows the per-connection
  `HashMap` without bound, and each frame costs `load_conversation` (2 DB queries).
- `Heartbeat` replies are unthrottled.

**Exploit:** one authenticated user (guest or bot is enough) streams small frames over one WebSocket
and generates thousands of DB queries / fan-out events per second, bypassing `user_writes_per_minute`.

**Fix:** a per-connection token bucket for client frames (close with 1008 on abuse), throttle
`SetActivity` like typing, validate before inserting into `last_typing` and cap its size (or use an
LRU/time-pruned map).

#### 5. Unbounded gateway sessions per user (low-medium)

**Status: fixed** in `d30c1d9` (`fix/security-server`). `gateway.max_sessions_per_device` and `max_sessions_per_user` evict the oldest sessions.

**Location:** `crates/teamcraft-server/src/gateway/hub.rs:102-124` (`open_session`), `:184-216`
(`publish`), sweep at `:325`.

There is no cap on sessions per user or device. Each `Identify` creates a new session with an
`mpsc` channel (1024) and a resume buffer (default `resume_buffer_events = 1000`) that survives
`resume_window_secs` (120 s) after disconnect. `publish` clones each event into every session buffer
while holding the global hub mutex.

**Exploit:** an authenticated user opens/closes gateway connections at the per-IP request rate
(1200/min default), accumulating thousands of live/detached sessions; every event for that user is
then cloned thousands of times under the global lock (memory and latency for all users).

**Fix:** cap sessions per (user, device) – replace the previous session on a new `Identify` for the
same device – and cap sessions per user.

#### 6. iMIP REPLY spoofing and amplification (low-medium)

**Status: fixed** in `41a28bf` (`fix/security-server`). A reply is accepted only from the envelope sender that matches the attendee it answers for (`X-Envelope-Sender` is required), and the attendee and line counts are capped.

**Location:** `crates/teamcraft-server/src/meetings/ical.rs:148-196` (`parse_reply`),
`crates/teamcraft-server/src/services/meetings.rs:328-375` (`receive_reply`, `apply_email_rsvp`,
`after_rsvp`), handler `handlers/receive_meeting_reply_handler.rs`.

`parse_reply` collects every `ATTENDEE;PARTSTAT=…:mailto:…` line in the event with no limit, and
`receive_reply` applies each. The internal endpoint is authenticated by the inbox token, but the
content comes from arbitrary e-mail senders: "the caller vouches for the sender" is not enforced
(the sender address is never compared with the attendee).

**Exploit:** any external invitee knows the meeting UID and all attendee addresses (they are in the
invitation `.ics`). Mailing a REPLY with all attendees and `PARTSTAT=DECLINED` flips everybody's
RSVP. Each entry also triggers `after_rsvp` → `sync_calendar` (a CalDAV PUT) and a gateway announce,
so a ~4 MB body with tens of thousands of attendee lines becomes as many DB writes and CalDAV
requests.

**Fix:** accept exactly one ATTENDEE per REPLY (RFC 5546 §3.2.3), require the inbox handler to pass
the authenticated envelope sender and match it, cap lines/entries in the parser, and run
`after_rsvp` once per reply instead of once per entry.

#### 7. Missing per-user quotas (low-medium)

**Status: fixed** in `508368a` (`fix/security-server`). New `limits.max_devices_per_user`, `max_pending_attachments_per_user`, `max_attachment_bytes_per_user` and `max_meeting_mails_per_user_per_hour`.

**Locations:**

- Devices: `crates/teamcraft-server/src/services/devices.rs:17-59` – no limit per user. Every
  registration broadcasts `DeviceAdd` to all users who share a conversation; their clients then claim
  key packages and commit adds. A member or bot can force work on every co-member's client.
- Attachments: `crates/teamcraft-server/src/services/attachments.rs:22-57` – size is capped per
  object, but there is no per-user/per-conversation storage quota or cap on pending (incomplete)
  attachments: 600 writes/min × `max_attachment_bytes` of S3 storage per user.
- Meeting mail: `crates/teamcraft-server/src/services/meetings.rs:193-265` – every `PATCH` re-sends
  "update" e-mails to all kept invitees with attacker-chosen title/description; with
  `max_invitees` external addresses per meeting and the write rate limit, a member can use the
  instance's SMTP identity for bulk mail/phishing.

**Fix:** configurable caps (devices per user, outstanding attachments and bytes per user, mails per
user per hour) as the "Limits everywhere" rule in `.claude/rules/security.md` asks.

#### 8. Denied guests keep a working session (low)

**Status: fixed** in `8d51173` (`fix/security-server`). Denying a guest deactivates the account, so every session ends and later requests get 403.

**Location:** `crates/teamcraft-server/src/services/meeting_guests.rs:185-208` (`deny`),
`crates/teamcraft-server/src/services/accounts.rs:21-29`.

`deny` only drops current gateway connections (`hub.revoke_user`). The HS256 guest session token
stays valid until `meeting.ends_at + guest_grace`; it is neither recorded in `Revocations` nor is the
guest user deactivated. `validate_token` also ignores the `meeting` claim, so the session is a general
guest login rather than a meeting-bound one.

**Impact:** limited (the guest is a member of nothing, guest directory/DM restrictions apply when
`guests_dm_only_with_shared_conversations` is on, which is the default), but a denied guest can
reconnect to the gateway, poll lobby state, register devices/identity keys and consume resources
until the meeting ends. If an admin later turns the DM restriction off, a denied stranger can DM
users by ID.

**Fix:** on deny, deactivate the guest account (or revoke the guest subject in `Revocations` and
persist it), and reject guest tokens whose lobby entry is `denied`.

#### 9. Logout tokens are replayable (low)

**Status: fixed** in `a73700f` (`fix/security-server`). Logout tokens must be recent (10 minutes) and their `jti` is remembered; a replay is refused.

**Location:** `crates/teamcraft-server/src/auth/validator.rs:150-187`; effect in
`crates/teamcraft-server/src/services/oidc.rs:25-60`.

`set_required_spec_claims(&["iss", "aud"])` drops `exp` from the required set; `jsonwebtoken` only
checks `exp` when present, so a logout token without `exp` is valid forever. `iat` is only checked for
non-zero, and `jti` is required but never remembered. The revocation is applied at *server receive
time* (`now`), not at `iat`.

**Exploit:** whoever obtains one logout token (proxy logs, misrouted request) can replay it to the
unauthenticated `/api/v1/auth/backchannel-logout` endpoint at any time and log the user out of every
session again, repeatedly.

**Fix:** require `exp` (OIDC Back-Channel Logout 1.0 §2.4 lists it as required), reject `iat` older
than a few minutes, keep a short-lived `jti` replay cache, and also check `typ`/`events` as now.

#### 10. Gateway error frames leak internal error text; gateway auth failures not throttled (low)

**Status: fixed** in `d30c1d9` (`fix/security-server`). Error frames carry the public message only, and failed gateway handshakes count towards the per-IP authentication throttle.

**Location:** `crates/teamcraft-server/src/gateway/connection.rs:145-151` (and `:219-223`); same
pattern in `crates/teamcraft-server/src/rendezvous/connection.rs:118-121` callers.

HTTP responses use `ApiError::public_message()` (`error.rs:171-178`), which hides `Database`,
`Internal` and `KeysUnavailable` details. The gateway builds `ErrorBody::new(error.error_code(),
error.to_string())`, so a pre-authentication client can see e.g. `signing keys unavailable: GET
https://…/protocol/openid-connect/certs: …` or database error text. Also, failed `Identify`
attempts never produce an HTTP 401, so `record_auth_failure` (`security.rs:186-188`) never counts
them; token guessing over the gateway is bounded only by the per-IP request limit (practically
irrelevant for 256-bit bot tokens and signed JWTs, but inconsistent).

**Fix:** make `public_message()` `pub(crate)` and use it in gateway/rendezvous error frames; call
`record_auth_failure` for failed handshakes.

#### 11. Server does not enforce `admin_self_join` / admin channel management (low, design)

**Status: fixed** in `9bf6710` (`fix/security-server`). Adding oneself to a private channel as a non-member admin is refused unless the signed policy `admin_self_join` is `visible`.

**Location:** `crates/teamcraft-server/src/services/members.rs:68-127` (`add_member`),
`services/conversations.rs:176-252` (`update_conversation`), `permission.rs:336-344`
(`ChannelManagement` reachable by instance admins without membership).

An instance admin who is not a member can add themselves (or anyone) to any private channel, flip
it to public, and set `history_policy = full`. admin.md §2.4 says member clients enforce
`admin_self_join`, so ciphertext stays protected; but the server grants the membership row
immediately, which exposes metadata (member list, read receipts, call roster, attachment download
URLs for new ciphertext, push metadata) even when the signed policy says `disabled`.

**Fix (defence in depth):** have the server read `admin_self_join` from the current signed policy
(`workspace::current_policy`) and refuse self-adds by non-member admins when `disabled` (or require
an owner approval record for `owner_approval`).

#### 12. Client binding via `aud` only (low)

**Status: fixed** in `a73700f` (`fix/security-server`). The client binding requires `azp` to equal the configured client; `aud` is no longer used.

**Location:** `crates/teamcraft-server/src/auth/validator.rs:202-220, 246-255`.

`validate_aud = false`; a token is accepted if `azp == client_id` *or* `aud` contains the client ID.
In Keycloak, with "Full scope allowed" (default) any other client in the realm gets `aud:
["teamcraft", …]` once its token carries TeamCraft client roles. A token minted for a different,
less trusted relying party of the same realm can therefore be replayed to TeamCraft by that party.

**Fix:** require `azp == client_id` (TeamCraft's own public client), or a dedicated audience mapper
and `aud` validation; document that other realm clients must not get TeamCraft client roles in scope.

#### 13. Presigned upload URL is reusable (low)

**Status: mitigated** in `09a80f1` (`fix/security-server`). The misleading doc comment is corrected. Open: a SigV4 presigned PUT can still be repeated until it expires; clients detect tampering through the attachment AEAD.

**Location:** `crates/teamcraft-server/src/storage.rs:64-72`.

The doc comment says "accepts exactly one upload", but a SigV4 presigned PUT can be repeated until it
expires, including after `complete_attachment`. The uploader can swap the blob contents (same size)
after recipients started downloading. Client-side AEAD/hash (`teamcraft-crypto/src/attachment.rs:112`)
detects tampering, so impact is integrity-DoS only.

**Fix:** fix the comment; optionally use `If-None-Match: *` conditional PUT (supported by recent S3 /
Garage) or a short expiry.

#### 14. Secret-bearing types with `Debug` / no zeroization (low)

**Status: fixed** in `09a80f1` (`fix/security-server`). `PushTarget` redacts its `Debug` output and zeroizes token and push key on drop, `RegisterPushRequest` redacts its fields, and the bridge keeps its source token in a `SecretString`.

- `RegisterPushRequest` (`crates/teamcraft-proto/src/api/users.rs:68-73`) derives `Debug` with
  `token` and `push_key` (the 256-bit key that seals push payloads).
- `PushTarget` (`crates/teamcraft-server/src/push/sender.rs:13-22`) derives `Debug`, holds
  `push_key: Vec<u8>` and the push token in plain `String`/`Vec`, not zeroized.
- Bridge `Source.token` (`crates/teamcraft-bridge/src/http.rs:47-73`) copies the `SecretString` into
  a plain `String`.

None is currently logged (I grepped all `tracing` macros), so this is a hardening item per
`.claude/rules/security.md` ("don't implement Debug with content", "zeroize on drop").

**Fix:** hand-written `Debug` with `[redacted]` (as done for `JoinMeetingRequest`,
`BotTokenResponse`, `GatewayClientMessage`), and `SecretBox<[u8]>` / `SecretString` for the fields.

#### 15. Revocations keyed by subject only (info)

**Status: fixed** in `a73700f` (`fix/security-server`). Revocations are keyed by issuer plus subject or session.

`Revocations` (`crates/teamcraft-server/src/auth/revocations.rs:22-44`) stores subjects without
issuer. Bot subjects are user IDs and guest subjects are invite IDs (decimal snowflakes), IdP
subjects are UUIDs, so collisions are not practical today. Key the map by `(issuer, subject)` to keep
it that way.

#### 16. Reverse-proxy default (info, operational)

**Status: fixed** in `09a80f1` (`fix/security-server`). The server warns once when requests look like they come through an unconfigured reverse proxy; docs/operations.md and `teamcraft.example.toml` say what a proxy has to set.

With `trust_forwarded_for = false` (default, `config.rs:306-308`) behind a reverse proxy, every client
has the proxy's address: all users share one 1200/min bucket, and 30 bad tokens per minute from
anyone block *everybody* via the auth-failure bucket (`security.rs:170-197`). Document this
prominently in `teamcraft.example.toml` / deployment docs, or detect a private peer address plus
`X-Forwarded-For` and warn at startup.

---

### Checked and found correct

- **Admin API:** every `admin_*` handler takes the `AdminUser` extractor (rejects non-admins before
  body parsing, `auth/extractor.rs:42-56`) and the service re-checks the permission
  (`ManageWorkspace`/`ManageUsers`/`ReadAuditLog`). Role comes from the token on every request.
  Admin endpoints return metadata only; `admin_list_conversation_members` refuses DMs
  (`admin.rs:85-108`). Admins can't read messages: `list_messages`, `list_mls_messages`,
  `download_attachment`, read receipts, acknowledgements, threads, call tokens all require
  membership.
- **IDOR:** conversation, space, message, attachment, meeting, key bag, device, push and identity
  endpoints all go through `load_conversation`/`load_space`/`visible_meeting`/`visible_user`/
  `is_active_device_of(caller, …)`; non-visible objects return "not found". Key bag is always the
  caller's own. Category and thread lookups check they belong to the path's space/conversation.
- **Permission evaluator** (`permission.rs:294-354`): single place, guests forced to guest base role,
  channel moderation can only remove, `ChannelContent` requires membership and not archived,
  `Unknown` roles/permissions deny.
- **Guests:** directory search, `get_user`, presence and DM creation are restricted to people who
  share a conversation (default policy on); guests can't join spaces/channels themselves, can't see
  space rosters or the channel browser; meeting guests sit in the lobby with no membership until a
  host (organizer or chat admin, `host_meeting`) admits them; invite tokens are one-time
  (`redeem_meeting_invite`) and bound to the meeting; guest secret must be >= 32 bytes; HS256 is
  only routed to the guest verifier, IdP tokens are checked against a configured algorithm list
  (algorithm confusion test present).
- **MLS:** commits/messages require membership and the caller's own active device; welcomes must
  target devices of members (no duplicates); payload sizes and welcome counts limited; backlog
  limited to `joined_floor`; key package claims require shared membership, the target must be a
  member, last-resort packages soften draining.
- **Roster/policy append:** `SignWorkspace` (owners only), signer device must be the caller's,
  version-checked append-only log, policy payload validated (no compliance archive, sane values).
- **JWT:** `kid` required, JWKS refresh rate-limited (`min_refresh_interval`), discovery issuer
  checked, response size capped, stale keys used on outage, `exp`/`nbf`/`iss`/`sub` required, empty
  `sub` rejected, highest configured role wins, error messages sanitized.
- **Bot tokens:** 32 random bytes from `aws_lc_rs::rand`, only SHA-256 stored, looked up by hash
  (no secret comparison in app code), rotation and revocation invalidate immediately, revoked bots
  deactivated; `BotTokenResponse` redacts `Debug`.
- **Constant-time comparisons:** SFU registration token (`services/calls.rs:173-180`), inbox token
  (`meetings/mod.rs:125-130`) and bridge source tokens (`teamcraft-bridge/src/http.rs:90-99`, loops
  over all sources) use `subtle::ConstantTimeEq`. The only `==` on a digest
  (`teamcraft-crypto/src/attachment.rs:112`) compares a public ciphertext hash, which is fine.
- **Bridge:** unknown tokens get 404, tokens >= 16 chars from a restricted alphabet and unique,
  per-source rate limits, 1 MiB body limit.
- **DoS limits:** global `DefaultBodyLimit` (4 MiB), WebSocket `max_message_size` (64 KiB gateway,
  16 KiB rendezvous), identify/join timeouts, heartbeat timeout, connection max age, slow consumers
  dropped (`try_send`), per-IP (IPv6 per /64) and per-user write token buckets, stricter bucket for
  unauthenticated endpoints, pagination clamped to `max_page_size` everywhere checked, attachment
  size signed into the presigned URL and verified on completion, ciphertext/MLS/key-package/key-bag
  size limits, iCalendar output escaping and e-mail header-injection checks.
- **SQL:** all queries use `sqlx::query!`/`query_as!` with bind parameters. The only `format!`-built
  SQL is `drop_empty_message_partitions` (`teamcraft-db/src/retention.rs:308-316`), where the table
  name comes from `pg_class` filtered by `^messages_[0-9]{4}_[0-9]{2}$` and re-parsed, and the test
  harness. LIKE patterns are escaped (`user.rs:273-278`, `admin.rs:191-197`).
- **Logging:** no `tracing` call logs tokens, keys, ciphertext, push payloads or e-mail addresses;
  internal errors are logged server-side and replaced by generic text in HTTP responses; tokens never
  travel in URLs (gateway/rendezvous authenticate in the first frame, guest invite token in a
  fragment/body). API responses get `no-store`, `nosniff`, CSP and optional HSTS.

## Part E: supply chain

### E1. Vendored `davey` matches its documentation (Info, verified)

`crates/vendor/davey` was diffed against the crates.io `davey 0.1.5` sources in the local cargo
registry. The only differences are the ones listed in `crates/vendor/davey/VENDORED.md`:

- `Cargo.toml`: openmls 0.8.1 → 0.9.1, `openmls_basic_credential` 0.6.0, `openmls_rust_crypto` 0.6.0,
  added `web-time`, empty `[workspace]` table.
- `src/session.rs` `process_commit`: own-commit handling moved from the
  `StageCommitError::OwnCommit` error arm to `ProcessedMessageContent::OwnPendingCommit`
  (`crates/vendor/davey/src/session.rs:630-634`). An echoed commit that does not match the pending one
  now fails with `OwnCommitMismatch` inside `ProcessingMessageFailed`; no new acceptance path.
- `src/cryptor/{encryptor,decryptor,cryptor_manager}.rs`: `std::time::Instant` → `web_time::Instant`.

No other source changes. `process_commit` itself accepts any valid staged commit from a group member;
it does not re-check that the commit only contains proposals previously approved in
`process_proposals` (where `expected_user_ids` is checked). TeamCraft compensates by verifying the
member set against the call key bindings after each commit (part C, "done correctly"), so this is
**Info**; worth keeping in mind if that post-commit check is ever relaxed.

Fix suggestion: send the upstream PR in `UPSTREAM_PR.md` and drop the vendor copy once released;
until then re-diff on every davey update as VENDORED.md describes.

### E2. `deny.toml` advisory ignores (Info, verified)

`cargo deny check advisories sources bans` passes on `9667ef3`. All 14 ignored advisories are
"unmaintained" notices with a reason, for build-time or proc-macro crates (uniffi_bindgen/askama,
Leptos, glib/hax-lib macros, tauri-utils urlpattern/plist/quick-xml) or the zbus 5 → 6 merge; none is
a vulnerability advisory in code that handles network input. `sources` denies unknown registries and
git sources; `openssl`/`native-tls` are banned. The RUSTSEC-2026-0207/0208/0212 libcrux issues that
motivated the vendoring are resolved (openmls 0.9 stack).

Low-priority suggestions: the two quick-xml ignores (RUSTSEC-2026-0194/0195) are argued as
"build-time trusted input"; re-check on each Tauri update that plist parsing never reaches runtime
data. Add an expiry convention (e.g. a review date in each reason) so ignores get revisited.

