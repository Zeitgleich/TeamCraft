# Fuzzing

`cargo-fuzz` (libFuzzer with sanitizers) targets for everything that parses bytes from outside. The
directory is not part of the main workspace (it needs a nightly toolchain); `run.sh` runs the targets
in a container so no local nightly is needed.

```sh
fuzz/run.sh list              # the targets
fuzz/run.sh                   # all targets, 180 s each
fuzz/run.sh imip 600          # one target for ten minutes (needs rootless podman or CONTAINER_ENGINE=docker)
```

| Target             | What it feeds                                                                                                    |
|--------------------|------------------------------------------------------------------------------------------------------------------|
| `proto_requests`   | every request body and query type of the REST API, and the notification/status types: decode, re-encode, decode  |
| `proto_gateway`    | gateway frames in both directions, events, rendezvous frames and the main response types                         |
| `bridge_ingest`    | Slack/Mattermost webhook bodies (JSON and form), plain/NDJSON log lines, Slack markup, then batching             |
| `bridge_multipart` | webhooks with files (`multipart/form-data`) cut at random points: same outcome, bounded buffer, limits held      |
| `thumbnail`        | untrusted pictures of every format apps and the bridge decode for thumbnails, within the bridge's pixel limit    |
| `imip`             | received iMIP REPLY text; rendering events from arbitrary strings (CR/LF injection, line folding)                |
| `workspace_log`    | signed workspace log entries (random payloads and signatures must fail verification) and the roster/policy chain |
| `crypto_decoders`  | binary decoders of messages, key bags, wrapped backup keys, membership changes, certificates, call bindings      |

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
