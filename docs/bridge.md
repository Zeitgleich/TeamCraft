# Integration bridge

`teamcraft-bridge` (ARCHITECTURE.md §4.5) puts machine output into end-to-end encrypted channels:
logs, monitoring alerts, CI results, the hotline bot. It is a small headless TeamCraft client that
runs **where the data comes from** (next to the service, on a NAS, on the hotline host), never on
the VPS that runs the server.

```
 Grafana, CI, scripts,        HTTP (loopback)         encrypted          TeamCraft
 journalctl | curl      ─────────────────────▶ bridge ─────────────▶ server (ciphertext only)
```

## What it is

- A **bot account** and device: an admin registers it (`POST /api/v1/admin/bots`), adds it to the
  channels it serves, and the bridge enrolls like any client: own identity key, own device, own MLS
  leaf. It holds the content keys of its channels and encrypts itself; the server never sees
  plaintext. See "Bots" below.
- Built on `teamcraft-core`: same sync engine, same encrypted local store (SQLCipher), same
  gateway. It keeps the gateway connection so welcomes, commits and key updates reach it.
- Two local endpoints (loopback on IPv4 and IPv6 by default, configurable; dual stack with `[::]`):

| Endpoint               | Accepts                                                                                                                                                         |
|------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `POST /hooks/<token>`  | Slack/Mattermost incoming-webhook JSON (`text`, `username`, `attachments`, ...) or the form `payload=<json>`; with files as `multipart/form-data` (see "Files") |
| `POST /ingest/<token>` | Log lines: plain text, or NDJSON with `Content-Type: application/x-ndjson`                                                                                      |
| `GET /healthz`         | `200 ok`, for the container health check                                                                                                                        |

  `icon_url`, `channel` and blocks of the Slack format are ignored: the source's token decides the
  conversation. Slack markup (`<url|label>`, `<@user>`, `<!here>`) becomes Markdown. A source added
  in the app opens one of the two endpoints (its kind); a token on the other one gets `404`.

```sh
curl -d '{"text":"Deploy finished","username":"ci"}' http://127.0.0.1:8088/hooks/<token>
journalctl -f | curl -T - -H 'Content-Type: text/plain' http://127.0.0.1:8088/ingest/<token>
```

## Behaviour

- **Batching:** log lines of one conversation are collected and sent as one message (a code block,
  so clients show it monospaced) one interval (default 1 s) after the first line, or at 64 KB.
  Webhooks are sent one message per request.
- **Rate limits** per source (token bucket, default 600 per minute): webhooks over the limit get
  `429` with `Retry-After`; log lines over it are dropped and reported in the channel
  ("dropped N lines from nas-logs"), the response says how many were accepted. Webhooks with files
  also count their bytes, per source and against the bridge-wide `files.total_bytes_per_minute`
  (see "Files").
- **Alerts** can @mention people: a webhook with `"mention": true` (an extension), webhook
  attachments with `color: "danger"` (if the source enables `danger_is_alert`), and log lines whose
  level is in `alert_levels`. The mentioned user IDs travel as message metadata, so they are notified
  even though integration channels push nothing else.
- **Search index:** the bridge has the plaintext, so it publishes the index segments of its channels
  (closed months, e2ee.md §10) itself, once an hour by default.
- **No channel yet:** messages for a channel the bot hasn't been added to wait (up to 1000) and go
  out once it joins. A lost connection is retried with backoff; sends that fail stay in the
  encrypted outbox of the local store.
- Tokens are part of the URL, so treat the URL as the credential: unknown tokens get `404`, the
  bridge knows only their SHA-256 and compares it in constant time, and the listener is loopback
  unless you open it.

## Files

A webhook may carry files, the way Discord's and Slack's take them: `POST /hooks/<token>` as
`multipart/form-data`, with the message as JSON in a `payload_json` (or `payload`) part, or in
plain `text` and `username` fields, and the files in parts that have a file name (`file`,
`files[0]`, `files[1]`, ...; any field name with a file name is a file). A message with files needs
no text of its own.

