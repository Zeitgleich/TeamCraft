# TeamCraft deployment

An Ansible playbook that installs TeamCraft on one Debian VPS with **rootless Podman and Quadlet**,
plus a dry run that tests it end to end on your machine. The step-by-step quickstart is in
[docs/operations.md](../docs/operations.md) ("Quickstart: a fresh server in 15 minutes").

| Path                                        | Content                                                       |
|---------------------------------------------|---------------------------------------------------------------|
| `ansible/site.yml`                          | The playbook                                                  |
| `ansible/roles/teamcraft/`                  | The role: users, podman secrets, Quadlet units, Caddy, timers |
| `ansible/roles/teamcraft/defaults/main.yml` | Every setting with its default                                |
| `ansible/inventories/example/`              | Inventory to copy for your own instance (placeholders only)   |
| `ansible/inventories/dry-run/`              | Inventory of the local dry run                                |
| `secrets/README.md`                         | The secrets a deployment needs and how to generate them       |
| `test/dry-run.sh`                           | End-to-end dry run against a throwaway local target           |

## Runtime model: rootless Podman + Quadlet, one user per service

Every service runs as its **own unprivileged user** with its own subuid range, its own systemd
`--user` instance (lingering) and only the podman secrets it needs. Quadlet turns the
`.container` files into native systemd units, so `systemctl --user status` and `journalctl` work as
for any other service.

| Service            | User        | Network                                | Secrets it gets                                                        |
|--------------------|-------------|----------------------------------------|------------------------------------------------------------------------|
| `caddy`            | `tc-proxy`  | host, 80/443 tcp+udp                   | none                                                                   |
| `teamcraft-server` | `tc-server` | host, 127.0.0.1:8080                   | database URL, S3 keys, SFU token, call signing key, guest token secret |
| `teamcraft-sfu`    | `tc-sfu`    | host, 4433/udp; 127.0.0.1:8081 control | SFU registration token                                                 |
| `postgres`         | `tc-db`     | host, 127.0.0.1:5432                   | DB password                                                            |

Why host networking: rootless containers of different users can't share a podman network, and the
SFU shouldn't pay the per-packet cost of user-mode networking. Each container still runs in its
own user namespace; internal services bind to loopback only.

Hardening: images pinned by digest, `DropCapability=ALL`, `NoNewPrivileges`, read-only root
filesystem with noexec tmpfs, `RunInit=yes`, memory limits, journald logging. Caddy refuses
`/api/internal/` from outside.

**Dual stack:** everything reachable from outside listens on IPv4 and IPv6 – Caddy binds both by
default, the SFU uses a dual-stack `[::]` socket. Requirements: an IPv6 address on the VPS, A **and**
AAAA records for both host names, firewall rules for both families. Loopback-only services stay on
127.0.0.1.

The SFU certificate is obtained by Caddy; a `tc-proxy` timer copies it to `/srv/teamcraft/certs`
(group `tc-certs`), which `tc-sfu` reads; a `tc-sfu` path unit restarts the SFU when it changes.

Not on the VPS: the OIDC provider, S3 storage, optional `teamcraft-bridge` (trusted hosts).

## Your own inventory

Nothing about your instance belongs in this repository: host names, addresses and secrets live in
an inventory of your own, ideally a **separate private repository** (or any directory outside this
checkout). The playbook and role stay here and are used as they are, so upgrading is a `git pull`.

```text
my-teamcraft-instance/          # private repository
├── hosts.yml                   # copied from ansible/inventories/example/
├── group_vars/all.yml          # your names, issuer, S3, image digests
├── secrets.enc.yaml            # sops-encrypted (keys: secrets/README.md)
└── .sops.yaml                  # your operators' age keys
```

```sh
cp -r deploy/ansible/inventories/example ~/my-teamcraft-instance
cd ~/my-teamcraft-instance && sops secrets.enc.yaml          # write the secrets
ansible-galaxy collection install -r /path/to/teamcraft/deploy/ansible/requirements.yml
ansible-playbook -i ~/my-teamcraft-instance /path/to/teamcraft/deploy/ansible/site.yml
```

`teamcraft_secrets_file` defaults to `{{ inventory_dir }}/secrets.enc.yaml` in the example, so the
secrets travel with the inventory. Pin the role to a release by checking out its tag (`vX.Y.Z`) and
set the image digests from that release's `images.txt`. Decrypted files (`*.dec.*`, `*.plain.*`) are
git-ignored here; keep the same rule in your inventory repository.

## Dry run

`test/dry-run.sh all` deploys to a throwaway Debian trixie container on the local machine (systemd,
rootless podman inside, its own dual-stack Docker network, a local registry for digest-pinned images
built from this checkout, Caddy's internal CA, the dev Keycloak and Garage of
`docker-compose.dev.yml`) and checks the result: health over IPv4 and IPv6, HTTP/3, the SFU's QUIC
handshake, that internal ports and the internal API are not reachable, the SFU restart on a renewed
certificate, and encrypted messages between two users through the end-to-end harness. A second
playbook run must change nothing. `test/dry-run.sh down` removes it. Run it after every change to
the role.
