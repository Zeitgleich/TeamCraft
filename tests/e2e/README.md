# teamcraft-e2e

End-to-end tests of TeamCraft's v1 flows against the real binaries: the **`teamcraft-server`**,
**`teamcraft-sfu`** and **`teamcraft-bridge`** processes run next to the dev stack (Keycloak, Postgres,
Garage, Mailpit), and headless `teamcraft-core` clients (the same code the desktop and mobile apps use)
or a headless Chromium drive them. Nothing is mocked: tokens come from the real Keycloak login flow,
messages are real MLS ciphertext, calls go through the SFU with DAVE.

```sh
scripts/e2e.sh                 # everything: dev stack, builds, web client, tests
scripts/e2e.sh --no-browser    # without the web client build and Chromium
scripts/e2e.sh -- calls::      # arguments after -- go to the test binary (name filter, --ignored, ...)
scripts/e2e.sh --down          # stop the dev stack afterwards
scripts/e2e.sh --prebuilt-web  # use the web client already in target/e2e-web (or $E2E_WEB_DIR)
```

The crate is empty without its `e2e` feature, so `cargo test --workspace` stays fast and needs no
Docker. With the feature, a missing dev stack makes every test **skip** (a message on stderr), so
`cargo test --workspace --all-features` also works on machines without Docker; `scripts/e2e.sh` sets
`E2E_REQUIRED=1`, which turns a missing service, binary or browser into a failure.

## What it covers

Each test starts its own server with a fresh Postgres database (`teamcraft_e2e_<random>` in the dev
Postgres, dropped afterwards), so tests are independent and run in parallel (15 to 25 minutes for all on a
four-core runner, most of it the browser tests, which share two ports).