```sh
# A report with a line of text.
curl --fail-with-body -F 'text=Nightly tests failed' -F 'file=@report.pdf' \
  http://127.0.0.1:8088/hooks/<token>

# The webhook JSON and two files, as Discord takes them.
curl --fail-with-body \
  --form-string 'payload_json={"text":"Release 2.4 built","username":"ci"}' \
  -F 'files[0]=@dist/app.tar.gz' -F 'files[1]=@dist/SHA256SUMS' \
  http://127.0.0.1:8088/hooks/<token>
```

`--form-string` sends the value as it is; with `-F`, curl reads a value starting with `@` or `<` as
a file and cuts it at `;`.

GitHub Actions, the test report of a failed run (`TEAMCRAFT_HOOK` is a repository secret with the
source's URL; `--retry` repeats the request on a timeout, `429`, `502`, `503` and the like and
honours `Retry-After`; the `Idempotency-Key` keeps a repeat after a lost answer from posting twice,
see "Retries"):

```yaml
- name: Report to TeamCraft
  if: failure()
  run: |
    run_url="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
    payload=$(jq -n --arg text "${GITHUB_WORKFLOW} failed on ${GITHUB_REF_NAME}: ${run_url}" \
      '{text: $text, username: "GitHub Actions", mention: true}')
    curl --fail-with-body --retry 3 --max-time 600 \
      -H "Idempotency-Key: ${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}-${GITHUB_JOB}-report" \
      --form-string "payload_json=${payload}" -F "file=@target/test-report.html" "$TEAMCRAFT_HOOK"
  env:
    TEAMCRAFT_HOOK: ${{ secrets.TEAMCRAFT_HOOK }}
```

