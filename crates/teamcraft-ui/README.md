# teamcraft-ui

Leptos (CSR) UI shared by the desktop app (`apps/desktop`, Tauri 2) and the web client. It always
signs in to a server. For UI work without a server there is a **mock backend** with deterministic demo
data behind the development-only `demo` feature:

```sh
(cd crates/teamcraft-ui && trunk serve --features demo)   # then open /?backend=mock
```

The demo data never ships: an optimized (`--release`) build with `demo` fails to compile, release builds
never enable it, and `scripts/check-no-demo-data.py` fails a release whose web bundle or desktop app
contains demo fixtures.

| Screen                          | Image                                                          |
|---------------------------------|----------------------------------------------------------------|
| Channel with thread panel       | ![Channel with thread](docs/screenshots/channel-thread.png)    |
| Threads inbox                   | ![Threads inbox](docs/screenshots/threads.png)                 |
| Quick switcher                  | ![Quick switcher](docs/screenshots/quick-switcher.png)         |
| Status menu                     | ![Status menu](docs/screenshots/status-menu.png)               |
| Log channel (50,000 lines)      | ![Log channel](docs/screenshots/log-channel.png)               |
| Pre-join screen                 | ![Pre-join](docs/screenshots/call-prejoin.png)                 |
| Call: gallery                   | ![Gallery](docs/screenshots/call-gallery.png)                  |
| Call: active speaker            | ![Active speaker](docs/screenshots/call-active-speaker.png)    |
| Call: shared screen             | ![Shared screen](docs/screenshots/call-screen-share.png)       |
| Huddle bar in a channel         | ![Huddle bar](docs/screenshots/huddle-bar.png)                 |
| Incoming call                   | ![Incoming call](docs/screenshots/incoming-call.png)           |
| Admin: spaces and channels      | ![Admin channels](docs/screenshots/admin-channels.png)         |
| Admin: users and devices        | ![Admin users](docs/screenshots/admin-users.png)               |
| Admin: security policy          | ![Admin security](docs/screenshots/admin-security.png)         |
| Admin: retention                | ![Admin retention](docs/screenshots/admin-retention.png)       |
| Admin: audit log                | ![Admin audit](docs/screenshots/admin-audit.png)               |
| Live: unread badges and statuses  | ![Live unread](docs/screenshots/live-unread.png)               |
| Live: threads inbox (server data) | ![Live threads](docs/screenshots/live-threads.png)             |
| Live: DM with "Seen by"           | ![Live DM](docs/screenshots/live-dm-seen.png)                  |
| Live: notification settings       | ![Live notifications](docs/screenshots/live-notification-settings.png) |
| Meetings: list                    | ![Meetings](docs/screenshots/meetings-list.png)                |
| Meetings: schedule dialog         | ![Schedule](docs/screenshots/meetings-schedule.png)            |
| Meetings: lobby of a hosted meeting | ![Lobby](docs/screenshots/meetings-lobby.png)                |
| Meetings: guest waiting / admitted | ![Guest waiting](docs/screenshots/meetings-guest-waiting.png) ![Guest admitted](docs/screenshots/meetings-guest-admitted.png) |
| Live admin: channels, users, security, audit | ![Channels](docs/screenshots/live-admin-channels.png) ![Users](docs/screenshots/live-admin-users.png) ![Security](docs/screenshots/live-admin-security.png) ![Audit](docs/screenshots/live-admin-audit.png) |
| Key-change banner               | ![Key change](docs/screenshots/key-change-banner.png)          |
| Compare keys                    | ![Compare keys](docs/screenshots/key-verify.png)               |
| New login approval              | ![Device approval](docs/screenshots/device-approval.png)       |
| QR device linking               | ![QR linking](docs/screenshots/device-link-qr.png)             |
| Lightbox with the thread        | ![Lightbox](docs/screenshots/lightbox.png)                     |
| Lightbox: tooltip with the key  | ![Tooltip](docs/screenshots/lightbox-tooltip.png)              |
| Lightbox: rotated and zoomed    | ![Rotated](docs/screenshots/lightbox-rotated.png)              |
| Lightbox: "…" menu (en, de)     | ![Menu](docs/screenshots/lightbox-menu.png) ![Menu de](docs/screenshots/lightbox-menu-de.png) |
| Lightbox: file details          | ![Details](docs/screenshots/lightbox-details.png)              |
| Lightbox: video                 | ![Video](docs/screenshots/lightbox-video.png)                  |
| Lightbox: GIF, reduced motion   | ![Paused GIF](docs/screenshots/lightbox-gif-paused.png)        |

