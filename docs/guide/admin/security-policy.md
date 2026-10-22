# Security policy

The workspace policy is **signed by an owner** and stored by the server verbatim; clients verify the
signature and refuse entries not signed by an owner. A compromised server therefore cannot change
policy. Edit it under Administration > **Security** (read-only for admins and in the browser).

**Saving a policy needs an owner on the desktop app**: the client core signs the new version with the
owner's device key. The first save also founds the signed roster of owners and admins from the
identity provider's roles. Policy versions are append-only; each save is a new version.

## Settings

| Setting               | Values and default                                                        | Meaning                                                                      |
|-----------------------|---------------------------------------------------------------------------|------------------------------------------------------------------------------|
| `admin_self_join`     | `visible` (default), `owner_approval`, `disabled`                         | May admins add themselves to private channels                                |
| Organization recovery | members on, guests off, 2 admins required                                 | Lets admins restore access if someone loses all devices and their passphrase |
| History default       | `full` (default) or `from_join`                                           | What new members of a channel can read; guests always `from_join`            |
| Retention             | default and limit: max age (days) and/or max messages; keep pinned `true` | See below                                                                    |
| Link previews         | `off` (default), `allow_list`, `block_list` + domains                     | Applied by sending and receiving clients                                     |
| Compliance archive    | only `off`                                                                | Reserved                                                                     |

The policy also holds the rules below. Where a value is set here it wins over the server file;
where it is absent the server file (`[guests]`, `[policy]`, `[local_accounts]`) decides.

| Console section         | Setting                                                    | Enforced by                           |
|-------------------------|------------------------------------------------------------|---------------------------------------|
| Security                | Require a second factor, session length, idle timeout      | Server, at every request              |
| Security                | Shortest password of local accounts (8 to 128)             | Server                                |
| Security                | Guest e-mail domains, guest DM scope, default guest expiry | Server                                |
| People and notification | Display name format, full name, e-mail visibility          | Server, in user responses             |
| People and notification | Statuses on or off, custom statuses, away timeout, presets | Server                                |
| People and notification | Push content: generic, sender or full (default full)       | The receiving device                  |
| People and notification | Follow threads people are mentioned in, reply view         | Clients, as the default for new users |

The console also edits things outside the signed policy, through the admin API: the permission
scheme (Permissions), per-channel moderation and owners (Spaces and channels), the announcement
banner (Announcement) and, read-only, the media server, push providers and storage (Instance).

### admin_self_join

- `visible`: allowed. Members see "Admin joined" with an admin badge that users cannot hide.
- `disabled`: refused by member clients.
- `owner_approval`: **not implemented**; clients refuse self-joins because there is no approval flow.

Member clients enforce this themselves, so the server cannot bypass it.

### Organization recovery

Lets two admins (configurable) unwrap a user's key backup into a one-time request key when the user
lost all devices and the passphrase. The core implements owner setup (shares sealed to admin
devices), member opt-in and the two-admin recovery. The console's **Start recovery** button and the
approval flow need server endpoints that do not exist yet: treat it as **coming soon** for
operators. Details: [docs/e2ee.md](../../e2ee.md) §6.3. Every recovery is audited.

### Retention

The policy holds the **default** and a **limit**; owners of a space or channel choose within the
limit (`PUT /conversations/{id}/retention`, `PUT /spaces/{id}/retention`). A background job
(`[retention]`, hourly by default) deletes ciphertext, attachments and search index segments older
than the setting and drops emptied month partitions. Pinned messages survive when `keep_pinned` is
on. Integration channels get `[integration] default_retention_days` (30) unless set.

Deleted data is gone from the database but **remains in your backups** until they expire
([Backups and upgrades](backups-upgrades.md)).

## Limits in the server file

`[limits]` and `[security]` (request size, rate limits) are config, not policy; see
[Configuration](configuration.md).
