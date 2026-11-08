# Operations

What an operator needs to run TeamCraft: a quickstart for a fresh server, the pieces, health checks,
backups, upgrades and secret rotation. Configuration reference: `teamcraft.example.toml`; images:
`containers/README.md`; releases: `docs/release.md`.

## Quickstart: a fresh server in 15 minutes

The reference deployment is the Ansible playbook in
[Zeitgleich/teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy). It installs TeamCraft
on one Debian 13 server with **rootless Podman and Quadlet**, each component under its own
unprivileged user, IPv4 and IPv6, with the identity provider (Keycloak) and the attachment storage
(Garage) bundled or external, secrets generated on the server, Let's Encrypt certificates, a firewall
and nightly database backups.

```sh
git clone https://github.com/Zeitgleich/teamcraft-deploy.git && cd teamcraft-deploy
mkdir -p inventory/host_vars/chat.example.com
cp examples/hosts inventory/hosts                                   # the server's address
cp examples/vars.yml inventory/host_vars/chat.example.com/vars.yml  # domain and admin e-mail
just install-all
just ensure-admin alice 'a long password'                           # first user, role owner
just self-check
```

What you need first: a Debian 13 VPS (2 vCPU, 4 GB RAM) with public IPv4 and IPv6, A and AAAA records
for `chat.example.com`, `sfu.chat.example.com` and, when bundled, `id.` and `s3.` below it, and UDP
port 3478 open for calls. The playbook's documentation covers DNS, configuration, secrets (generated
or sops + age), updating, backups and every component.

Several brands on one server (`docs/branding.md`) are `teamcraft_brands` in the playbook: it adds
the host names to Caddy and the Keycloak client.

Then point the desktop app at `https://chat.example.com` and sign in. The first device of every user
creates their keys; save the recovery kit when asked (e2ee.md §6.5).

### Running the images yourself

The rest of this guide applies to any way of running the images. Pitfalls the deployment's dry run
found:

- The server image sets `TEAMCRAFT_SERVER__LISTEN_ADDR=[::]:8080`, and environment variables win over
  the TOML file. With host networking, set the variable yourself (e.g. `127.0.0.1:8080`), or the server
  is reachable on every address of the host, bypassing the proxy.
- The images are distroless: health checks must use the exec form
  (`["/usr/local/bin/teamcraft-server", "healthcheck", "--ready"]`), there is no shell.
- Rootless containers run as a subordinate uid: bind-mounted configuration files must be readable by
  others (they hold no secrets; pass secrets as podman secrets or environment variables).
- When the identity provider runs on the same host, point `[oidc] jwks_uri` at its loopback address:
  the server then fetches keys without a round trip through the proxy (and without trusting its CA).

The playbook's dry run (`test/dry-run.sh` there) deploys to a throwaway container and uses this
repository's end-to-end harness against it (`tests/e2e/tests/suite/deployed.rs`, `E2E_DEPLOYED_URL`,
`DEV_TOKEN_ISSUER` for `dev/dev-token.py`).

## Pieces and what holds state

| Piece                 | State                                                                                                            | Needs backup |
|-----------------------|------------------------------------------------------------------------------------------------------------------|--------------|
| `teamcraft-server`    | none of its own (calls roster and rate limits are in memory)                                                     | config only  |
| PostgreSQL            | accounts, structure, **ciphertext** of messages and MLS queues, audit log                                        | yes          |
| S3 bucket             | encrypted attachments and search index segments                                                                  | yes          |
| `teamcraft-sfu`       | none (rooms live while a call lasts)                                                                             | config only  |
| `teamcraft-bridge`    | SQLCipher database with the bot's keys and its channels' content keys                                            | yes          |
| `teamcraft-notetaker` | SQLCipher database with the bot's keys, its conversations' content keys and the transcripts of calls in progress | yes          |
| Identity provider     | users, roles, sessions                                                                                           | its own      |

The server only holds ciphertext. A stolen database or bucket gives an attacker metadata (who is in
which conversation, when messages were sent) but no message content or attachment keys. Clients
hold the keys; see `docs/e2ee.md` for what is recoverable and how (backup key, recovery kit, device
linking, organization recovery). **Losing the database loses everybody's history** unless a backup
exists; clients can still read what they have stored locally.

## Health and monitoring

- `GET /healthz` is liveness, `GET /readyz` also checks the database and answers 503 once a shutdown
  has begun. The images ship
  `teamcraft-server healthcheck [--ready]`, `teamcraft-sfu healthcheck`, `teamcraft-bridge
  healthcheck` and `teamcraft-notetaker healthcheck` (its liveness file, the notetaker listens on
  no port) for container health checks (no curl in the images).
