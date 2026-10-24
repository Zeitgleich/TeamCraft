# Changelog

All notable changes to TeamCraft are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). How a release is made:
[docs/release.md](docs/release.md).

## [Unreleased]

## [1.0.0] - 2026-10-24

First release. TeamCraft is a self-hosted business team chat with calls and meetings in which the
server only ever sees ciphertext. From here on the wire format, the API and the database stay
compatible within 1.x; database migrations run by themselves and are forward-only.

### Added

- **One Home and the brand everywhere**: Home holds the global entries (threads, activity, calls,
  meetings, people, saved, search) above the direct messages and is where a new workspace starts.
  The top bar, the sign-in card and the favicon show the brand's logo (light or dark) and name from
  the admin console.
- **Linux without a tray**: the desktop app starts without libayatana-appindicator and then runs
  without a tray icon; deb and rpm only recommend the library.
- **Server entry in the apps**: the desktop, Android and iOS apps no longer have a server built in.
  On first start they ask for the server address (a host name, URL or any link into the server),
  show the server's name and host for confirmation and sign in with the settings the server
  announces in the new public `GET /api/v1/server-info` (issuer, public client, scopes, optional
  redirect URIs, brand, versions). Clear messages for captive portals, foreign servers, TLS problems
  and version skew; "Change server" on the sign-in screen. Companies can preconfigure and lock the
  server (desktop `desktop.json`, Android managed configuration, iOS managed app configuration).
  Tokens and local data are kept per server origin; installs from release candidates sign in again.
  New settings `[oidc] app_scopes` and `app_redirect_uris`
  ([docs/operations.md](docs/operations.md) "Server address in the apps"). The web client now signs
  in with the server's announced settings too.
- **New sign-in page on every client**: a prominent "Sign in with <provider>" button and the e-mail,
  password and one-time code form of local accounts inline (no hidden guest link), with an "or"
  divider, password reveal, help text and link, in light and dark and on phones. Admins choose the
  methods and their order, the help text and link (Settings → Sign-in page), the provider name and
  icon per brand, and switch local accounts off and on at runtime (sign-in then answers 403
  `local_accounts_disabled` and their sessions end). Everything is announced in
  `GET /api/v1/server-info`. The desktop, Android and iOS apps now sign local accounts in too.
- **Durable push retries**: wake-ups that fail because a push service or the push gateway is down or
  rate limited wait in the database and are tried again with backoff and jitter, honouring
  `Retry-After`, until their TTL ends; a restart or a killed server loses none and sends none twice.
  New settings `push.retry_max_delay_secs`, `push.retry_workers`, `push.retry_max_queued_per_device`,
  `push.retry_poll_interval_ms`; `push.max_retries` now defaults to 200 (the TTL usually ends retries
  first).
- **Android (UnifiedPush)**: the app registers with the distributor again by itself after a failed
  registration, a temporarily unavailable or uninstalled distributor.
- **Runtime settings in the admin console**: brands (create, edit, delete, take over configured ones;
  logos, icons and favicons with dark variants in the object storage; colors with preview and
  contrast check; login texts in English and German; links; mail sender; host names limited to the
  served ones), the mail server (password write-only and encrypted with the new `[secrets]` settings
  key, delivery status, test mail), the meeting calendar (CalDAV, connection test) and instance
  settings (server rules, notification defaults, mention digest, share links, limits, integration
  channels, local accounts, mail and meeting texts). The configuration file stays the seed; every
  change is audit-logged and can be reset to the file's value
  ([docs/admin.md](docs/admin.md) §6).
- **Calls survive network changes**: a phone moving from Wi-Fi to LTE or a laptop changing networks
  stays in the call. Native apps keep their QUIC connection (connection migration); when that isn't
  possible, or in the browser, the app rejoins with a fresh token while the SFU keeps its place for
  30 s (`TEAMCRAFT_SFU_RECONNECT_GRACE_SECS`) and the others see "reconnecting". Android and iOS
  report network changes to the call, desktops notice a new route themselves. On bad links video
  layers give way to audio, Opus forward error correction follows the measured loss, and the jitter
  buffer drops a handover backlog instead of keeping its delay ([ARCHITECTURE.md](ARCHITECTURE.md)
  §6.5). Calls still need UDP; networks that block it are covered in §6.6.
