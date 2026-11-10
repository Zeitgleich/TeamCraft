# Identity provider: Keycloak setup

TeamCraft uses OpenID Connect. Any OIDC provider that issues client roles in the access token and
can emit a custom ID token claim works; Keycloak is the reference. The development realm
[dev/keycloak/teamcraft-dev-realm.json](../../../dev/keycloak/teamcraft-dev-realm.json) is a working
example; do not use its credentials in production.

## Realm checklist

| Item                                   | Setting                                                                                                                                                  |
|----------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|
| Public client `teamcraft`              | Authorization code + PKCE (`S256`), no direct access grants, no service account                                                                          |
| Redirect URIs                          | Web client origin, `http://127.0.0.1/callback` and `http://[::1]/callback` (desktop, any port), `de.zeitgleich.teamcraft:/oauth/callback` (Android, iOS) |
| Web origins                            | The web client origin                                                                                                                                    |
| Client roles on `teamcraft`            | `owner`, `admin`, `member`, `guest`                                                                                                                      |
| Back-channel logout                    | URL `https://chat.example.com/api/v1/auth/backchannel-logout`, session required `true`                                                                   |
| Confidential client `teamcraft-server` | Service account only (no flows), used for user sync                                                                                                      |
| Service account roles                  | `realm-management`: `view-users`, `query-users`, `query-groups`, `view-realm`, `view-clients`, `query-clients`                                           |
| Realm token lifespan                   | Short access tokens (the dev realm uses a few minutes); the gateway re-checks every hour                                                                 |

With [several brands](configuration.md#several-brands-on-one-server) every brand host name goes into the
client's **valid redirect URIs**, **web origins** and **post logout redirect URIs** (for example
`https://chat.brand-a.example.com/*`); one client serves all brands. To show people's **organization**, add
a token mapper to the client (a group name or the Organizations feature of the realm, "Add to access
token") and set `organization_claim` in `[oidc]` to its claim name. The claim must come from something only
administrators can change; a user attribute that people can edit themselves would let anybody label
themselves as another company.

The server needs no secrets from the public client. The client names and role names are
configurable ([Configuration](configuration.md)); the reference names above are the defaults.

## Roles

Assign the client roles of `teamcraft` to users, directly or through groups (group role mapping, as
`carol` in the dev realm is a member through the group `/project-a`). A user with several roles gets
the highest (`owner` > `admin` > `member` > `guest`). A user with none is rejected at login.

Guests live in the **same realm**, with the `guest` client role only. To use existing role names,
map them in `[oidc.roles]` (for example `guest = "external"`).

## Identity key attestation

Each user's TeamCraft identity key is attested by the identity provider, so a malicious server cannot
swap it.

1. **User-profile attribute** `tc_identity_key`: length max 512, view by admin and user, **edit by
   the user only** (not by admins, not by the service account). In the declarative user profile:

   ```json
   {"name": "tc_identity_key", "displayName": "TeamCraft identity key",
    "validations": {"length": {"max": 512}},
    "permissions": {"view": ["admin", "user"], "edit": ["user"]}, "multivalued": false}
   ```

2. **Protocol mapper** on the `teamcraft` client: *User Attribute* mapper, attribute
   `tc_identity_key`, claim `tc_identity_key`, type String, **add to ID token only** (not to the
   access token or userinfo).
3. The app client's tokens must be allowed to call the account API (audience `account`), so the
   client can write the attribute for the signed-in user.

`./dev/verify-realm.py` checks all three against the dev realm.

## Deprovisioning

- **Back-channel logout** ends sessions immediately when Keycloak ends them.
- **User sync** polls the realm admin API (every 5 minutes by default) and deactivates users that
  were disabled, deleted or lost all TeamCraft roles, and picks up role changes. It is **on as soon
  as `[oidc.sync] client_secret` (the service account) is set**, and **required for production**:
  without it, someone disabled in Keycloak stays active in TeamCraft and keeps working until their
  sign-in ends. `is_enabled = false` switches it off; the console then warns.

Both should be on in production; see [Users, devices and guests](users-devices-guests.md).

- **Leavers leave the encrypted chats** on their own: once the sync has deactivated somebody, an
  owner's or admin's app takes them out of every channel after a waiting time (one hour by default,
  Administration → Security, "People who leave"). See [provisioning.md](../../provisioning.md) §3.

### Events: seconds instead of minutes

Polling reacts within the interval. To react within seconds, let Keycloak report changes:

1. In TeamCraft, **System health → Identity provider events → Create secret**, and copy it.
2. In Keycloak, install an event listener that posts admin and user events to a URL (Keycloak has
   none built in; a community "events to HTTP" or webhook provider, or an own event listener SPI).
   Point it at `https://chat.example.com/api/v1/identity-provider/events`, with the header
   `Authorization: Bearer <secret>`, and enable it for the realm under **Realm settings → Events**
   with admin events on.
3. Disable a test user: the card shows the event, and the sync runs within seconds
   (`event_min_gap_secs`, 10 by default, keeps runs apart).

TeamCraft doesn't read what the event says: it only starts a run, which reads Keycloak itself. A
forged or replayed event can't change anybody. Polling stays on as the safety net for missed events.

### What the sync does when Keycloak looks wrong

A misconfigured Keycloak (wrong realm, a client role mapping removed) must not lock a company out:

- The sync reads everything from Keycloak first and only then changes anything. If Keycloak fails
  halfway, or lists no users at all, nothing changes and the run shows as failed in the console.
- **Safety brake:** one run may deactivate at most 10 % of the active users, at least 2, and never
  everybody on an instance of two or more. A run over that holds them all back, changes no roles
  or groups, and shows who is held under **System health** with a warning and, after the hold time,
  an alert. Users already held do not block anyone else: a leaver who shows up later is deactivated
  as usual, and if you fix Keycloak the list empties by itself. To go ahead with the list as it
  stands, confirm it in the console; an owner has to if an admin or owner is on it. Set
  `brake_max_percent` and `brake_min_allowed` under `[oidc.sync]`.
- **Dry run:** `mode = "dry_run"` under `[oidc.sync]` reports what the sync would change and changes
  nothing. The console's "Check what a run would change" asks the same at any time.
- System health shows the last run, how long it took, the changes and any error, and warns when
  the sync has not run for `stale_after_intervals` intervals (3 by default).

### Upgrading an instance

If `client_secret` is set and `is_enabled` is **not** set, the sync starts on the first start after
the upgrade: `initial_delay_secs` later, users disabled or deleted in Keycloak or without a role are
deactivated (a large backlog is held back by the brake instead), roles are picked up, and with
`sync_groups` so are the groups. Set `mode = "dry_run"` for the first start to see what it would do.
If `is_enabled = false` is set explicitly, nothing changes, and the console and the log warn. If the
issuer is not `<base>/realms/<realm>` and `admin_url` is not set, the sync stays off with a warning
rather than stopping the server.

## Status

The server stores the attestation token; clients do not verify it against the provider's keys yet
(client core README). Until then the identity key is pinned on first use and checked against the
signed admin roster. See [Gaps](troubleshooting.md#known-gaps-between-design-and-code).
