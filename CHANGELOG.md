# Changelog

All notable changes to TeamCraft are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). How a release is made:
[docs/release.md](docs/release.md).

## [Unreleased]

## [1.0.0-beta.1] - 2026-10-22

First public beta. Everything below is new; the protocol, wire types and database schema may still
change before 1.0.0, with migrations.

### Added

- **Server** (`teamcraft-server`): REST API and WebSocket gateway with resumable sessions, spaces,
  public and private channels, categories, DMs and group DMs, threads, reactions, mentions, user groups,
  presence (including "in a meeting" / "in a call"), unread counts, read receipts and urgent DMs.
- **End-to-end encryption**: MLS (openmls) for every conversation, device certificates and signed
  device lists, encrypted key bag with backup key and passphrase, device linking through the rendezvous
  relay, organization recovery with admin approvals, client-side search index segments.
- **Login** through any OIDC provider (Keycloak is the reference setup); TeamCraft roles from client
  roles, deprovisioning sync and back-channel logout.
- **Administration without content access**: signed instance policy, admin API, audit log, retention
  per channel and space, guests with sponsor and expiry, rate limits.
- **Attachments** in S3-compatible storage (encrypted on the client) and share links for external
  download whose key never reaches the server.
- **Calls** through `teamcraft-sfu` (WebTransport): 1:1 and group calls with DAVE end-to-end media
  encryption, Opus audio with echo cancellation, H.264 video with simulcast, screen share, background
  effects, push-to-talk, host controls.
- **Meetings**: scheduled and instant meetings with their own chat, iMIP invitations, CalDAV calendar,
  external guests with a lobby.
- **Integration channels** and `teamcraft-bridge`: Slack/Mattermost-compatible webhooks and log
  shipping into encrypted channels, with a compact log view.
- **Push notifications**: encrypted Web Push wake-ups.
- **Clients**: Leptos UI shared by the desktop app (Tauri: Linux deb/rpm/AppImage/Flatpak for x86_64
  and aarch64, macOS dmg), the web client (served by the server) and an Android app on the shared Rust
  core (UniFFI), with `full` (FCM) and `libre` (no Google services) flavors.
- **Operations**: distroless OCI images for server, SFU and bridge; health checks; dual-stack
  networking; operations guide and a release pipeline that publishes images, packages, SBOMs and
  checksums for every tag.

[Unreleased]: https://github.com/Zeitgleich/TeamCraft/compare/v1.0.0-beta.1...HEAD
[1.0.0-beta.1]: https://github.com/Zeitgleich/TeamCraft/releases/tag/v1.0.0-beta.1
