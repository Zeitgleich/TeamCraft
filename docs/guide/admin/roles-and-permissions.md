# Roles and permissions

## Instance roles

They come from the identity provider's client roles ([Keycloak setup](keycloak.md)).

| Role   | Typical person        | Can                                                                                |
|--------|-----------------------|------------------------------------------------------------------------------------|
| owner  | Company owner/IT lead | Everything an admin can, plus **sign** the admin roster and the workspace policy   |
| admin  | IT admin              | Manage all spaces, channels (metadata), users, devices, bots; read the audit log   |
| member | Employee              | Chat, create channels and group DMs, mention groups, create share links            |
| guest  | External person       | Write and react in conversations they were added to; start no conversation alone   |

Inside a space there is also **space admin** (manage the space's channels and members, invite guests)
and inside a channel **channel admin** (manage members, roles and properties, delete others' posts)
and **moderator** (remove members and delete others' posts and reactions, but no roles, properties,
privacy, and not admins or moderators). Handing out either role needs `manage_channel_roles`;
guests cannot hold them.

## How permissions are decided

One function, `teamcraft_proto::is_allowed`, is used by the server and the clients. The effective
permission is what the **permission scheme** grants the role **and** what the channel has not removed.
A channel can only *remove* permissions from its members and guests (post, react, mentions,
bookmarks, member management), never add any.

Default scheme, in short:

| Permission group                                                      | guest | member | space admin            | admin | owner |
|-----------------------------------------------------------------------|-------|--------|------------------------|-------|-------|
| Post, react, create DM and group DM                                   | yes   | yes    | yes                    | yes   | yes   |
| `@channel` and group mentions, bookmarks, share links, emoji          | -     | yes    | yes                    | yes   | yes   |
| Create channels                                                       | -     | yes    | yes                    | yes   | yes   |
| Manage space properties and members, invite guests                    | -     | -      | yes                    | yes   | yes   |
| Manage channel members, roles, properties; delete others' posts       | -     | -      | yes (as channel admin) | yes   | yes   |
| Create spaces, manage workspace and users, read audit, bots, webhooks | -     | -      | -                      | yes   | yes   |
| Sign roster and policy (`sign_workspace`)                             | -     | -      | -                      | -     | yes   |

Admins and owners can manage every channel, including private ones they are not in. **Acting on
content still requires membership**, and membership requires being an MLS group member: admins cannot
read what they are not part of.

## Guests

Guests see only the spaces and channels they were added to, find only people they share a
conversation with, and always get history "from join". See
[Users, devices and guests](users-devices-guests.md).

## Custom roles

Custom roles and per-space schemes are not available yet: the default scheme is built in.
