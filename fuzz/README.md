# Fuzzing

`cargo-fuzz` (libFuzzer with sanitizers) targets for everything that parses bytes from outside. The
directory is not part of the main workspace (it needs a nightly toolchain); `run.sh` runs the targets
in a container so no local nightly is needed.

```sh
fuzz/run.sh list              # the targets
fuzz/run.sh                   # all targets, 180 s each
fuzz/run.sh imip 600          # one target for ten minutes (needs rootless podman or CONTAINER_ENGINE=docker)
```

CI (`vendored-and-fuzz` in `ci.yml`, part of `ci-gate`) builds every target on stable with
`cargo build --manifest-path fuzz/Cargo.toml --bins --locked`, so API drift in a decoder or a stale
`fuzz/Cargo.lock` fails the pull request. Run the same command locally after changing a fuzzed API; if
it says the lock needs an update, run `cargo update --manifest-path fuzz/Cargo.toml -w` and commit
the lockfile. Fuzzing itself stays manual (`run.sh`).

| Target                | What it feeds                                                                                                    |
|-----------------------|------------------------------------------------------------------------------------------------------------------|
| `proto_requests`      | every request body and query type of the REST API, and the notification/status types: decode, re-encode, decode  |
| `call_fallback_frame` | a call's WebSocket fallback frames (ARCHITECTURE.md §6.6) and the JSON inside them                               |
| `proto_gateway`       | gateway frames in both directions, events, rendezvous frames and the main response types                         |
| `bridge_ingest`       | Slack/Mattermost webhook bodies (JSON and form), plain/NDJSON log lines, Slack markup, then batching             |
| `bridge_multipart`    | webhooks with files (`multipart/form-data`) cut at random points: same outcome, bounded buffer, limits held      |
| `thumbnail`           | untrusted pictures of every format apps and the bridge decode for thumbnails, within the bridge's pixel limit    |
| `app_link`            | links the operating system hands to the apps: no panic, no traversal, stable round trip through both spellings   |
| `voice_decode`        | voice messages (Ogg or WebM Opus) decoded to WAV: no panic, output bounded by the input's size                   |
| `imip`                | received iMIP REPLY text; rendering events from arbitrary strings (CR/LF injection, line folding)                |
| `workspace_log`       | signed workspace log entries (random payloads and signatures must fail verification) and the roster/policy chain |
| `crypto_decoders`     | binary decoders of messages, key bags, wrapped backup keys, membership changes, certificates, call bindings      |
| `dpop_proof`          | DPoP proofs from the `DPoP` header or a first WebSocket frame, also valid ones with bytes changed: never a panic |
| `scim`                | SCIM filters, PATCH paths and operations, user and group resources: no panic, bounded, stable round trip         |

There is no target for CalDAV responses: the CalDAV client only looks at HTTP status codes and
parses no XML.

## Corpus

`fuzz/corpus/<target>/` holds hand-written seeds (valid examples) and regression inputs named
`regression_*`: every crash that was found and fixed stays as a test input. Entries the fuzzer
generates (named by their 40-character hash) are ignored by git; keep them locally for faster runs.
`cargo fuzz run <target> artifacts/<target>/crash-...` reproduces a crash, `cargo fuzz tmin` minimizes it.

## Findings so far

| Target          | Found                                                                                                                                                                      | Fixed                                                                    |
|-----------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------|
| `bridge_ingest` | Runs of backticks in a log line re-formed a code fence after the first escaping (`regression_backtick_runs`)                                                               | Escaping inserts a zero-width space before every third backtick of a run |
| `bridge_ingest` | Escaping happened after the size accounting, so a batch could exceed its limit by up to 2.5x                                                                               | Lines are escaped first, then cut and counted                            |
| `imip`          | A line break in the join URL, UID or an address ended the content line (iCalendar injection); the values come from the server and the identity provider, not from invitees | Non-text values drop control characters                                  |