- **Desktop updates**: Windows, macOS and AppImage installs check for updates in the background,
  download and verify them (minisign, bound to the version) and install them on restart or quit,
  never during a call; Settings → Advanced → Updates offers automatic, notify only or off. deb and rpm installs
  update from signed APT and RPM repositories on the project's Pages site, and the app shows the
  command or asks PackageKit. Owners control updates in the signed policy (`client_updates`:
  allowed/disabled, channel, pinned and minimum version, mirror), and the server announces the oldest
  client it supports (`GET /api/v1/client-compatibility`); older apps ask for an update
  ([docs/updates.md](docs/updates.md)).
- **macOS**: a dmg and an update per architecture, Apple silicon (`TeamCraft_<version>_aarch64.dmg`) and
  Intel (`TeamCraft_<version>_x64.dmg`): smaller downloads and
  updates than one universal dmg; universal installs from release candidates update to the build of
  their own Mac. The download page offers
  both, Apple silicon first.
- All of the server's mail, meeting invitations included, goes through one mail server;
  `[meetings.smtp]` only counts when `[mail.smtp]` is missing.
- **Chat**: spaces, public and private channels with categories, DMs and group DMs, threads,
  reactions, mentions, user groups, pins and saved items, read receipts, urgent DMs, typing indicators,
  unread counts with an unread divider, jump to date, search with jumps to the message, an emoji
  picker, a fuzzy conversation switcher and keyboard shortcuts, default memberships, channel conversion
  (public/private), moderators, undo after leaving a channel.
- **Spaces**: move channels between spaces, mute a whole space, search within a space, an all-spaces
  sidebar, retention set by the space owner.
- **Activity and presence**: an activity feed (mentions, reactions, thread replies) with a read mark
  that follows you across devices; presence including "in a meeting" / "in a call"; last seen, which
  each person can hide and admins can switch off (`presence.is_last_seen_enabled`); call history with
  call back.
- **End-to-end encryption**: MLS (openmls) for every conversation, device certificates and signed
  device lists, encrypted key bag with backup key and passphrase, device linking through the rendezvous
  relay, organization recovery with admin approvals, client-side search index segments.
- **Calls** through `teamcraft-sfu` (WebTransport): 1:1 and group calls with DAVE end-to-end media
  encryption, Opus audio with echo cancellation, H.264 video with simulcast, screen share with the
  computer's sound (PipeWire, WASAPI loopback, ScreenCaptureKit; opt-in), push-to-talk, host controls.
  Background effects with blur, drawn illustrations, sixteen public-domain and CC0 photos (offices,
  living rooms, libraries; sources in `assets/backgrounds/SOURCES.md`, licences in `NOTICE`), personal
  and organization backgrounds; on macOS a pointer to the system's own camera effects.
- **Meetings**: scheduled and instant meetings with their own chat, iMIP invitations, CalDAV calendar,
  external guests with a lobby.