- **Dependencies** (`[dependencies]`, issue #743). Each backend (database, object storage, identity
  provider, push gateway, SFU control) has a circuit breaker: after `failure_threshold` (3) failures
  in a row it counts as down, requests that need it are answered at once with **503, `Retry-After`
  and `error_code: "unavailable"`** instead of waiting for a timeout each, and after `open_secs`
  (15) one request probes it again. Idempotent calls are retried twice within a request first
  (200 ms, doubling, with jitter; timeouts are not repeated). `/readyz` lists every dependency's
  state:

  ```json
  {"status": "degraded", "dependencies": [{"dependency": "database", "state": "ok"},
    {"dependency": "object_storage", "state": "down"}]}
  ```

  Only the database makes `/readyz` answer 503, and only after the breaker opened (one slow ping,
  bounded by `readiness_timeout_ms`, doesn't take the instance out of the load balancer). Another
  dependency that is down makes it `degraded` with 200: every instance would meet the same outage,
  and taking them out would turn a storage outage into a full one: chat, calls and the gateway don't
  need it. Alert on `degraded` and on the log lines `dependency is down` / `dependency is back`.
- **Object storage outages.** Clients move the bytes with presigned URLs, so the server asks the
  storage itself every `storage_probe_secs` (10 s, 0 turns it off, one `HEAD` of a key that doesn't
  exist). Without traffic through the server an outage is seen within about three of those and the
  end of one at the next probe. While the storage is down no upload or download URL is handed out.
  Deletes that fail wait in the table `storage_cleanup` for the next background sweep (every
  10 minutes); a growing table means the storage refuses them, the log line is `deleting a queued object
  failed`.
- **Database outages.** Handlers' errors feed the database's breaker, so it opens under traffic
  without anybody asking `/readyz`: requests that need the database then answer at once rather than
  after `acquire_timeout_secs`, and one request pings after `open_secs`. Server info, brand and client
  compatibility answer from memory and stay up. Queries are not repeated by the server; the clients
  do that after `Retry-After`. A pool that is exhausted under load looks the same as an outage once
  nothing has succeeded three times in a row, and sheds load for `open_secs`.
- **Request deadline** (`request_deadline_secs`, 60 s, 0 turns it off). An API request that makes no
  progress this long answers 503, whatever it hangs on; receiving a piece of its body is progress, so
  a slow upload is not cut off. Imports and the gateway are exempt. Log line: `a request made no
  progress within the deadline`. Raise it if a legitimate admin action runs longer behind your proxy.
- Every 503 of the server carries `Retry-After` (in the header and as `retry_after_secs` in the
  body); the apps wait at least that long (one shared retry policy, capped exponential backoff
  with jitter). A short database blip at startup — also between the connect and the migrations —
  is waited for within `[database] startup_wait_secs` instead of exiting into a crash loop.
- Logs are structured `tracing` output on stderr (`RUST_LOG` / `[logging]`). They never contain
  tokens, keys or message content (checked by tests); IDs of public objects are logged.
- Built-in alerts for the failures that matter: see "Health and alerts" below.
- Watch: 5xx rate, 429 rate (rate limits, `docs/performance.md`), database connection saturation
  (`[database] max_connections`), disk of Postgres (message partitions), gateway connections.

## Metrics

The server, the SFU and the bridge can serve `GET /metrics` in Prometheus text format (issue #213).
It runs on a listener of its own that serves nothing else, so the reverse proxy doesn't publish it.
It is **off by default**, binds **loopback by default** (IPv4 and IPv6), and has no authentication:
to scrape from another host, use `[::]:<port>` (dual stack) with a firewall that only lets the scraper
in; the binary warns at start about any address that isn't loopback. A port that can't be bound stops
the start; only an IPv6 address that fails after another one worked is skipped (hosts without IPv6).

| Binary | Setting                                                             | Default addresses              |
|--------|---------------------------------------------------------------------|--------------------------------|
| server | `[metrics] enabled`, `listen_addrs`, `database_check_interval_secs` | `127.0.0.1:9464`, `[::1]:9464` |
| SFU    | `TEAMCRAFT_SFU_METRICS_ENABLED=1`, `TEAMCRAFT_SFU_METRICS_ADDRS`    | `127.0.0.1:9465`, `[::1]:9465` |
| bridge | `[metrics] enabled`, `listen_addrs`                                 | `127.0.0.1:9466`, `[::1]:9466` |

| Metric                                         | Type      | Labels                    | Binary         | Meaning                                                                           |
|------------------------------------------------|-----------|---------------------------|----------------|-----------------------------------------------------------------------------------|
| `teamcraft_build_info`                         | gauge     | `service`, `version`      | all            | Always 1                                                                          |
| `teamcraft_http_requests_total`                | counter   | `route_class`, `status`   | server, bridge | Requests answered; the rate limit's refusals included                             |
| `teamcraft_http_request_duration_seconds`      | histogram | `route_class`             | server, bridge | Time until the response's head (5 ms to 10 s); WebSockets until the upgrade       |
| `teamcraft_gateway_sessions`                   | gauge     |                           | server         | Gateway sessions, also those waiting to be resumed                                |
| `teamcraft_gateway_connections`                | gauge     |                           | server         | Sessions with a live connection                                                   |
| `teamcraft_gateway_resume_buffer_bytes`        | gauge     |                           | server         | Events buffered for resuming; compare `[gateway] max_buffered_bytes`              |
| `teamcraft_db_pool_connections`                | gauge     | `state`: `idle`, `in_use` | server         | The database pool                                                                 |
| `teamcraft_db_pool_max_connections`            | gauge     |                           | server         | `[database] max_connections`; saturation is `in_use` over this                    |
| `teamcraft_db_size_bytes`                      | gauge     |                           | server         | `pg_database_size`, from the periodic check                                       |
| `teamcraft_push_sends_total`                   | counter   | `provider`                | server         | Wake-ups the push service took or answered with a dead registration               |
| `teamcraft_push_failures_total`                | counter   | `provider`                | server         | Sends that failed at the push service                                             |
| `teamcraft_push_endpoint_failures_total`       | counter   | `provider`                | server         | Sends that failed at an endpoint a user registered: never makes it "failing"      |
| `teamcraft_push_provider_failing`              | gauge     | `provider`                | server         | 1 while the last attempt that says anything about the push service failed         |
| `teamcraft_push_retries_pending`               | gauge     |                           | server         | Wake-ups in the retry queue (`push_retries`), from the periodic check             |
| `teamcraft_dependency_state`                   | gauge     | `dependency`, `state`     | server         | 1 for the state the circuit breaker is in (`[dependencies]`), else 0              |
| `teamcraft_job_last_success_timestamp_seconds` | gauge     | `job`                     | server         | Unix time of the last success; 0: not since the start, or the job is switched off |
| `teamcraft_dpop_requests_total`                | counter   | `outcome`                 | server         | Requests with an identity provider's token: `bound`, `unbound`, `refused`         |
| `teamcraft_sfu_rooms`                          | gauge     |                           | SFU            | Calls running                                                                     |
| `teamcraft_sfu_participants`                   | gauge     |                           | SFU            | Devices in those calls, reconnecting ones included                                |
| `teamcraft_sfu_fallback_connections`           | gauge     |                           | SFU            | Open connections on the WebSocket fallback, for networks that block UDP           |
| `teamcraft_bridge_queue_depth`                 | gauge     |                           | bridge         | Submissions waiting for the worker (it holds 4096)                                |

Label values come from fixed lists, so the number of series is bounded, and each binary's tests check
its whole output against an allow-list. **No user, conversation or device IDs, no content, no tokens,
no error texts.**

- `route_class`, server: `admin`, `attachments`, `auth`, `calls`, `conversations`, `devices`, `e2ee`,
  `gateway`, `health`, `internal`, `meetings`, `messages`, `other`, `public`, `spaces`, `sync`, `users`,
  `web` (everything outside `/api/`). Bridge: `health`, `hooks`, `ingest`, `other`.
- `status`: `1xx` (WebSocket upgrades), `2xx`, `3xx`, `4xx` without 429, `429`, `5xx`.
- `provider`: `apns`, `fcm`, `unifiedpush`, `webpush`.
- `dependency`: `database`, `object_storage`, `identity_provider`, `push_gateway`, `sfu`, once in use.
  `state`: `ok`, `down`, `recovering`.
- `job`: `database_check`, `email_digest`, `guest_expiry`, `holiday_resync`, `key_package_pruning`,
  `meeting_notices`, `oidc_sync`, `partition_maintenance`, `retention`, `scheduled_messages`,
  `urgent_scheduler`.

**Cost.** With the listener off, requests pay nothing for it. With it on, one request costs a look at
the path's first segments, two label lookups and two atomic updates, whatever the size of the
instance. A scrape takes one pass over the gateway sessions in memory and no query. The database size
and the retry queue are read by a timer (`database_check_interval_secs`, default 60); while that fails
the last values stay, and the timestamp of `database_check` shows how old they are. Counters restart
at 0 with the process, which Prometheus handles, and everything is per instance.

The endpoint can't tell you that the server is down: probe `/readyz` from outside as well.

## Health and alerts

The server watches a fixed list of checks and tells owners and admins inside the app when one
keeps failing (issue #213). Administration → **System health** shows the checks, and the
Administration menu shows a count of open alerts. Names, states, times and one number per check:
nothing from any conversation, no addresses, no error texts.

| Check                   | Fails when                                                                      | Set up                                   |
|-------------------------|---------------------------------------------------------------------------------|------------------------------------------|
| `database`              | the database's breaker is down                                                  | always                                   |
| `object_storage`        | the storage's breaker is down                                                   | with `[storage]`                         |
| `identity_provider`     | the identity provider's breaker is down                                         | always                                   |
| `push_gateway`          | the push gateway's breaker is down                                              | with a push gateway                      |
| `push_service`          | the last attempt of a direct push service (APNs, FCM, Web Push) was refused     | with one of them                         |
| `media_server`          | the SFU has not reported for 2 minutes                                          | with `[calls]` and SFU reports           |
| `oidc_sync`             | the sync missed `[oidc.sync] stale_after_intervals` runs (default three)        | with the sync on                         |
| `oidc_sync_brake`       | its safety brake holds deactivations back (admin.md §3.2.3)                     | with the sync on                         |
| `retention`             | the job missed three runs (`[retention] interval_secs`)                         | with `[retention] enabled`               |
| `partition_maintenance` | the job missed three runs (every 12 hours)                                      | always                                   |
| `backup`                | the last backup is older than the limit (36 hours)                              | the deployment's heartbeat, see below    |
| `certificate`           | the served certificate expires within the limit (14 days)                       | the deployment's heartbeat, see below    |
| `disk_space`            | the disk is fuller than the limit (85 %)                                        | the deployment's heartbeat, see below    |
| `holiday_feed`          | the holiday feed's copy is older than `holidays.feed_max_age_secs` (7 days), or no fetch worked yet | with `holidays.feed_url` (console)       |
| `holiday_resync`        | the job that writes calendar events again when holidays change missed three weekly runs (docs/meetings.md, "Holidays") | with `[meetings]`                        |

**When a failure becomes an alert.** A background task evaluates the checks every
`[health] check_interval_secs` (30). It reads what the server keeps anyway (breakers, push results,
when jobs last ran) and three small files: no database query and no call to a backend, so a slow
dependency cannot slow it down. A failing check shows as *Failing*; it becomes an *Alert* only after
failing for the hold time **without a break** (default 10 minutes). It ends only after the check has
been fine for the clear time (default 5 minutes). So:

- a restart raises nothing: breakers start closed, jobs are measured from the start plus their first
  delay plus `[health] startup_grace_secs` (300), and the media server gets the same grace;
- a slow or flaky dependency raises nothing unless it keeps failing: one good answer starts the
  hold again, and a breaker needs `failure_threshold` failures in a row to count as down at all;
- a frozen process or a clock jump (more than four intervals between two evaluations) forgets what
  was counted instead of adding the pause to the hold;
- a breaker that is probing again (`recovering`) changes nothing until the next call decides.

Known limit: a direct push service whose last attempt failed stays failing until its next
attempt succeeds. On an instance that sends no push for the whole hold time after one failure, that
shows as an alert, and it ends with the next successful send.

**Who is told, and how.** Raising or resolving an alert sends the gateway event `health_alert_update`
to the instance's owners and admins and nobody else. It has no payload: the console reads
`GET /api/v1/admin/health` once when told (and once after the gateway reconnects, and once at the
start), so nothing polls. The System health page itself reloads every 30 seconds while it is open and
the page is visible; every other page reads nothing for it. Alerts are kept in memory (the last 50);
a restart starts the list again, and a failure that lasts is raised again after the hold time.
Delivery by e-mail or to a webhook is not part of this yet (#213).

**Settings** (Administration → Settings → Health alerts; the configuration file only names paths):

| Setting                | Default  | Meaning                                                      |
|------------------------|----------|--------------------------------------------------------------|
| `is_enabled`           | on       | Off: the checks still show, nobody is told                   |
| `hold_secs`            | 10 min   | How long a check must fail before it is an alert (1 min up)  |
| `clear_secs`           | 5 min    | How long it must be fine before the alert ends (1 min up)    |
| `backup_max_age_secs`  | 36 hours | The backup is too old after this (1 hour to 30 days)         |
| `certificate_min_days` | 14       | Too close to expiry with fewer days (1 to 365)               |
| `disk_max_percent`     | 85       | Too full above this (50 to 99)                               |

**What the deployment reports.** The server cannot see its backups, its certificate or the disk
from inside its container. With `[health] heartbeat_dir = "/path"` it reads three files there at
every evaluation, each holding one number as text (a trailing newline and a decimal are fine):

| File                    | Content                                                          | Written by the deployment                 |
|-------------------------|------------------------------------------------------------------|-------------------------------------------|
| `backup_last_success`   | Unix time in seconds when the last backup finished successfully  | the backup job, after each success        |
| `certificate_not_after` | Unix time in seconds when the served certificate expires         | a probe of the public endpoint, daily     |
| `disk_used_percent`     | Percent of the busiest data disk in use, 0 to 100                | a cron job, at least every 15 minutes     |

Without the directory these three checks show as *Not set up*. With it, a file that is not there yet
shows as *No data yet* (the deployment should write `backup_last_success` once at install time and after
every backup, so that a backup that never works is still found) and one that is not a number as well.
A timestamp a little in the future (clock skew) counts as just now. The files are read-only for the
server, at most 64 bytes each. The deployment repository has to write them and mount the directory
into the server container; that is tracked as a follow-up of #213.

An external probe of `/readyz` stays part of the story: the built-in alerts cannot tell you that
the server itself is down.

## Reverse proxy and network

- TLS terminates at the proxy. It must pass WebSocket upgrades on `/api/v1/gateway` and
  `/api/v1/rendezvous` and not buffer them. Set `[security] trust_forwarded_for = true` and
  `hsts = true` once the proxy sets `X-Forwarded-For` (the last entry is used) and serves HTTPS only.
  **Without `trust_forwarded_for` behind a proxy, every client has the proxy's address**: they share one
  rate limit, and bad tokens from anybody block everybody. The server warns once when it sees
  `X-Forwarded-For` from a private address.
- Answer `/share-target` at the proxy, so a body never reaches disk there (nginx buffers request
  bodies by default):

  ```nginx
  location = /share-target { return 303 /?share-failed; }
  ```

  Sharing into the installed web app posts plaintext to that path, and only the client's service
  worker may see it (docs/e2ee.md §9.3). The server answers the same way without reading the body, as
  a fallback. The web manifest declares `/share-target` as the web app's share target, so the proxy
  line must be in place.
- Never expose `/api/internal/` to the internet: it is for the SFU and the mail inbox handler, and is
  protected by bearer tokens, but there is no reason to publish it.
- Everything facing the network is dual stack (IPv4 and IPv6). The SFU needs UDP `3478` reachable
  from clients (QUIC); its health endpoint (`8081/tcp`) is for the orchestrator only. On networks
  that block outbound UDP (some corporate, hotel and guest networks), calls use the WebSocket
  fallback below, at lower quality, if your proxy passes it.
- **The call fallback for networks that block UDP** (ARCHITECTURE.md §6.6). The SFU listens for it on
  `TEAMCRAFT_SFU_FALLBACK_BIND`, plain HTTP: **never publish this port**, the join token would go in
  the clear. The default is `127.0.0.1:8082`, for a proxy on the same machine; the container image
  sets `[::]:8082`, since the proxy is another container there, so don't publish it and don't run
  that image with host networking without changing it (the reference deployment does run the SFU
  with host networking: there it is `127.0.0.1:8082`). The SFU warns at start when it listens on
  every address but trusts only a proxy on the machine itself, which is one of those two mistakes.
  If the port can't be bound, the SFU logs an error and runs on without the fallback. **An existing
  deployment has to add the route**, or clients on such networks keep failing to join as before;
  nothing else breaks without it. Configurations for Caddy and nginx (Plesk included), the order of
  the rollout and how to check it: [Call fallback through the proxy](#call-fallback-through-the-proxy).
  - The reverse proxy serves `wss://<SFU host>/call/fallback` on 443 and passes it to that port:
    WebSocket upgrade allowed, no response buffering, idle timeout above a few seconds (the SFU pings
    each second), `X-Forwarded-For` set. The SFU host name needs a TCP/443 route and a certificate;
    the bundled playbook's certificate already covers `sfu.<host>`.
  - `TEAMCRAFT_SFU_FALLBACK_TRUSTED_PROXIES` names the proxy: addresses or networks, comma-separated
    (`172.18.0.0/16`). Only from these peers does the last `X-Forwarded-For` entry count for the
    limit of 16 unjoined connections per address (an IPv6 /64 is one address). The default is the
    machine itself, which fits a proxy on the same host; **a proxy in another container has to be
    listed, or all its clients share one limit**, and the SFU logs one warning when it sees the
    header from a peer it doesn't trust. Don't list networks that clients can connect from: they
    could then name any address.
  - Clients are told the URL with their join token. The default is `wss://<host of sfu_url>/call/fallback`;
    `[calls] sfu_fallback_url` sets another one, and the web client's `connect-src` follows it.
  - **Turning it off:** `[calls] sfu_fallback_enabled = false` on the server stops offering it, and
    `TEAMCRAFT_SFU_FALLBACK_ENABLED=0` on the SFU closes the listener. Without an offer, clients behave
    as if there were no fallback; QUIC's behaviour doesn't depend on it either way.
  - Bounds, each with a setting: `TEAMCRAFT_SFU_FALLBACK_MAX_CONNECTIONS` (in a call, default 1024),
    `TEAMCRAFT_SFU_FALLBACK_MAX_UNJOINED_CONNECTIONS` (not yet in a call, default 128),
    `TEAMCRAFT_SFU_FALLBACK_HANDSHAKE_TIMEOUT_MILLIS` (the upgrade, default 3000),
    `TEAMCRAFT_SFU_FALLBACK_FIRST_FRAME_TIMEOUT_MILLIS` (the `join` of a connection that showed no
    join token in the upgrade, default 3000),
    `TEAMCRAFT_SFU_FALLBACK_QUEUE_BYTES` (per connection, default 1 MiB, so at most about 1 GiB in all
    with every connection full), `TEAMCRAFT_SFU_FALLBACK_QUEUE_MAX_AGE_MILLIS` (default 300) and
    `TEAMCRAFT_SFU_FALLBACK_SEND_BUFFER_BYTES` (default 128 KiB). `teamcraft_sfu_fallback_connections`
    in the metrics shows how many use it.
  - Clients send their join token with the upgrade too (`Sec-WebSocket-Protocol`, #1297), and the
    SFU checks it before upgrading: a client with a valid one doesn't hold the slots anybody can
    open. **Update the SFU before or with the server and the apps**: a client that offers the header to an
    SFU from before can't open the fallback (QUIC is not affected). Once no client from before #1297
    is left, `TEAMCRAFT_SFU_FALLBACK_REQUIRE_UPGRADE_TOKEN=1` refuses upgrades without a token, so
    someone with many addresses can hold an unjoined slot only for the upgrade. The proxy must pass
    `Sec-WebSocket-Protocol` through (nginx and Caddy do) and shouldn't log it.
  - **Back to QUIC during a call** (#1309, #1338): a client on the fallback (the apps, and browsers
    with WebTransport) tries QUIC again about every minute and moves the running session over
    without a rejoin. It needs nothing to set up: the SFU's QUIC port as before. It is on only
    between an SFU and apps that both have it, in any order of deploying them: an SFU from before
    sends no `switch_secret`, so newer apps never try, and older apps ignore it. Behind a load
    balancer with SFUs of both versions, a newer app that reaches an older SFU is told "expected
    join" and stays on the fallback for that call. The usual order, the SFU before or with the
    server and the apps, is fine.
  - **Descriptors:** the SFU warns at start when `RLIMIT_NOFILE` is below those two connection limits
    plus a margin. Raise it (`LimitNOFILE=` in systemd, `--ulimit nofile=` in Docker) or lower the
    limits: running out would also hurt QUIC calls through the roster reports and the health endpoint.
- The SFU asks for a 4 MiB UDP receive buffer (`TEAMCRAFT_SFU_UDP_RECEIVE_BUFFER_BYTES`), so a
  keyframe's burst isn't lost while it is busy. The kernel caps it at `net.core.rmem_max` without an
  error, often about 200 KiB: raise that sysctl on the host to at least the same value (it isn't
  per container). The SFU logs a warning at start when it got less. With less, a 1440p share's
  keyframe can get lost while the SFU is busy: viewers ask for another, and fall back to the lower
  layer if that keeps happening, so a share turns blurry or freezes for seconds rather than ends.
- Keep the SFU's UDP port out of anything that tracks UDP "connections" by address and port (a
  load balancer without QUIC connection-ID routing, a strict stateful firewall): when a client
  changes networks its QUIC connection continues from a new address (connection migration,
  ARCHITECTURE.md §6.5), and such a box would drop or misroute those packets. A plain DNAT/port
  forward to one SFU is fine.
- A participant whose connection is lost keeps their place in the call for
  `TEAMCRAFT_SFU_RECONNECT_GRACE_SECS` (default 30) while their app rejoins; a connection is lost
  after `TEAMCRAFT_SFU_IDLE_TIMEOUT_SECS` (default 10) without packets. Longer grace periods cover
  longer outages (trains, lifts) but keep a departed participant listed as "reconnecting" longer.
- Restarting or redeploying the SFU doesn't end running calls. On SIGTERM it closes every call
  connection as "going away", waits up to `TEAMCRAFT_SFU_SHUTDOWN_DRAIN_SECS` (default 2) for the
  closes to go out, and exits. Apps rejoin with a fresh join token, spread over about a second, and
  keep trying with back-off for the reconnect grace period they learnt (default 30 s): bring the
  new SFU up at the same address within that time and the calls continue after a short gap, with a
  fresh DAVE group. A crash works the same way, only the apps notice the dead connection after
  about 5 s instead of at once. Rooms, locks and host removals live in the SFU's memory and start
  fresh; the server's meeting admission still applies to every rejoin. Desktop and mobile apps of
  version 1.0.2 and older end the call on "going away" (a browser tab rejoins).
- The SFU reports its whole call roster to the server at start and every
  `TEAMCRAFT_SFU_ROSTER_INTERVAL_SECS` (default 30); the server drops participants the SFU no
  longer has (after `[calls] roster_grace_secs`, default 60, while the SFU is fresh from a restart)
  and ends calls with nobody left. If the SFU stops reporting for `[calls] roster_timeout_secs`
  (default 120), the server ends its calls. Keep the timeout a few intervals long.
- One account may have `[calls] max_devices_per_user_per_call` devices (default 3) in the same call.
  The server refuses a join token for a further device and the SFU refuses its join, both with
  `call_device_limit`. Set the SFU's `TEAMCRAFT_SFU_MAX_DEVICES_PER_USER_PER_CALL` to the same value.
- Restarting `teamcraft-server` doesn't end running calls either: at start it reads the SFU's
  roster over `[calls] sfu_control_url` (and otherwise from the SFU's next roster) and takes over
  the calls that kept running, without ringing again. Calls whose join tokens came from a server
  before 1.0.8 can't be taken over (the tokens don't name the conversation).

### Call fallback through the proxy

What the reverse proxy needs so that clients on networks that block UDP reach the SFU's fallback on
TCP 443 (#1226). The server and the apps need nothing for it: from 1.0.20 on the server offers
`wss://<host of sfu_url>/call/fallback` with every join token, and a client uses it only when QUIC
doesn't connect. Until the route exists such a client fails to join, as it did before there was a
fallback.

**The SFU's listener.** Where the proxy is decides the SFU's two settings:

- **Proxy on the same machine, SFU with host networking** (the reference deployment: Caddy or the
  host's nginx, rootless Podman with `Network=host`): `TEAMCRAFT_SFU_FALLBACK_BIND=127.0.0.1:8082`,
  the address the proxy forwards to (`[::1]:8082` if that is IPv6). The image's `[::]:8082` would
  listen on every address of the host, plain HTTP, guarded only by the firewall. The default trusted
  proxy, the machine itself, fits.
- **Proxy in another container**: keep the image's `[::]:8082`, don't publish the port, and set
  `TEAMCRAFT_SFU_FALLBACK_TRUSTED_PROXIES` to the proxy's address or its container network.

Take another port if 8082 is in use on the host (the SFU logs `no call fallback: its port can't be
bound` and runs on without it). The defaults need about 1,216 descriptors (1024 joined, 128 unjoined
and a margin): the SFU warns at start when `RLIMIT_NOFILE` is lower; in Quadlet,
`PodmanArgs=--ulimit=nofile=4096:4096` raises it (for rootless Podman, up to the user's hard limit).

**Caddy** (the bundled proxy of the reference deployment). Caddy passes WebSocket upgrades, doesn't
buffer them, and sets `X-Forwarded-For` to the client's address; a path matcher without `*` is exact:

```caddy
sfu.chat.example.com {
	handle /call/fallback {
		reverse_proxy 127.0.0.1:8082 {
			# Otherwise a reload of Caddy closes every call on the fallback at once (they rejoin).
			stream_close_delay 5m
		}
	}
	handle {
		respond "TeamCraft SFU" 200
	}
}
```

**nginx**, also as Plesk's "Additional nginx directives" of the domain that serves the SFU's host
name:

```nginx
location = /call/fallback {
  proxy_pass http://127.0.0.1:8082;
  proxy_http_version 1.1;
  proxy_set_header Upgrade $http_upgrade;
  proxy_set_header Connection "upgrade";
  # The client's address, replacing whatever the client sent (the SFU takes the last entry).
  proxy_set_header X-Forwarded-For $remote_addr;
  proxy_buffering off;
  proxy_read_timeout 30s;
  proxy_send_timeout 30s;
}
```

Keep the `=`: nginx prefers a regular-expression location over a plain prefix one, so behind a
regex catch-all (Plesk's directives of the reference deployment have one) `location /call/fallback`
would send the fallback to the chat server. When the SFU's host name is an alias of the chat domain
(one Plesk domain, one certificate), the location answers on the other names as well; no harm, only
clients given the SFU's URL ask for it. With a CDN or a second proxy in front, use
`$proxy_add_x_forwarded_for` and list the inner proxy as trusted instead.

Other proxies need the same four things: the exact path, the upgrade, no buffering, and
`X-Forwarded-For`. Apache (2.4.47 or later): `ProxyPass "/call/fallback"
"http://127.0.0.1:8082/call/fallback" upgrade=websocket`; HAProxy and Traefik pass upgrades on
their own (HAProxy: `timeout tunnel` of at least 30 s).

**Order of the rollout.** Each step is safe without the next one:

1. SFU and server of 1.0.21 or later (the SFU before or with the server). Calls on QUIC don't change.
2. The SFU's bind address and, for a proxy in another container, the trusted proxies; restart the
   SFU. Running calls rejoin within seconds (above).
3. The route in the proxy; reload it.
4. Check it (below).
5. Once no app from before #1297 is left: `TEAMCRAFT_SFU_FALLBACK_REQUIRE_UPGRADE_TOKEN=1`.

To roll back, remove the route (clients behave as before it) or stop offering the fallback with
`[calls] sfu_fallback_enabled = false`.

**Checking it**, from outside the server:

```sh
curl -sS -o /dev/null -w '%{http_code}\n' --http1.1 --max-time 10 \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' \
  -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' https://sfu.chat.example.com/call/fallback
```

- `101`: the SFU took the upgrade (and closes it 3 s later for want of a `join`). The route works.
- `401`: the SFU as well, with `TEAMCRAFT_SFU_FALLBACK_REQUIRE_UPGRADE_TOKEN=1`.
- `200` or `404`: the request didn't reach the SFU (Caddy's placeholder, the chat server, or a
  prefix location losing to a regex one).
- `502` or `504`: the proxy can't reach the SFU's port (bind address, port, firewall), or the
  request had no upgrade headers: to a plain `GET` the SFU doesn't answer at all.

Then a call from a client with UDP blocked (a guest network, or a firewall rule on the client that
drops UDP to the SFU's port) joins after about 3 s and shows "Limited network, call quality may be
lower"; `teamcraft_sfu_fallback_connections` in the SFU's metrics counts it, and the SFU logs no
warning about `X-Forwarded-For`.

**In the reference deployment** (Zeitgleich/teamcraft-deploy) this means: a port `sfu_fallback`
(8082) among the loopback ports, checked by the preflight like the others;
`Environment=TEAMCRAFT_SFU_FALLBACK_BIND=<upstream address>:<that port>` in the SFU's Quadlet unit;
the Caddy route above in the SFU's site; for deployments without Caddy, the location in the generated
nginx, Plesk, Apache, HAProxy and Traefik configurations of the SFU's host name, which so far only
gets a certificate; and the `curl` check in the self-check.

### Company proxies, TLS inspection and captive portals (clients)

How the apps behave on networks between the user and the server (issue #157):

| Platform | Proxy                                                                                    | Certificate authorities                                                                                              |
|----------|------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------|
| Desktop  | `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY`, then macOS and Windows settings | The operating system's store (Keychain and MDM profiles, Windows store and group policy, the Linux system store)     |
| iOS      | The system's HTTP(S) proxy (Wi-Fi settings, MDM profile)                                 | The system's trust settings, including profiles the MDM installed                                                    |
| Android  | The network's HTTP proxy (`LinkProperties`)                                              | The system CAs; user and MDM-installed CAs only with the managed configuration `trust_user_certificates` set to true |
| Web      | The browser's                                                                            | The browser's                                                                                                        |

- REST and the WebSockets (gateway, device-link relay) use the same proxy; the WebSockets tunnel with
  HTTP `CONNECT`, including `user:password@` from a proxy URL. REST also takes `https://` proxies;
  the WebSockets tunnel through `http://` ones only and connect directly otherwise. SOCKS proxies
  are not used. PAC scripts are never evaluated: on a network that only has one, the apps connect
  directly. On Linux desktops only the environment variables count (no GNOME/KDE settings). A proxy
  change on the desktop applies after a restart of the app; on mobile it applies when the network or
  its proxy changes, and the iOS notification extension reads it whenever it opens the core.
- Android ignores CAs that users install themselves, as Android apps do by default, so a CA someone
  talked the user into installing reads nothing. An organization that inspects TLS on managed
  devices turns on `trust_user_certificates` in its MDM's managed configuration for the app; it
  applies at the next app start.
- Calls over QUIC never go through an HTTP proxy. Where that is all there is, the call fallback's
  WebSocket does: the apps tunnel it with `CONNECT` as they do the gateway, the browser by itself.
- A captive portal (a login page answering instead of the server: HTML where the API answers JSON, a
  redirect to another site, HTTP 511) and a refused certificate are shown in the connection banner
  as such ("sign in to this network", "this network may inspect encrypted traffic"); queued messages
  and uploads wait and are sent once the server answers.
- Happy Eyeballs (RFC 8305, issue #420): when the host's IPv6 route swallows packets, the apps fall
  back to IPv4 instead of staying "unreachable". REST (hyper-util, 300 ms), the gateway and relay
  WebSockets and the QUIC handshake to the SFU for calls (250 ms) race the address families. The
  Android app's own requests (sign-in at the identity provider and token refresh, a local
  account's sign-in, the server check, brand images, the video codec download) go through the
  core's HTTP client too, with the same proxy and certificate authorities (#433).

## Backup and restore

### PostgreSQL

Use continuous archiving for production (base backup + WAL, `pg_basebackup` or pgBackRest/Barman) so
that a restore can stop at a point in time. A logical dump is enough for small instances:

```sh
pg_dump --format=custom --file=teamcraft-$(date +%F).dump "$DATABASE_URL"
```

Notes:

- `messages` and `mls_messages` are partitioned by month. `pg_dump` handles that; a dump of a big
  instance is large. Backups of closed months never change, so incremental tools save a lot.
- Restore into an empty database with the **same or an older** server version (migrations are
  forward-only; the server migrates on startup):

  ```sh
  createdb teamcraft && pg_restore --dbname=teamcraft --no-owner teamcraft-2026-10-22.dump
  ```

  Then start the server. It creates the partitions it needs ahead of time.
- Test the restore regularly: restore to a scratch database, start a server against it, and check
  `/readyz` and a login.
- Retention deletes rows (and drops emptied partitions) permanently. A backup is the only way back,
  which is also why a backup must be deleted when the retention promise says so.
- A restore loses the channel role entries signed after the backup, while members' devices still hold
  them: their apps raise the alarm that the server shows different role logs, in every channel with a
  role change since, and channel owners can't approve admins there. A workspace admin looks at each
  channel's roles once ("Check this channel's roles" in its members section) to end it (docs/e2ee.md §4.6.3).

### S3

Attachments, index segments and brand images are immutable objects named by random IDs, so a plain
`rclone sync`/`mc mirror` to a second bucket, or the provider's bucket versioning and replication,
works. The database references objects by key; a restore needs the database and the bucket from
about the same time. Objects without a database row are garbage (an upload that never completed) and
harmless; rows without an object show up as failed downloads.

Files over 8 MiB go up in parts (S3 multipart). Browsers read each part's `ETag` from the storage's
answer, so the bucket's CORS rule must expose it (`<ExposeHeader>ETag</ExposeHeader>`, as
`dev/garage-cors.py` sets); without it web uploads of large files fail. The server aborts uploads in
parts that got no new part for `multipart_expiry_secs` (`[storage]`, default one day). The bucket
also needs a lifecycle rule that aborts incomplete multipart uploads (`AbortIncompleteMultipartUpload`,
for example after 3 days): the server can't abort an upload it lost track of (it crashed between
creating the upload and recording it, or aborting a cancelled one failed), and their parts would
take space unseen. `dev/garage-cors.py` sets both rules on the dev bucket.

### Restore order after a total loss

1. Postgres from the latest backup, S3 bucket from its copy.
2. Server and SFU with the same configuration and **the same secrets** (see below), including the
   settings key (`[secrets]`; without it the mail and calendar passwords set in the console must be
   entered again), identity provider reachable.
3. Bridges and the notetaker: restore their database file (it holds the bot's keys) or re-enroll
   them with their passphrase (the bot's backup key is on the server, so a lost database is
   recoverable; a lost notetaker database also loses the transcript of a call in progress).
4. Clients reconnect and run a full sync; devices whose rows were lost re-register.

## Owners

Owners (the role `owner` in the identity provider, `just ensure-admin` above) sign the workspace's
admin roster and policy with their devices (admin.md §2.3). The server can't do it for them, and
there is no operator override: an operator with full access to the server and database still can't
make someone an owner.

- Make sure there are **at least two owners** with a set-up device and recovery kit. Give the
  second person the owner role in the identity provider before the first owner signs the first
  policy: the first roster takes every owner and admin the server lists then. The console
  (Administration → Security → Owners) warns when the signed roster has fewer owners than
  recommended and shows when each owner last signed.
- **Handing over:** the successor goes into the signed roster as owner first, the old owner is
  removed afterwards. A roster without an owner is refused by the apps and the server
  (`last_owner`). Both steps are made in the console (Administration → Security → Owners) by an
  owner, who signs each change on their device. Role changes in the identity provider show up there
  as a difference until an owner signs them.
- An owner's app re-signs the roster every 15 days while it is in use. If no owner opens TeamCraft
  for 30 days, member clients stop accepting the admins until an owner does; the console warns a
  week ahead.
- **If every owner is lost** (left, or lost all devices and the recovery kit), the workspace
  can't be administered any more, and there is **no recovery path** (issue #188): chat keeps
  working, admin actions stop once the freshness window (30 days) ran out, restoring a backup does
  not help, and a new first owner is refused by every client (`RootChanged`). The only way out is
  [setting up again](#setting-up-again). Details: admin.md §2.3.1.

### Role signing

Owners' apps keep the admin roster following the identity provider by themselves (e2ee.md §3.1.3). Updating needs
no action in the apps. **The realm has to give the apps what they verify**, once, and only on instances where
that isn't there yet. `Zeitgleich/teamcraft-deploy` does it for the realm it runs; for any other Keycloak:

| What                                                 | Where in Keycloak                                                                                                                                                                                       |
|------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Client roles in the ID token (new)                   | Client → Client scopes → the client's dedicated scope → Add mapper → "User Client Role": client ID your client, token claim name `resource_access.${client_id}.roles`, multivalued, "Add to ID token" on |
| Identity key in the ID token                         | Same scope → "User Attribute": user attribute and token claim name `tc_identity_key`, "Add to ID token" on                                                                                              |
| The attribute itself                                 | Realm settings → User profile: attribute `tc_identity_key`, users may view and edit, admins may view                                                                                                    |
| Account API for users                                | Keep the client's default `account` audience and the users' default `manage-account` role                                                                                                               |

Or with `kcadm.sh`, for the client's internal ID `$CLIENT` in realm `$REALM`:

```sh
kcadm.sh create clients/$CLIENT/protocol-mappers/models -r $REALM \
  -s name='client roles in ID token' -s protocol=openid-connect \
  -s protocolMapper=oidc-usermodel-client-role-mapper \
  -s 'config."usermodel.clientRoleMapping.clientId"='$CLIENT_ID \
  -s 'config."claim.name"=resource_access.${client_id}.roles' \
  -s 'config."jsonType.label"=String' -s 'config.multivalued=true' \
  -s 'config."id.token.claim"=true' -s 'config."access.token.claim"=false' \
  -s 'config."userinfo.token.claim"=false'
```

ID tokens may be signed with RS256 (Keycloak's default), ES256 or EdDSA. The server refuses ID tokens as access
tokens, whatever they carry. Another provider needs the same three things under the same claim names, and an account
API or some other way for users to set the attribute (`crates/teamcraft-core/src/attestation.rs` is the Keycloak flavour).

**Role names.** The apps read `owner` and `admin`, whatever `[oidc.roles]` says, because they don't take the server's word
for what a role means. If your realm calls them something else, an owner enters the names once under Administration → Security.

**Without the mapper** nothing breaks and nobody is prompted: the provider is still pinned, the roster stays as it is, re-signing for
freshness goes on, and the Security section of the console says what is missing to those who see it, for owners with the way to change roles by hand.
The apps log `the identity provider's ID tokens carry no client roles`. On an instance with no signed roster yet, the first
owner signs themselves in by saving the policy once.

**Taking people off.** A role removed in the realm counts as soon as the person's app or the server has a token that shows it. For
somebody who never signs in again the server needs `[oidc.sync]` (the service account that already deactivates disabled users), and
then an admin comes off after a week, one a week per owner's device, named meanwhile only in the Security section. An owner never comes off that way, and
without `[oidc.sync]` nobody does: another owner removes them by hand. Keep two owners. **To hand over ownership,** move the role in the realm
while the old owner still holds a client role, for example `member`: their app signs the successor. If they hold no role any more, the
server refuses them and nobody can sign; give them `member` for a moment.

**Moving to another identity provider** (a new realm or a new provider): set up the new one as above, then an owner
chooses "Trust the provider I sign in with next" under Administration → Security (the role signing details) and saves. The next owner's app that signs in
at the new provider pins it, and the roster forgets the accounts (`sub`) noted at the old one, also for people an owner
edited by hand; each person's account is noted again from their first token at the new provider, with nobody asked.
Nobody has to be edited by hand. Until somebody's first token counts, the new provider can't take their role away, so
have everybody sign in once. People listed as contradictory after a move from before this version: reset once more and
sign in again; that counts the move and clears them. The apps log `forgetting the accounts noted at the identity
provider before the move` once.

**To check** a realm: `python3 dev/verify-realm.py` shows the checks against the dev realm; a token of your own decoded with any
JWT tool should show `resource_access.<client>.roles` and `tc_identity_key` in the ID token.

**Identity key attestation** (e2ee.md §3.1) uses the same two rows for the identity key: with them, every member's app
publishes its key's attestation by itself. Users may edit the attribute, so a token only confirms a key for the account
the owners signed for that person: today owners and admins (their accounts are in the signed roster). Everybody else's
key is pinned on first use, as before, whatever the setting. Administration → Security → "Colleagues' keys" can require
confirmed keys for those signed accounts. Keys apps pinned before stay trusted either way. The web app fetches the
provider's signing keys from the browser: add the web app's origin to the client's web origins.

**Manual instead of automatic:** Administration → Security → "Who signs role changes". Whoever controls the provider can appoint
admins under the automatic default (e2ee.md §3.1.3); manual gives every change to an owner's hand.

### Setting up again

Only when no owner can be brought back. It starts a new workspace; the old one's history is not
carried over. Work through it in this order:

1. **Tell everybody.** All members must re-enroll, so pick a time. The old history stays readable
   only on devices that still hold it.
2. **Stop** `teamcraft-server`, `teamcraft-sfu`, bridges and the notetaker. Keep a last backup of the
   old database for the record, but do not restore it.
3. **Start from an empty database** (new database or dropped schema; the server migrates it on
   start) and an empty bucket (or a new one: the old objects have no rows any more and are
   ciphertext only). Keep the configuration and secrets, the identity provider and the domain.
4. **Give the role `owner` to at least two people** in the identity provider (`just ensure-admin`
   for the first), before anybody signs.
5. **Every member reinstalls or clears the app's local data** and signs in again. This is not
   optional: each app pins the old root of trust in its local store and would refuse the new
   genesis (`RootChanged`). Everybody sets up a new key (new identity key, key bag and recovery
   kit).
6. **The first owner saves the first policy** (Administration → Security). That signs the genesis
   roster with the first owner as root of trust; confirm the proposed owners and admins one by one
   (admin.md §2.3.1, "The first roster"). Members compare the root fingerprint with that owner.
7. **Re-enroll bridges and the notetaker** (their keys belong to the old workspace), then recreate
   spaces, channels and memberships; guests are invited again.

| Kept                                                 | Lost                                                        |
|------------------------------------------------------|-------------------------------------------------------------|
| Configuration file, secrets, domain, certificates    | Messages, files, channels, spaces, memberships              |
| Identity provider accounts and roles (incl. `owner`) | Devices, identity keys, key backups, recovery kits          |
| Deployment of server, SFU and push gateway           | The signed roster and policy (signed again)                 |
|                                                      | Settings made in the console (mail, calendar, policy)       |
|                                                      | Guests, invitations, integration sources, bridge enrollment |

## Upgrades

- Images are pinned by digest in the deployment; build or pull the new ones, never `latest`.
- Migrations are forward-only and run automatically when the server starts. **Take a database backup
  first.** There is no downgrade: to go back, restore the backup and start the old version.
- Run exactly **one** `teamcraft-server` instance. Gateway sessions, presence, device-linking relays,
  rate limits and the call roster live in the server's memory, and there is no bus between instances
  yet: events published on one instance never reach clients connected to another. Upgrade by
  replacing the instance (a short interruption), not by rolling several.
- Order: database backup, new `teamcraft-server`, then `teamcraft-sfu`, then bridges and the
  notetaker. The SFU verifies join tokens the server signs; the token format is versioned with the
  server. An old server binary refuses to start against a database migrated by a newer one.
- On SIGTERM the server drains: `/readyz` answers 503, every gateway and device-linking socket is
  closed with code 1012 ("service restart"), and the process waits up to `[gateway]
  drain_timeout_secs` (default 10) for the sockets to close before it exits. Give the container a
  stop grace period above that. The new server forgets the sessions: every client reconnects (with
  jitter) and runs a full sync, so expect a burst of API traffic right after the restart. A reverse proxy in front must keep idle WebSockets open for at least 60 s (clients send a
  heartbeat every 25 s). Calls in progress
  live in the SFU and survive a server restart (the server's call roster is in memory and starts empty;
  it is filled by the SFU's participant reports once the SFU sends them).
- Check `docs/` and the release notes for configuration changes before upgrading. The server rejects
  unknown configuration keys at startup, so a stale key fails fast instead of being ignored.

## Binding tokens to devices

DPoP (RFC 9449) ties access tokens to a key on the device, so a stolen token, or one relayed by a
look-alike server, doesn't work without that key (e2ee.md §3.1.2). The setting is in the admin console:
Instance settings, "Binding sign-ins to devices". The default `optional` changes nothing for clients
that don't support it. **The desktop and web apps send proofs, the mobile apps don't yet**, so stay on
`optional` for now if people use those.

An identity provider that advertises DPoP (`dpop_signing_alg_values_supported`, on by default with
current Keycloak) gets proofs from the desktop and web apps as soon as this server knows its origin,
and binds their tokens without further setup. To hold that back, leave the origin out of
`public_origins` and the other settings below: `server-info` then offers no nonce and the apps stay
unbound.

**What the server has to know: its public origin.** Proofs name the URL the client used, and the
server compares it with what it was told, never with what a request claims. Host names of `[[brands]]`
and `[branding].served_hostnames` count as `https://<host>`, and the `public_base_url` values of
`[meetings]`, `[mail]` and `[shares]` count too. Anything else, such as a port, goes here:

```toml
[security]
public_origins = ["https://chat.example.com:8443"]
```

Behind a reverse proxy nothing else changes. It must pass the `DPoP` request header and the
`DPoP-Nonce` and `WWW-Authenticate` response headers, as proxies do by default, and must not rewrite
the path: the path of the request is compared with the path in the proof. Without a known origin
proofs with unbound tokens are ignored, bound tokens are refused, the server warns once in its log,
and the console refuses `required`.

**Rollout**

1. Upgrade the server. The policy stays `optional`. Check that the origin above is right.
2. Ship apps that send proofs to everybody, on all platforms in use.
3. Let the identity provider bind tokens: allow DPoP on the app's OIDC client, without forcing it yet.
   Apps that send a proof at the token endpoint then get bound tokens, the others go on as before.
   Check it from one browser: in the provider's discovery document, `dpop_signing_alg_values_supported`
   lists `ES256`; `GET /api/v1/server-info` has a `DPoP-Nonce` header; after a sign-in `bound` counts up.
   A web app needs the provider's CORS rule to allow the `DPoP` request header and to expose `DPoP-Nonce`.
4. Watch `teamcraft_dpop_requests_total`. `unbound` counts requests that `required` would refuse, and
   it should reach 0 for longer than your slowest users take to update. `refused` should stay near
   0. Bots, local accounts and meeting guests aren't counted, and `required` doesn't affect them.
5. In the console, from an updated app, set the policy to `required`. The server refuses the change
   from a session that isn't bound, so you can't lock yourself out with an old app. Tokens that are
   not bound now get 401 `dpop_required`, and an old app has to be updated before it can sign in.
6. Force binding at the identity provider, so that nobody is issued an unbound token any more.
   Forcing it before step 2 is finished breaks sign-in for old apps.

**When something goes wrong**

- 401 `invalid_dpop_proof` for everybody, "names another URL" in `WWW-Authenticate`: the public origin
  is wrong or missing, or the proxy rewrites the path. Set `public_origins` and restart. The apps show
  this as a banner naming a proxy or the server's address and keep the session.
- The apps stay unbound although the provider advertises DPoP: `GET /api/v1/server-info` on the public
  address has no `DPoP-Nonce`. The proxy strips it, the policy is `off`, or the host is not among the
  public origins (behind a proxy that rewrites `Host`, the server also has to trust its
  `X-Forwarded-Host`).
- "This device lost the key" for one person: the app's local data was cleared or damaged, and the bound
  refresh token can't be used without its key. Signing in again makes a new key. No data is lost.
- "this device has a DPoP key and signed in without binding" in an app's log, or "refused a request with
  a proof": the session went on unbound. A provider that advertises DPoP may not take proofs for this
  client, or the device's clock is off and the provider's `Date` isn't readable. For a web app, expose
  `Date` and `DPoP-Nonce` in the provider's CORS rule. Under `required` such a session is refused.
- A sign-out that doesn't end the provider session: bound refresh tokens are ended by the end-session
  request or, if the provider refuses that, by revocation. The discovery document has to name
  `revocation_endpoint`, and for a web app its CORS rule has to allow the app's origin.

Refused proofs and `dpop_required` don't count as failed sign-ins, so a wrong public origin doesn't block
an address with 429. Guessed tokens still do, and every request stays under the per-address limit.
- 401 `invalid_dpop_proof` for one person, "too old or from the future": their device's clock is off
  by more than the proof's window, and the app doesn't correct for it. Raise `proof_leeway_secs` for
  the time being.
- 503 for requests with bound tokens and "replay cache is full" in the log (at most every five
  minutes): more requests than `replay_cache_max_entries` within the window. Raise it (some 150 bytes
  per entry); entries expire on their own after the window (default 3 minutes).
- 429 for one account: it sent more than `replay_cache_max_entries_per_user` requests within the
  window. If it is an import or a busy integration, raise that value.
- "the stored DPoP policy" or "the stored DPoP settings" in the log at error level: the row can't
  be used by this version (usually after going back a version), so `optional` applies. Set the policy
  again in the console.
- Nobody can get in to change the policy: `off` takes every token of the identity provider without
  checking it, as before DPoP. On the database, then restart the server:

  ```sql
  UPDATE server_settings SET value = jsonb_set(value, '{values,dpop_policy}', '"off"')
   WHERE name = 'settings.token_binding';
  ```

**A restart** forgets the nonces and the proofs seen, on purpose: proofs from before it are refused
and each client signs a new one after one extra round trip, also while the policy is `required`. No
data goes with it. Run one `teamcraft-server` only (see "Upgrades"): a second would refuse the first
one's nonces.

## Server address in the apps

The desktop, Android and iOS apps have no server built in. On first start they ask for the server
address; users type `team.example.com`, the full URL, or paste any link into the server (an invitation,
a message link). The app shows the server's logo, name, host and identity provider for confirmation
and then signs in with the settings the server announces. "Change server" on the sign-in screen goes
back to the entry. When a host name does not resolve, the desktop app says so and offers "Check
similar addresses": only when the user presses it does it look up the same name under a few other
common top-level domains (`.com` typed for `.de`) and offer the first one that exists as "Did you
mean …?". Taking it only fills in the field; the app never connects on its own. The web client needs
none of this: it signs in to the server that served it.

After the identity provider, the browser tab of the desktop sign-in shows a page from the app's
loopback listener in the app's language and theme, with the server's logo and name. It is answered
only once the tokens are in, so it says whether the sign-in really worked; it is self-contained
(content security policy `default-src 'none'`, no scripts) and never shows the redirect's
parameters.

Every sign-in the user starts shows the identity provider's login page, also when the browser still
has a session there (OIDC `prompt=login`, issue #448), so people with several accounts see which one
they sign in with and can switch. The page is prefilled with the account last used on that device
for that server (`login_hint`, from the ID token's `preferred_username` or `email`) and shown in the
app's language (`ui_locales`). The mobile apps also use an ephemeral browser session. Staying signed
in is unaffected: the apps renew their tokens without the browser, and the web client's silent
renewal after a reload never asks for the login page. Operators who prefer silent single sign-on
turn it off (Admin → Settings → Sign-in page, `is_login_page_always_shown`).

When the identity provider refuses a sign-in in its redirect (OAuth `error`, issue #1063), every
app names the reason: `access_denied` says the account isn't allowed, `invalid_request`,
`unauthorized_client`, `unsupported_response_type` and `invalid_scope` say the client is set up
wrongly (tell the admin), and any other code is shown as it is. Keycloak's `already_logged_in`
means the browser re-sent a login that had already finished (a resumed tab, a second submit, or a
redirect that never reached the app): the app sends one new authorization request with a fresh
`state` and PKCE verifier and the same `prompt`, so the login page shows again; never a silent
sign-in with the browser's session. Only a second one is shown. Each failure is logged with its
type and the provider's code (logcat tag of the app, the iOS log category `sign-in`, the desktop
and web logs), never with tokens or the redirect's code.

What the server announces, public and cacheable, at `GET /api/v1/server-info`:

| Field                            | Source                                                                    |
|----------------------------------|---------------------------------------------------------------------------|
| `sign_in.issuer`                 | `[oidc] issuer`; apps require `https` (plain `http` only on loopback)     |
| `sign_in.client_id`              | `[oidc] client_id`, the public client of the apps                         |
| `sign_in.scopes`                 | `[oidc] app_scopes`, default `openid profile email`                       |
| `sign_in.redirect_uris`          | `[oidc] app_redirect_uris`, optional (see below)                          |
| `sign_in.has_local_accounts`     | Whether local guest accounts are enabled                                  |
| `brand`                          | The brand of the requested host name, as `GET /api/v1/brand`              |
| `server_version`, `minimum_client_version` | As `GET /api/v1/client-compatibility`                           |

Nothing in it is secret. Register these redirect URIs for `client_id` at the identity provider:

| App            | Redirect URI                                                     |
|----------------|------------------------------------------------------------------|
| Desktop        | `http://127.0.0.1/callback` (any port: loopback flow, RFC 8252)  |
| Android, iOS   | `de.zeitgleich.teamcraft:/oauth/callback`                        |
| Web            | `https://<host>/` of each host name                              |

In Keycloak, `http://127.0.0.1/*` as a valid redirect URI covers the desktop app's changing port. When
`app_redirect_uris` lists them, an app built with another redirect URI (a self-build with its own app
ID) tells its user that the server is not set up for it instead of failing at the provider.

**Staying signed in.** The apps ask for `offline_access` (`[oidc] app_scopes`), so each device gets a
long-lived refresh token and stays signed in; the provider's client must allow that scope (Keycloak:
Clients → client → Client scopes → `offline_access`, optional is enough) and its offline session idle
time decides how long an unused device stays signed in (Realm settings → Sessions, 30 days by
default). Without the scope a device's sign-in ends with the provider's browser session.

**Devices that were off for a long time** (issue #883). A computer that was off or asleep for 12
hours, a weekend or weeks wakes up with a long-expired access token; the app shows "offline" (never
"sign in again"), and refreshes with its stored refresh token as soon as the network is back. Whether
that refresh is accepted is the provider's decision:

- Sessions with `offline_access` (every sign-in since #667) hold an offline token: it is accepted as
  long as the device was off for less than the offline session idle time (Keycloak default 30 days)
  and, if set, the offline session max. Every refresh starts the idle time anew. Set the idle time
  longer if devices may be off longer (holidays, parental leave).
- Sessions from before #667, or from a client without the `offline_access` scope, hold a normal
  refresh token, which ends with the provider's browser session: Keycloak's SSO session idle (30
  minutes by default) and SSO session max (10 hours). Such a device has to sign in again after a
  longer pause; after that it has an offline token.
- When the provider refuses the token (`invalid_grant`), the app forgets it and goes back to the
  sign-in screen once, with "Your session has expired. Please sign in again." Without an answer
  (no network, the provider down) the token is kept and tried again with a growing pause.

teamcraft-deploy imports the realm with `offline_access` as an optional client scope and leaves the
session times at Keycloak's defaults (checked in its `realm.json.j2`); an existing realm keeps
whatever it was set to.

**Several devices per account.** Every device signs in with its own provider session, and all of
them stay signed in side by side. Signing out on one device ends that device's provider session; the
provider's back-channel logout names it (`sid`), and the server ends only that session. A logout
token without `sid` ends all sessions of the user (OIDC Back-Channel Logout 1.0 §2.4). In Keycloak:

| Setting                                                  | Value                                               |
|----------------------------------------------------------|-----------------------------------------------------|
| Client `teamcraft` → Backchannel logout URL              | `https://<host>/api/v1/auth/backchannel-logout`     |
| Client `teamcraft` → Backchannel logout session required | On: the logout token names the session              |
| Browser flow → User session count limiter                | Not used, or without "Terminate oldest session" (¹) |
| Realm settings → Tokens → Revoke Refresh Token           | Either: it applies to each device's own token       |

(¹) With it, a sign-in on one device ends the oldest session of the user, another device's.

**Signed out too often?** (issue #667) Each app runs exactly one token refresh at a time per server
and account and hands its result to everyone waiting: the browser's tabs through a Web Lock and the
shared token vault, the desktop app in its shell (the core only reads the token), iOS across the app
and its notification extension through a lock file in the App Group, Android in its process. A
refresh token is only dropped when the provider answers `invalid_grant`; network errors and other
answers (`temporarily_unavailable`, `server_error`) keep it for the next try. The desktop app keeps
the access token's expiry in wall-clock time, so a token that ran out while the computer slept is
refreshed right after wake-up, and a request the server refuses with 401 gets one refresh and one
retry (issue #883). When the session
really ends the apps say why: signed out at the provider or on another device (`session_ended`,
usually a back-channel logout), or the workspace's session policy timed it out
(`session_timed_out`, Admin → Security → session length and idle timeout).

Recommended provider settings (Keycloak):

| Setting                                                | Recommendation                                                                                     |
|--------------------------------------------------------|----------------------------------------------------------------------------------------------------|
| Client scopes → `offline_access`                       | Assigned to the client (optional is enough); without it devices fall back to browser sessions      |
| Realm settings → Sessions → Offline session idle       | At least 30 days (default); how long an unused device stays signed in                              |
| Realm settings → Sessions → Offline session max        | Off, or as long as the organization's policy wants a sign-in at most                               |
| Realm settings → Tokens → Revoke Refresh Token         | Off, or on with "Refresh token max reuse" at least 1: a reused token then doesn't end the session  |
| Realm settings → Tokens → Access token lifespan        | 5 to 15 minutes; the apps refresh a minute ahead                                                   |

To find out why someone was signed out: the client log names the error code (`session_ended`,
`session_timed_out`, or a refresh refused with `invalid_grant`); the server logs back-channel logouts
and the audit log records them (`oidc_backchannel_logout`); Keycloak's events (Realm settings →
Events, "Save events" on) show `REFRESH_TOKEN_ERROR` with `invalid_refresh_token` for a reused token.

Up to 1.0.3 the server ended every session of the user on any back-channel logout, since Keycloak
names the subject in each logout token as well: signing out (or "sign in again", which signs out
first) on one device signed the others out. To end all sessions of a user, sign them out in the
provider (Keycloak: Users → user → Sessions → Sign out) or disable the user.

The apps reject a server that is not a TeamCraft server, that answers with an HTML page or a redirect
(a captive portal), whose version is too old (or for which the app is too old), or whose issuer is not
`https`. Tokens and local data are kept per server origin; an app never switches to a server because
a server told it to.

**Preconfigured server (managed devices).** Companies can set the server so users don't type it, and
optionally lock the apps to it:

| Platform | Where                                                                                                                                          |
|----------|------------------------------------------------------------------------------------------------------------------------------------------------|
| Desktop  | `desktop.json` with `{"server_url": "team.example.com", "lock_server": true}` in `/etc/teamcraft/` (Linux) or `/Library/Application Support/TeamCraft/` (macOS), owned by root and writable only by root (else ignored); or the environment variable `TEAMCRAFT_SERVER_URL` (not locked). Windows: the policy registry key follows (#220) |
| Android  | Managed configuration (Android Enterprise / MDM app restrictions): `server_url`, `lock_server`                                                  |
| iOS      | Managed app configuration (`com.apple.configuration.managed`): `server_url`, `lock_server`                                                      |

With a preconfigured server the app checks it on first start and goes straight to the confirmation.
`lock_server` hides "Change server" and drops an account of another server.

**The sign-in page.** Every client renders the same page from `server-info`: a prominent "Sign in
with <provider>" button, and the e-mail and password form of local accounts inline, in the order the
admin console sets (Admin → Settings → Sign-in page: SSO and local, local first, SSO only, local
only; help text and help link). The provider name and its icon are brand settings (Admin → Brands:
"Sign-in provider" and the "Sign-in provider icon" image). Local accounts need `[local_accounts]` with
a session secret in the configuration; the console switches them off and on (Settings → Local
accounts).

## Push notifications

Push wakes sleeping phones and browsers. The payload is sealed with a per-device key (e2ee.md §7), so
Apple, Google and push relays only see ciphertext; the app then fetches and decrypts the message.
Without a provider configured, devices of that kind only get messages while the app is open. Every
provider is on as soon as its credentials are set; `GET /api/v1/admin/instance` and the admin console
(Instance → Push providers) show per provider whether it is configured, when it last delivered, how
many pushes failed since the server started and the last error.

| Provider    | For                       | Needs                                                                              | Configuration                                    |
|-------------|---------------------------|------------------------------------------------------------------------------------|--------------------------------------------------|
| Web Push    | browsers (PWA)            | a VAPID key pair                                                                   | `[push] vapid_private_key`, `vapid_subject`      |
| APNs        | iOS, including call rings | store app: a push gateway; own app: an Apple `.p8` key, key ID, team ID, bundle ID | `[push.gateway]` or `[push.apns]`                |
| FCM         | Android with Google       | store app: a push gateway; own app: a Firebase service account JSON key            | `[push.gateway]` or `[push.fcm]`                 |
| UnifiedPush | Android without Google    | nothing (the app hands over the distributor's endpoint and keys)                   | `[push.unified_push] is_enabled` (on by default) |

**Store apps and the push gateway.** The iOS and Android store apps are signed by their publisher, and
Apple and Google only deliver to the holder of that signing identity, so your server can't send to
them with credentials of its own. Point it at the publisher's push gateway instead
([push-gateway.md](push-gateway.md)): it forwards your sealed wake-ups and sees only device tokens and
ciphertext. Every server does this, the publisher's own included.

```toml
[push.gateway]
url      = "https://push-gw.zeitgleich.de"   # the publisher's gateway for the official store apps
key_path = "/var/lib/teamcraft/push-gateway.key"   # created on first start; your identity at the gateway
```

The APNs and FCM rows above are for **self-built, self-signed apps**: with `[push.apns]` or `[push.fcm]`
set, the server sends to that provider directly and never to the gateway. Web Push and UnifiedPush
never use the gateway. Apps seal their device token for the gateway, bound to your server, so your
server never holds a usable token and can't be used to push to other servers' users; old apps that
don't seal yet need `accept_unbound_tokens = true` as a transition (docs/push-gateway.md §3a). Running a gateway is only for whoever publishes apps (its own repository, see [push-gateway.md](push-gateway.md)).

Common settings in `[push]`: `ttl_secs` (how long a push service holds a wake-up for an offline
device), `request_timeout_secs`, `max_concurrent_sends` (per pool: APNs/FCM, and Web Push/UnifiedPush),
`max_concurrent_sends_per_user` (Web Push/UnifiedPush sends one user may have in flight, so endpoints
that never answer only hold up their owner). `allow_private_endpoints` is for development only: it lets
Web Push and UnifiedPush endpoints point at private addresses and plain http, and allows plain http
APNs/FCM base URLs for local fakes; otherwise the server refuses to start with them.

### Web Push (VAPID)

```sh
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='      # the private key, 32 random bytes
```

Set it as `TEAMCRAFT_PUSH__VAPID_PRIVATE_KEY` and `vapid_subject` to a `mailto:` or `https:` contact for
push services to reach you. At startup the server logs the matching public key; browsers get it from
`GET /api/v1/push/web-key`. Changing the key means browsers must subscribe again.

### APNs (iOS)

1. In the Apple Developer account: Keys → create a key with "Apple Push Notifications service (APNs)"
   enabled and download the `.p8` file once. Note the **Key ID**; the **Team ID** is shown under
   Membership; the **bundle ID** is the iOS app's.
2. Configure the server. The key is the file's contents, as `TEAMCRAFT_PUSH__APNS__KEY` or, for a
   mounted secret file, `key_path`:

   ```toml
   [push.apns]
   key_path = "/run/secrets/apns_key.p8"
   key_id = "ABC123DEFG"
   team_id = "DEF456GHIJ"
   bundle_id = "de.zeitgleich.teamcraft"
   use_sandbox = false
   ```

Messages go to the app's topic as alerts with `mutable-content` (the notification service extension
rewrites the generic `alert_text`); calls go to `<bundle_id>.voip` as VoIP pushes that are not stored.
One key serves production and sandbox; `use_sandbox = true` points the server at Apple's sandbox for
development builds (a token only works in the environment its build was signed for). A partial setup
(for example a key without a team ID) stops the server at startup.

### FCM (Android with Google services)

1. In the Firebase console: Project settings → Service accounts → "Generate new private key" (the
   account needs the "Firebase Cloud Messaging API Admin" role or `firebase.messaging`).
2. Give the JSON file to the server as `TEAMCRAFT_PUSH__FCM__SERVICE_ACCOUNT` (the contents) or
   `[push.fcm] service_account_path`.

The server trades the account's key for short-lived OAuth tokens itself and sends data-only
high-priority messages that carry only the sealed wake-up.

### UnifiedPush (Android without Google services)

Nothing to configure on the server: the app registers the endpoint of the user's distributor (for
example a self-hosted ntfy) with its Web Push keys, in the Web Push subscription format. The server
sends exactly as for Web Push: the sealed wake-up encrypted with RFC 8291 and posted to the endpoint,
with a VAPID token when `vapid_private_key` is set (distributors may ignore it). Endpoints must be
public https URLs. Set `[push.unified_push] is_enabled = false` to refuse it.

In the admin console, failures at Web Push and UnifiedPush endpoints are counted apart: users choose
those endpoints, so they never mark the provider as failing.

### Variables in the deployment repository

The Ansible deployment passes these on (secrets as podman secrets, the rest as variables):

| Deployment name              | Kind     | Becomes                                                                          |
|------------------------------|----------|----------------------------------------------------------------------------------|
| `vapid_private_key`          | secret   | `TEAMCRAFT_PUSH__VAPID_PRIVATE_KEY`                                              |
| `apns_key`                   | secret   | `TEAMCRAFT_PUSH__APNS__KEY` (the `.p8` text)                                     |
| `fcm_service_account`        | secret   | `TEAMCRAFT_PUSH__FCM__SERVICE_ACCOUNT`                                           |
| `teamcraft_apns_key_id`      | variable | `[push.apns] key_id`                                                             |
| `teamcraft_apns_team_id`     | variable | `[push.apns] team_id`                                                            |
| `teamcraft_apns_bundle_id`   | variable | `[push.apns] bundle_id`                                                          |
| `teamcraft_apns_use_sandbox` | variable | `[push.apns] use_sandbox` (default `false`)                                      |
| `teamcraft_push_gateway_url` | variable | `[push.gateway] url` (the publisher's push gateway, for the store apps)          |
| `push_gateway_key`           | secret   | `TEAMCRAFT_PUSH__GATEWAY__KEY` (generated: the server's identity at the gateway) |
| `teamcraft_unified_push`     | variable | `[push.unified_push] is_enabled` (default `true`)                                |

The `[push.apns]` block is only rendered when `apns_key` is a secret.

## Secrets and key rotation

| Secret                                | Where                                      | Rotating it                                                                       |
|---------------------------------------|--------------------------------------------|-----------------------------------------------------------------------------------|
| Call join token signing key (Ed25519) | `[calls] signing_key_path`; SFU public key | see below                                                                         |
| SFU registration token                | `[calls] sfu_registration_token`           | the SFU does not send reports yet; once it does, change both together             |
| Bot tokens                            | `POST /api/v1/admin/bots/{id}/token`       | the old token stops working immediately; update the bridge's configuration        |
| Guest token secret                    | `[meetings] guest_token_secret`            | invalidates open invite links and guest sessions; use between meetings            |
| Mail inbox token                      | `[meetings] inbox_token`                   | change on the server and the inbox handler together                               |
| VAPID key (Web Push)                  | `[push] vapid_private_key`                 | browsers must subscribe again; plan for a short push gap                          |
| APNs key (`.p8`)                      | `[push.apns] key`                          | create a new key at Apple, switch, then revoke the old one; no gap                |
| FCM service account key               | `[push.fcm] service_account`               | create a new key in Firebase, switch, then delete the old one; no gap             |
| OIDC server client secret             | `[oidc.sync] client_secret`                | rotate in the identity provider, then restart the server                          |
| Database password, S3 keys            | `[database]`, `[storage]`                  | change at the provider, then in the configuration, then restart                   |
| SMTP and CalDAV passwords             | Administration → Mail / Calendar           | change at the provider, then enter the new one in the console; no restart         |
| Settings key                          | `[secrets] key` or `key_path`              | a new key makes the stored SMTP and CalDAV passwords unreadable: enter them again |

Prefer environment variables or files for secrets (`TEAMCRAFT_...`), not the TOML file; never commit
them.

### Rotating the call signing key

The server signs short-lived join tokens (`join_token_ttl_secs`, 60 s by default) with an Ed25519
private key; the SFU verifies them with the matching public key and the issuer. Tokens are only
checked when somebody joins, so a rotation never disturbs calls that are running.

```sh
openssl genpkey -algorithm ed25519 -out calls-signing-new.pem
openssl pkey -in calls-signing-new.pem -pubout -out calls-signing-new.pub.pem
```

1. Put the new private key where `[calls] signing_key_path` points (a new file name is fine) and the
   new public key where the SFU reads `TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY`.
2. Restart the SFU, then the server, one right after the other. Joins that happen between the two
   restarts fail with an invalid-token error and succeed on retry; a token signed with the old key
   lives at most one TTL.
3. Remove the old key files.

The SFU accepts one public key at a time, so there is a short window with failing joins; do it when
few calls start. Rotate on a schedule that fits your policy and immediately after any suspected
exposure of the private key (it lets its holder join any call as any user).

## Smoke test of the images

`dev/smoke-images.sh` builds `teamcraft-server`, `teamcraft-sfu` and `teamcraft-bridge` and starts
them with rootless podman: non-root user, read-only root filesystem, no capabilities,
`no-new-privileges`. It checks that each starts, passes its `healthcheck`, answers on IPv4 and IPv6
where it has an endpoint, and stops cleanly on SIGTERM. Run it after changing a Containerfile or a
base image digest.