Screenshots are produced with Playwright against `trunk build --features demo` (English, 1360×840); the
tour also walks every screen in German and fails on missing translation keys.

The 50,000-line `#app-logs` channel (see `docs/screenshots/log-channel-dark.png`) keeps scrolling at
frame rate because only the visible rows are in the DOM.

## Motion and loading

- Tokens in the first block of `style/main.css`: `--dur-fast` (120 ms), `--dur-base` (180 ms),
  `--dur-slow` (260 ms), `--dur-exit` (100 ms) and the curves `--ease-standard`,
  `--ease-emphasized`, `--ease-exit`, `--ease-pop`. With `prefers-reduced-motion` every duration is
  ~0. The transitions themselves are in the stylesheet's last section ("Motion").
- Only `transform` and `opacity` animate; entrances run once when an element mounts, nothing loops
  while it isn't visible (placeholders pulse a few times, then stand still).
- Exits: `motion::use_leave_ghost` leaves an inert copy of a dialog, menu or toast that plays the
  exit while the real element is already gone, so focus and the next view never wait.
- The feed decides how a row enters (`feed_motion.rs`): new since the conversation opened, the
  user's own message settling once confirmed, reactions whose count changed.
- Lists that load show `components/skeleton.rs` placeholders, empty ones `components/empty_state.rs`
  (icon, headline, one sentence, at most one action).

## What is in the shell

- Layout like Mattermost: space rail, sidebar (Unreads, Favorites, Channels, Integrations, Direct
  messages; collapsible), channel header with topic, members and favorite, bookmarks bar
  (placeholder), thread side panel, members panel, threads inbox.
- Virtualized message list with measured row heights, author/time grouping, date dividers, reactions,
  thread reply counts, edited marker, system messages, compact log rows with level colours, follow
  mode ("Jump to latest").
- Composer: Markdown hint, attachments (names only), Enter or Ctrl/Cmd+Enter to send (setting).
- Quick switcher (Ctrl/Cmd+K), status menu (online, away, do not disturb, offline, custom status
  with expiry and presets), settings modal (display, sidebar work; notifications and advanced are
  stubs), shortcuts reference (Ctrl/Cmd+/).
- Light and dark theme through CSS variables (`style/main.css`, first block), "match system" default.
- i18n with Fluent: `i18n/en.ftl` is the source, `i18n/de.ftl` the translation. A test fails when
  German misses a message that English has.
- Keyboard and screen readers: skip link, landmarks, labelled controls, focus-visible rings, roving
  focus in menus and tabs, modal focus trap and focus return, arrow keys between messages,
  `prefers-reduced-motion`.

- **Calls** (PLAN.md §8): pre-join screen with camera, microphone and speaker choice, microphone
  meter, speaker test, background effects (blur light/strong, presets) and mirror toggle; call
  screen with gallery, active speaker and shared-screen layouts, mute/camera/share/raise hand,
  effects popover; huddle bar in channels and a sidebar pill while a call is minimized; incoming-call
  card. Media is simulated (stylised tiles), the roster comes from `CallBackend`.
- **Administration** (PLAN.md §13, admin.md): spaces and channels incl. private ones with metadata
  only, users with devices, guests with sponsor and expiry, security policy (`admin_self_join`,
  organization recovery, sign-in and sharing rules) saved as a new signed version, retention per
  channel with validation, audit log with filters. Reachable from the account menu for admins.
- **Device security** (e2ee.md §3, §5): approval prompt for a new login with the short code, QR
  linking dialog (real QR code of a demo payload), Devices tab in Settings, key-change banner with a
  fingerprint comparison dialog.

- **Search** (PLAN.md §7): `Ctrl/Cmd+Shift+F` or the sidebar item opens the search page: words, "phrases",
  `from:`, `in:`, `has:file`, `has:link`, `before:`, `after:`, `in-thread`, removable filter chips (unreadable
  filters are flagged), highlighted snippets, saved searches and recent mentions. The logic is in
  `src/search.rs` behind `SearchBackend`; `teamcraft-core` answers from its local index. `Ctrl+Enter` in the quick switcher searches for its text.
