# Users, devices and guests

## Where users come from

Accounts are created at first login from the identity provider's token. Roles come from the client
roles ([Roles and permissions](roles-and-permissions.md)); TeamCraft has no local passwords. To
create a user, create them in Keycloak and assign a role.

## User list

Administration > **Users and guests** shows accounts with role, devices, spaces and last activity
(`GET /admin/users`, devices at `GET /admin/users/{id}/devices`). For people who hide their last
seen or appear offline, last activity is shown to the day only: enough to spot unused devices,
not when someone was active.

## Deactivate and reactivate

- **Deactivate** (console or `POST /admin/users/{id}/deactivate`) suspends the account: the user is
  signed out on every device at once and can't sign in (they see "Your account is deactivated"), and
  gets no notifications. They stay in their chats and spaces with their devices; others see them as
  deactivated.
- **Reactivate** (`/reactivate`) lets them sign in again on the same devices with everything as before.
- **Remove from all chats** (console, after deactivating; needs the desktop app or a browser where your account is set up, because your app signs the removals)
  is the second, explicit step for people who left: their devices are removed and they leave every
  chat and space, and the members' apps rotate the keys. A reactivation does not undo it. To do this
  automatically, set *Remove deactivated users from all chats after* (Administration > Settings >
  Server rules) to a number of days; empty means never.
- The audit log records each step (`user_deactivated`, `user_reactivated`, `user_removed_from_chats`).
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

### Guests with their own password

For people who are not in the identity provider. Off by default: set `[local_accounts] is_enabled =
true` with a `session_secret` (32+ random bytes, `TEAMCRAFT_LOCAL_ACCOUNTS__SESSION_SECRET`) and
`[mail]` with `[mail.smtp]` and `public_base_url`.

1. `POST /spaces/{id}/local-guests` with `email`, `display_name`, optional `channel_ids` and
   `expires_at` creates the account, adds it to the space as a guest and e-mails the activation
   link `<public_base_url>/activate#token=...`.
2. The guest chooses a password (default minimum 12 characters, or the policy's value) and connects
   an authenticator app through `POST /auth/local/activate`. **The web app has no activation page
   yet** (see [Troubleshooting](troubleshooting.md#known-gaps-between-design-and-code)); until it
   has, complete this step with the guest through the API.
3. Sign-in is on the login screen (*Sign in with a guest account*): password plus TOTP code.
   Defaults: TOTP required, 5 failed attempts lock the account for 15 minutes, sessions last 12
   hours at most (the workspace session policy can only shorten it).
4. An expired activation link is replaced with `POST /admin/local-accounts/{id}/activation`.

Guests may also be limited to certain e-mail domains, in the server file (`[guests]
allowed_email_domains`) and in the signed policy; the check applies to invitations, local guests,
external meeting invitees and sign-in. Guests expire automatically (90 days by default); the sponsor
is told when it happens.

Rules for guests: see only what they were added to; find only people they share a conversation with;
DMs only with shared contacts (`[policy] guests_dm_only_with_shared_conversations`), and a DM or
group DM with a guest needs a shared conversation with every participant; history always
from join; their organization recovery is off by default.

**Meeting guests** (no account) are separate: see [Meetings](meetings.md).

## Audit log

Every admin action, device change and role change is logged with actor, target and time:
Administration > Audit (`GET /admin/audit`). Users see actions affecting them in
`GET /me/security-log`. The log holds metadata only. The CSV export button is a placeholder.

## Bots

Bridges authenticate as bots, see [Integrations](integrations-bridge.md).
