# Changelog

All notable changes to TeamCraft are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). How a release is made:
[docs/release.md](docs/release.md).

## [Unreleased]

### Added

- **Device tokens bound to servers**: the apps seal their APNs/FCM token for the push gateway, bound to
  the one server that may push to it and an expiry, so a server (or a copy of its database) can't push
  to devices through another server's identity; the sealed token is renewed regularly and revoked when
  the user leaves the server. `GET /api/v1/push/gateway` tells the apps how to seal; registrations of
  raw tokens are refused where a gateway sends (`[push.gateway] accept_unbound_tokens` is a transition
  setting for old apps, off by default).
- **Push gateway support**: `[push.gateway]` sends the APNs and FCM wake-ups of the store apps through the
  publisher's push gateway (its own project, `teamcraft-push-gateway`), which holds the credentials that
  only the publisher has; every server, the publisher's own included, uses it the same way. The gateway
  sees device tokens and ciphertext only; the server authenticates with its own key pair. Servers with
  self-built apps keep `[push.apns]` / `[push.fcm]` ([docs/push-gateway.md](docs/push-gateway.md)).
- **Activity feed**: mentions, reactions to your messages and replies in your threads in one list with
  an unread state and a jump to the message. Built by the client from decrypted messages; the read
  mark follows you across devices.
- **Last seen** in the profile card and the DM header. Each person can hide theirs (`PUT /me/privacy`),
  appearing offline hides it too, and the admin can switch it off for the workspace
  (`presence.is_last_seen_enabled` in the signed policy).
- **Call history** list: missed, incoming and outgoing calls with a button to call back.
- **Photo backgrounds**: sixteen public domain and CC0 pictures (offices, living rooms, libraries,
  an empty room, a misty lake) next to blur and the drawn illustrations, with thumbnails in the
  picker.
  Licences and sources: `assets/backgrounds/SOURCES.md`.

### Changed

- The server, SFU and bridge images are built on Debian 13 (trixie) and run on distroless
  `cc-debian13`; dependencies and the dev stack (Keycloak 26.8, Garage 2.4.1, Mailpit 1.32) are
  current.

### Fixed

- `PUT /me/profile` accepted time zones that are not IANA names; it now checks the name.
- Database overload (no free connection, too many connections, deadlock) answers `503` instead of
  `500`; the dev Postgres and the e2e servers use connection limits that fit together.

## [1.0.0] - 2026-10-23

First release. TeamCraft is a self-hosted business team chat with calls and meetings in which the
server only ever sees ciphertext. From here on the wire format, the API and the database stay
compatible within 1.x; database migrations run by themselves and are forward-only.

### Added

- **Chat**: spaces, public and private channels with categories, DMs and group DMs, threads,
  reactions, mentions, user groups, read receipts, urgent DMs, presence
  (including "in a meeting" / "in a call"), unread counts, jump to date, fuzzy search, default
  memberships, channel conversion (public/private) and moderators.
- **End-to-end encryption**: MLS (openmls) for every conversation, device certificates and signed
  device lists, encrypted key bag with backup key and passphrase, device linking through the rendezvous
  relay, organization recovery with admin approvals, client-side search index segments.
- **Calls** through `teamcraft-sfu` (WebTransport): 1:1 and group calls with DAVE end-to-end media
  encryption, Opus audio with echo cancellation, H.264 video with simulcast, screen share, background
  effects with personal and organization backgrounds, push-to-talk, host controls.
- **Meetings**: scheduled and instant meetings with their own chat, iMIP invitations, CalDAV calendar,
  external guests with a lobby.
- **Login** through any OIDC provider (Keycloak is the reference setup): TeamCraft roles from client
  roles, deprovisioning sync, back-channel logout, MFA and guest rules.
- **Administration without content access**: signed instance policy edited in the admin console,
  admin API, audit log, retention per channel and space, guests with sponsor and expiry, rate limits,
  e-mail digests.
- **Brands**: one server under several host names, each with its own name, logo, colors, login text,
  links and e-mail sender (`[branding]`, `[[brands]]`, `GET /api/v1/brand`); see
  [docs/branding.md](docs/branding.md). **Organizations**: an optional label per user from a token
  claim (`[oidc] organization_claim`), shown as a badge and used as a directory filter.
- **Attachments** in S3-compatible storage (encrypted on the client) and share links for external
  download whose key never reaches the server.
- **Integration channels** and `teamcraft-bridge`: Slack/Mattermost-compatible webhooks and log
  shipping into encrypted channels, with a compact log view.
- **Push notifications**: encrypted wake-ups through Web Push. The server's APNs, FCM and UnifiedPush
  senders are not implemented yet (see the release notes' known limitations).
- **Clients**: one Leptos UI for the desktop app (Tauri: Linux deb/rpm/AppImage/Flatpak for x86_64 and
  aarch64, macOS universal dmg, Windows x64 and arm64 msi/exe) and the web client (served by the
  server); Android (`full` with FCM, `libre` without Google services) and iOS (SwiftUI, CallKit,
  notification service extension) on the shared Rust core.
- **Operations**: distroless OCI images for server, SFU and bridge (amd64 and arm64); health checks;
  dual-stack networking; the deployment playbook
  [Zeitgleich/teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy); an end-to-end and
  v1 acceptance suite.

### Security

- The server is untrusted by design ([ARCHITECTURE.md](ARCHITECTURE.md) §2): it stores ciphertext,
  signed policy and signed membership only. An external review's findings are fixed
  ([docs/security-review.md](docs/security-review.md)).
- The API client only talks HTTPS outside development; the service worker checks its origin.
- Releases ship SBOMs (SPDX and CycloneDX), SHA-256 checksums and build provenance attestations; the
  container images are signed with Sigstore (keyless). CI actions are pinned to commit SHAs.

[Unreleased]: https://github.com/Zeitgleich/TeamCraft/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/Zeitgleich/TeamCraft/releases/tag/v1.0.0
