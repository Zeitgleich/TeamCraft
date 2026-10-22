# Administrator guide

For the people who install and run TeamCraft. Start with the installation overview.

| Page                                                  | Read it to                                                            |
|-------------------------------------------------------|-----------------------------------------------------------------------|
| [Installation overview](installation.md)              | See the pieces and the order to set them up                           |
| [Identity provider: Keycloak](keycloak.md)            | Create the client, roles, mapper and `tc_identity_key`                |
| [Configuration](configuration.md)                     | Use the TOML file and `TEAMCRAFT_*` overrides                         |
| [Roles and permissions](roles-and-permissions.md)     | Understand owner, admin, member, guest and the scheme                 |
| [Spaces and channels](spaces-and-channels.md)         | Manage rooms without reading them                                     |
| [Users, devices and guests](users-devices-guests.md)  | Deactivate users, remove devices, invite guests, audit                |
| [Security policy](security-policy.md)                 | `admin_self_join`, recovery, history, retention                       |
| [Integrations and the bridge](integrations-bridge.md) | Put logs and webhooks into encrypted channels                         |
| [Meetings](meetings.md)                               | CalCraft/CalDAV, mail, RSVP, guest lobby                              |
| [Calls and the SFU](calls-sfu.md)                     | Run the media server and rotate keys                                  |
| [Push](push.md)                                       | Web Push (VAPID); APNs and FCM status                                 |
| [Backups and upgrades](backups-upgrades.md)           | Back up, restore, upgrade ([docs/operations.md](../../operations.md)) |
| [Troubleshooting](troubleshooting.md)                 | Fix common problems; where design and code disagree                   |

Design background: [ARCHITECTURE.md](../../../ARCHITECTURE.md), [docs/admin.md](../../admin.md),
[docs/e2ee.md](../../e2ee.md).