- **Settings** (PLAN.md §4, §5): notifications (levels, push timing, keywords, highlight-only keywords, thread
  replies, DND schedule, per-conversation level and mute), display (density, status in messages), sidebar
  (sort, DM limit), language, privacy, devices and security (sign out devices, identity fingerprint, safety
  numbers), calls (default devices, join behaviour, audio processing, background). Values are one JSON
  document (`src/prefs.rs`); notification and privacy values are saved but enforced by the notification engine
  later, call and sidebar values already apply.

Demo triggers (`demo` builds only): Settings > Advanced simulates an incoming call, a new login, the QR
linking screen and a chat with a changed security key.

Shortcuts: `Ctrl/Cmd+K` switcher, `Ctrl/Cmd+,` settings, `Ctrl/Cmd+/` shortcuts, `Alt+Up/Down`
previous/next conversation, `Alt+T` threads, `Alt+Left/Right` back/forward, `Esc` close dialog or
side panel.

**URLs** (`src/route.rs`, `src/history.rs`): every main view has a path, so reload, Back/Forward,
bookmarks and "open in new tab" work. Paths hold public IDs only, never names, message text or
search queries; the message paths are the permalinks' (`teamcraft:/c/{c}/m/{m}[/t/{t}]`). The
desktop app uses the same paths in its webview and maps the mouse's back/forward buttons and
`Alt+Left/Right` to the history itself. The server answers every non-API path with `index.html`.

| Path                              | Shows                                                   |
|-----------------------------------|---------------------------------------------------------|
| `/`                               | The start: first channel or the getting-started page    |
| `/home`                           | Home                                                    |
| `/s/{space}`                      | A space (lands in its first channel)                    |
| `/s/{space}/c/{channel}`          | A channel                                               |
| `/dm/{conversation}`              | A DM or group DM                                        |
| `/c/{conversation}`               | A conversation whose kind hasn't loaded yet             |
| `/c/{conversation}/t/{thread}`    | A conversation with a thread open                       |
| `/c/{conversation}/m/{message}`   | A message (`/t/{thread}` appended for thread replies)   |
| `/threads`, `/activity`, `/calls` | Threads inbox, activity, calls                          |
| `/meetings`, `/people`, `/saved`  | Meetings, people and groups, saved messages             |
| `/search`                         | Search (the query stays out of the URL)                 |
| `/settings/{tab}`                 | The settings dialog on a tab                            |
| `/admin/{section}`                | A section of the administration console (admins)        |

A path the app doesn't know, or an ID that doesn't exist or isn't the user's, shows Home with a
notice. IDs that haven't loaded yet are waited for (up to 20 s on an empty store). Sign-in returns
to the path it started on.

## Layout of the crate

