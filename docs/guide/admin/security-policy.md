# Security policy

The workspace policy is **signed by an owner** and stored by the server verbatim; clients verify the
signature and refuse entries not signed by an owner. A compromised server therefore cannot change
policy. Edit it under Administration > **Security** (read-only for admins and in the browser).

**Saving a policy needs an owner on the desktop app**: the client core signs the new version with the
owner's device key. The first save also founds the signed roster of owners and admins from the
identity provider's roles. Policy versions are append-only; each save is a new version.

## Settings

| Setting               | Values and default                                                        | Meaning                                                                                     |
|-----------------------|---------------------------------------------------------------------------|---------------------------------------------------------------------------------------------|
| `admin_self_join`     | `visible` (default), `owner_approval`, `disabled`                         | May admins add themselves to private channels                                               |
| Organization recovery | members on, guests off, 2 admins required                                 | Lets admins restore access if someone loses all devices and their passphrase                |
| History default       | `full` (default) or `from_join`                                           | What new members of a channel can read; guests always `from_join`                           |
| Retention             | default and limit: max age (days) and/or max messages; keep pinned `true` | See below                                                                                   |
| Link previews         | `off` (default), `allow_list`, `block_list` + domains                     | Applied by sending and receiving clients                                                    |
| Compliance archive    | only `off`                                                                | Reserved                                                                                    |
| Sessions              | require a second factor; maximum age; idle timeout                        | Server, at every request                                                                    |
| Passwords             | shortest password of local accounts (8 to 128)                            | Server                                                                                      |
| Guests                | allowed e-mail domains; DM scope; default expiry                          | Server                                                                                      |
| Directory             | display name format; show full name; show e-mail to members               | Server, in every response that names a person                                               |
| Statuses              | statuses on or off; custom statuses; away timeout; presets                | Server                                                                                      |
| Threads               | threaded view on or off; follow threads people are mentioned in           | Server (threads off: replies are refused); the follow default is for people who never chose |
| Push content          | generic, sender or full (default full)                                    | The receiving device, after opening the sealed payload                                      |

Where a value is set in the policy it wins over the server file (`[guests]`, `[policy]`,
`[local_accounts]`); the guest domain lists of both apply together.

The **Administration** console edits these in four sections: **Security** (self-join, recovery,
sessions, passwords, guests, history), **People and notifications** (directory, statuses, push
content, threads), **Retention** and, outside the signed policy, **Permissions**, **Announcement**
and **Instance** (below).

### admin_self_join

- `visible`: allowed. Members see "Admin joined" with an admin badge that users cannot hide.
- `disabled`: refused by member clients.
- `owner_approval`: **not implemented**; clients refuse self-joins because there is no approval flow.

Member clients enforce this themselves, so the server cannot bypass it.

### Organization recovery

Lets two admins (configurable) unwrap a user's key backup into a one-time request key when the user
lost all devices and the passphrase. The core implements owner setup (shares sealed to admin
devices), member opt-in and the two-admin recovery, and the server stores the sealed shares, requests
and approvals. The console's **Start recovery** button is not wired to this yet; recoveries are
started from the clients involved. After a recovery the organization secret and the user's backup
key are rotated. Details: [docs/e2ee.md](../../e2ee.md) §6.3. Every recovery is audited.

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

## Outside the signed policy

These are edited through the admin API by admins (not only owners) and are audited:

- **Permissions:** the scheme of what each role may do, for the instance and per space, as a matrix.
  A channel can only take permissions away (per-channel moderation in the channel details).
  The owner's rights and the signing right are fixed; guests can lose permissions but only gain the
  ones that are safe for them (posting, reactions, DMs).
- **Announcement:** one banner for everybody, with importance (information, warning, critical), an
  optional expiry and whether people may dismiss it. Clients show it above the app until dismissed
  or expired.
- **Instance:** read-only status of the media server (last contact, running calls), the push
  providers (Web Push, FCM, UnifiedPush, APNs: active, not configured, or not delivering yet) and
  storage (attachments and search index segments). Telephony is a placeholder.
