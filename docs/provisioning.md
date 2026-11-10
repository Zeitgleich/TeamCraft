# Provisioning and deprovisioning

Joiners and leavers need no manual TeamCraft work: the identity provider (IdP) is the source of
truth for who works here, and TeamCraft follows it. Issue #210. The first part (the sync on by
default, its safety brake, sync health, the deactivation sequence and its audit) is described in
[admin.md §3.2.3](admin.md); this document covers the rest and how it all fits together.

| Part                                                    | State                         |
|---------------------------------------------------------|-------------------------------|
| Sync with the IdP's admin API, brake, health, audit     | Done (#1525), admin.md §3.2.3 |
| Leavers leave every encryption group automatically (§3) | This document                 |
| IdP events start a sync at once (§4)                    | This document                 |
| IdP group to space and channel mapping (§5)             | This document                 |
| Accounts listed before their first sign-in (§6)         | This document (#1632)         |
| SCIM 2.0 endpoint (§7)                                  | This document (#1631)         |

## 1. The shape

```
            IdP (Keycloak, …)
   admin API │        │ event (hint only)
     (poll)  ▼        ▼
   ┌──────────────────────────────┐   deactivates, maps groups,
   │ server: sync (one at a time) │── audit, brake (admin.md §3.2.3)
   └──────────────────────────────┘
                 │ GET /admin/leavers, leaver_update event
                 ▼
   ┌──────────────────────────────┐   signs the removals, members
   │ an owner's or admin's app    │── commit them, keys rotate
   └──────────────────────────────┘   (e2ee.md §4.6)
```

- **The server reads the provider, the apps sign.** The server decides who is deactivated (it
  already refuses them every request from that moment), but it can't change an encryption group
  (security review A8). Taking a leaver out of the groups is a signed removal by an owner's or
  admin's device; the apps do it on their own, with nobody asked, the way roles follow the provider
  (e2ee.md §3.1.3).
- **Polling is the safety net, events are the fast path.** A sync run reads the whole provider and
  is idempotent, so a missed, duplicated or forged event costs at most an extra run.
- **Vendor-neutral.** The provider is behind one interface: the sync reads users, roles and groups
  from an admin API (Keycloak today), and an event is only "something changed". SCIM, where a
  provider pushes instead, is a second source of the same plan (§7).

## 2. Joiners

| Path                             | What happens                                                                                                                                                                                                          |
|----------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| The provider adds a member       | The next sync run makes the account before the first sign-in, listed as "not yet signed in" and addable to channels (§6); the group mapping applies to it in the same run (§5)                                        |
| First sign-in (just in time)     | The account is created from the token, or the one made before is taken; default spaces and channels apply once (admin.md §3.4); the group mapping applies at the next sync run, which the sign-in starts at once (§4) |
| The provider adds a group member | The next sync run, by event within seconds or by polling within an interval, applies the mapping (§5)                                                                                                                 |
| An import names a user           | Created from the provider before the first sign-in (admin.md §3.8), the same way as §6                                                                                                                                |
| A SCIM client creates a user     | The account is made at once, the same way as §6, and its groups' mapping applies (§7)                                                                                                                                 |

Group mapping only ever adds: spaces, and *public* channels of those spaces, on the server, which is
what a self-join is (the member's own app joins the encryption group, e2ee.md §4.4). Private channels
need a member's signed add and are not mapped.

## 3. Leavers

### 3.1 The sequence and its bound

| Step | Who                       | What                                                                                                                                                                                                                                              | Bound                                                  |
|------|---------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------|
| 1    | IdP                       | User disabled, deleted, or loses every TeamCraft role                                                                                                                                                                                             | –                                                      |
| 2    | Server sync               | Deactivation sequence (admin.md §3.2.3): every request, gateway and sign-in refused, sessions end, push stops, bots and links paused, audit with `source`                                                                                         | Seconds with events (§4), else `interval_secs` (5 min) |
| 3    | Server                    | Lists the user under `GET /admin/leavers`, tells admins' apps (`leaver_update`)                                                                                                                                                                   | At once                                                |
| 4    | An owner's or admin's app | After the grace (`leavers.grace_minutes`, default 60), claims the user, signs their removal from every channel, then `POST /admin/users/{id}/remove-from-chats`: devices revoked (push registrations and key packages go), DMs and spaces dropped | The grace, plus until one such app is online           |
| 5    | Members' apps             | Commit the posted removals, which rotates the keys                                                                                                                                                                                                | The next online member of each channel                 |

From step 2 an honest server gives the leaver nothing. Steps 4 and 5 protect against a server that
would: until the commit, a compromised server could still hand the leaver new ciphertext of the
channels they were in (e2ee.md §4.6.4). The grace exists because a deactivation by the sync is a
suspension the provider can undo, while a removal from all chats can't be undone by a reactivation
(they have to be added again): a disable by mistake, put right within the hour, costs nothing.
`grace_minutes = 0` removes at once.

### 3.2 What the app does, and what it refuses

The leaver round (`client/leavers.rs`) runs in every app of an owner or admin that holds device keys
(desktop, mobile, a browser with the account set up):

- **When:** once per gateway connection, on `leaver_update`, and at the time the next listed leaver
  becomes due (a local timer that starts no request before then). Failures back off exponentially,
  like role signing. Nothing runs while nothing is listed.
- **Whom:** users the server lists as deactivated **by the identity provider** (reason `oidc`, by the sync or by SCIM, §7)
  whose grace has passed. Not: users an admin deactivated by hand (the admin removes them as a second,
  explicit step, admin.md §3.2.1), bots and meeting guests (they have their own paths), and **nobody on the
  signed roster**: an owner or admin who leaves goes off the roster through role evidence first
  (e2ee.md §3.1.3), and the console lists them for an owner meanwhile.
- **Its own brake:** at most `leavers.max_per_day` (default 10) automatic removals per device in 24
  hours of its own clock. The server's brake protects against a provider's mistake; this one protects
  against a hostile server, which could otherwise get every member removed by reporting them
  deactivated. Over the limit, System health says how many wait, and the Users page lists them for an
  admin ("Remove from all chats"), which is the same signed action by hand.
- **One at a time per user:** before signing, the app claims the user (`POST /admin/leavers/{id}/claim`,
  a lease of 10 minutes in the database, one statement, so also safe with several server
  instances). Another device that claims meanwhile gets 409 `leaver_claimed` and skips the user; a
  lease that ran out (the app crashed) is claimed by the next one. Everything after the claim is
  idempotent (admin.md §3.2.1), so a retry finishes what is left. The server step of an automatic
  removal (`remove-from-chats?as_leaver=true`) is refused if the provider enabled the person again
  since the claim. An admin's app is only shown leavers it may act on: an admin who left is removed
  by an owner's app (admin.md §3.2.1, only owners act on admins).
- **Mode:** `leavers.mode = manual` in the signed policy turns the automation off; the console then
  lists leavers for an admin to remove by hand.

Settings are part of the signed workspace policy (Administration → Security), so the server can't
switch the automation on, raise the limit or shorten the grace:

| Setting                 | Default     | Range                 |
|-------------------------|-------------|-----------------------|
| `leavers.mode`          | `automatic` | `automatic`, `manual` |
| `leavers.grace_minutes` | 60          | 0 – 43 200 (30 days)  |
| `leavers.max_per_day`   | 10          | 1 – 1 000             |

### 3.3 What a hostile server can and can't do with it

| It does                            | Effect                                                                                                                                                                                                                                                                                                      |
|------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Lists an active member as a leaver | An admin's app removes them from the channels, at most `max_per_day` per admin device per day; the removal is announced in each channel and audited. Availability only: a removal never gives anybody access, and the server can already refuse that member service. They are added back by a channel admin |
| Lists an owner or admin            | Nothing: roster members are never removed automatically                                                                                                                                                                                                                                                     |
| Withholds a leaver                 | The leaver stays in the groups, as without this feature: the console of an honest server, and the provider, still show them deactivated                                                                                                                                                                     |
| Replays an old claim or answer     | Claims are leases in the database; the removal itself is the signed change of e2ee.md §4.6, checked by every committing member                                                                                                                                                                              |

### 3.4 Everything else a leaver held

| Item                        | Handled by                                                                                                                                                                                                 |
|-----------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Sessions, tokens            | Deactivation (step 2): local sessions end; provider tokens are refused; refresh tokens are the provider's                                                                                                  |
| Devices, push               | Step 4: revoked, registrations and key packages deleted                                                                                                                                                    |
| Direct messages             | Step 4 drops the server's side. In a group DM an admin is in, the admin's app commits the removal; elsewhere the participants' apps list the user as left over for an admin who is in it (admin.md §3.2.1) |
| Channels they own           | Not reassigned automatically (the server can't sign a channel role): the "No owner" filter finds them (admin.md §2.5)                                                                                      |
| Bots, share and guest links | Paused at deactivation (admin.md §3.2.1)                                                                                                                                                                   |
| Signed roster               | Role evidence and the server's report (e2ee.md §3.1.3)                                                                                                                                                     |
| Audit                       | `user_deactivated` with `source`, `leaver_removal_claimed` (automatic, by which admin's device), `user_removed_from_chats`                                                                                 |

## 4. Identity provider events

`POST /api/v1/identity-provider/events` with `Authorization: Bearer <secret>`. The body is ignored
(anything up to 64 KiB is accepted), because the request is only a hint: it starts a sync run soon,
which reads the provider itself. So a forged or replayed event can't deactivate or create anybody; at
worst it starts a run, and runs are at least `event_min_gap_secs` (default 10) apart. Events arriving
during a run start one more run after it, never more than one.

- **The secret** is generated in the console (System health, the identity provider card),
  shown once, stored as a SHA-256 hash (it is 32 random bytes, so no slow hash is needed) and compared
  in constant time. Rotating it replaces it at once; deleting it turns the endpoint off.
- **Answers:** 202 when accepted; 401 `invalid_token` for a missing or wrong secret, and also while
  no secret is set (the same answer, so the endpoint says nothing about the setup); 413 for a body over
  64 KiB; 503 `unavailable` when the sync isn't configured. A wrong secret costs one read of one row and
  a constant-time comparison; runs stay at least the gap apart whatever arrives.
- **Keycloak** has no built-in HTTP event sender; an event listener extension posts admin and user
  events to a URL (e.g. a community "events to HTTP" provider or an own SPI). Which events: user
  updated, deleted, role mapping and group membership changes. Setup in the Keycloak guide.
- **A first sign-in** starts a run the same way, so the group mapping of a new account applies within
  seconds.

## 5. Group to space and channel mapping

The console (Administration → Settings, "Groups from the identity provider") keeps rules: a provider
group (its path, e.g. `/engineering`) to spaces and public channels of those spaces, entered as IDs like
the default spaces. Stored in the database (`GET`/`PUT /api/v1/admin/identity-provider/group-mappings`,
`ManageWorkspace`, audit `group_mappings_changed`) and applied by every sync run:

- For each rule, the provider's direct members of the group who have an active account here join the
  spaces (as members) and those of the listed channels that are public, **once per account and revision
  of the rule** (`identity_provider_group_mapping_applied`). Somebody who leaves a mapped channel by
  choice is not added back by the next run. Changing a rule's spaces or channels raises its revision, so
  it applies once more to everybody in the group. Each addition is audited (`group_mapping_applied`,
  `source: oidc_sync`) and announced like a default space.
- Only additions. Leaving the provider group removes nobody (a removal needs a signed change, and
  people often stay in channels after changing teams); leavers are handled by §3.
- A rule names 1 to 20 spaces and up to 100 channels; the server refuses unknown spaces and channels
  that are private or not in the rule's spaces. Archived or deleted ones are skipped when applying.
  Private channels need a member's signed add and can't be mapped. At most 100 rules.
- A group the provider doesn't have (renamed, deleted) is skipped with a warning; the other rules apply.
- It runs with the sync's apply phase: a dry run and a run the safety brake holds change nothing here
  either. A first sign-in and saving the rules ask for a run (§4), so a new colleague lands in their
  spaces within seconds.
- Accounts are mapped once they exist, which with pre-creation (§6) is before the first sign-in: a
  new colleague is in their spaces and public channels when they first open the app.

## 6. Accounts before their first sign-in

A new colleague is listed in the directory as "not yet signed in" and can be added to channels before
they ever open the app (#1632). Each sync run makes the accounts the provider has and TeamCraft doesn't,
the way the import already does (admin.md §3.8, both use the same function; SCIM makes them the same way, §7):

- **Who.** Enabled provider users whose TeamCraft role is owner, admin or member, without an account
  here. **Guests are not made in advance:** they come from outside, are only found by people they share
  a conversation with, and are added through a guest invitation or their own sign-in, as before. An
  identity whose account an admin deleted (admin.md §3.2.1, "released") isn't made again before its
  own next sign-in, so a deletion sticks.
- **The account.** The provider's name, user name, e-mail and role, like the account a first sign-in
  makes, plus the time it was made (`users.awaiting_first_sign_in_since`). The first sign-in finds it
  by issuer and subject, refreshes it from the token and clears the mark; default spaces and channels
  apply then, as for any new account (admin.md §3.4). Audit `user_provisioned` with `source: oidc_sync`.
- **Listed.** The directory and every people picker (desktop, web, the phones) mark such a person "not
  yet signed in" instead of "hasn't finished setting up". The mark is only in directory answers and is
  the server's word for display; nothing anybody signs depends on it.
- **Added to a channel.** Like anybody without a device (e2ee.md §4.3, "Nobody to add yet"): the
  server records the membership at once, the adding device keeps a pending addition and commits the
  signed add when the person's first device registers, with the history choice made when adding. Who
  may add them is who may add anybody (`manage_members`, channel roles, guests never: the same checks,
  tested per role). Channels of the group mapping (§5) are public: the person's first device joins
  them with its own signed self-join.
- **Brake on creation.** A run looks up the roles of at most `[oidc.sync] max_created_per_run`
  (default 100) people without an account and so makes at most that many; the next run goes on where
  this one stopped (in subject order, round after round), so people without a TeamCraft role, who are
  looked up and skipped every round, never keep others waiting. Not a hold for an admin like the
  deactivation brake: a wrong creation costs a directory entry nobody signed in to, and the next run
  after the provider is fixed removes it (below). A dry run and the console's preview say how many a
  run would make; a run the deactivation brake holds makes none. `precreate_accounts = false` turns
  it off: accounts then appear at the first sign-in, as before.
- **Removed outright.** When the provider disables or deletes such an account, or takes its last role,
  before it ever signed in, the run deletes it (admin.md §3.2.1, audit `user_deleted` with `source:
  oidc_sync`, `reason: never_signed_in`) instead of deactivating it: no device ever joined a group, so
  there is nothing to suspend or to rotate, and no leaver round (§3) is needed. These removals count
  for the deactivation brake like any other, so a provider that suddenly loses many people holds them
  for an admin. Its server memberships go, and its 1:1 direct messages go with it as with any deleted
  account (admin.md §3.2.1). The identity isn't released: if the provider enables the person again,
  the next run makes the account again. The check is made under the account's row lock, so a first
  sign-in at the same moment wins and the account is deactivated instead, like any other.

| Setting                           | Default | Meaning                                                  |
|-----------------------------------|---------|----------------------------------------------------------|
| `[oidc.sync] precreate_accounts`  | `true`  | Make the provider's members' accounts in advance         |
| `[oidc.sync] max_created_per_run` | 100     | Role lookups, and so accounts made, per run (at least 1) |

**What a hostile server can do with it.** Nothing it can't already: it makes accounts at a sign-in
anyway, and the directory is its word. It can't put anybody into an encryption group (§4.6 of
e2ee.md): an addition is signed by a member allowed to make it and committed by members, whether the
person has a device yet or not. Listing a real member as "not yet signed in" changes a label; a device
the server made up for a pre-made account fails the checks of a first device like any other
(identity key, signed device list).

**Imported accounts** (admin.md §3.8) carry the same mark, set by the migration for accounts an
import made that never signed in, and are removed the same way when the provider disables them before
a sign-in.

## 7. SCIM 2.0

Identity providers that push instead of being polled (Entra ID, Okta, Keycloak with a SCIM extension,
others) provision TeamCraft through SCIM 2.0 (RFC 7643, RFC 7644) at `<server>/scim/v2` (#1631). It is
a second source of the same plan as the sync, not a second implementation of it: a create makes the
account of §6, a disable or delete runs the deactivation sequence (admin.md §3.2.3) and the leaver
round (§3), group members feed the mapping (§5). Sync and SCIM can run side by side; the periodic sync,
where configured, stays the net that reconciles what a lost SCIM request missed.

### 7.1 Clients and tokens

Administration → System health, "SCIM provisioning", lists the SCIM clients: one per identity provider
application that pushes, at most 10. Making one (`POST /api/v1/admin/scim/clients`, `ManageUsers`,
audit `scim_client_created`) shows its token once: `tcscim_<client id>_<32 random bytes>`. The server
keeps the SHA-256 of it (high entropy, no slow hash needed), finds the client by the ID in the token and
compares the hash in constant time; it never logs the token. "New token" replaces it at once
(`scim_client_token_rotated`), "Revoke" deletes the client (`scim_client_deleted`): its token stops
working and its groups go, the accounts it made stay. The console shows when a client was last used (to
the minute) and how many users and groups it provisioned.

Each client names its **identity attribute**: the SCIM attribute that equals the subject (`sub`) of
the person's sign-in at the identity provider, `externalId` (Okta, Keycloak extensions) or `userName`.
An account SCIM makes carries the identity provider's issuer and that subject, so the first sign-in
finds it like one the sync made. A user without it is refused (`scim_identity_immutable`), and it can't
change later: a different subject is a different person at sign-in.

### 7.2 Users

| SCIM                            | TeamCraft                                                                                                                                                                                                                    |
|---------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `POST /Users`                   | Makes the account before its first sign-in (§6), a **member** (`user_provisioned`, `source: scim`), or links the account of that subject that is there (`scim_user_linked`). A retried create answers with the same resource |
| `GET /Users`, `GET /Users/{id}` | The client's own users only: `userName`, `externalId`, `name`, `displayName`, `emails`, `active`, `groups`, `meta`                                                                                                           |
| `PUT`, `PATCH /Users/{id}`      | Changes the attributes; the account takes name, user name and address only until its first sign-in, then the sign-in's token is their source and SCIM keeps what it sent in the resource                                     |
| `active: false`                 | The deactivation sequence with reason `oidc` and `source: scim`; a leaver like any the provider deactivated (§3). The resource stays: `active: true` lifts it with everything intact                                         |
| `DELETE /Users/{id}`            | The resource goes. An account that never signed in is deleted outright (§6); any other is deactivated like a disable                                                                                                         |

- **Roles.** SCIM carries no TeamCraft role: what it makes is a member, and the first sign-in's token
  decides the role from then on (§2). A SCIM token can't make an admin, and can't change a role.
- **Admin deactivations stick.** Enabling through SCIM lifts only a deactivation by the identity provider
  (reason `oidc`), never one an admin made by hand.
- **Deleted by an admin.** An identity whose account an admin deleted (released, admin.md §3.2.1) isn't
  made again by SCIM (`scim_identity_released`, 409) until its own next sign-in, as with the sync.
- **Guests** are not provisioned as guests: SCIM makes members, and guests come through invitations.
  Assign only staff to the SCIM application.
- Attributes TeamCraft doesn't keep (the enterprise extension, phone numbers, addresses, a password)
  are accepted and dropped.

### 7.3 Groups

`/Groups` keeps the groups the client pushes, with their members (users of the same client), up to
`[scim] max_group_members` (20 000). A group mapping rule (§5) whose group is the SCIM group's display
name, without the slashes and case ignored (`/engineering` for `Engineering`), adds the members to its
spaces and public channels: on create, on each added member, for all members when the group is renamed,
and for all SCIM groups when the rules are saved. Once per member and revision of the rule, additions
only, like the sync's: leaving or deleting a SCIM group removes nobody (`group_mapping_applied`,
`source: scim`). Nested paths (`/a/b`) are the sync's; SCIM groups have no hierarchy.

### 7.4 What the endpoint speaks

- Create, read, replace (`PUT`), `PATCH` (`add`, `replace`, `remove`; paths with value filters such as
  `emails[type eq "work"].value` and `members[value eq "…"]`; no-path values; `op` in any case and
  `active` as text, as Entra ID sends it), delete, for users and groups. A group's `PATCH` answers 204
  with the new `ETag`, so a large group isn't sent back for every member change.
- Filters (RFC 7644 §3.4.2.2) with every operator, `and`, `or`, `not` and value paths. Equality on
  `userName`, `externalId`, `id` and (groups) `displayName` is looked up by index; any other filter
  reads the client's resources up to `[scim] max_filter_scan` (10 000) and is refused beyond
  (`tooMany`); a group filter that names `members` reads their members, up to `[scim]
  max_filter_members` (100 000) in all. Attribute names ignore case; `id` and `externalId` compare
  exactly, all other values ignore case.
- Paging with `startIndex` and `count` (at most 200), `attributes` and `excludedAttributes`. A group page
  carries at most 10 000 members in all and stops early beyond them (`itemsPerPage` says how many).
- ETags: `meta.version` and the `ETag` header are `W/"<version>"`; `If-Match` on `PUT`, `PATCH` and
  `DELETE` (412 on a mismatch), `If-None-Match` on `GET` (304).
- `/ServiceProviderConfig`, `/ResourceTypes`, `/Schemas`. No bulk, no sorting, no password change.
- `application/scim+json` both ways (`application/json` is accepted). Errors are SCIM error bodies with
  `scimType` and TeamCraft's stable `error_code` (`invalid_token`, `rate_limited`, `scim_not_found`,
  `scim_invalid_syntax`, `scim_invalid_value`, `scim_invalid_filter`, `scim_invalid_path`,
  `scim_too_many`, `scim_body_too_large`, `scim_uniqueness`, `scim_linked_elsewhere`,
  `scim_identity_released`, `scim_identity_immutable`, `scim_unknown_member`, `scim_version_mismatch`,
  `unavailable`, `internal`).

### 7.5 The brake, per time window

A SCIM client sends one request per user, not one run, so the brake counts deactivations and deletions
by SCIM (all clients together) in a sliding window: `[scim] brake_window_secs` (an hour) may take
`max(brake_min_allowed, brake_max_percent % of the active people who have signed in)` (defaults 2 and
10 %), never everybody. Accounts made in advance that never signed in don't count, so a client can't
raise the allowance by making dummy accounts first. Beyond it, the deactivation is **held**: the
request succeeds, the resource still says `active: true`, the person keeps working, and the held entry
shows in the console's brake list next to the sync's (`scim_deactivation_held`, the `oidc_sync_brake`
check). Confirming the list applies it (an owner confirms when an owner or admin is on it). Only an
explicit `active: true` from the provider (a PUT, a create, or a PATCH that names `active`) takes a
hold off: the resource keeps the provider's requested `active`, so a PATCH of other attributes leaves
the hold as it is. **Owners and admins are always held**: a SCIM token alone never deactivates them.
The sync's runs replace only their own entries of the list and never apply SCIM's.

### 7.6 What a SCIM token can do, and what a hostile one can

A SCIM token is admin-level power over accounts, as strong as the identity provider itself, and is
treated like one: rate-limited per client (`[scim] requests_per_minute`, 600), bodies bounded (`[scim]
max_body_bytes`, 1 MiB; 100 PATCH operations; 1 000 members per request), every failed authentication
counted against the address like a failed sign-in.

| It can                                                                  | Bound                                                                                                                       |
|-------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------|
| Read the users and groups of its own client, with what it sent itself   | Nothing else: no other client's resources, no other accounts, no message content, keys, devices, sessions or audit log      |
| Make member accounts and link existing ones by their subject            | No roles; an account another client holds is refused; a released identity is refused                                        |
| Link any account of the instance's identity provider by its subject     | Intended: a provider set up on an instance in use takes over the people already there; the token then manages them as above |
| Deactivate the members it linked, and delete those that never signed in | The brake's window; owners and admins only with an admin's confirmation; an admin's own deactivation is never lifted by it  |
| Add its users to mapped spaces and public channels                      | Only what the admins' rules name; private channels need a member's signed add (§5)                                          |

The server never trusted the identity provider's word for encryption groups and doesn't trust SCIM's
either: membership, removal and roles stay signed by members' devices (e2ee.md §4.6), so a stolen SCIM
token is an availability risk (accounts suspended up to the brake, directory entries made), not a
confidentiality one. Revoking the client in the console ends it at once.

### 7.7 Robustness

| Condition                         | Behaviour                                                                                                                           |
|-----------------------------------|-------------------------------------------------------------------------------------------------------------------------------------|
| Retried create, lost answer       | The same subject finds the same account and resource: same `id`, 201 again                                                          |
| Retried disable, delete           | A disabled account stays as it is; a deleted resource answers 404, which providers take as done                                     |
| Out-of-order PATCHes              | `If-Match` when the provider sends it (412 otherwise); without it, last writer wins per resource, like the provider's own model     |
| Group change naming unknown users | Refused as a whole (`scim_unknown_member`), nothing half-applied; removing a user that is gone removes nothing                      |
| Server restart                    | Everything is in the database: resources, groups, the brake's window and its holds                                                  |
| Database briefly down             | 503 with `Retry-After`; providers retry                                                                                             |
| Several requests at once          | Each write is one transaction; the brake's window is counted under an advisory lock, so two requests can't both take its last place |
| A provider flooding requests      | 429 with `Retry-After` per client; the address limit of the API applies as well                                                     |
| Sync and SCIM both on             | Both write the same accounts by subject; the sync's run reconciles against the provider, SCIM's holds stay SCIM's                   |

| Setting                      | Default | Meaning                                                         |
|------------------------------|---------|-----------------------------------------------------------------|
| `[scim] requests_per_minute` | 600     | Requests of one client per minute (and its burst)               |
| `[scim] brake_max_percent`   | 10      | Share of signed-in active people SCIM may deactivate per window |
| `[scim] brake_min_allowed`   | 2       | Deactivations per window whatever the share                     |
| `[scim] brake_window_secs`   | 3600    | The brake's window                                              |
| `[scim] max_group_members`   | 20000   | Members of one SCIM group                                       |
| `[scim] max_body_bytes`      | 1048576 | Largest request body                                            |
| `[scim] max_filter_scan`     | 10000   | Resources a filter no index narrows may read                    |
| `[scim] max_filter_members`  | 100000  | Group members a filter on `members` may read in all             |

**Setting it up.** Make a client in the console, give the identity provider the base URL (`https://<server>/scim/v2`)
and the token as a bearer token ("secret token" in Entra ID, "HTTP header" authentication in Okta), and
choose as the client's identity attribute the one that carries the same value as the `sub` claim of the
person's sign-in. The provider's test connection reads `/Users?filter=…` and the discovery endpoints.

## 8. Robustness

| Condition                         | Behaviour                                                                                                                                                                                          |
|-----------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| IdP unreachable                   | The run fails before it changes anything (admin.md §3.2.3); health shows it and alerts after `stale_after_intervals`. Leavers already deactivated are still removed: step 4 needs no provider      |
| Events missed or never configured | The periodic run catches everything up within `interval_secs`                                                                                                                                      |
| Duplicate events, retries         | One run per gap; runs are idempotent; group mapping is recorded per account and rule                                                                                                               |
| Admin app offline                 | Leavers wait listed (the console's `pending_removals`); the first owner's or admin's app online removes them                                                                                       |
| Two admin apps at once            | The claim lease: one signs, the other skips; a crashed claimer's lease runs out after 10 minutes                                                                                                   |
| App crash midway                  | The removals already posted stay in the groups' queues (e2ee.md §4.6); the next round claims again and finishes                                                                                    |
| Server restart                    | Leaver state and claims are in the database; a run in flight is redone by the next                                                                                                                 |
| Several server instances          | Claims are single SQL statements. The sync's lock is per process: several instances would run it twice, which is idempotent but not supported (the server is single-instance, robustness-audit.md) |
| Large instance                    | `GET /admin/leavers` is paged (100); a round handles at most `max_per_day` users; group mapping adds in batches; a run makes at most `max_created_per_run` accounts (§6)                           |
| First sign-in during a run        | Making the account is one insert that leaves an existing one alone; a removal of a never-signed-in account checks the mark under the row lock, so the sign-in wins                                 |
| SCIM                              | See §7.7                                                                                                                                                                                           |
| Server restart mid-creation       | Accounts are made one by one and idempotently; the round's position is in memory and starts over, which only repeats lookups                                                                       |
| Clock skew                        | The grace is judged by the server's clock (`deactivated_for_secs`); the per-day limit by the device's own                                                                                          |