| Path                    | Content                                                                 |
|-------------------------|-------------------------------------------------------------------------|
| `src/mock.rs`           | `MockBackend`, demo data (feature `demo` and tests only)                |
| `src/remote_backend.rs` | `RemoteChat`: `ChatBackend` over the desktop shell's client core (IPC)  |
| `src/admin_live.rs`     | Admin console on the admin API: channels, users, devices, audit, policy  |
| `src/meetings.rs`       | Meetings REST client, schedule form logic, guest link parsing and client |
| `src/live.rs`           | `LiveServer`: unread counts, followed threads, presence, receipts (REST)  |
| `src/notify_sync.rs`    | Server-enforced notification settings in step with the preferences      |
| `src/notifier.rs`       | Desktop notification policy and the watcher for new messages            |
| `src/hybrid.rs`         | `Hybrid`: chat backend plus live state, admin API and desktop calls     |
| `src/server_backend.rs` | `RestChat`: web build, read-only chat over the REST API                 |
| `src/web_core.rs`       | Web build: runs the client core in the page, answers the `core_*` commands |
| `src/calls.rs`          | `CallState` (session state of the user's call)                          |
| `src/state.rs`          | `AppState` (context): current space, view, panel, overlay, backend      |
| `src/route.rs`          | `Route`: the app's places as URL paths, parsing and formatting          |
| `src/history.rs`        | Address bar and history in step with `AppState`, Back and Forward       |
| `src/settings.rs`       | Per-device preferences (theme, language, send mode) in `localStorage`   |
| `src/i18n.rs`           | Fluent bundles, `I18n::t`, `t_with`, `t_count`                          |
| `src/markdown.rs`       | Safe Markdown subset producing a tree (no HTML injection)               |
| `src/components/`       | One module per screen area                                              |
| `style/main.css`        | Design tokens, layout, components                                       |

The backend traits (`ChatBackend`, `SearchBackend`, `CallBackend`, `AdminBackend`, `SecurityBackend`)
and the view models live in `teamcraft-client-api`, which `teamcraft-core` (feature `ui`) implements
too; `teamcraft-ui` re-exports them.

## Client core in the desktop app

The desktop shell hosts `teamcraft-core` on a dedicated thread (see `apps/desktop/src/core/`), because
the core is single-threaded and keeps the encrypted store (SQLCipher, key in the OS keystore) on the
native side. After sign-in the webview:

1. calls `core_start`; the answer is `needs_enrollment`, `ready` or `failed`;
2. on first use shows the key setup screen (recovery passphrase, device name) and calls `core_enroll`;
3. builds `Hybrid<RemoteChat>`: chat, search, own devices and keys come from the core, the admin console
   from the admin API, calls from the shell.

`ChatBackend` is synchronous but IPC is not, so `RemoteChat` keeps a mirror (snapshot plus pages of
100 messages). A read miss returns an empty window, starts a load and re-renders when the page
arrives. The shell emits `core-changed` after every sync step; the mirror refreshes then. Writes
(`core_action`) go through the core, so they are encrypted, queued and sent by the sync loop.
In `demo` builds, Settings > Data source switches to the mock backend.

Local data lives in the app data dir (`<data>/core/<user>.db`); `TEAMCRAFT_DATA_DIR` overrides it.

### Live server state and notifications

Things the server owns (it is not message content) are read over REST by `LiveServer` and laid over
the chat by `Hybrid`: unread and mention counts, the followed-threads inbox (thread roots come from the
core), presence with custom status, DM read receipts ("Seen by …"), thread follow and read. A refresh
runs at start, when the chat changes and every 30 s.

`NotificationSync` applies the server's notification settings to the local preferences at start and
sends later changes: push mode, do-not-disturb schedule, read receipts, auto-follow and the level and
mute time of each conversation. Desktop level, keywords and highlights stay on the device.

Desktop notifications: `Notifier` watches the chat for new messages and thread replies and applies
`decide` (level, per-conversation override, mutes, keywords, do-not-disturb schedule and manual DND,
the conversation in front). The shell shows the result with the `notify_user` command
(`tauri-plugin-notification`). Message text only appears when "Show message text" is on.

### Meetings

Signed-in builds get a Meetings screen (sidebar): upcoming and just-ended meetings by day, "Start now"
(instant meeting), a schedule dialog (title, time and length, description, members, external email
invitees, recurrence, lobby on/off), RSVP for invited members, cancel and a personal guest link for
hosts, and the lobby of the meetings the user hosts (let in / turn away). Join opens the meeting's chat
and the (still demo) call pre-join screen. Everything is the server's state (docs/meetings.md);
`MeetingsHub` mirrors it and refreshes every 10 s while the screen is open.

A guest opens `…/meeting/<id>#invite=<token>`: the web build shows a page without sign-in (name,
lobby wait, admitted or turned away). The token stays in the URL fragment. Browser media for guests is
not wired yet; the page says so.

### Administration

For admins and owners the console reads the admin API (`AdminLive`, metadata only): spaces and channels
with owners and retention, users with roles, devices and spaces, the audit log with names resolved, and
the workspace policy. Deactivate/reactivate, device removal and channel retention go to the server.
The security policy is signed: only an owner on the desktop can save it, because the core signs the
new version with the owner's device key (and founds the signed roster from the admin and owner roles
if there is none yet). Browser builds and non-owners see it read-only; fields the signed policy does
not hold (sign-in rules, guest domains, DM scope) are shown disabled.

### The web build served by the server

