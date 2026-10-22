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

Demo triggers: Settings > Advanced simulates an incoming call, a new login, the QR linking screen and
a chat with a changed security key, standing in for events the server will send later.

Shortcuts: `Ctrl/Cmd+K` switcher, `Ctrl/Cmd+,` settings, `Ctrl/Cmd+/` shortcuts, `Alt+Up/Down`
previous/next conversation, `Alt+T` threads, `Esc` close dialog or side panel.

## Layout of the crate

| Path                | Content                                                                     |
|---------------------|-----------------------------------------------------------------------------|
| `src/backend.rs`    | `ChatBackend`, `CallBackend`, `AdminBackend`, `SecurityBackend` (= `Backend`) |
| `src/mock.rs`       | `MockBackend`, demo data, lazily generated log channels                     |
| `src/model.rs`      | View models; IDs are re-exported from `teamcraft-proto`                     |
| `src/calls.rs`      | Call view models and `CallState` (session state of the user's call)         |
| `src/admin.rs`      | Admin view models (channels, users, policy, audit)                          |
| `src/security.rs`   | Device approval and key-change models                                       |
| `src/state.rs`      | `AppState` (context): current space, view, panel, overlay, backend access   |
| `src/settings.rs`   | Per-device preferences (theme, language, send mode) in `localStorage`       |
| `src/i18n.rs`       | Fluent bundles, `I18n::t`, `t_with`, `t_count`                              |
| `src/markdown.rs`   | Safe Markdown subset producing a tree (no HTML injection)                   |
| `src/components/`   | One module per screen area                                                  |
| `style/main.css`    | Design tokens, layout, components                                           |

To connect the real client core, implement `ChatBackend` over `teamcraft-core` and pass it to
`AppState::new` in `components/app.rs`. The trait is synchronous because it models the local,
decrypted store; network and decryption update that store and the UI re-reads after
`AppState::write`.

## Run the web build

Prerequisites: the `wasm32-unknown-unknown` target (`rust-toolchain.toml` lists it when you use
rustup) and [Trunk](https://trunkrs.dev) (`cargo install trunk --locked`, or a release binary).

```sh
cd crates/teamcraft-ui
trunk serve            # http://127.0.0.1:1420, rebuilds on change
trunk build --release  # static files in crates/teamcraft-ui/dist
```

Release builds are smaller with `CARGO_PROFILE_RELEASE_OPT_LEVEL=s CARGO_PROFILE_RELEASE_LTO=true`
and `data-wasm-opt="s"` on the Rust link in `index.html` (Trunk downloads binaryen for that).
Debug builds are slow in the browser; judge performance with `--release`.

## Run the desktop build

Linux needs the WebKitGTK 4.1 and GTK 3 development packages (Fedora:
`webkit2gtk4.1-devel gtk3-devel libsoup3-devel`; Debian/Ubuntu: `libwebkit2gtk-4.1-dev
libgtk-3-dev libsoup-3.0-dev`). macOS needs only Xcode command line tools.

```sh
cargo install tauri-cli --version "^2" --locked   # once
cd apps/desktop
cargo tauri dev        # starts `trunk serve` and opens the window
cargo tauri build      # release bundles (deb, rpm, AppImage on Linux; dmg on macOS)
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

- Mock data only; sending, reactions, favorites and presence live in memory and reset on reload.
- No drafts per conversation, message editing, emoji picker, search results page or meetings.
- Calls are mocked: no real media, reactions in calls, recording or captions; own backgrounds cannot be
  uploaded yet.
- Admin: assigning owners, archiving, creating users and inviting guests, and the CSV audit export are
  placeholders; policy and retention edits only change the mock.
- Bookmarks bar is read-only; settings tabs for notifications and advanced are placeholders.
- The sidebar has no drag-and-drop ordering and no channel browser yet.
