# Robustness

Users run TeamCraft on trains, in basements, behind hotel Wi-Fi and on laptops that sleep for a
weekend. A feature is not done when the happy path works; it is done when it behaves sensibly under
the conditions below. Every feature plan and every PR review goes through this checklist. The current
state of the codebase is recorded in [docs/robustness-audit.md](../../docs/robustness-audit.md).

## How to use it

- **Plan:** the feature plan (PR description, design doc section) states for each condition below what
  happens, or says "not applicable" with a reason. "We'll see" is not an answer.
- **Review:** the reviewer checks the same list against the code. A gap is either fixed in the PR or
  recorded in docs/robustness-audit.md with severity and an owner.
- **Test:** conditions that can be simulated in a test (dropped connection, duplicate request,
  expired resume window, stale epoch, clock offset) get a test. Inject clocks and transports; don't
  sleep.

## Checklist

### Network loss and change

- Connection drop is detected within seconds, not minutes: heartbeats/pings with a timeout, and the
  platform's network-change signal (Android `NetworkCallback`, iOS `NWPathMonitor`, browser
  `online`/`visibilitychange`, desktop resume) triggers an immediate reconnect.
- Reconnect uses exponential backoff with jitter and a cap; it never stops for good while the app is
  in use.
- After reconnect the client catches up: gateway resume with sequence numbers, or a full resync when
  the resume window has expired. No event is silently lost; a gap is detected and repaired.
- Long-running transfers (uploads, downloads, device linking) resume or retry cleanly and never leave
  the user with a stuck spinner. Orphaned server objects are cleaned up.
- Tokens that refresh in the background (auth, push tokens) are retried when the network comes back.

### Offline and queued actions

- User actions taken offline (send, edit, delete, react, read marker) are queued, persisted across a
  restart, retried, and shown as pending/failed with a retry action.
- Every mutating request that may be retried carries an idempotency key (client-generated ID), and the
  server deduplicates on it, so a retry after a lost response never creates a duplicate.
- Order is preserved where it matters (messages in a conversation, edits after the send they edit).
- Monotonic state (read markers, sequence numbers) never moves backwards on a late or replayed write.

### Lifecycle

- Mobile: backgrounding, process death and OS kills mid-action. In-flight work either finishes under
  a background task / WorkManager or is restored from the queue on next start. Drafts survive.
- Desktop: sleep/resume and hide-to-tray. Web: frozen/discarded tabs, several tabs of the same account.
- App update mid-action: local store migrations are forward-only and tested; queued actions survive.
- OS battery restrictions (Doze, App Standby, iOS background limits) are accounted for; push, not
  polling, wakes the app.

### Hostile networks

- Slow and lossy links: timeouts are per phase (connect, idle/read), not one short total timeout that
  kills a large transfer; progress is visible.
- Captive portals and proxies: an HTML login page or a TLS failure gives a clear error, not a parse
  panic; system proxies are honoured; WebSocket pings are shorter than common proxy idle timeouts
  (keep them under 30 s).
- IPv6-only and NAT64: no IPv4 literals, bind `[::]`, use `SocketAddr`/`IpAddr` (see code-style.md).

### Multi-device and long absence

- Concurrent changes from two devices (edits, settings, MLS commits) converge: versioned writes or a
  defined merge rule, epoch conflicts retried on the fresh state.
- A device offline for weeks while keys rotated can still catch up, or is told clearly that it
  must re-link, and nothing on the server grows without bound waiting for it.

### Server side

- Restart and rolling deploy: in-memory state (sessions, replay buffers, rendezvous, queues) is either
  persisted or safe to lose, and clients recover without user action. SIGTERM drains gracefully.
- Dependencies (Postgres, S3, identity provider, push gateway) being briefly unavailable causes
  retries and clear errors, not a crash loop or a hung request.
- Every queue, buffer and cache has a configurable bound; a slow consumer can't exhaust memory.

### Large data

- First load is paged or lazy; nothing loads "all conversations" or "all members" of a big instance
  at once. Memory stays bounded with long history, many spaces and big channels.
- Attachments are streamed, not held in memory whole, where the platform allows it.
- Local stores and search indexes have a known growth rate and an eviction or compaction story.

### Time

- Clients never trust their own clock for security or ordering. Server time decides ordering;
  expiry checks allow skew (leeway) and are done by the party whose clock is authoritative.
- Displayed relative times tolerate a wrong local clock (no "in 3 hours" for a message just sent).

### Performance and resources

TeamCraft must stay light: people keep it open all day next to everything else, often on battery.
Measure, don't guess: `tests/e2e` `perf.rs` (web, nightly with generous bounds), numbers and method
in [docs/performance.md](../../docs/performance.md) ("Clients"). A change that touches a hot path
(opening a channel, sending, sync, the message list, timers) states its effect on these budgets.

| Budget                                                  | Desktop   | Web (Chrome)                | Mobile                                       |
|---------------------------------------------------------|-----------|-----------------------------|----------------------------------------------|
| Signed-in start until the channel list is usable        | ≤ 2 s     | ≤ 1.5 s (reload)            | ≤ 2 s                                        |
| Channel switch, p95                                     | ≤ 100 ms  | ≤ 100 ms                    | ≤ 150 ms                                     |
| Memory with a large workspace (300 channels, 100k msgs) | ≤ 400 MiB | ≤ 350 MiB above the browser | ≤ 250 MiB                                    |
| Memory growth over 100 channel switches                 | ≤ 10 %    | ≤ 10 %                      | ≤ 10 %                                       |
| Idle, connected, in the foreground: CPU                 | ≤ 0.5 %   | ≤ 1 %                       | ≤ 1 %                                        |
| Network while idle besides the gateway heartbeat        | none      | none                        | none                                         |
| In the background                                       | –         | –                           | no wakelocks, no polling; push wakes the app |
| Download size (first visit, compressed)                 | –         | ≤ 4.7 MB wasm, gzip -9      | –                                            |

- The download size counts the compressed wasm a signed-in user's first visit transfers: the app,
  the signed-in app's chunks and the service worker, measured as gzip -9 (brotli, where the server
  offers it, is about 30 % smaller). CI checks it on every change to the web client
  (`.github/workflows/web-size.yml`); see docs/performance.md, "Web bundle size", for where it
  stands. Parts of the app a first visit doesn't need are split off and load when first used
  (`areas.rs`).
- Lists that can grow (messages, members, channels, search hits) are virtualised or paged; nothing
  renders or loads "all of them".
- No work proportional to a conversation's or workspace's size on a hot path (send, switch, scroll,
  tick). Reading the whole conversation to send one message is the kind of bug this rules out.
- Timers only run while their result can be seen; push and gateway events, not polling, deliver
  changes. Every periodic task states its interval and why.
- Calls: hardware codecs where available, video effects on the GPU, and no work for streams nobody
  watches; battery measurements per call type are tracked in docs/performance.md.

### User-visible status

- The user can always tell whether they are connected, whether an action is pending or failed, and
  what to do about it. Errors go through i18n.
- No silent failure (issue #322): a failed user action reaches the user, as an error toast through
  `feedback::report_failure` (worded by the server's `error_code`) or inline. Buttons that start
  async work use `PendingAction`/`PendingButton` and are disabled while it runs.
- No dead app: async tasks, timers and shell-event callbacks can outlive their component. After an
  `.await` they touch signals only with `set_if_alive`/`update_if_alive`/`try_*` and the app state
  with `AppState::try_write` (CI: `scripts/check-async-signals.py`). A panic shows the reload
  overlay (`panic_overlay.rs`), never a frozen page.
