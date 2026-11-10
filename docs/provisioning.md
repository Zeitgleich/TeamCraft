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
| SCIM 2.0 endpoint (§6)                                  | Follow-up (v1.1)              |
| Accounts listed before their first sign-in (§6)         | Follow-up (v1.1)              |

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
  provider pushes instead, is a second implementation of the same plan (§6).

## 2. Joiners

| Path                             | What happens                                                                                                                                                                         |
|----------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| First sign-in (just in time)     | The account is created from the token; default spaces and channels apply once (admin.md §3.4); the group mapping applies at the next sync run, which the sign-in starts at once (§4) |
| The provider adds a group member | The next sync run, by event within seconds or by polling within an interval, applies the mapping (§5)                                                                                |
| An import names a user           | Created from the provider before the first sign-in (admin.md §3.8), as today                                                                                                         |

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

The console (Administration → Identity provider → "Group mapping") keeps rules: a provider group (its
path, e.g. `/engineering`) to spaces and public channels of those spaces. Stored in the database as
instance settings, applied by the sync run:

- For each rule and each active account the provider lists in the group, the spaces and channels are
  added **once** per account and rule (`group_mapping_applied`), like default spaces. Somebody who
  leaves a mapped channel by choice is not added back on the next run.
- Only additions. Leaving the provider group removes nobody (a removal needs a signed change, and
  people often stay in channels after changing teams); leavers are handled by §3.
- A rule naming a deleted or archived space or channel skips it. A private channel can't be chosen.
- Dry run and the brake don't apply: additions are no risk to the instance's availability. The run
  reports how many it added; each addition is audited (`group_mapping_applied`, `source: oidc_sync`).

## 6. Not yet (follow-up issues, v1.1)

- **SCIM 2.0** (`/scim/v2/Users`, `/Groups`, its own bearer token): the provider pushes create, update,
  disable, delete and group membership. It plugs in as a second source of the same plan: a SCIM
  disable runs the deactivation sequence with `source: scim`, group changes feed §5, and the brake
  applies per minute instead of per run.
- **Accounts listed before their first sign-in:** pre-created from the provider (the import already
  does this, admin.md §3.8), shown as "not yet signed in", addable to channels; the encryption group
  takes them in once their first device registers.

## 7. Robustness

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
| Large instance                    | `GET /admin/leavers` is paged (100); a round handles at most `max_per_day` users; group mapping adds in batches                                                                                    |
| Clock skew                        | The grace is judged by the server's clock (`deactivated_for_secs`); the per-day limit by the device's own                                                                                          |
