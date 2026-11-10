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
| SCIM 2.0 endpoint (§7)                                  | Follow-up (v1.1), #1631       |

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
  provider pushes instead, is a second implementation of the same plan (§7).

## 2. Joiners

| Path                             | What happens                                                                                                                                                                                                          |
|----------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| The provider adds a member       | The next sync run makes the account before the first sign-in, listed as "not yet signed in" and addable to channels (§6); the group mapping applies to it in the same run (§5)                                        |
| First sign-in (just in time)     | The account is created from the token, or the one made before is taken; default spaces and channels apply once (admin.md §3.4); the group mapping applies at the next sync run, which the sign-in starts at once (§4) |
| The provider adds a group member | The next sync run, by event within seconds or by polling within an interval, applies the mapping (§5)                                                                                                                 |
| An import names a user           | Created from the provider before the first sign-in (admin.md §3.8), the same way as §6                                                                                                                                |

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
- **Whom:** users the server lists as deactivated **by the identity provider** (`oidc`, later `scim`)
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
the way the import already does (admin.md §3.8, both use the same function, and SCIM will too):

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

## 7. Not yet (follow-up issues, v1.1)

- **SCIM 2.0** (`/scim/v2/Users`, `/Groups`, its own bearer token): the provider pushes create, update,
  disable, delete and group membership. It plugs in as a second source of the same plan: a SCIM
  create makes the account as in §6 (`source: scim`), a SCIM disable runs the deactivation sequence
  with `source: scim` (or removes a never-signed-in account outright, §6), group changes feed §5, and
  the brakes apply per minute instead of per run.

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
| Server restart mid-creation       | Accounts are made one by one and idempotently; the round's position is in memory and starts over, which only repeats lookups                                                                       |
| Clock skew                        | The grace is judged by the server's clock (`deactivated_for_secs`); the per-day limit by the device's own                                                                                          |
