# Identity provider: Keycloak setup

TeamCraft uses OpenID Connect. Any OIDC provider that issues client roles in the access token and
can emit a custom ID token claim works; Keycloak is the reference. The development realm
[dev/keycloak/teamcraft-dev-realm.json](../../../dev/keycloak/teamcraft-dev-realm.json) is a working
example; do not use its credentials in production.

## Realm checklist

| Item                                   | Setting                                                                                                                                           |
|----------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------|
| Public client `teamcraft`              | Authorization code + PKCE (`S256`), no direct access grants, no service account                                                                   |
| Redirect URIs                          | Web client origin, `http://127.0.0.1/callback` and `http://[::1]/callback` (desktop, any port), `com.example.teamcraft:/oauth/callback` (Android) |
| Web origins                            | The web client origin                                                                                                                             |
| Client roles on `teamcraft`            | `owner`, `admin`, `member`, `guest`                                                                                                               |
| Back-channel logout                    | URL `https://chat.example.com/api/v1/auth/backchannel-logout`, session required `true`                                                            |
| Confidential client `teamcraft-server` | Service account only (no flows), used for user sync                                                                                               |
| Service account roles                  | `realm-management`: `view-users`, `query-users`, `query-groups`, `view-realm`, `view-clients`, `query-clients`                                    |
| Realm token lifespan                   | Short access tokens (the dev realm uses a few minutes); the gateway re-checks every hour                                                          |

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
- **User sync** (`[oidc.sync] is_enabled = true` plus the service account secret) polls the realm
  admin API every 5 minutes and deactivates users that were disabled, deleted or lost all TeamCraft
  roles, and picks up role changes.

Both should be on in production; see [Users, devices and guests](users-devices-guests.md).

## Status

The server stores the attestation token; clients do not verify it against the provider's keys yet
(client core README). Until then the identity key is pinned on first use and checked against the
signed admin roster. See [Gaps](troubleshooting.md#known-gaps-between-design-and-code).
