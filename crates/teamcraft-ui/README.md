# teamcraft-ui

Leptos (CSR) UI shared by the desktop app (`apps/desktop`, Tauri 2) and the web client. It currently
runs on a **mock backend** with deterministic demo data, so the shell can be built and reviewed
without a server.

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
| Key-change banner               | ![Key change](docs/screenshots/key-change-banner.png)          |
| Compare keys                    | ![Compare keys](docs/screenshots/key-verify.png)               |
| New login approval              | ![Device approval](docs/screenshots/device-approval.png)       |
| QR device linking               | ![QR linking](docs/screenshots/device-link-qr.png)             |

Screenshots are produced with Playwright against `trunk build --release` (English, 1360×840); the
tour also walks every screen in German and fails on missing translation keys.

The 50,000-line `#app-logs` channel (see `docs/screenshots/log-channel-dark.png`) keeps scrolling at
frame rate because only the visible rows are in the DOM.

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
  `src/search.rs` behind `SearchBackend`; the mock answers today, `teamcraft-core` will answer from the local
  index. `Ctrl+Enter` in the quick switcher searches for its text.
- **Settings** (PLAN.md §4, §5): notifications (levels, push timing, keywords, highlight-only keywords, thread
  replies, DND schedule, per-conversation level and mute), display (density, status in messages), sidebar
  (sort, DM limit), language, privacy, devices and security (sign out devices, identity fingerprint, safety
  numbers), calls (default devices, join behaviour, audio processing, background). Values are one JSON
  document (`src/prefs.rs`); notification and privacy values are saved but enforced by the notification engine
  later, call and sidebar values already apply.

Demo triggers: Settings > Advanced simulates an incoming call, a new login, the QR linking screen and
a chat with a changed security key, standing in for events the server will send later.

Shortcuts: `Ctrl/Cmd+K` switcher, `Ctrl/Cmd+,` settings, `Ctrl/Cmd+/` shortcuts, `Alt+Up/Down`
previous/next conversation, `Alt+T` threads, `Esc` close dialog or side panel.

## Layout of the crate

| Path                    | Content                                                                 |
|-------------------------|-------------------------------------------------------------------------|
| `src/mock.rs`           | `MockBackend`, demo data, lazily generated log channels                 |
| `src/remote_backend.rs` | `RemoteChat`: `ChatBackend` over the desktop shell's client core (IPC)  |
| `src/hybrid.rs`         | `Hybrid`: a real chat backend plus mock search, calls, admin, security  |
| `src/server_backend.rs` | `RestChat`: web build, read-only chat over the REST API                 |
| `src/calls.rs`          | `CallState` (session state of the user's call)                          |
| `src/state.rs`          | `AppState` (context): current space, view, panel, overlay, backend      |
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
3. builds `Hybrid<RemoteChat>`: chat comes from the core, search, calls, admin and security stay mock.

`ChatBackend` is synchronous but IPC is not, so `RemoteChat` keeps a mirror (snapshot plus pages of
100 messages). A read miss returns an empty window, starts a load and re-renders when the page
arrives. The shell emits `core-changed` after every sync step; the mirror refreshes then. Writes
(`core_action`) go through the core, so they are encrypted, queued and sent by the sync loop.
Settings > Data source still switches to the mock backend for demos.

Local data lives in the app data dir (`<data>/core/<user>.db`); `TEAMCRAFT_DATA_DIR` overrides it.

### Try it without the desktop window

An ignored test serves the shell's commands over loopback HTTP, with a real core for alice and a
second core (carol) that chats with her; `apps/desktop/dev/bridge-shim.js` implements
`window.__TAURI__` on top of it, so the web build in any browser talks to a real core:

```sh
docker compose -f docker-compose.dev.yml up -d              # dev stack; run teamcraft-server on :8081
(cd crates/teamcraft-ui && trunk serve --port 8082)         # proxies /api to the server
cargo test -p teamcraft-desktop --features integration -- --ignored serve_dev_bridge --nocapture
# open http://localhost:8082/?backend=server with the shim injected (e.g. a Playwright init script)
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

- Demo data is the default. The desktop build shows real encrypted chat (messages, send, reactions,
  channels, DMs); the web build only lists conversations read-only. Search, calls, admin and
  security are still mock on top of the core.
- The core integration test (`cargo test -p teamcraft-desktop --features integration core::`) needs a
  fresh dev database and must not run in parallel with the sign-in tests, which end alice's session.
- No drafts per conversation, message editing, emoji picker, search results page or meetings.
- Calls are mocked: no real media, reactions in calls, recording or captions; own backgrounds cannot be
  uploaded yet.
- Admin: assigning owners, archiving, creating users and inviting guests, and the CSV audit export are
  placeholders; policy and retention edits only change the mock.
- Bookmarks bar is read-only; settings tabs for notifications and advanced are placeholders.
- The sidebar has no drag-and-drop ordering and no channel browser yet.
