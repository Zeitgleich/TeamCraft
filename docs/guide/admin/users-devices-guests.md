# Users, devices and guests

## Where users come from

Accounts are created at first login from the identity provider's token. Roles come from the client
roles ([Roles and permissions](roles-and-permissions.md)); TeamCraft has no local passwords. To
create a user, create them in Keycloak and assign a role.

## User list

Administration > **Users and guests** shows accounts with role, devices, spaces and last activity
(`GET /admin/users`, devices at `GET /admin/users/{id}/devices`).

## Deactivate and reactivate

- **Console or API:** `POST /admin/users/{id}/deactivate` (and `/reactivate`). Sessions are revoked,
  devices removed, the user leaves their MLS groups at the next member commit, and the audit log
  records it.
- **Automatically:** disable or delete the user in the identity provider, or remove their TeamCraft
  roles. Back-channel logout ends sessions at once; the user sync (`[oidc.sync]`) catches the rest
  within its interval (default 5 minutes). Enable both.
- Reactivate by enabling the user again in the identity provider and reactivating in TeamCraft.

## Devices

`DELETE /admin/users/{id}/devices/{device_id}` (or the console) removes one device, for example a
lost phone: it stops receiving messages and its sessions end. The user sees it in their security
log. Users can remove their own devices in Settings.

## Guests

A guest is a user with the `guest` role (same realm, see [Keycloak setup](keycloak.md)).

1. Create the user in Keycloak with only the `guest` client role.
2. Add them to the spaces and channels they need, or send a **space invite**:
   `POST /spaces/{id}/invites` with `email`, `role: guest`, `channel_ids` and optional `expires_at`.
   The invite applies when somebody signs in with that e-mail address and the provider has verified
   it (`[policy] invites_require_verified_email`). **No e-mail is sent**: tell the person yourself.
   The inviter is recorded as sponsor.
3. Review guests regularly (Users and guests > Guests: sponsor, expiry, where they are).

Rules for guests: see only what they were added to; find only people they share a conversation with;
DMs only with shared contacts (`[policy] guests_dm_only_with_shared_conversations`); history always
from join; their organization recovery is off by default.

**Meeting guests** (no account) are separate: see [Meetings](meetings.md).

## Audit log

Every admin action, device change and role change is logged with actor, target and time:
Administration > Audit (`GET /admin/audit`). Users see actions affecting them in
`GET /me/security-log`. The log holds metadata only. The CSV export button is a placeholder.

## Bots

Bridges authenticate as bots, see [Integrations](integrations-bridge.md).