CI artefacts, every file of a directory in one message (at most the source's files per message):

```sh
args=()
for artefact in build/artefacts/*; do args+=(-F "files[]=@${artefact}"); done
curl --fail-with-body -F "text=Build ${CI_PIPELINE_ID}: artefacts" "${args[@]}" "$TEAMCRAFT_HOOK"
```

What happens:

- Before a byte of the body is read, the request needs a place: one of
  `files.max_concurrent_requests`, one of its source's, and one in the worker's queue. Without one
  it gets `503 bridge_busy` at once.
- The form is read as it arrives. Each file is written to the spool directory (the server needs a
  file's size before its upload starts, and a form doesn't say it); the limits below apply while it
  arrives, so a file over one is refused before the rest is read. Every piece is paid from the
  source's share of file bytes and from the bridge's total; a file costs at least 64 KiB. A request
  that is refused, or whose producer hangs up, before its upload begins gets its bytes back.
- Pictures (up to 32 MiB and `files.max_thumbnail_pixels`) get a thumbnail and their size, decoded
  one at a time off the endpoint's threads; a picture the decoder chokes on goes without.
- The bridge then encrypts each file like a client upload (e2ee.md §4.5): a fresh key per file,
  files larger than 8 MiB in parts through presigned part URLs, read and encrypted one part at a
  time, so memory stays at a part however large the file. The uploads are **completed only once
  every file of the message is up**, and then it sends **one** message with all of them into the
  source's channel.
- The answer comes once the message is in the encrypted outbox: `200 ok`. A failed upload answers
  `413`, `502` or `503` (see "Answers") and sends nothing; since nothing was completed, every file
  is given up and the server frees it. Retry the request. Only a failure while completing (a few
  short requests at the end) can leave completed files without their message.
- A message is accepted only for a channel the bot can write in, and only if its text and files fit
  the server's message limit; otherwise nothing is uploaded.
- One message's files upload at a time, beside logs and other webhooks; the others wait in line. A
  request whose producer hangs up is dropped, and its uploads are given up.
- The spool directory holds the plaintext only for the time of a request, readable by the bridge's
  user alone (`0600` in a `0700` directory; at start a wider mode is narrowed, and a link or another
  user's directory stops the bridge), and is emptied whatever the answer; files a stopped bridge
  left there are deleted at the next start.

Limits (`max_file_bytes` <= `source_bytes_per_minute` <= `total_bytes_per_minute`, and
`max_file_bytes` <= `max_spool_bytes`, or the configuration is refused):

| Setting                                    | Default                      | What                                                                                                                                                                |
|--------------------------------------------|------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `files.max_file_bytes`                     | 100 MiB                      | Largest file. The server's attachment limit (`max_attachment_bytes`, encrypted size) applies too                                                                    |
| `files.max_files_per_message`              | 10                           | Files per message for sources that don't set their own (`max_files_per_message` of a `[[sources]]` block or of an app source's options); at most 16, `0` takes none |
| `files.source_bytes_per_minute`            | 512 MiB                      | Bytes of webhooks with files per minute and source; no request carries more                                                                                         |
| `files.total_bytes_per_minute`             | 1 GiB                        | The same for all sources together                                                                                                                                   |
| `files.max_concurrent_requests`            | 4                            | Requests with files running at once, all sources together                                                                                                           |
| `files.max_concurrent_requests_per_source` | 2                            | The same per source                                                                                                                                                 |
| `files.idle_timeout_secs`                  | 30                           | A body that sends nothing for this long is cut off                                                                                                                  |
| `files.max_spool_bytes`                    | 2 GiB                        | Bytes in the spool directory, all requests together                                                                                                                 |
| `files.max_message_bytes`                  | 1 MiB                        | The server's message limit (`[limits] max_ciphertext_bytes`); text and files that won't fit are refused before any upload                                           |
| `files.max_thumbnail_pixels`               | 16 MP                        | Larger pictures get no thumbnail                                                                                                                                    |
| `listen.max_body_bytes`                    | 1 MiB                        | The form's fields besides files together (the message), at most 32 of them                                                                                          |
| `listen.drain_timeout_secs`                | 10                           | On shutdown, requests still running after this long are cut off                                                                                                     |
| `files.spool_dir`                          | `spool` next to the database | Where files wait while they go up; the container image uses `/var/spool/teamcraft-bridge`, a volume of its own                                                      |

A message's files are unfinished uploads until all are up; the server allows 20 per account by
default (`max_pending_attachments_per_user`), which is why a message carries at most 16.

**Reverse proxy.** A proxy in front of the bridge (the deployment's, "Where producers reach the
bridge") must let bodies of that size through and pass them on as they arrive. With nginx:
`client_max_body_size` at least `files.source_bytes_per_minute`, `proxy_request_buffering off` (or
the proxy holds the whole body on its disk first, and the bridge's limits and timeouts see only its
pace), `client_body_timeout` about `files.idle_timeout_secs`, and `proxy_read_timeout` long enough
for the uploads to the server.

## Retries

A producer that loses the answer (the connection dropped, its timeout ran out) can't tell whether
the bridge posted the message, and its retry would post it again. A webhook with an
`Idempotency-Key` header avoids that: the bridge remembers the key of a request it accepted, and
answers a repeat with the same key and the same message `200 ok` again, with
`Idempotent-Replayed: true`, without posting anything. It is the header of the IETF draft
(`draft-ietf-httpapi-idempotency-key-header`), as Stripe and others take it; Slack's, Mattermost's
and Discord's webhooks have no equivalent. Both kinds of webhook take it, JSON and forms with
files.

```sh
# One key per message: made once, sent with every attempt.
key=$(uuidgen)
curl --fail-with-body --retry 5 --retry-all-errors --max-time 60 \
  -H "Idempotency-Key: ${key}" \
  -d '{"text":"Backup of nas finished","username":"backup"}' \
  http://127.0.0.1:8088/hooks/<token>

# With files, the same.
curl --fail-with-body --retry 5 --retry-all-errors -H "Idempotency-Key: ${key}-report" \
  -F 'text=Nightly tests failed' -F 'file=@report.pdf' http://127.0.0.1:8088/hooks/<token>
```

With a key, `--retry-all-errors` is safe: curl then also repeats a request whose connection dropped
after the bridge took it, and the bridge answers that from memory (it repeats refusals like `400`
too, which change nothing). In GitHub Actions, `${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}-<step>` makes
a good key (see the example under "Files"): stable over curl's retries, new when the run is re-run.

- **Per source.** A key is 1 to 255 printable ASCII characters (a UUID is ideal; the draft's quoted
  form `"..."` is the same key). An empty or longer one, other characters, or the header twice get
  `400 invalid_idempotency_key`. Keys are never logged.
- **The same message** is what the request would post: the text as rendered (from JSON, `payload=`
  or a form), whether it is an alert, and each file's name, type and bytes; not the raw body. JSON
  written differently, or a form with another boundary (curl picks a new one each run), is the same
  message. The same key with another message gets `422 idempotency_key_reused` and posts nothing.
- **Only accepted requests count.** A refused one (any `4xx` or `5xx`) posted nothing, so its retry
  with the same key runs again: a `503 bridge_busy` retried after `Retry-After` is taken then.
- **A repeat while the first request still runs** (its files still uploading) waits for it, holding
  its places like any request with files: once the first is accepted, the repeat gets its `200`;
  if the first fails or its producer hangs up before its upload, the repeat runs itself. A message
  the bridge sends after its producer hung up keeps its key all the same. A repeat is read and paid
  like any request (its file bytes come back), so it can be refused like one.
- **For a window.** A key is remembered for `idempotency.window_secs` from the answer on; retries
  should end well within it. All sources together keep at most `idempotency.max_keys` keys in
  `idempotency.max_bytes`; beyond, the source that holds the most loses its oldest keys first,
  before their window ends.
- **In memory only.** A restarted bridge has forgotten every key, so a retry across a restart can
  post twice.
- `/ingest` takes no key: log lines are a stream, and lines sent again are taken again.
- Keys are per source, so only the holder of a source's URL could test a guessed key against a
  guessed message (`200` or `422`); random keys leave nothing to guess.

| Setting                   | Default | What                                                                      |
|---------------------------|---------|---------------------------------------------------------------------------|
| `idempotency.window_secs` | 600     | How long the key of an accepted request is remembered, from its answer on |
| `idempotency.max_keys`    | 10000   | Keys remembered, all sources together                                     |
| `idempotency.max_bytes`   | 4 MiB   | Their memory; each key counts its length and 128 bytes                    |

## Answers

`200` with `ok` for an accepted request (`/ingest` says how many lines it took; a repeat of an
accepted webhook with the same `Idempotency-Key` adds `Idempotent-Replayed: true`), `404` without a
body for an unknown token, before anything else is looked at or read. Every other refusal is JSON
with a stable `error_code`, and every `429` and `503` says in `Retry-After` when to try again:
`{"error_code": "too_many_files", "message": "more than 10 files"}`.

| Status | `error_code`              | When                                                                                                                              |
|--------|---------------------------|-----------------------------------------------------------------------------------------------------------------------------------|
| 400    | `invalid_payload`         | The message JSON or text is invalid or empty (a message with files needs no text)                                                 |
| 400    | `invalid_idempotency_key` | The `Idempotency-Key` header is empty, longer than 255 bytes, not printable ASCII, or sent twice                                  |
| 400    | `invalid_multipart`       | The form is malformed: no or a wrong boundary, broken part headers, cut off                                                       |
| 408    | `request_timeout`         | The body sent nothing for `files.idle_timeout_secs`                                                                               |
| 413    | `file_too_large`          | A file is larger than `files.max_file_bytes`, or the server refused its size (its limit, or the bot's attachment storage is full) |
| 413    | `too_many_files`          | More files than the source takes                                                                                                  |
| 413    | `payload_too_large`       | The fields besides files, or a body without files, are larger than `listen.max_body_bytes`                                        |
| 413    | `too_many_fields`         | More than 32 fields besides files                                                                                                 |
| 413    | `request_too_large`       | The request is larger than `files.source_bytes_per_minute` (by its `Content-Length`, or once read that far): it can never pass    |
| 413    | `message_too_large`       | The text and the files (with room for the pictures' thumbnails) won't fit into one message (`files.max_message_bytes`)            |
| 422    | `idempotency_key_reused`  | The `Idempotency-Key` was used for another message within `idempotency.window_secs` ("Retries")                                   |
| 429    | `rate_limited`            | Requests over the source's or the bridge's limit                                                                                  |
| 429    | `file_rate_limited`       | File bytes over the source's share or the bridge's total; the request got its bytes back                                          |
| 500    | `spool_failed`            | A file could not be written to or read from the spool directory                                                                   |
| 500    | `internal_error`          | The files are up, but the bridge's database could not keep the message                                                            |
| 502    | `upload_failed`           | The server or the storage refused or failed an upload                                                                             |
| 503    | `bridge_busy`             | As many requests with files run as may (in all or for the source); refused before the body is read                                |
| 503    | `spool_full`              | The spool holds `files.max_spool_bytes`                                                                                           |
| 503    | `channel_not_ready`       | The bot can't write in the channel yet (its welcome is on the way); nothing was uploaded                                          |
| 503    | `server_unreachable`      | The server can't be reached                                                                                                       |
| 503    | `shutting_down`           | The bridge is stopping                                                                                                            |

## Setup

In the app:

1. **Admin console → Integrations:** an admin creates the bot (a name). The bot's token (`tcb_...`)
   is **shown once** with a ready-to-paste `teamcraft-bridge.toml` snippet; "New token" replaces the
   token at once, "Revoke" deactivates the bot. (API: `POST /api/v1/admin/bots`.)
2. **Channel settings → Integrations** (channel admins): add the bot to the channel. If the bot is not
   in the channel's space yet it joins it as a guest first (space admins), then its device is
   committed into the channel's encryption group from the client and handed the content keys. A bot
   can be added to **any** channel; a bot's messages carry a bot badge.
3. **Channel settings → Integrations → Sources** (workspace admins): "Add source" with a name and a
   kind (webhook or log lines) shows the URL once, to point Grafana, CI or `journalctl` at; the
   bridge serves it within a second, with no configuration change and no restart (see "Sources"
   below).
4. **Channel settings → Integrations → Mode** (or "Channel type" in the new-channel dialog): "Normal"
   or "Log channel" (`is_integration`: log view, log retention, 30 days by default, `[integration]`
   in the server config, and push only for @mentions). Log channels are private; switch an existing
   private channel at any time.
5. Configure the bridge once (`crates/teamcraft-bridge/teamcraft-bridge.example.toml`) with the
   server URL, the bot token, a database key and a recovery passphrase. Start it; the first run
   enrolls the bot's device and it picks the welcomes of its channels up through its gateway
   connection. Sources need no entry here.

The bridge never adds itself to a channel, and the app never shows a bot's or a source's token again
after the first display (only their hashes are kept).

## Sources

A source is a token that posts into one channel. Channel admins manage them in the app; the bridge
reads them from the channel itself (e2ee.md §13.1):

1. "Add source" (workspace admins) generates the token in the app (256 random bits, `tcs_...`) and sends an encrypted,
   hidden control message into the channel: source ID, name, kind, the token's SHA-256 and the
   options (webhooks: attachments with `color: "danger"` mention people; log lines: the default alert
   levels; 600 per minute). The token and its URL are shown once, with a copy button.
2. The bridge is a member of the channel, so it decrypts the message like every member device. It
   keeps the sources in its encrypted store and swaps its table of tokens live (checked once a
   second). A restart serves them from the store at once.
3. "Revoke" sends another control message; the token stops working as soon as the bridge read it,
   and a revoked source never comes back. The list shows the sources in force with who added them
   and when; to replace a leaked URL, revoke the source and add a new one.

Who may: **workspace admins** of the verified roster (admin.md §2.3). Channel roles aren't signed
yet (issue #380), so the bridge doesn't take the server's word for them, and the app offers "Add
source" only to workspace admins. The creator must also be a member of the channel, and stay an
admin: a creator who loses the role or leaves takes their sources out of force. Admins and the
source's creator may revoke; changes from anybody else are ignored before they are stored. The
server only carries ciphertext, so it can neither add nor change a source. It can withhold
messages, though: a bridge with a new database may miss a revocation the server leaves out of the
history; one that read it keeps it (e2ee.md §13.1).

Limits: at most 50 sources in force per channel (the oldest), each with its rate limit, and all
sources together at most `listen.total_rate_limit_per_minute` (12000) requests and lines a minute.

The bridge learns of sources through the channel, so the bot must be in the channel before a source
is added (the app offers "Add source" only then): a bot added later reads history from its join on.
When its database is new or was lost, the bridge reads the channels' older history once
(`[app_sources] history_pages`, 200 pages of 50 messages) for older announcements; sources older than
that have to be added again.

**Sources in the file.** A bridge on its own host may still list `[[sources]]` (token, conversation,
mentions) in its configuration, read at start; they serve both endpoints. `[app_sources] enabled =
false` turns app-managed sources off for such a bridge.

### Where producers reach the bridge

The channel section's "Where the bridge runs" switch picks the base of the URLs it shows:

| Choice             | URLs                                                       | When                                                                                                                          |
|--------------------|------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------|
| On this server     | `https://<instance host>/hooks/<token>`, `/ingest/<token>` | The deployment runs the bridge (teamcraft-deploy role `teamcraft_bridge`); its proxy routes both paths to the loopback bridge |
| On its own machine | `http://127.0.0.1:8088/hooks/<token>`, `/ingest/<token>`   | The bridge runs next to the producers on its default listener; use its own address instead if you opened the listener         |

The first is the default. The app does not check whether a bridge answers there: on an instance
without the bridge role the instance's host name has no `/hooks/` route.

## Bots

| What            | How                                                                                                                                                        |
|-----------------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Create          | `POST /api/v1/admin/bots` (admins); the token is returned once, only its SHA-256 is stored                                                                 |
| Authenticate    | `Authorization: Bearer tcb_...` on the REST API and in the gateway `identify`                                                                              |
| Permissions     | Those of a guest: sees only the conversations it was added to                                                                                              |
| Rotate / revoke | `POST /admin/bots/{id}/token` (old token dies at once), `DELETE /admin/bots/{id}` (deactivates the account and removes it from all chats: devices removed, sessions ended) |

A token is a long random string that the server can revoke, so there is no identity provider client
per bridge (client credentials would mean one confidential Keycloak client each) and no signed
token that outlives its revocation. Everything is audited.

## Container

Each release publishes `ghcr.io/zeitgleich/teamcraft-bridge:X.Y.Z` (multi-arch, signed, like the
server image; docs/release.md), built from `containers/teamcraft-bridge.Containerfile` (see
`containers/README.md`). The deployment playbook runs it with `teamcraft_bridge_enabled`. Mount the configuration
and the secrets, keep a volume at `/var/lib/teamcraft-bridge` for the database and one at
`/var/spool/teamcraft-bridge` for the files of webhooks while they upload, publish the port on
loopback only: `-p 127.0.0.1:8088:8088`.

## Limits of the current version

- Files come only as uploads in a form ("Files"); file links in a webhook's JSON (Slack `files`)
  are ignored. An upload that fails is not resumed: the producer sends the request again.
- Idempotency keys live in memory: a retry across a restart of the bridge can post the message
  twice ("Retries").
- The app has no field for a source's files per message yet; app sources take the bridge's
  `files.max_files_per_message` unless their options say otherwise.
- `[[sources]]` in the configuration file are read at start; changing one needs a restart. Sources
  added in the app don't.
- Sources added in the app have no alert mentions yet (the file's `mentions` has no app equivalent)
  and no rename in the UI (the control message exists).
- Channel membership of the bot is managed by members/admins, as for any user; the bridge never
  adds itself.
- The bot list shows no last-activity time and bots have no picture yet.
