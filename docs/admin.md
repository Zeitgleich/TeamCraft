# TeamCraft – Administration

Goal: server admins can manage **every** space and channel – including private ones they're not in –
but can't read messages of conversations they aren't a member of.

Status: design draft. Builds on [e2ee.md](e2ee.md).

## 1. Roles

| Role                          | Scope       | Can                                                                         |
|-------------------------------|-------------|-----------------------------------------------------------------------------|
| **Server owner**              | instance    | everything an admin can, plus manage admins and the org recovery key shares |
| **Server admin**              | instance    | manage all spaces, channels, users, guests, settings (§3)                   |
| **Space admin**               | one space   | manage that space's channels and members                                    |
| **Channel owner / moderator** | one channel | manage that channel's members, settings, pins; delete messages              |
| Member, guest                 | –           | normal use                                                                  |

Permissions follow the scheme in PLAN.md §13 (instance / space / channel scope; a channel can only
restrict). Custom roles: later.

## 2. Managing rooms without reading them

### 2.1 What an admin sees for every conversation

- Name, icon, description (unless the space encrypts names, then "Encrypted channel"), space, category
- Members and their roles, guests in it, history policy, retention policy
- Created at, created by, last activity, message count, storage used
- Admin actions in the audit log

**Not** visible: message text, attachments, reactions, thread contents – the admin's devices aren't
MLS members and don't have the content keys.

### 2.2 How admin changes work without being a member

Adding or removing members changes the MLS group, and only members can commit MLS changes. So admin
actions run in two steps:

1. The admin's client creates a **signed admin action** (e.g. "add Anna to #project-a-dev",
   "remove Max from #project-b-internal"), signed with the admin's device key.
2. The admin's client posts the signed action into the group's queue first and only then asks the
   server to drop the membership, so a crash in between can't leave a removal without its signed
   action (e2ee.md §4.6, removal order). The server enforces it immediately for access control (e.g.
   stops delivering to removed users) and hands the signed action to the group's member clients. v1 delivers it as a pending change; the
   committing member puts it into the commit's authenticated data (e2ee.md §4.6). Later option: send
   it as an MLS **external proposal** (RFC 9420: the workspace's admin key registered as an
   `external_senders` entry in every group); the signed action stays the same.
3. The next online member client checks the signature and the admin's role (§2.3), commits the change
   automatically and, for adds, shares history keys according to the channel's history policy.
   Members see a system message: "Admin Paul added Anna".

If no member is online, the change is cryptographically pending; access control on the server already
applies. Removal is completed by the next commit, which also rotates the content key.

### 2.3 Why the server can't fake admin actions

The VPS is untrusted, so "the server says Paul is admin" isn't enough – otherwise a compromised server
could add itself to every channel.

- At setup, the **server owner's identity key** becomes the workspace root of trust. Every client pins
  it on first login (shown once, can be verified like a contact).
- The **admin roster** (who is owner/admin) is a signed, append-only log: each change is signed by an
  existing owner. Owners' clients sign changes automatically when they match the OIDC provider's
  signed role claims (ARCHITECTURE.md §5.1.1).
- Each roster and policy entry names the hash of the previous entry of its log and when it was
  signed; a policy also names the roster version its signer was an owner in, and is checked against
  exactly that roster. Clients apply rosters before policies and refuse a policy naming an older
  roster than the previous policy did; owners' clients sign the current policy again after every
  roster change, so an owner removed later can't sign a "back-dated" policy.
- **Freshness.** Owners' clients sign the roster again regularly; when the newest roster a client has
  is older than the accepted age (default 30 days, a client setting), admins have no authority on that
  client until an owner re-signs. A server that holds back a demotion gains at most that long. For
  commits the age is judged at the server's time of the commit, so every member decides the same way
  whatever its clock shows (e2ee.md §4.6).
- **Never without an owner.** A roster must name at least one owner with a usable identity key:
  nobody could sign the next roster or policy otherwise. Owners' clients refuse to sign such a
  roster (`last_owner`), the server refuses to store one, and every client refuses to apply one even
  when it is validly signed (an older or compromised owner device), keeping the last verified roster.