`trunk build --release` produces the web client; point the server's `[web] static_dir` at the output
and it serves the app with its Content-Security-Policy (no inline scripts or styles). A Trunk hook
(`scripts/externalize-init.sh`) moves the loader script into `init.js`, and all dynamic styles are set
through the CSSOM (`style:prop=`), so the default policy needs no `unsafe-inline`. `sw.js` is copied to
the root and registered in the browser (not in the desktop shell); it wakes the app for pushes and, until
the worker can open sealed payloads, shows a generic notification.

`index.html` carries a loading screen in plain HTML and CSS (`style/loader.css`), painted before any
script runs. `loader.js` is the Trunk initializer: it adds the cached brand and theme, the download
progress and an error with a retry button, and fades the screen out once the app is mounted. Its texts
are the `loader-*` messages of `i18n/*.ftl`, which `scripts/finish-loader.sh` (a Trunk hook) copies into
`init.js` together with the size of the built wasm. Release builds use the `web-release` Cargo profile
(size over speed) and `wasm-opt -Oz`; the release tarball adds `.br`/`.gz` variants
(`scripts/compress-web.sh`), which the server serves by `Accept-Encoding`. Browser login needs the server's
origin registered as a redirect URI of the identity provider's client.

### Try it without the desktop window

An ignored test serves the shell's commands over loopback HTTP, with a real core for alice and a
second core (carol) that chats with her; `apps/desktop/dev/bridge-shim.js` implements
`window.__TAURI_INTERNALS__` (the shell sets `withGlobalTauri: false`) on top of it, so the web build in any browser talks to a real core:

```sh
docker compose -f docker-compose.dev.yml up -d              # dev stack; run teamcraft-server on :8081
(cd crates/teamcraft-ui && trunk serve --port 8082)         # proxies /api to the server
cargo test -p teamcraft-desktop --features integration -- --ignored serve_dev_bridge --nocapture
# open http://localhost:8082/ with the shim injected (e.g. a Playwright init script)
```

Alice's recovery passphrase in the dev stack is `dev-stack core passphrase`. Dev tool only: no
authentication, loopback only, fixed database key.

Tests against the running dev stack: `cargo test -p teamcraft-desktop --features integration`
(full desktop flow with a fake browser, restore after restart, logout, an account without role, cancel,
keyring). The browser flow was exercised with Playwright against `trunk serve`.

## Run the desktop build

Linux needs the WebKitGTK 4.1 and GTK 3 development packages (Fedora:
`webkit2gtk4.1-devel gtk3-devel libsoup3-devel`; Debian/Ubuntu: `libwebkit2gtk-4.1-dev
libgtk-3-dev libsoup-3.0-dev`). macOS needs only Xcode command line tools.

```sh
cargo install tauri-cli --version "^2" --locked   # once
cd apps/desktop
cargo tauri dev        # starts `trunk serve` and opens the window
cargo tauri build      # release bundles (deb, rpm, AppImage on Linux; dmg on macOS), see apps/desktop/packaging/README.md
```

Without the Tauri CLI, build the UI first and then run the shell with the embedded files:

```sh
(cd crates/teamcraft-ui && trunk build --release)
cargo run -p teamcraft-desktop --features custom-protocol
```

## Checks

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p teamcraft-ui
cargo check -p teamcraft-ui --target wasm32-unknown-unknown
```

The crate (and `apps/desktop`) set `rust-version = "1.88"`: current Leptos and Tauri need a newer
compiler than the workspace baseline.

## Known gaps

- Calls need the desktop shell: the web build offers no call buttons and meetings open their chat
  only.
- The core integration test (`cargo test -p teamcraft-desktop --features integration core::`) needs a
  fresh dev database and must not run in parallel with the sign-in tests, which end alice's session.
- No drafts per conversation, message editing, emoji picker, search results page or meetings.
- Meetings: no edit dialog, no per-occurrence handling of recurring meetings and the guest page stops at
  "admitted" (no media in the browser build yet).
- Calls: no reactions in calls, recording or captions; own backgrounds cannot be uploaded yet.
- Admin: assigning owners, archiving, creating users and inviting guests, organization recovery and the
  CSV audit export are placeholders; storage size is unknown to the server (shown as 0).
- Bookmarks bar is read-only; settings tabs for notifications and advanced are placeholders.
- The sidebar has no drag-and-drop ordering and no channel browser yet.
