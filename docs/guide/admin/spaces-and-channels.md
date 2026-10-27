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

Two places. The **Administration** console (account menu) shows every space and channel, including
private ones, as metadata and edits what only admins edit. Everything else is done in the normal
client by anyone whose role allows it, because it needs the encryption keys of a member.
Everything is also available through the REST API (OpenAPI document at `/api/v1/openapi.json`,
authenticate with a bearer token).

| Task                                           | Where                               | API                                                                    |
|------------------------------------------------|-------------------------------------|------------------------------------------------------------------------|
| List all spaces, channels, members             | Console                             | `GET /admin/spaces`, `/admin/conversations`, `.../members`             |
| Set retention per channel or space             | Console                             | `PUT /conversations/{id}/retention`, `/spaces/{id}/retention`          |
| Join a private channel as admin                | Console (visible to members)        | core action `AdminJoinChannel`                                         |
| Per-channel moderation (deny post, react...)   | Console                             | `PUT /admin/conversations/{id}/moderation`                             |
| Assign an owner to an orphaned channel         | Console                             | `POST /admin/conversations/{id}/owner`                                 |
| Permission scheme per instance and space       | Console (Permissions)               | `PUT /admin/permission-scheme`, `/admin/spaces/{id}/permission-scheme` |
| Create space, channel, category; rename; topic | Client (channel and space settings) | `POST /spaces`, `/spaces/{id}/channels`, `PATCH /conversations/{id}`   |
| Archive, unarchive                             | Client (channel settings)           | `PATCH /conversations/{id}` (`is_archived`)                            |
| Make public / private                          | Client (channel settings)           | `PUT /conversations/{id}/visibility`                                   |
| Flag as integration channel                    | API                                 | `PATCH /conversations/{id}` (`is_integration`)                         |
| Add or remove members, change roles            | Client (members panel)              | `/spaces/{id}/members`, `/conversations/{id}/members`                  |
| Invite by e-mail (member or guest)             | Client (Invite people)              | `POST /spaces/{id}/invites`                                            |

Not available yet: moving a channel to another space, deleting a channel completely (archive it,
and let retention remove the content), space templates, default spaces for new users, and CSV
export of the audit log.

Converting a channel between public and private notifies the members with a system message and is
audited with the old and new value. Members of a channel that became private keep access; others
lose it. Cryptographic details: [docs/e2ee.md](../../e2ee.md) §4.7.

## How changes work without membership

Adding or removing members changes the MLS group, which only members can commit. The server enforces
the change for access control at once (it stops delivering to removed users) and the next online
member client commits it ([docs/admin.md](../../admin.md) §2.2). If no member is online the change is
pending cryptographically; access control already applies.

Today member clients accept membership changes from current group members and from roster admins.
Per-channel roles are not yet in a signed log, so every member may add and remove; the server's
permission check is the finer control.

## Admin joining a private channel

An admin can add themselves to a private channel, never silently: every member's app shows "…
joined this channel as a workspace admin", worked out from the encrypted membership change itself.
The policy `admin_self_join` controls it, see [Security policy](security-policy.md); with
`owner_approval` the admin asks and a channel owner lets them in.

## History policy

When adding a member, **full** shares earlier history with the new member's devices; **from join**
shares nothing earlier. The instance default is in the security policy (default *full*); guests are
always *from join*. Set per channel with `PATCH /conversations/{id}` (`history_policy`).

## Retention

See [Security policy](security-policy.md#retention).

## Orphaned channels

When the last owner of a private channel leaves, assign a new owner from the remaining members; you
do not need to be a member. The console button is a placeholder, use the members API.
