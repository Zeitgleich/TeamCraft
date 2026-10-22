# Managing spaces and channels

Admins and owners manage **every** space and channel, including private ones they are not in, without
being able to read them. Messages are end-to-end encrypted and an admin's devices are not members of
the groups.

## What you see for every conversation

Name, topic and description, space, category, visibility (public or private), archived flag, owners
and members with their roles, guests, history policy, retention, and the audit entries. **Not
visible:** message text, attachments, reactions and thread contents. Storage size is not known to
the server yet (shown as 0).

## Where to do it

Account menu > **Administration** > Channels (read live from the admin API). Everything below is
also available through the REST API (OpenAPI document at `/api/v1/openapi.json`, authenticate with
a bearer token).

| Task                                         | Console                  | API                                                           |
|----------------------------------------------|--------------------------|---------------------------------------------------------------|
| List all spaces, channels, members           | yes                      | `GET /admin/spaces`, `/admin/conversations`, `.../members`    |
| Set retention per channel                    | yes                      | `PUT /conversations/{id}/retention`, `/spaces/{id}/retention` |
| Join a private channel as admin              | yes (visible to members) | core action `AdminJoinChannel`                                |
| Create space                                 | not yet                  | `POST /spaces` (admin or owner)                               |
| Create channel, rename, topic, categories    | not yet                  | `POST /spaces/{id}/channels`, `PATCH /conversations/{id}`     |
| Archive, unarchive, change visibility        | not yet                  | `PATCH /conversations/{id}` (`is_archived`, `visibility`)     |
| Flag as integration channel                  | not yet                  | `PATCH /conversations/{id}` (`is_integration`)                |
| Add or remove members, change roles          | not yet                  | `/spaces/{id}/members`, `/conversations/{id}/members`         |
| Invite by e-mail (member or guest)           | not yet                  | `POST /spaces/{id}/invites`                                   |
| Per-channel moderation (deny post, react...) | not yet                  | `PATCH /conversations/{id}` (`moderation`)                    |

Console items marked *not yet* are placeholders in the current UI: assigning owners, archiving,
creating users, inviting guests, organization recovery and CSV audit export.

## How changes work without membership

Adding or removing members changes the MLS group, which only members can commit. The server enforces
the change for access control at once (it stops delivering to removed users) and the next online
member client commits it ([docs/admin.md](../../admin.md) §2.2). If no member is online the change is
pending cryptographically; access control already applies.

Today member clients accept membership changes from current group members and from roster admins.
Per-channel roles are not yet in a signed log, so every member may add and remove; the server's
permission check is the finer control.

## Admin joining a private channel

An admin can add themselves to a private channel, never silently: members see a system message with
an admin badge. The policy `admin_self_join` controls it, see
[Security policy](security-policy.md). `owner_approval` is not implemented yet and currently behaves
like a refusal.

## History policy

When adding a member, **full** shares earlier history with the new member's devices; **from join**
shares nothing earlier. The instance default is in the security policy (default *full*); guests are
always *from join*. Set per channel with `PATCH /conversations/{id}` (`history_policy`).

## Retention

See [Security policy](security-policy.md#retention).

## Orphaned channels

When the last owner of a private channel leaves, assign a new owner from the remaining members; you
do not need to be a member. The console button is a placeholder, use the members API.