| File                    | Flow                                                                                                                                        |
|-------------------------|---------------------------------------------------------------------------------------------------------------------------------------------|
| `accounts.rs`           | Keycloak login for owner/admin/member/guest, a user without a TeamCraft role is refused, first device enrollment                            |
| `messaging.rs`          | Space + channel, encrypted messages, threads, reactions, DMs; the server sees ciphertext only (also not in its log); outsiders can't read   |
| `guests.rs`             | A guest sees only the conversations they were added to, can't join private channels, create channels, use the admin API or DM strangers     |
| `membership.rs`         | Removing a member rotates the keys: the removed member can't read what is sent afterwards, history stays, their writes never arrive         |
| `devices.rs`            | Device linking through the rendezvous relay, the linked device joins the channel and decrypts new messages                                  |
| `admin.rs`              | Admin deactivation (token refused, devices revoked, gateway closed, audit log) and reactivation; retention job (`max_messages`)             |
| `bridge.rs`             | `teamcraft-bridge` process: Slack-style webhook and log lines land decrypted in an integration channel, bad input is refused                |
| `meetings.rs`           | Scheduling sends iMIP invitations (caught by Mailpit), RSVP, an external guest joins with the mailed link, lobby, admission, cancellation   |
| `calls.rs`              | 1:1 DM call through the server's join tokens and the SFU: device-bound DAVE keys, both sides decode audio, equal privacy codes              |
| `web_calls.rs`          | Browser and native client in one channel call: call module, DAVE keys signed by the page, audio and camera                                  |
| `meetings_firefox.rs`   | A guest joins a meeting by its link in Firefox (44.1 kHz sound device), is let in, and shares the call with the organizer (#1721)           |
| `moderator.rs`          | Channel moderator role: removes members (keys rotate), can't manage roles, properties or the admin                                          |
| `conversion.rs`         | Public/private conversion: who may, who can join afterwards, newcomers' history, system event, audit                                        |
| `threads_policy.rs`     | Signed policy: auto-follow default for people who never chose, threaded view switch the server enforces                                     |
| `guest_domains.rs`      | Guest e-mail domain restriction: invitations, meeting invitees, guest sign-in                                                               |
| `mfa.rs`                | Require-MFA policy: local accounts need their TOTP code, IdP tokens without a second factor are refused                                     |
| `retention_limits.rs`   | Retention limit in the policy: channel/space settings stay within it, also after it is lowered                                              |
| `display_names.rs`      | Directory policy (name format, full-name and e-mail privacy) in every response that names a person                                          |
| `email_digest.rs`       | Content-free digest of missed mentions: opt-in, batched, skips what was read in time                                                        |
| `dependency_outages.rs` | Storage and identity provider cut or hanging, Postgres cut: 503 + Retry-After, `/readyz`, recovery                                          |
| `demo_signin.rs`        | The demo seed (`demo-seed`) on a test server, database snapshot and restore as `demo-reset.sh` does, a reviewer's new device restores from the passphrase and reads a message, a DM and a file |
| `server_robustness.rs`  | SIGTERM with clients connected, resume vs. full sync, expired window, retries, Postgres outage, a reader that stalls, RSS/CPU               |
| `web.rs` (`browser`)    | The web client in headless Chromium: Keycloak login page, welcome screen, the channel list shows the account's channels                     |
| `web_directory.rs`      | The directory in the web client: people cards, the Channels tab, resending and revoking an invitation |
| `web_single_image.rs`   | One picture of an album in the web client: tile menu, a quote with its picture that opens it, a forward of it alone |
| `desktop_perf.rs` (`desktop`) | The Linux desktop app on a large workspace, visible and closed to the tray: CPU, wakeups, memory per process, the server's requests (#1022) |

The `acceptance_*` scenarios cover the v1 sections of PLAN.md one by one; what each proves, the known gaps
and the bugs found are in [docs/acceptance.md](../../docs/acceptance.md).

### Ignored

| Test                                                            | Why                                                               |
|-----------------------------------------------------------------|-------------------------------------------------------------------|
| `acceptance_people::a_time_zone_that_does_not_exist_is_refused` | Bug: `PUT /me/profile` accepts time zones that are not IANA names |

## How it works

- **`Stack`** (`src/stack.rs`): creates the database, writes the configuration as `TEAMCRAFT_*`
  environment variables, starts `target/<profile>/teamcraft-server`, waits for `/healthz`. With
  `.with_calls()` it also starts the SFU (join-token key pair, registration token, self-signed
  certificate whose hash is read from the SFU's log). `.with_meetings()` points the invitation mail at
  Mailpit. Ports are picked per test and never reused within a run. The processes die with the test;
  their logs are in `$TMPDIR/teamcraft-e2e-<id>/` (removed when a test ends cleanly).
- **`Peer`** (`src/peer.rs`): a real `teamcraft-core` client with an in-memory store, enrolled as a dev
  realm user, with a gateway connection. `wait_until`/`wait_for_text` pump the gateway until a
  condition holds (real network delivery, no fixed sleeps).
- **`UserTokens`** (`src/tokens.rs`): access tokens through `dev/dev-token.py` (the realm has no password
  grant), refreshed before the five-minute lifetime ends.
- **`src/call.rs`**: joins calls like the desktop app does (join token, device-bound call keys signed by
  the core, the conversation's verified devices as trusted roster) with `teamcraft-media`'s `NativeCall`
  and test tones. The `teamcraft-call` binary only supports unauthenticated development tokens, so the
  tests use the library behind it.
- **`src/browser.rs`** (feature `browser`): headless Chromium over the DevTools protocol.
- **`src/firefox.rs`** (feature `browser`): headless Firefox over WebDriver BiDi, the protocol
  Firefox itself serves with `--remote-debugging-port` (no geckodriver, no new crate: JSON over the
  suite's `tokio-tungstenite`). Its tests skip without a Firefox (`FIREFOX`, else `firefox` on
  `PATH`), unless `E2E_REQUIRED=1`.

### Why chromiumoxide for the browser

Playwright needs Node and a browser download, `fantoccini` needs a driver binary next to Chrome, and the
repository has no TypeScript application or test code. `chromiumoxide` is a Rust crate that talks to the
Chrome or Chromium that is already installed (GitHub's runners ship Chrome), so the browser tests are
Rust like everything else, run in the same test binary as the other flows and share its helpers (the
second client in the same scenario is a `Peer`). The cost is a smaller ecosystem than Playwright's:
no auto-waiting, so `tests/suite/web.rs` has its own small `wait_for_*` helpers. Failing browser tests save a
full-page screenshot to `target/e2e-artifacts/<test>.png`.

### The web client build

`tests/e2e/web/build.sh` builds `crates/teamcraft-ui` with `trunk build --release` in a container
(`web/Containerfile`: `rust:1.99.1-trixie`, the `wasm32-unknown-unknown` target and trunk, which
downloads the `wasm-bindgen` version of `Cargo.lock`) into `target/e2e-web`. Cargo's registry and the
build output are kept in the Docker volumes `teamcraft-e2e-web-cargo` and `teamcraft-e2e-web-target-<id>`
(one per checkout, which are all mounted at the same path) so reruns are incremental (`docker volume rm`
them to reclaim the disk space). The server under test serves
that directory as its web client (`[web] static_dir`).

The dev realm registers `http://127.0.0.1:8082/*` and `:8083/*` as the web client's redirect URIs, so the
browser tests start their server on one of those and take turns per port, also across test processes and
checkouts (a lock file in the temp directory). Tests on the two ports run at the same time; `perf.rs` holds
both, as it measures. Its default Content-Security-Policy allows only
`https:` for `connect-src`; the tests widen it to the plain-http dev Keycloak.

### The desktop app

`desktop_perf.rs` (feature `desktop`) starts the Linux desktop app as it ships, a debug build with the
web client embedded, and drives its page over WebKit's remote inspector (`crate::desktop`). Nothing of
the user's session is used: each run gets its own session bus (`dbus-daemon`), a keyring on it
(`gnome-keyring-daemon`, where the app keeps the key of its secrets file), a tray watcher (so closing
the window hides it to the tray), and its own home and XDG directories. The sign-in opens the "system
browser", a script that writes the address down; the harness fills in Keycloak's form from there. The
inspector is closed while anything is measured: an attached one instruments the page.

```sh
cp -r target/e2e-web crates/teamcraft-ui/dist          # the web client the app embeds
cargo build -p teamcraft-desktop --features custom-protocol
E2E_DESKTOP_BIN=$PWD/target/debug/teamcraft-desktop xvfb-run -a \
  cargo test -p teamcraft-e2e --features desktop --test suite -- --nocapture desktop_perf::
```

Without `custom-protocol` a debug build loads its page from the dev server URL instead
(`devUrl`, port 8082, where the browser tests start their servers). `PERF_SCALE` is the one of
`perf.rs`; `PERF_DESKTOP_HOLD_SECS` keeps the app open after the channel switches, to look at it
with `WEBKIT_INSPECTOR_HTTP_SERVER`'s port from its environment. The nightly workflow runs it in a
job of its own (`desktop`), and `scripts/e2e.sh --desktop` runs only these tests.

## Running by hand

```sh
docker compose -f docker-compose.dev.yml up -d postgres keycloak garage mailpit && ./dev/init-garage.sh
cargo build -p teamcraft-server -p teamcraft-sfu -p teamcraft-bridge
tests/e2e/web/build.sh                                  # only for the browser tests
cargo test -p teamcraft-e2e --features browser --test suite            # or --features e2e
cargo test -p teamcraft-e2e --features browser --test suite -- --include-ignored web::
```

| Variable                 | Meaning                                                                                    |
|--------------------------|--------------------------------------------------------------------------------------------|
| `E2E_REQUIRED=1`         | A missing dev stack, binary, web build or browser fails instead of skipping                |
| `E2E_BIN_DIR`            | Directory with the server, SFU and bridge binaries (default: the test binary's profile)    |
| `E2E_WEB_DIR`            | The `trunk build` output (default `target/e2e-web`)                                        |
| `E2E_ARTIFACTS`          | Where failure screenshots go (default `target/e2e-artifacts`)                              |
| `CHROME`                 | Chrome or Chromium executable (default: first of `google-chrome`, `chromium`, ... on PATH) |
| `FIREFOX`                | Firefox executable (default: `firefox` on PATH, or the macOS app)                          |
| `E2E_CONSOLE`            | Browser tests print the page's console as it happens (`web_calls.rs`)                      |
| `RUST_MIN_STACK`         | Stack of a test thread: 16 MiB in `scripts/e2e.sh`, set it for a plain `cargo test`        |
| `E2E_DESKTOP_BIN`        | The desktop app's binary for `desktop_perf.rs` (without it the test skips)                 |
| `E2E_DESKTOP_REQUIRED=1` | A missing desktop binary or session (bus, keyring) fails instead of skipping               |

A test that aborts (a stack overflow, a kill) leaves its server and Chromium running; the web tests share port
8082, so stop them (`ss -ltnp | grep 8082`) before the next run, or every web test talks to the stale server.

The harness removes every inherited `TEAMCRAFT_*` variable from the processes it starts (the server reads
them all as configuration), which is also why none of these variables carries that prefix.

## CI

`.github/workflows/e2e.yml` runs `scripts/e2e.sh` on demand (`workflow_dispatch`) and nightly: one job
builds the web client (about 25 minutes, mostly its link-time optimization), a second one takes that build,
compiles the rest and runs the tests (about 30 minutes), each within its own time limit. On failure it
uploads the screenshots and the logs of the server, SFU and bridge, not Chromium's profiles. The regular CI
workflow doesn't run it.

`.github/workflows/scheduled-runs.yml` keeps one open issue per red scheduled workflow on `main`
(`Scheduled run is red: <workflow>`), comments on it each time it fails again and closes it after the next
green run, so a red night shows up in the issue list.

## Adding a test

Start a stack in a new `tests/suite/<flow>.rs` (add it to `tests/suite/main.rs`), enroll `Peer`s, use the
helpers in `scenario.rs` for spaces and channels, call REST through `stack.get/post/...` for what the core
doesn't do, and wait with `wait_until` instead of sleeping. Use neutral names (`Project A`, `example.org`);
mail addresses must be unique per run because Mailpit keeps all mail of earlier runs.