- **Notifications**:
  - Push through Web Push, APNs, FCM and UnifiedPush; payloads are sealed for the device.
  - The official store apps are woken through the publisher's push gateway
    ([Zeitgleich/teamcraft-push-gateway](https://github.com/Zeitgleich/teamcraft-push-gateway), default
    `https://push-gw.zeitgleich.de`, `[push.gateway]`), which holds the APNs and FCM credentials only the
    publisher has and sees device tokens and ciphertext only. Devices seal their token for the gateway,
    bound to the one server that may push to it and an expiry. Servers with self-built apps use their
    own `[push.apns]` / `[push.fcm]` instead ([docs/push-gateway.md](docs/push-gateway.md)).
  - Phones wait while a desktop of the same person is actively in use (attention lease;
    `notifications.is_desktop_attention_enabled` switches it off), and notifications read on another
    device are withdrawn.
  - Calls ring every device without a live connection.
  - Do not disturb with weekly schedules in the person's time zone, and overrides for allowed people and
    urgent messages.
  - A test push and delivery diagnostics for people (`POST /devices/{id}/push/test`) and admins
    (`GET /admin/users/{id}/push-health`, counts only).
  - On Android: notification channels, conversation notifications and full-screen incoming calls. On
    iOS: communication notifications and incoming-call alerts. On the desktop: a tray badge.
- **Login** through any OIDC provider (Keycloak is the reference setup): TeamCraft roles from client
  roles, deprovisioning sync, back-channel logout, MFA and guest rules.
- **Administration without content access**: signed instance policy edited in the admin console,
  admin API, audit log, retention per channel and space, guests with sponsor and expiry, rate limits,
  e-mail digests.
- **Brands**: one server under several host names, each with its own name, logo, colors, login text,
  links and e-mail sender (`[branding]`, `[[brands]]`, `GET /api/v1/brand`); see
  [docs/branding.md](docs/branding.md). **Organizations**: an optional label per user from a token claim
  (`[oidc] organization_claim`), shown as a badge and used as a directory filter.
- **Attachments** in S3-compatible storage (encrypted on the client) and share links for external
  download whose key never reaches the server.
- **Integration channels** and `teamcraft-bridge`: Slack/Mattermost-compatible webhooks and log
  shipping into encrypted channels, with a compact log view.
- **Clients**, all on one shared Rust core:
  - Desktop (Tauri) for Linux (deb, rpm, AppImage, Flatpak; x86_64 and aarch64), macOS 14 or newer
    (one universal dmg) and Windows 10 (1809) or newer (x64 and arm64, msi and exe). Tray badge,
    window state, start minimised, zoom.
  - The web client, served by the server, with the same UI.
  - Android 8.0 (API 26) or newer, built for Android 17 (API 37): `full` with Firebase, `libre` without
    Google services. Two-pane layout on large screens, sharing into the app, multi-select, image grid,
    app lock.
  - iOS 17 or newer: SwiftUI app with CallKit, a notification service extension, sharing and app lock.
- **Operations**: distroless OCI images for server, SFU and bridge (amd64 and arm64) built on Debian 13
  (trixie); health checks; dual-stack networking; the deployment playbook
  [Zeitgleich/teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy); an end-to-end and v1
  acceptance suite. Building needs Rust 1.98 or newer.

### Security

- Removing or leaving a member is crash- and retry-safe: the member's devices are committed out of
  the MLS group before the server drops the membership, a leave from a device outside the group
  still removes the user's other devices, and an interrupted removal or device revocation is
  finished on the next attempt ([docs/e2ee.md](docs/e2ee.md) §4.6). Before, a lost response between
  the two steps could leave a removed member able to read new messages.
- The apps check the server's identity-provider discovery strictly (exact issuer, HTTPS endpoints),
  show the provider's host on the confirm screen and warn when it is outside the server's
  registrable domain (Public Suffix List).
- The server is untrusted by design ([ARCHITECTURE.md](ARCHITECTURE.md) §2): it stores ciphertext,
  signed policy and signed membership only. The findings of the security reviews are fixed
  ([docs/security-review.md](docs/security-review.md)).
- Push: device tokens are bound to a server; the gateway sees tokens and ciphertext only; push senders
  are isolated per user, use no proxy, put no secrets in errors and refuse private address ranges.
- The API client only talks HTTPS outside development; the service worker checks its origin; mobile
  notifications hide their text while the app lock is on.
- Releases ship SBOMs (SPDX and CycloneDX), SHA-256 checksums and build provenance attestations; the
  container images are signed with Sigstore (keyless). CI actions are pinned to commit SHAs.

[Unreleased]: https://github.com/Zeitgleich/TeamCraft/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/Zeitgleich/TeamCraft/releases/tag/v1.0.0
