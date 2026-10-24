# Changelog

All notable changes to TeamCraft are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). How a release is made:
[docs/release.md](docs/release.md).

## [Unreleased]

### Added

- **Crop dialog for profile and space pictures** (desktop and web): after picking or dropping a
  picture, drag it into place, zoom with the slider, the wheel or a pinch, rotate by 90°, and see it
  live at the sizes it is shown (round for avatars, rounded square for spaces). Arrow keys move,
  `+`/`−` zoom, `R` rotates. "Apply" uploads the cropped square; "Cancel" keeps the old picture
  (#323).
- **Lightbox controls on desktop and web**: the sender, relative time, conversation and file name at
  the top; rotate (`R`) and zoom (−, slider, +, fit) at the bottom left; download, the thread panel
  beside the picture (`T`), open in a new window and a "…" menu (view thread, jump to message `J`,
  add to Saved, copy link to message, share link, copy image `Ctrl/⌘+C`, forward, file details) at
  the bottom right; round previous/next buttons on hover, a thumbnail strip, and a backdrop blurred
  and tinted from the picture. Every control has a tooltip with its key and is reachable by
  keyboard.
- **Videos and animated images in the lightbox**: videos play in the viewer with the browser's
  controls; GIFs and APNGs animate and can be paused (`K`), and start paused when the system asks
  for reduced motion.
- **Inline pictures on desktop and web** (#316): images and GIFs show large in the conversation (up to
  360 px, in their aspect ratio, sharp on HiDPI), several as a grid; GIFs play inline and stay still
  under reduced motion. Download, share link, forward, save and "…" float over a picture on hover,
  focus or long-press instead of permanent buttons; other files are compact cards with a download
  button and "…". The lightbox opens at once with the thumbnail and a small spinner, and decrypted
  pictures stay cached for the session (bounded), so reopening is instant.
- **DM encryption setup status**: a DM whose device waits for the other side's encryption group says
  "Setting up encryption…", and after two minutes without it that the setup failed; "Retry" asks the
  other side's devices again (#278).
- **Channel preview**: "Preview" in "Browse channels" shows a public channel before joining it,
  with "Details" and "Join channel" at the bottom (its messages are end-to-end encrypted for the
  members, so they show once joined).

### Changed

- The in-memory viewer opens files up to 128 MiB on every platform (the desktop app had no limit);
  bigger files offer "Download" instead.
- **Smoother web and desktop app**: one set of motion tokens (durations, easings; all near-instant
  with the system's "reduce motion"). Channel switches cross-fade, dialogs, menus and the search
  scale in and leave faster than they came, toasts and banners slide, sidebar sections unfold, a
  new message rises in, the user's own message is shown at once and settles when the server has
  it, reactions and unread badges pop when they change, buttons give a little on press. Only
  transform and opacity move, nothing loops off screen.
- **Global destinations on the rail**: Home, Activity and Threads sit at the top of the rail with
  a label under each icon; the sidebar keeps calls, meetings, people and saved items.
- **Channel tabs**: Messages and Files in the conversation header; the files side panel is now
  the Files tab.
- **Activity**: tabs for all activity, DMs, mentions and threads, an "unread" toggle and an "all
  caught up" state.
- **Empty states and loading**: one empty state everywhere (icon, headline, one sentence, at most
  one action), placeholders instead of "nothing here" while user groups load, fixed sizes for
  thumbnails and link-preview images so the feed doesn't jump when they decode; one radius and type
  scale in the stylesheet.

### Fixed

- The web loading screen offers a reload after 30 seconds when no script runs at all (#136).
- **Uploads that keep failing** (#316): a retried upload goes into the attachment it already
  announced instead of announcing new ones until the server answers 409 Conflict; retries back off
  (2 s doubling to 5 min) and stop after 8 attempts with "Upload failed" and "Try again" on the
  file. The message waits for it.
- **No crash when a file is read after leaving the conversation** (#316): the composer no longer
  writes to its disposed list, which panicked and stopped the web app.
- **Faster desktop start on Linux**: the tray no longer waits for D-Bus before the window is created.
- **No failed requests before the first device**: a new `GET /users/{id}/enrollment` answers 200 with
  the identity key and device list a user has published (`{}` before enrollment), so first sign-in
  and pinning peers who haven't enrolled yet no longer log red 404s (#234). Newer apps still work
  against servers without the endpoint.
- **Share links of messages with several files**: "Create share link" shares the file it belongs to,
  and every file shows its own link, instead of all of them pointing to the message's first file.
- **Interrupted device revocations are finished by the user's other devices**: if the device that
  revoked one was lost before it removed it from every conversation, the user's other devices do it,
  once the signed device list names the device revoked; a revocation the server claims alone changes
  nothing (#232).

### Security

- **The first admin roster is confirmed before it is signed**: the first policy save no longer signs
  the identity provider's owners and admins as the server reports them; the owner checks each with
  user ID and key fingerprint (attested, trusted on first use, own key), or signs only themselves
  (#265).
- **Sessions from before the server entry are signed out**: the desktop revokes the old refresh token
  at its identity provider and deletes it, the old database keys and the old store; Android and iOS
  unregister the old device's push and revoke the old token once the user entered its server, then
  delete the old session, key and store (after 14 days without that server: deleted without it)
  (#223, #224).

## [1.0.1] - 2026-10-24

### Changed

- **One search for everything**: the search in the top bar (Ctrl/Cmd+K, Ctrl/Cmd+F,
  Ctrl/Cmd+Shift+F) finds people, threads, channels, spaces and messages, grouped in that order,
  and lists the conversations visited last before anything is typed; "all messages for …" opens
  the search page with its filters. The sidebar's "find or jump to" button, its "Search" entry and
  the DM filter field are gone; the home chips sit in one compact row.
- **Sign-in screens**: the server entry has its label above a full-width field, plain-language
  errors with the technical detail folded away, and a "check similar addresses" button for a host
  that doesn't resolve (nothing is looked up until it is pressed). Until a server is chosen the app
  shows the TeamCraft mark; the confirmation shows the server's logo, name and identity provider,
  buttons side by side. After signing in, the browser page is in the app's language and theme
  instead of a white page in two languages; a logo that carries the name is no longer followed by
  the name again.

### Added

- **Space picture and color**: space admins upload a picture (PNG, JPEG or WebP; cropped to a square
  in the app, re-encoded by the server) and pick a color in the space settings; the rail, the space
  header and the search show it, the letter tile stays the fallback. Like the space name it is
  directory data the server can read ([docs/e2ee.md](docs/e2ee.md) §4.4). New settings
  `max_space_image_bytes`, `limits.max_concurrent_image_decodes`, `limits.image_decode_timeout_secs`.
  Invitations from the space settings default to member instead of guest.
- **Image lightbox on desktop and web**: clicking an image opens it at full resolution, decrypted in
  memory on demand (thumbnail as placeholder, loading and error states with retry). Zoom and pan with
  the wheel, a trackpad or touch pinch, double-click or double-tap and `+` `-` `0`; previous/next
  with the arrow keys, buttons or a swipe. A message with several images shows an album grid and
  pages through its images, with a switch to all images of the conversation. Save, forward, sender
  and time; `Esc` or a click beside the image closes it. Only the image on screen is held at full
  size; its `blob:` URL is released when the viewer moves on or closes.

### Fixed

- **Find colleagues who haven't joined anything yet**: the people page, the search and "new
  message" ask the server's directory (debounced, one page at a time) instead of only knowing
  people who share a space or conversation; guests still only find whom they share a conversation
  with. People without a device show "hasn't set up the app yet".
- **DM with somebody who has no device**: the DM is made, a banner says the person hasn't set up
  the app yet, and they are added to the encryption group when their first device appears,
  without the DM repair retrying in the meantime. Somebody added to a channel whose devices ran
  out of key packages is retried with a growing delay, and one none of whose devices can be
  verified waits for a new device; the banner says which, instead of the person being silently
  left out.
- **Window controls look like the desktop's own**: on Linux the custom title bar follows the running
  desktop (`XDG_CURRENT_DESKTOP`): GNOME's round, lightly filled buttons, or on KDE Plasma Breeze's
  flat chevrons with a round highlight and a red close circle; other desktops get the GNOME look.
  Hover no longer shows a square behind the round buttons. On KDE the button order comes from
  KWin's own setting when it is readable. Windows gets 46×32 caption buttons in the corner with
  Fluent-style glyphs and the red close hover. Smooth hover transitions (off with reduced motion) and
  focus rings for the keyboard on every platform; macOS keeps its native traffic lights.
- **Release candidates upgrade through `dnf` and `apt`**: candidate packages were built with the final
  version (`1.0.0-1`), so a package manager never saw an update between candidates or from a candidate
  to the release. Candidates now carry `1.0.0~rc.N` (deb) and `1.0.0-0.rc.N` (rpm), which sort before
  the release. Machines that installed a candidate of 1.0.0 need
  `sudo dnf reinstall team-craft` or `sudo apt install --reinstall team-craft` once; after that
  updates arrive normally.
- **Desktop sign-in on a new device**: right after signing in, the app opened its encrypted store
  before the sign-in token had reached it and stayed on "Deine verschlüsselten Daten konnten nicht
  geöffnet werden" ("no access token"), with "Change server" blocked. The store now reads the
  live token, shows "Connecting …" while it waits, recovers by itself, and the error card offers
  Retry and Sign out.
- **Web app on large workspaces**: a change re-reads and re-renders only what changed instead of the
  whole workspace, and the channel list shows from the local store while a sync catches up instead
  of after it.
- **Server**: waits for Postgres at start (backoff, `[database] startup_wait_secs`, default 300)
  instead of exiting into a restart loop, answers 503 while the database is briefly away, and drops
  gateway clients that stop reading after 30 s instead of holding their buffers.
- **Direct messages whose setup was cut short** (a crash, a lost answer, or two devices founding the
  same DM at once) are repaired when the DM is opened or on the next sync. A device that lost the
  race joins the winning group, after checking its welcome, instead of writing into a group of one
  that nobody else could read ([docs/e2ee.md](docs/e2ee.md) §4.6).
- The tray menu shows "Open" and "Quit" in the system language from the start; on KDE it showed
  a bare "×".
- The brand in the top bar links to Home, and Administration has Back at the top; there was no way
  back from it.
- The shortcut hint in the top bar search no longer stretches across the field.

## [1.0.0] - 2026-10-24

First release. TeamCraft is a self-hosted business team chat with calls and meetings in which the
server only ever sees ciphertext. From here on the wire format, the API and the database stay
compatible within 1.x; database migrations run by themselves and are forward-only.

### Added

- **Owner warnings in the console**: Administration → Security lists the owners of the signed roster
  with the time each last signed, warns when there are fewer than the recommended number (a signed
  policy setting, default 2) and a week before admin authority would lapse for lack of a fresh
  owner signature.

- **Edit the signed roster**: owners add, promote, demote and remove owners and admins in
  Administration → Security and sign the change on their device; differences to the identity
  provider's roles are listed; every change is confirmed with user ID and key fingerprint before
  signing, and an edit of an outdated roster is refused. Admins can't sign (#261).
- **Addresses for every view**: the web and desktop apps put the open channel, DM, thread or page
  into the URL; reload keeps the place, back and forward (also the mouse buttons) work, and links
  to a channel or message can be shared. Only public IDs are in the path.
- **Android main screen**: the brand's logo, name and accent color in the top bar, the avatar for
  account and settings, one bottom navigation (Home, Activity, Spaces), Home with threads, saved,
  meetings and people above the direct messages, and one "New message" button.
- **Live counters over the gateway**: unread and mention counts and presence arrive as gateway
  events instead of a 30 s poll; after a reconnect the app reads them once.
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

### Fixed

- **The last owner can't be removed**: a roster without an owner is refused by the owner's app
  before signing, by the server, and by every client verifying the roster, so the workspace can't
  end up without anyone who can administer it (#188).
- **Removals that don't go through at once** (e2ee.md §4.6): a removal or leave on its way shows as
  pending in the conversation; one the server keeps refusing backs off up to a day and then shows
  as failed with "retry". Admins see people who are still in a channel's encrypted group although
  the server no longer lists them, and remove them with one click; an admin outside the group signs
  its removal again before members would refuse it as too old. The goodbye and the removal
  announcement are never sent twice after a crash, and a removal finished later still announces
  it. A server that refuses a member the encrypted group is shown instead of going unnoticed, and
  a device whose join from a welcome was cut off asks to be added back instead of staying out.
- Mention badges count mentions again; they were always 0.
- "Download" in the web app saves the decrypted file through the browser (files up to 256 MiB).
- Creating a space, channel or meeting twice (a retry after a lost answer, a double click) creates
  it once.

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

[Unreleased]: https://github.com/Zeitgleich/TeamCraft/compare/v1.0.1...HEAD
[1.0.1]: https://github.com/Zeitgleich/TeamCraft/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/Zeitgleich/TeamCraft/releases/tag/v1.0.0
