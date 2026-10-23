# Changelog

All notable changes to TeamCraft are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). How a release is made:
[docs/release.md](docs/release.md).

## [Unreleased]

### Added

- **Runtime settings in the admin console**: brands (create, edit, delete, take over configured ones;
  logos, icons and favicons with dark variants in the object storage; colors with preview and
  contrast check; login texts in English and German; links; mail sender; host names limited to the
  served ones), the mail server (password write-only and encrypted with the new `[secrets]` settings
  key, delivery status, test mail), the meeting calendar (CalDAV, connection test) and instance
  settings (server rules, notification defaults, mention digest, share links, limits, integration
  channels, local accounts, mail and meeting texts). The configuration file stays the seed; every
  change is audit-logged and can be reset to the file's value
  ([docs/admin.md](docs/admin.md) §6).

- **Desktop updates**: Windows, macOS and AppImage installs check for updates in the background,
  download and verify them (minisign, bound to the version) and install them on restart or quit,
  never during a call; Settings → Advanced → Updates offers automatic, notify only or off. deb and rpm installs
  update from signed APT and RPM repositories on the project's Pages site, and the app shows the
  command or asks PackageKit. Owners control updates in the signed policy (`client_updates`:
  allowed/disabled, channel, pinned and minimum version, mirror), and the server announces the oldest
  client it supports (`GET /api/v1/client-compatibility`); older apps ask for an update
  ([docs/updates.md](docs/updates.md)).

### Changed

- All of the server's mail, meeting invitations included, goes through one mail server;
  `[meetings.smtp]` only counts when `[mail.smtp]` is missing.

## [1.0.0] - 2026-10-23

First release. TeamCraft is a self-hosted business team chat with calls and meetings in which the
server only ever sees ciphertext. From here on the wire format, the API and the database stay
compatible within 1.x; database migrations run by themselves and are forward-only.

### Added

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
