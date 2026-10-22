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
2. The server enforces it immediately for access control (e.g. stops delivering to removed users) and
   hands the signed action to the group's member clients. v1 delivers it as a pending change; the
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
- Space admin and channel owner assignments are signed by an admin or a higher role.
- Member clients only commit membership changes that are signed by someone allowed to make them
  (admin, space admin, channel owner, or a member with `manage_members`). Unsigned or wrongly signed
  proposals are rejected and reported.

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
| Default spaces/channels for new users                                                                                       | v1    |
| Space templates                                                                                                             | later |

### 3.2 Users and guests

| Feature                                                                                                            | Prio |
|--------------------------------------------------------------------------------------------------------------------|------|
| User list with status, devices, last seen, role, spaces                                                            | v1   |
| Deactivate user (also automatically when disabled in the OIDC provider): sessions revoked, removed from all groups | v1   |
| Remove a single device (lost phone)                                                                                | v1   |
| Start org recovery for a user (needs a second admin, see e2ee.md §6.3)                                             | v1   |
| Invite guests, set expiry and sponsor, see all guests and where they are                                           | v1   |
| Local accounts: create, reset password/TOTP                                                                        | v1   |
| OIDC group → role/space mapping                                                                                    | v1   |
| User groups (`@devteam`)                                                                                           | v1   |

### 3.3 Moderation

Admins can't read reported content unless a member shows it to them:

| Feature                                                                                                                                           | Prio |
|---------------------------------------------------------------------------------------------------------------------------------------------------|------|
| **Report message**: a member sends the decrypted message + context to the admin team through an E2EE admin channel – the member's explicit choice | v1.x |
| Delete a message by ID (from a report): server deletes ciphertext, clients honor a signed delete tombstone                                        | v1.x |
| Channel moderators delete messages in their channels                                                                                              | v1   |
| Mute/kick a user from a channel                                                                                                                   | v1   |

### 3.4 Instance

| Feature                                                                                                           | Prio |
|-------------------------------------------------------------------------------------------------------------------|------|
| Settings: name, logo, colors, space label ("Project"/"Space"/"Team"), file size limits                            | v1   |
| Security policy: `admin_self_join`, org recovery (members/guests, admins required), guest rules, history defaults | v1   |
| SFU nodes: registration token, health, load                                                                       | v1   |
| Telephony: SIP trunks and their status, hotline config, queues                                                    | v1.x |
| Storage usage (Postgres, S3) per space                                                                            | v1.x |
| Audit log (metadata only), exportable                                                                             | v1   |
| Push setup (APNs, FCM, UnifiedPush)                                                                               | v1   |
| Backups status                                                                                                    | v1.x |

## 4. Audit log

Every admin action, recovery, device change and role change is logged with actor, target and time.

- Admins see the full log.
- Members see admin actions affecting their conversations as system messages.
- Users see actions affecting themselves (device removed, recovery started) in their security settings.
- The log contains metadata only, never message content.

## 5. Admin UI

Part of the regular client (desktop/web) under "Administration", visible only to admins. No separate
admin app, so admin actions get signed with the admin's device key.