- **Fork detection.** Members send their verified roster and policy heads inside their encrypted
  messages. A colleague with a different entry for the same version, or with entries this client
  isn't shown, raises an alarm.
- Space admin and channel owner assignments are signed by an admin or a higher role.
- Member clients only commit membership changes that are signed by someone allowed to make them
  (admin, space admin, channel owner, or a member with `manage_members`). Unsigned or wrongly signed
  proposals are rejected and reported.

#### 2.3.1 Owner lifecycle

The workspace can only be administered while an owner with a working device exists: owners sign
every roster and policy, and admin authority lapses on member clients 30 days after the newest owner
signature (freshness above).

- **Keep at least two owners.** The Security section of the console lists the owners with the time
  each last signed a roster or policy, as this device verified it, and warns when there are fewer
  than the recommended number. That number is part of the signed policy (`owners.recommended_minimum`,
  default 2; 1 switches the warning off).
- **When an owner leaves, add the successor first, then remove the old owner.** Removing the last
  owner is refused, so a handover always passes through a roster with both.
- **The first roster.** The first policy save in a workspace without a signed roster signs the
  genesis roster too: the signer as first owner (the root of trust), proposed together with the
  owners and admins the identity provider lists. That list comes from the server, so it goes
  through the same confirmation as later edits (user ID, fingerprint, attested, trusted on first use
  or own key) before anything is signed; the owner can also sign themselves alone and add the
  others later. The core signs only the confirmed list (`publish_policy_with_genesis`) and ignores
  it once a roster is verified.
- **Editing the roster.** Administration → Security → Owners lists the signed roster. An owner on a
  device with signing keys (desktop or web) can add a person as owner or admin, make an owner an
  admin or the other way round, or remove someone. The console shows the changes, refuses a roster
  without an owner, and signs the new version on the owner's device (`publish_roster`, then the
  current policy again under it). People need an identity key (they set up TeamCraft once) before
  they can be added.
- **Confirm before signing.** Names come from the server and can be faked, so nothing is signed
  from the editor directly: a confirmation lists every change with the user ID and the fingerprint
  of the identity key this device pinned, marks keys that the identity provider didn't attest, that
  aren't pinned yet (they are pinned when signing) or that the server now shows changed, and only
  then signs. The owner compares ID and fingerprint with the person.
- **Concurrent edits.** The editor signs only if the verified roster is still the version it
  started from (`publish_roster_based_on`); if another owner signed meanwhile, nothing is signed
  (`roster_changed`) and the owner edits the new roster.
- **Identity provider roles.** The `owner` and `admin` roles in the identity provider don't change
  the signed roster by themselves: the server can't sign. When they differ from it, the console
  lists the difference, and an owner signs the identity provider's role holders through the same
  confirmation.
- **Admins can't sign.** For admins the form is read-only; the core refuses to sign a roster or
  policy for anyone who isn't an owner of the verified roster (`not_an_owner`), and every client
  would refuse such an entry anyway.
- **Keep an owner's app in use.** An owner's client signs the roster again by itself when half of
  the 30 days have passed. The console warns a week before the window runs out (a client setting,
  `workspace_expiry_warning`) and once it has: then an owner only needs to open TeamCraft.
- **All owners lost** (every owner left, or lost all devices and the recovery passphrase): nobody can
  sign a roster or policy again, and a new genesis is refused by every client that pinned the root of
  trust (`RootChanged`). Admins keep their authority until the freshness window runs out; after that
  the workspace can't be administered. A recovery path is still open (issue #188); until it exists,
  the only way out is setting the workspace up from scratch (operations.md, "Owners").

### 2.4 Admins joining private channels

An admin could add *themselves* to a private channel and then, with full history policy, read its
history. That is allowed, but never silent:

- Per instance setting `admin_self_join` for private channels:
  - `visible` (default) – allowed; members see the same system message as for any added member
    ("Paul joined"), marked with an admin badge. Unlike normal join/leave messages, it can't be
    hidden by the user setting,
  - `owner_approval` – a channel owner must approve,
  - `disabled`.
- Member clients enforce this setting themselves (it's part of the signed workspace policy), so the
  server can't bypass it.

### 2.5 Orphaned channels

When the last owner of a private channel leaves the company, an admin assigns a new owner from the
remaining members. The admin doesn't need to be a member for that.

## 3. Admin features

### 3.1 Spaces and channels

| Feature                                                                                                                     | Prio  |
|-----------------------------------------------------------------------------------------------------------------------------|-------|
| List all spaces/channels incl. private ones, filter (space, private, with guests, inactive, size)                           | v1    |
| Create, rename, describe, archive, unarchive, delete spaces and channels                                                    | v1    |
| Move channel between spaces, reorder categories                                                                             | v1    |
| Add/remove members, change roles, assign owners/space admins                                                                | v1    |
| Set posting permissions (announcement channels), history policy, guest access                                               | v1    |
| Retention per space/channel: last N days and/or last N messages; admin defaults and maximums, owners choose within them     | v1    |
| Integration channels: who may create them, register bridges, default retention; allow server-readable mode (off by default) | v1    |
| Delete a channel completely (all ciphertext + attachments)                                                                  | v1    |
| Default spaces/channels for new users (`policy.default_spaces` / `default_channels`, applied once at first sign-in)         | v1    |
| Space templates                                                                                                             | later |

### 3.2 Users and guests

| Feature                                                                                                          | Prio |
|------------------------------------------------------------------------------------------------------------------|------|
| User list with status, devices, last seen, role, spaces                                                          | v1   |
| Deactivate (suspend) user, also automatically when disabled in the OIDC provider; reactivate restores everything | v1   |
| Remove a deactivated user from all chats (devices leave every group, keys rotate); optionally after a set period | v1   |
| Delete an account: a tombstone stays on old messages, direct messages go, the sign-in is released                | v1   |
| Remove a single device (lost phone)                                                                              | v1   |
| Start org recovery for a user (needs a second admin, see e2ee.md §6.3)                                           | v1   |
| Invite guests, set expiry and sponsor, see all guests and where they are                                         | v1   |
| Local accounts: create, reset password/TOTP                                                                      | v1   |
| OIDC group → role/space mapping                                                                                  | v1   |
| User groups (`@devteam`)                                                                                         | v1   |

#### 3.2.1 Deactivating, reactivating, removing and deleting users

Deactivation and removal are two steps (issue #631); deletion is the final one (issue #630):

| Step                      | What happens                                                                                                                                                                                                                                                                                                                                                                                                          | Reversible                                  |
|---------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------|
| **Deactivate** (suspend)  | Gateway connections close and can't resume, local sessions end, every request and sign-in (SSO and local) is refused with `account_deactivated`, no pushes are sent (registrations are kept). Memberships, space memberships and devices stay; others see the user as deactivated.                                                                                                                                    | Yes: reactivate, same devices sign in again |
| **Reactivate**            | Lifts a deactivation: the same account signs in again with its devices and whatever memberships it still has. Not possible after a deletion.                                                                                                                                                                                                                                                                          | –                                           |
| **Remove from all chats** | Only for deactivated users. The admin's client signs the user's removal from every channel they are in; members' clients commit it, which rotates the keys (e2ee.md §4.6). Then the server revokes the devices and drops direct messages and spaces.                                                                                                                                                                  | No: they have to be added again             |
| **Delete account**        | The removal from all chats, then: devices revoked (push registrations and key packages go), identity key, key bag, local account, sessions, profile and recovery shares removed, the OIDC identity released (signing in again creates a new, empty account), 1:1 direct messages deleted. The row stays as a tombstone named "Deleted user" because messages, reactions, the audit log and call history reference it. | No: final                                   |

- **Delete account** (`DELETE /admin/users/{id}`, audit `user_deleted`). Like "Remove from all
  chats", it runs on the admin's client, which needs its device keys: it signs the user's removal
  from every channel (`GET /admin/conversations?user_id=`) and commits the removal in every group of
  its own where the user is left over, and only then calls the server. The console asks for the
  account's name to be typed and explains deactivating versus deleting. Rules:
  - Admins delete members and guests; only owners delete admins and owners (403). Nobody deletes
    their own account (409 `cannot_delete_self`), and deleting the last active owner (400
    `last_owner`) or admin (409 `last_admin`) is refused.
  - Idempotent: a retry after a lost answer finishes what is left and answers 204; the console's
    second try after a failed channel does the same.
  - **Direct messages.** A 1:1 DM with the deleted user is deleted entirely: the conversation, its
    messages and ciphertext, attachments (also in storage) and MLS group state. A group DM stays
    like a channel unless only one other person would remain; then it is deleted like a 1:1 DM. In
    a group DM that stays, the admin's device commits the removal if it is in the group; elsewhere
    the user is listed as left in the group for an admin who is (below). The
    remaining participants' devices drop it with its local history when the gateway announces
    `conversation_delete`, or on their next full sync (`deleted_conversation_ids` of
    `GET /conversations`, remembered for `[policy] deleted_conversations_kept_days`, default 180).
  - **Channels.** The user's messages stay; clients show the author as "Deleted user" (in their own
    language) in messages, profiles and member lists, and leave the tombstone out of people lists,
    the DM list and the mention picker. Optionally deleting their messages is out of scope: the
    server can't read them.
  - **The identity.** The OIDC issuer of the row gets a `deleted:{id}:` prefix, so the issuer and
    subject pair is free and the identity provider sync no longer sees the account. Only a sign-in
    after the deletion creates the new account: tokens from an earlier sign-in (`auth_time`, else
    `iat`, at or before the deletion), also ones refreshed since, are refused on REST, the gateway
    and device linking with 401 `account_deleted`. To keep the person out for good, disable them at
    the identity provider too.
  - **Signed roster.** A deleted admin or owner stays in the signed roster until an owner signs a
    new one; their devices are revoked, so the entry grants nothing.
- **Users left in encryption groups** (issue #734). The console's "Spaces and channels" lists, as one
  compact list, users still in the encryption group of a conversation the admin's device is in,
  whom the server no longer lists (issue #661): with the account's current name (marked when it is
  deactivated) and where (`#channel`, the direct message with someone, or a deleted conversation).
  "Remove from the group" (and "Remove from all" for several) makes the admin's device sign and
  commit the removal, which rotates the keys; nothing is removed on the server's word alone. The
  admin page of a deactivated user offers "Remove from all chats" also when only such leftovers
  remain.

- **Who triggers what.** An admin deactivates in the console (`POST /admin/users/{id}/deactivate`) and
  removes in a second, explicit step. "Remove from all chats" runs on the admin's client, which
  needs its device keys (desktop app, or a browser with the account set up; otherwise the console
  explains and offers nothing): it lists the user's channels (`GET /admin/conversations?user_id=`),
  signs a removal from each like any admin removal (§2.2), and only then calls
  `POST /admin/users/{id}/remove-from-chats` (409 `conflict` for an active user), which revokes the
  devices and drops direct messages and spaces. A channel that fails leaves the server step out; the
  admin removes again. Recommended for people who left the organization.
- **Automatic removal.** `server_rules.remove_deactivated_after_days` (config `[policy]
  remove_deactivated_after_days`, default none = never): the server removes users that long after
  their deactivation (checked with the account housekeeping). Without an admin's client this is the
  server's removal alone: it ends their memberships and devices on the server, but no client signs
  a removal on its word (security review A8), so the users stay in the encryption groups and are
  listed for admins in the console (issue #661) until an admin removes them. Users the identity
  provider sync deactivated in a burst (10 or more within an hour: an outage or a misconfigured
  provider) are left out of the automatic removal; an admin decides.
- **Direct messages.** Admins don't sign changes in DMs and group DMs; the server's removal drops
  the user there, and the participants' clients list them for admins like any server-only removal.
- **Share links** of a deactivated user are paused (they answer like an unknown link) and work again
  after a reactivation if they haven't expired.
- **Per reason** (`deactivation_reason`): `admin` and `oidc` only suspend; the OIDC sync reactivates a
  user it deactivated once the provider enables them again, with everything intact. Bots that are
  revoked and meeting guests (`left`, turned away, removed from the meeting, access ended) are
  removed from all chats at once: they don't come back.
- **Why removal is separate.** While suspended, the user's devices stay in the groups and new
  messages are still encrypted to them. The server refuses them every request, but a compromised
  server could keep handing them that ciphertext. Removal is what takes them out
  cryptographically: the admin's signed change, committed by members, rotates the keys. The
  server's own removal is only a hint to the clients (security review A8).

### 3.3 Moderation

Admins can't read reported content unless a member shows it to them:

| Feature                                                                                                                                                                                                                       | Prio |
|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------|
| **Report message**: a member sends the decrypted message + context to the admin team through an E2EE admin channel – the member's explicit choice                                                                             | v1.x |
| Delete a message by ID (from a report): server deletes ciphertext, clients honor a signed delete tombstone                                                                                                                    | v1.x |
| Channel moderators delete messages in their channels                                                                                                                                                                          | v1   |
| Mute/kick a user from a channel (the **channel moderator** role: `moderator`, grants `manage_members`, `delete_others_posts`, `remove_others_reactions`; can't manage roles, properties, privacy or remove admins/moderators) | v1   |

### 3.4 Instance

| Feature                                                                                                           | Prio |
|-------------------------------------------------------------------------------------------------------------------|------|
| Settings: name, logo, colors, space label ("Project"/"Space"/"Team"), file size limits                            | v1   |
| Security policy: `admin_self_join`, org recovery (members/guests, admins required), guest rules, history defaults | v1   |
| Desktop updates: allowed/disabled, channel, pinned/minimum version, mirror (`client_updates`, updates.md §4.4)    | v1   |
| SFU nodes: registration token, health, load                                                                       | v1   |
| Telephony: SIP trunks and their status, hotline config, queues                                                    | v1.x |
| Storage usage (Postgres, S3) per space                                                                            | v1.x |
| Audit log (metadata only), exportable                                                                             | v1   |
| Push setup (APNs, FCM, UnifiedPush)                                                                               | v1   |
| Backups status                                                                                                    | v1.x |

### 3.5 Call backgrounds

Admins upload pictures for the organization's call backgrounds; owners decide how they are used. The
rule is part of the signed workspace policy (`call_backgrounds`, §2.3), so member clients enforce it
themselves and the server can't relax it.

| Mode       | What people may use behind themselves                                                                      |
|------------|------------------------------------------------------------------------------------------------------------|
| `free`     | Anything; the organization's pictures are offered first, next to blur, built-in and own pictures (default) |
| `org_only` | The organization's pictures, blur or nothing                                                               |
| `required` | Always one of the organization's pictures while the camera is on; people pick which                        |

- **Default picture:** optional; preselected, and used when a choice isn't allowed (or none was made
  under `required`).
- **Guests:** exempt by default (`guests_exempt`), so external participants aren't forced into the
  organization's look; owners can include them.
- **Enforcement:** the client applies the rule when a background is chosen and when the camera turns
  on. Under `required`, a device that can't replace the background (no GPU or segmentation model) can't
  turn the camera on, so the room behind the person is never sent. The browser and the mobile apps don't
  send camera video yet; they get the same check when they do.
- **Storage:** the pictures are stored unencrypted (every call participant sees them anyway); the policy
  lists each one with its SHA-256, and clients refuse a picture whose hash doesn't match (e2ee.md §8.1).
- **Admin UI:** Administration → Backgrounds: upload (JPEG or PNG; re-encoded as a Full HD JPEG without
  metadata), delete, order, default, mode and guest exemption. Admins upload; publishing the list is a
  policy change, which needs an owner (`sign_workspace`). A picture the current policy lists can't be
  deleted on the server; removing it from the list first is one step in the UI.
- **Limits:** 50 organization pictures, each up to `max_background_bytes` (8 MiB). Clients also refuse
  pictures over 8 MiB or 36 megapixels before decoding them.
- **Deleting:** a presigned upload URL keeps working until it expires, so a deleted picture's row stays,
  and counts towards the limits, until then; a sweeper (every 10 minutes) then deletes the object once
  more and the row. Uploads announced but never completed are swept the same way. The same applies to
  people's own backgrounds.

## 4. Audit log

Every admin action, recovery, device change and role change is logged with actor, target and time.

- Admins see the full log.
- Members see admin actions affecting their conversations as system messages.
- Users see actions affecting themselves (device removed, recovery started) in their security settings.
- The log contains metadata only, never message content.

### 4.1 Server API of the admin features

What the server offers for the features above (all under `/api/v1`; instance admins unless noted).

| Feature                                  | Calls                                                                              |
|------------------------------------------|------------------------------------------------------------------------------------|
| Announcement banner                      | `PUT/DELETE /admin/announcement`; everybody: `GET /announcement`, `POST /announcement/{id}/dismiss` |
| Channels without an admin                | `GET /admin/conversations?orphaned=true`, `POST /admin/conversations/{id}/owner`   |
| System messages from the audit log      | `GET /conversations/{id}/system-events` (members), live as `system_event` gateway events |
| Permission scheme                        | `GET/PUT /admin/permission-scheme`, `GET/PUT /admin/spaces/{id}/permission-scheme` (grant, revoke, inherit) |
| Move a channel to another space         | `PUT /conversations/{id}/space` (`manage_space_properties` in both spaces; audit `channel_moved`, system event `moved`) |
| Channel moderation for members / guests  | `GET/PUT /admin/conversations/{id}/moderation` (channel admins use `PATCH /conversations/{id}`) |
| Local guest accounts                     | `POST /spaces/{id}/local-guests`, `POST /admin/local-accounts/{user}/activation`   |
| User groups                              | `/user-groups` (create, edit and manage members need `create_user_group` / `manage_user_group_members`) |
| Organization recovery                    | `/org-recovery/...`, see e2ee.md §6.3.1                                            |
| Share links                              | `/shares` (members), public `/shares/{id}`; `[shares]` configuration               |
| Call backgrounds                         | `POST /admin/backgrounds`, `POST /admin/backgrounds/{id}/complete`, `DELETE /admin/backgrounds/{id}`; everybody: `GET /org-backgrounds/{id}/download` |
| Brands (§6)                              | `GET/POST /admin/brands`, `PUT/DELETE /admin/brands/{id}`, `POST /admin/brands/{id}/take-over`, `GET/PUT/DELETE /admin/brands/{id}/assets/{slot}` |
| Mail server (§6.2)                       | `GET/PUT/DELETE /admin/settings/mail`, `POST /admin/settings/mail/test`            |
| Calendar (§6.2)                          | `GET/PUT/DELETE /admin/settings/calendar`, `POST /admin/settings/calendar/test`    |
| Instance settings (§6.3)                 | `GET/PUT /admin/settings`, `DELETE /admin/settings/{section}`                      |
| Deactivate, reactivate, remove (§3.2.1)  | `POST /admin/users/{id}/deactivate`, `.../reactivate`, `.../remove-from-chats`      |
| Delete an account (§3.2.1)               | `DELETE /admin/users/{id}`                                                         |

Session length, idle timeout, required second factor, display name format and the privacy of names and
e-mail addresses, and the presence switches (statuses on/off, custom status on/off, presets, away
timeout, last seen on/off) the thread options (threaded view on/off, auto-follow default) and the allowed guest e-mail domains
(`guests.allowed_email_domains`: invitations, meeting invitees and guest sign-in; added to the server's
`[guests] allowed_email_domains`) are part of the signed workspace policy; the server enforces them. The display name format and the full name/e-mail privacy apply
to every response that names a person (directory, member lists, `/me`, meeting attendees, gateway events)
and to the names in e-mail the server sends (iCalendar organizer and attendees, `{inviter}`); only the
admin console's user list shows the identity provider's name, because admins must recognise accounts. A permission scheme change
cannot touch the owner role or `sign_workspace`, cannot give guests permissions beyond channel content
and direct messages, and cannot change instance-wide permissions inside a space.

## 5. Admin UI

Part of the regular client (desktop/web) under "Administration", visible only to admins. No separate
admin app, so admin actions get signed with the admin's device key.

## 6. Settings: GUI vs config

Everything an admin changes while the instance runs is changed in the admin console. The
configuration file (`teamcraft.toml`, written by the deployment playbook) is for **bootstrap**: what
the server needs before it can read its database, and what the layers in front of it must agree on.
For runtime settings it is the **seed**: a setting nobody changed in the console comes from the file
(`source = config`); changing it in the console stores it in the database (`source = database`) on
top, and "Reset to configuration file" removes the stored value again. So a fresh deployment works
from the file alone, and a later playbook run never overwrites what an admin set in the console.

Every change is an audit log entry (`brand_*`, `mail_settings_*`, `mail_test_sent`,
`calendar_settings_*`, `calendar_test`, `settings_updated`, `settings_reset`) with what changed but
never a password. All endpoints need `manage_workspace` (instance admins and owners).

| Area              | Console page                                 | In the configuration file                                                                                                              |
|-------------------|----------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------|
| Brands            | Administration → Brands                      | `[[brands]]` (seed, read-only until taken over), `[branding]`                                                                          |
| Mail server       | Administration → Mail                        | `[mail.smtp]` (seed; `[meetings.smtp]` if that is missing)                                                                             |
| Calendar (CalDAV) | Administration → Calendar                    | `[meetings.caldav]` (seed); `[meetings]` itself is bootstrap                                                                           |
| Instance settings | Administration → Settings                    | `[policy]`, `[notifications]`, `[shares]`, `[limits]`, `[integration]`, `[local_accounts]`, `[mail]` texts, `[meetings]` texts (seeds) |
| Signed policy     | Security, Retention, Updates, Backgrounds, … | none: owner-signed, see §2.3                                                                                                           |

### 6.1 Brands

Admins list, create, edit and delete brands (docs/branding.md §2.1): display name, organization name,
host names, e-mail domains, colors for the light and dark theme (with a live preview and a contrast
check), login title and text per language, support/privacy/terms/imprint links, the mail sender
(name and From address) and the images (logo, icon, favicon, each with a dark variant), which are
kept in the object storage. Brands of the configuration file are shown as "from configuration file"
and are read-only; "Take over into the database" copies one (with its images) into the database,
where it can be edited. Deleting a database brand that replaced a configuration brand brings the
configuration brand back.

**Host names** are bootstrap: the reverse proxy and TLS layer must route and certify every host name,
so a brand can only use host names the server is deployed for, `[branding].served_hostnames` plus the
host names of `[[brands]]`. The console shows each served host name with the brand it selects now
(or none). A per-brand content security policy and extra connect origins stay in the file: they
widen what the web client may load and talk to.

### 6.2 Mail server and calendar

All mail the server sends (invitations, activations, digests, meeting invitations, test mail) goes
through one SMTP server: host, port, security (STARTTLS, TLS, or none for a server on the same
machine only), user, password, default sender address and name. Brands may have their own sender
(§6.1). The page shows the delivery status since the start (last success, last failure and its
error) and sends a test mail. The CalDAV calendar of meetings has URL, account, password and the
optional free/busy URL template, and a connection test; it needs `[meetings]`.

**Passwords are write-only.** They are never sent back (the console sees "has password"). Before they
reach the database they are encrypted with the server's **settings key** (XChaCha20-Poly1305 under a
key derived with HKDF from `[secrets] key` or the file `[secrets] key_path`, which the server creates
on first start; the setting's name is the associated data). This keeps them out of database dumps
and backups. It is not end-to-end encryption: the server must hold the plaintext to log in to those
services. Without a settings key passwords can only be set in the configuration file, and the console
says so. Losing the key means entering the passwords again.

### 6.3 Instance settings

Groups that are stored and reset as a whole:

| Group            | Values                                                                                                                                        |
|------------------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| `server_rules`   | invites need a verified e-mail address; default spaces and channels for new members; days until deactivated users are removed from all chats  |
| `notifications`  | read receipt default, urgent repeat interval and count, phone push delay, desktop attention                                                   |
| `email_digest`   | on/off, default and shortest interval, grace period, subjects and texts                                                                       |
| `shares`         | on/off, default and longest expiry, download cap, links per user                                                                              |
| `limits`         | attachment size and per-user total, avatar and background sizes, backgrounds per user, group DM size, user group size, meeting mails per hour |
| `integration`    | default retention of integration channels                                                                                                     |
| `local_accounts` | on/off (off: the form disappears, `/auth/local/*` answers 403 `local_accounts_disabled`, sessions end), second factor required, activation and session lifetime, failed attempts, lockout |
| `sign_in_page`   | which methods the sign-in page offers (SSO, e-mail and password) and in which order; help text per language; help link; whether every sign-in shows the identity provider's login page |
| `mail_texts`     | invitation and activation mail                                                                                                                |
| `meeting_mail`   | most invitees; invitation, update and cancellation mail                                                                                       |

**Signed or not.** Settings that member clients must be able to verify against a compromised server
are part of the owner-signed workspace policy (§2.3) and are edited there: `admin_self_join`, org
recovery, history and retention defaults, session rules, directory privacy, presence, threads, guest
rules (allowed e-mail domains, default expiry, DMs only with shared conversations), the shortest
password, push content, call backgrounds, desktop updates and the recommended number of owners.
Everything in this section is enforced
by the server alone (the server sends the mail, checks the sizes, applies invites): a compromised
server could ignore these settings whether they were signed or not, so a signature would protect
nothing. Brands, mail and calendar are server-side presentation and transport for the same reason.

### 6.4 What deliberately stays in the configuration file

| Setting                                                           | Why                                                                                                  |
|-------------------------------------------------------------------|------------------------------------------------------------------------------------------------------|
| `[server]`, `[database]`, `[logging]`                             | needed before the database can be read                                                               |
| `[oidc]` (issuer, clients, roles, sync)                           | sign-in itself depends on it; a mistake would lock every admin out                                   |
| `[storage]` endpoint and keys                                     | brand images and attachments live there; also needed to start                                        |
| `[branding]` served host names, unknown host policy, proxies      | must match the reverse proxy and its certificates                                                    |
| `[web]` and per-brand content security policies                   | widen what the web client may load; a console mistake could open it up                               |
| `[security]` request limits, rate limits, `trust_forwarded_for`   | must match the proxy in front; wrong values block everybody                                          |
| `[gateway]`, `[rendezvous]`, protocol `[limits]`                  | tuning of connections and message, MLS and key sizes, not policy                                     |
| `[retention]` job and `[presence]` intervals, scheduler intervals | operational tuning; read when the jobs start                                                         |
| `[push]` credentials and the push gateway URL and key             | the store apps register against the gateway of their publisher; changing it re-registers every phone |
| `[calls]` SFU URL, signing key, tokens                            | shared with the SFU's own configuration                                                              |
| `[meetings]` base URL, guest token secret, inbox token            | secrets shared with other components, and the base of every join link                                |
| `[local_accounts]` availability and session secret                | the password hashing setup is built at start; the console switches configured accounts off and on    |
| `[secrets]`                                                       | the key that protects the console's passwords cannot live next to them                               |
