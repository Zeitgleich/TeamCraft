# TeamCraft – Feature plan and roadmap

Scope: business team chat in the Slack / Mattermost / Microsoft Teams mould, for one company plus
external guests, with calls and meetings. No fixed size limit (scale targets: ARCHITECTURE.md §1.1).
Mattermost's feature set is the reference for chat and admin features. Not a community or gamer chat:
Fluxer and Discord are only references for the voice transport and DAVE.

Everything is end-to-end encrypted, invisibly for users ([docs/e2ee.md](docs/e2ee.md)).
Architecture: [ARCHITECTURE.md](ARCHITECTURE.md).

Legend: **v1** = needed before we switch the company over, **v1.x** = soon after, **later** = nice to have.
"Server sees" lists metadata the server can see; everything else is encrypted.

---

## 1. Workspace, spaces and channels

Structure: **workspace** (the company instance) → **spaces** (projects, e.g. "Project A", "Project B",
"Internal", "Infrastructure") → **channels**. DMs and group DMs live outside spaces. Like teams in Microsoft
Teams or Mattermost, inside one company instance with one login.

The UI label is configurable per instance (e.g. "Project", "Space", "Team", translated via i18n);
internally it's `space`.

| Feature                                                                                                                                                                     | Prio  | Notes                                                               |
|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|---------------------------------------------------------------------|
| One workspace per instance                                                                                                                                                  | v1    | Multi-workspace not planned                                         |
| **Brands**: one server under several host names, each with its own name, logo, colors, login text, links and e-mail sender                                                  | v1    | docs/branding.md; presentation only, same users and spaces          |
| Brand chosen by the request host (allow-list, unknown host → default or 421), `GET /api/v1/brand`, per-brand CSP                                                            | v1    | Nothing but configured host names selects a brand                   |
| Clients apply the brand before login (web) or from the entered server URL (desktop, mobile), cached                                                                         | v1    |                                                                     |
| Mail, iMIP invitations and notification texts use the recipient's home brand; share links bound to the creating brand                                                       | v1    | Home brand = e-mail domain, else default                            |
| Optional brand tag on a space (default accent theming)                                                                                                                      | v1.x  | Hint only, no access effect                                         |
| **Spaces**: name, icon, color, description                                                                                                                                  | v1    | Encrypted name/description optional; see notes below                |
| Space switcher (Mattermost/Teams-style team sidebar) with unread badges per space; optional "all spaces in one sidebar" view (Slack-style, channels grouped by space)       | v1    |                                                                     |
| Space membership: open (everyone can join) or invite-only                                                                                                                   | v1    |                                                                     |
| Space roles: space admin, member, guest; permissions inherited by channels                                                                                                  | v1    |                                                                     |
| Public channels (anyone in the space can join)                                                                                                                              | v1    |                                                                     |
| Private channels (invite only)                                                                                                                                              | v1    |                                                                     |
| Channel browser + join per space                                                                                                                                            | v1    | Members only; guests don't see it                                   |
| Channel categories inside a space (admin-defined)                                                                                                                           | v1    |                                                                     |
| Personal sidebar sections across spaces ("Today", "Customers")                                                                                                              | v1.x  | Per-user, synced encrypted                                          |
| Channel topic, description, icon                                                                                                                                            | v1    | Encrypted                                                           |
| Archive / unarchive channel and space                                                                                                                                       | v1    |                                                                     |
| Favorites / starred conversations                                                                                                                                           | v1    |                                                                     |
| Default spaces/channels for new members                                                                                                                                     | v1    | e.g. "Company" space with #general                                  |
| Announcement channels (only some roles may post)                                                                                                                            | v1    | Permission `create_post`                                            |
| Guests invited to a space (all its public channels) or to single channels                                                                                                   | v1    |                                                                     |
| Shared channels with guests (badge "contains external people")                                                                                                              | v1    |                                                                     |
| Move channel between spaces                                                                                                                                                 | v1    |                                                                     |
| Convert group DM to private channel                                                                                                                                         | v1.x  |                                                                     |
| Space-wide search scope, mute whole space                                                                                                                                   | v1    |                                                                     |
| Space templates (default channels for a new project)                                                                                                                        | later |                                                                     |
| Channel bookmarks bar (links/files pinned to the header, emoji, order)                                                                                                      | v1    | Encrypted                                                           |
| Channel banner (colored notice strip set by moderators)                                                                                                                     | v1.x  |                                                                     |
| Convert public ↔ private channel                                                                                                                                            | v1    | Permission-gated                                                    |
| **Retention per channel**: keep last N days and/or last N messages (whichever is stricter); attachments and index segments removed with them                                | v1    | Server deletes ciphertext; set by channel owner within admin limits |
| Retention exceptions: keep pinned messages, keep threads with recent replies                                                                                                | v1.x  |                                                                     |
| **Integration channels** for logs/bots/webhooks, E2EE via `teamcraft-bridge` next to the source (Slack/Mattermost-compatible webhooks, log batching, builds index segments) | v1    | ARCHITECTURE.md §4.5                                                |
| Log view: compact monospace, collapsible batches, level colours, follow mode                                                                                                | v1    |                                                                     |
| Optional server-readable integration channels (no bridge)                                                                                                                   | v1.x  | Off by default, admins only, clearly marked                         |
| Huge channels: paged history, no full prefetch on phones, "jump to date"                                                                                                    | v1    |                                                                     |
| "Share history" toggle when adding members (default on, configurable in the admin panel; guests always from join)                                                           | v1    |                                                                     |
| Discoverable private channels with join requests (approve/deny queue)                                                                                                       | v1.x  |                                                                     |
| Sidebar: sort by recency/manual/alpha per category, "Unreads" group, limit visible DMs                                                                                      | v1    |                                                                     |
| System messages: rename, topic/purpose change, privacy change, moved, member joined/left (join/leave hideable per user)                                                     | v1    |                                                                     |
| Limits: max channels per space, max members per space                                                                                                                       | v1.x  | Admin setting                                                       |

Server sees: which spaces and channels exist, who is in them. Names: plaintext by default so the server
can serve the space/channel list to guests and new members quickly; option to encrypt names per space.

## 2. Messaging

| Feature                                                                                                 | Prio  | Notes                                                                                                                                                         |
|---------------------------------------------------------------------------------------------------------|-------|---------------------------------------------------------------------------------------------------------------------------------------------------------------|
| DMs and group DMs                                                                                       | v1    |                                                                                                                                                               |
| Markdown + rich-text composer, code blocks with syntax highlighting                                     | v1    |                                                                                                                                                               |
| Mentions: `@user`, `@channel`, `@here`, `@usergroup`                                                    | v1    | Server sees mentioned user IDs (for push)                                                                                                                     |
| Edit (with "edited" marker, edit history)                                                               | v1    |                                                                                                                                                               |
| Delete (for everyone)                                                                                   | v1    | Server deletes ciphertext                                                                                                                                     |
| Reactions (emoji)                                                                                       | v1    |                                                                                                                                                               |
| Replies / quotes                                                                                        | v1    |                                                                                                                                                               |
| Forward message to other conversation                                                                   | v1    | Re-encrypted by the client                                                                                                                                    |
| Pinned messages per conversation                                                                        | v1    |                                                                                                                                                               |
| Saved items / bookmarks (personal)                                                                      | v1    |                                                                                                                                                               |
| Message permalinks                                                                                      | v1    | Only resolve for people with access                                                                                                                           |
| Drafts synced across devices                                                                            | v1    | Encrypted                                                                                                                                                     |
| Typing indicators                                                                                       | v1    | Server sees "user X typing in Y"                                                                                                                              |
| Read receipts in DMs/group DMs                                                                          | v1    | Per-user setting; server sees read position                                                                                                                   |
| Unread markers, "jump to first unread", mark as unread                                                  | v1    |                                                                                                                                                               |
| Link previews                                                                                           | v1    | Generated by the sender's client (works for login-protected pages); domain allow/block list from the signed policy, enforced by sending and receiving clients |
| Custom workspace emoji                                                                                  | v1.x  | Workspace assets, not secret                                                                                                                                  |
| Scheduled send                                                                                          | v1.x  | Ciphertext held by server, released at time T                                                                                                                 |
| Reminders ("remind me about this in 2h")                                                                | v1.x  | Client-side or encrypted server timer                                                                                                                         |
| Polls                                                                                                   | v1.x  |                                                                                                                                                               |
| Voice messages                                                                                          | v1.x  | Opus, encrypted attachment                                                                                                                                    |
| Message formatting toolbar                                                                              | v1.x  |                                                                                                                                                               |
| Disappearing messages per conversation                                                                  | later |                                                                                                                                                               |
| Message translation                                                                                     | later | Client-side                                                                                                                                                   |
| **Message priority**: Important / Urgent badge                                                          | v1    | Flag inside ciphertext + server-visible "urgent" bit for push routing                                                                                         |
| **Request acknowledgement**: recipients click "Acknowledge", sender sees who did                        | v1    |                                                                                                                                                               |
| **Persistent notifications** for urgent DMs: re-push every N min until read (max count, max recipients) | v1.x  | Admin-configurable, can be disabled for guests                                                                                                                |
| Recurring scheduled messages + list of scheduled messages (edit/cancel)                                 | v1.x  |                                                                                                                                                               |
| Burn-on-read (message disappears N s after reading)                                                     | later |                                                                                                                                                               |
| Permalink previews (linked message embedded inline)                                                     | v1    | Rendered client-side                                                                                                                                          |
| `/me`, `/shrug`, silent message (no notification)                                                       | v1    |                                                                                                                                                               |
| Copy text, mark unread from here                                                                        | v1    |                                                                                                                                                               |
| Edit time limit, max distinct reactions per message                                                     | v1.x  | Admin setting                                                                                                                                                 |
| LaTeX (block + inline), optional SVG rendering                                                          | v1.x  | Admin toggles                                                                                                                                                 |
| Composer: WYSIWYG toggle, Ctrl+Enter to send, emoticons → emoji                                         | v1    | User settings                                                                                                                                                 |

## 3. Threads

Threads are first-class (Slack model), not just quotes.

| Feature                                                     | Prio  | Notes                                                              |
|-------------------------------------------------------------|-------|--------------------------------------------------------------------|
| Reply in thread from any channel/group message              | v1    | Server sees `thread_id` (needed for unread counts + notifications) |
| Thread side panel                                           | v1    |                                                                    |
| "Also send to channel"                                      | v1    |                                                                    |
| Thread participants auto-follow                             | v1    |                                                                    |
| Follow / unfollow a thread manually                         | v1    |                                                                    |
| **Threads inbox**: all followed threads with unread replies | v1    |                                                                    |
| Thread unread counts, mentions in threads                   | v1    |                                                                    |
| Resolve / close thread                                      | v1.x  |                                                                    |
| Thread summary (client-side LLM)                            | later |                                                                    |
| Move thread to another channel                              | v1.x  | Client re-encrypts into the target group; permission-gated         |
| Auto-follow threads where I'm mentioned via @channel        | v1    | User setting                                                       |
| Admin options: threaded view on/off, auto-follow default    | v1    |                                                                    |

## 4. Presence and status

| Feature                                                                        | Prio  | Notes                                                       |
|--------------------------------------------------------------------------------|-------|-------------------------------------------------------------|
| Automatic **online** / **away** (idle after 10 min, configurable)              | v1    | Aggregated over all devices: online if any device is active |
| **Do not disturb** (manual, for a duration, or scheduled e.g. 18:00–08:00)     | v1    | Suppresses push; mentions still collected                   |
| **Appear offline** (invisible)                                                 | v1    |                                                             |
| **In a call** (automatic)                                                      | v1    |                                                             |
| **In a meeting** (automatic from CalCraft free/busy)                           | v1    | Needs CalCraft integration                                  |
| **Out of office** with until-date and optional auto-reply in DMs               | v1.x  | Synced from CalCraft/Keycloak or set manually               |
| Custom status: emoji + text + expiry ("🌴 Vacation until Mon")                 | v1    | Server-visible (low sensitivity), could be encrypted later  |
| Last seen                                                                      | v1    | Respect "appear offline"                                    |
| Working hours + time zone, local time shown in profile and DM header           | v1    | "It's 22:14 for Anna"                                       |
| Notification "Anna is online again" for a contact                              | later |                                                             |
| Standalone auto-responder in DMs (independent of out-of-office)                | v1.x  |                                                             |
| Status shown next to messages (user setting)                                   | v1    |                                                             |
| Recent custom statuses and admin presets ("In a meeting", "Commuting", "Sick") | v1    |                                                             |
| Admin: enable/disable statuses and custom statuses, away timeout               | v1    |                                                             |

Server sees: presence state, custom status. Presence is metadata the server needs anyway to route
pushes.

## 5. Notifications

| Feature                                                               | Prio  | Notes                                      |
|-----------------------------------------------------------------------|-------|--------------------------------------------|
| E2EE push with preview on all platforms                               | v1    | See e2ee.md §7                             |
| Per-conversation level: all / mentions & DMs / nothing                | v1    |                                            |
| Mute conversation (until time)                                        | v1    |                                            |
| Keyword notifications                                                 | v1    | Matched client-side after decryption       |
| Thread notifications (followed threads)                               | v1    |                                            |
| Don't push to mobile while desktop is active (with delay, e.g. 2 min) | v1    |                                            |
| Push gateway so store apps work with any self-hosted server           | v1    | Sees only sealed wake-ups; e2ee.md §7      |
| DND schedule, DND override for specific people (e.g. manager)         | v1.x  |                                            |
| Notification sound per conversation                                   | v1.x  |                                            |
| Email digest of missed mentions                                       | later | Can only say "3 new mentions", not content |
| Activity feed (mentions, reactions to my messages, thread replies)    | v1    | Built client-side from decrypted data      |
| Push depends on status (push only when away/offline, or always)       | v1    |                                            |
| First name counts as mention; ignore @channel/@here per conversation  | v1    |                                            |
| Reply notifications: threads I started / any thread I replied to      | v1    |                                            |
| Separate settings for desktop / push / email, and for threads         | v1    |                                            |
| Highlight-only keywords (highlight without notifying)                 | v1.x  | Client-side                                |
| Confirm before @channel in conversations with more than N members     | v1    |                                            |
| Admin: push content mode generic / sender only / full preview         | v1    | Generic = no preview even after decryption |
| Email batching interval for missed mentions                           | later |                                            |

## 6. Files

TeamCraft is a chat, not a file-sharing service (use a dedicated file service for that) – but a file
shared in a chat can get a download link for people outside (e2ee.md §4.5.1).

| Feature                                                                                | Prio  | Notes                                            |
|----------------------------------------------------------------------------------------|-------|--------------------------------------------------|
| Upload via drag & drop, paste, picker                                                  | v1    | Encrypted before upload                          |
| Image/video preview + gallery                                                          | v1    | Thumbnails generated client-side                 |
| PDF preview                                                                            | v1.x  | Rendered client-side                             |
| Files tab per conversation                                                             | v1    |                                                  |
| Max file size (configurable, e.g. 2 GB), resumable chunked upload                      | v1    |                                                  |
| Screenshot paste + basic annotation                                                    | later |                                                  |
| Admin: max image resolution, allow/block downloads, edit attachments                   | v1.x  |                                                  |
| **Share link** for a file from a chat: download in any browser, no account             | v1.x  | Key in the URL `#fragment`, server never sees it |
| Share link options: expiry, max downloads, optional password, revoke; list of my links | v1.x  |                                                  |
| Admin: share links on/off, max expiry, allowed roles (default: members, not guests)    | v1.x  | Permission `create_share_link`                   |

## 7. Search

Search has to be fast and complete on every device right after login – a common weak point of
self-hosted chats – without the server reading anything: [docs/e2ee.md](docs/e2ee.md) §10.

| Feature                                                                     | Prio  | Notes                                                                                         |
|-----------------------------------------------------------------------------|-------|-----------------------------------------------------------------------------------------------|
| **Full-history search on every device** right after login                   | v1    | Encrypted shared index segments per conversation, built once, synced via S3, searched locally |
| Ranking, snippets, highlighting, stemming (de/en), fuzzy matching           | v1    | `tantivy`                                                                                     |
| Filters: `from:` `in:` `has:file` `has:link` `before:` `after:` `in-thread` | v1    |                                                                                               |
| Works offline; phones prefetch segments on Wi-Fi with optional size cap     | v1    |                                                                                               |
| Search people and channels                                                  | v1    | Guests: only people they share a conversation with                                            |
| Quick switcher (Ctrl/Cmd+K)                                                 | v1    |                                                                                               |
| Recent mentions view, saved searches                                        | v1    |                                                                                               |
| Hashtags + hashtag search                                                   | v1.x  |                                                                                               |
| Search inside files (PDF, Office)                                           | later | Text extracted by the uploading client or the optional vault node                             |

## 8. Calls

| Feature                                                                         | Prio  | Notes                                                                                                                  |
|---------------------------------------------------------------------------------|-------|------------------------------------------------------------------------------------------------------------------------|
| 1:1 audio/video call from DM                                                    | v1    |                                                                                                                        |
| Group call in a group DM or channel (50+ participants)                          | v1    |                                                                                                                        |
| **Huddle**: drop-in call in a channel, join/leave freely                        | v1    | Slack-style                                                                                                            |
| Ring all devices, accept on any; CallKit (iOS), Jetpack Telecom (Android)       | v1    |                                                                                                                        |
| Screen share (with system audio where possible)                                 | v1    | Separate stream/session                                                                                                |
| Mute, deafen, device selection, push-to-talk                                    | v1    |                                                                                                                        |
| Noise suppression + echo cancellation                                           | v1    | WebRTC APM on desktop, browser APM in web                                                                              |
| Speaker detection, active-speaker view, gallery view                            | v1    |                                                                                                                        |
| Raise hand, reactions in call                                                   | v1.x  |                                                                                                                        |
| Move call between devices ("continue on phone")                                 | v1.x  |                                                                                                                        |
| Call history + missed-call message in the conversation                          | v1    |                                                                                                                        |
| **Background blur** (light / strong)                                            | v1    | Client-side segmentation (see below); E2EE means the server never sees raw video anyway                                |
| **Background images**: presets + own uploads, synced across devices (encrypted) | v1    |                                                                                                                        |
| Preview with effects before joining (lobby / "pre-join" screen)                 | v1    | Camera, mic, speaker test                                                                                              |
| Mirror self-view, hide self-view                                                | v1    |                                                                                                                        |
| Brightness / low-light correction, soft focus ("touch up")                      | v1.x  |                                                                                                                        |
| Auto-framing (keep face centred)                                                | later |                                                                                                                        |
| Background effects on macOS via system Portrait/Background mode                 | v1    | Works automatically with AVFoundation capture                                                                          |
| Local recording by participant, saved encrypted into the channel                | later | Server can't record                                                                                                    |
| Live captions / transcription                                                   | later | Client-side (Whisper)                                                                                                  |
| Network quality indicator + stats panel                                         | v1.x  |                                                                                                                        |
| **Android Auto**: incoming/outgoing calls, in-call controls on the car screen   | v1    | See note below the table                                                                                               |
| Android Auto: contact list / recent calls on the car screen                     | v1.x  | Car App Library templates                                                                                              |
| **Android Auto messaging**: read aloud, voice reply                             | v1    | `MessagingStyle` notifications with reply + mark-as-read actions, built locally after decryption; category `MESSAGING` |
| **CarPlay** calls                                                               | v1    | Comes with CallKit                                                                                                     |
| CarPlay messaging (Siri read/reply)                                             | v1.x  | SiriKit `INSendMessageIntent` + communication notifications                                                            |

**Android Auto calling:** Jetpack Telecom (`androidx.core.telecom`) with remote-surface callbacks plus a
`CarAppService` with category `androidx.car.app.category.CALLING`; Android Auto draws the in-call UI.
Google lists the public calling API as beta: only Internal/Closed Testing tracks on Google Play
(WhatsApp has had calls since 2023 as an early-access partner). That is fine for a company app
distributed to employees via a Closed Testing track. Sideloaded APKs are hidden by Android Auto unless
"unknown sources" is enabled in its developer settings. We also apply as early-access partner.

Video effects pipeline (all on the device, before encoding):

| Platform      | Segmentation                                                                         | Compositing                                            |
|---------------|--------------------------------------------------------------------------------------|--------------------------------------------------------|
| macOS / iOS   | Apple Vision `VNGeneratePersonSegmentationRequest`                                   | Core Image / Metal                                     |
| Android       | ML Kit Selfie Segmentation                                                           | GPU (OpenGL ES / Vulkan)                               |
| Linux desktop | MediaPipe selfie segmentation model (Apache-2.0) via ONNX Runtime (`ort`) or `tract` | `wgpu` shader                                          |
| Web           | Same model via WebGPU (fallback WebGL)                                               | WebGPU shader on `VideoFrame`s before WebCodecs encode |

Shared parts (mask smoothing over time, edge feathering, blur kernel, background scaling) live in
`teamcraft-media` as WGSL shaders, so all platforms look the same.

## 9. Meetings (CalCraft)

| Feature                                                                          | Prio  | Notes                            |
|----------------------------------------------------------------------------------|-------|----------------------------------|
| Schedule meeting from TeamCraft (title, time, invitees, recurring)               | v1    | Creates CalDAV event in CalCraft |
| Invitations by email (iMIP + `.ics` + join link)                                 | v1    |                                  |
| RSVP handling (accept/decline shows in TeamCraft)                                | v1    |                                  |
| Join link for externals, browser join, lobby, host admits                        | v1    |                                  |
| Meeting chat (persisted as a conversation, guests included for that meeting)     | v1    |                                  |
| Instant meeting ("Meet now") with shareable link                                 | v1    |                                  |
| Host controls: mute all, remove, lock, end for everyone                          | v1    |                                  |
| Meetings created in any CalDAV client (special property or "add TeamCraft link") | v1.x  |                                  |
| Calendar view inside TeamCraft (embed CalCraft)                                  | v1.x  |                                  |
| "In a meeting" status from calendar                                              | v1    |                                  |
| Breakout rooms                                                                   | later |                                  |
| Webinar mode (many viewers, few speakers)                                        | later |                                  |

Server sees: meeting title, time, invitees (they are in CalDAV and email anyway). Meeting chat and media
are encrypted.

## 10. Telephony and AI hotline

TeamCraft runs an AI phone hotline itself (`teamcraft-pbx`) directly on SIP trunks of any provider –
no Asterisk, no cloud telephony API. Details: [docs/telephony.md](docs/telephony.md).

| Feature                                                                                | Prio  | Notes                                                 |
|----------------------------------------------------------------------------------------|-------|-------------------------------------------------------|
| SIP spike against the first provider (`rsipstack`)                                     | v1.x  | First step                                            |
| Provider-agnostic SIP trunks (G.722 HD voice) + voice agent adapters, first ElevenLabs | v1.x  | Existing agents keep working (tool name configurable) |
| Hotline config per number in the admin panel, optional sync API                        | v1.x  |                                                       |
| `#hotline` threads: start, escalation, who answered, AI summary                        | v1.x  | Integration channel, `teamcraft-pbx` is the bridge    |
| Queues, "available for hotline" toggle, ring strategies                                | v1.x  |                                                       |
| Take hotline calls in the app (all platforms), hold, transfer, add colleague           | v1.x  |                                                       |
| Forwarding to mobile numbers (parallel / fallback / off), "press 1" to accept          | v1.x  | Outbound legs via the trunk                           |
| Goodbye announcement after max wait                                                    | v1.x  |                                                       |
| Outbound calls (call back from thread)                                                 | later |                                                       |

## 11. People, profiles, groups

| Feature                                                                                                                                | Prio  | Notes                                          |
|----------------------------------------------------------------------------------------------------------------------------------------|-------|------------------------------------------------|
| Profile: name, avatar, title, department, phone, time zone                                                                             | v1    | Synced from the OIDC provider; avatar editable |
| Member directory                                                                                                                       | v1    | Not for guests                                 |
| **Organization** per user from an OIDC claim or group (configurable); badge in profile, directory and mention picker; directory filter | v1    | Display data only; docs/branding.md §9         |
| User groups (`@devteam`) mapped from Keycloak groups or created manually                                                               | v1    |                                                |
| Org chart (manager field from Keycloak)                                                                                                | later |                                                |

## 12. Guests

| Feature                                                                                                               | Prio | Notes                                          |
|-----------------------------------------------------------------------------------------------------------------------|------|------------------------------------------------|
| Invite guest by email to a space or specific channels                                                                 | v1   |                                                |
| Guest login: the company Keycloak realm with client role `guest`, or optional local account (password + TOTP/passkey) | v1   |                                                |
| Guests see only their channels; no directory, no channel browser                                                      | v1   | Enforced by server and by MLS group membership |
| Guests can DM only people in their shared channels (toggle)                                                           | v1   |                                                |
| Guest badge everywhere, "external people" banner in channels                                                          | v1   |                                                |
| Expiry date, sponsor (responsible employee)                                                                           | v1   | Auto-removal on expiry                         |
| Meeting-only guests (no account, one-time token)                                                                      | v1   |                                                |
| History for guests: always from join                                                                                  | v1   |                                                |

## 13. Administration

Server admins manage all spaces and channels, including private ones, without being able to read
them. Details: [docs/admin.md](docs/admin.md).

| Feature                                                                                                                                | Prio  | Notes                                                                                                                     |
|----------------------------------------------------------------------------------------------------------------------------------------|-------|---------------------------------------------------------------------------------------------------------------------------|
| Roles: server owner, server admin, space admin, channel owner/moderator                                                                | v1    | Custom roles later                                                                                                        |
| See and manage all spaces/channels incl. private (metadata, members, settings)                                                         | v1    | No content access                                                                                                         |
| Membership changes as signed admin actions → MLS external proposals, committed by member clients                                       | v1    | Admin needn't be a member                                                                                                 |
| Signed admin roster rooted in the owner's identity key                                                                                 | v1    | Server can't fake admin actions                                                                                           |
| Admin self-join to private channels: visible (default) / owner approval / disabled                                                     | v1    | Enforced by clients                                                                                                       |
| Assign new owner to orphaned channels                                                                                                  | v1    |                                                                                                                           |
| Users: deactivate (also via the OIDC provider), remove devices, start org recovery                                                     | v1    |                                                                                                                           |
| Guests: invite, expiry, sponsor, overview                                                                                              | v1    |                                                                                                                           |
| Report message (member shares decrypted content with admins) + delete by ID                                                            | v1.x  |                                                                                                                           |
| Retention policies per space/channel (see §1)                                                                                          | v1    | Admin defaults + limits; channel owners set their own within them                                                         |
| Audit log (metadata only), visible to affected members as system messages                                                              | v1    |                                                                                                                           |
| Instance settings, security policy (incl. org recovery), SFU nodes, telephony, push, storage                                           | v1    |                                                                                                                           |
| Admin UI inside the normal client                                                                                                      | v1    | Actions signed with the admin's device key                                                                                |
| **Permission schemes** (Mattermost model): roles at instance / space / channel scope; a channel can only restrict, never grant         | v1    | See permission list below                                                                                                 |
| Channel moderation: per channel toggle for members and guests separately – post, react, manage members, use @channel, manage bookmarks | v1    | Covers announcement channels                                                                                              |
| Custom terms of service users must accept                                                                                              | v1.x  |                                                                                                                           |
| Announcement banner (instance-wide, dismissible)                                                                                       | v1    |                                                                                                                           |
| Session length, idle timeout, enforce MFA (incl. guests), password policy for local accounts                                           | v1    |                                                                                                                           |
| Restrict guest email domains; restrict DMs (anyone / only shared spaces)                                                               | v1    |                                                                                                                           |
| Display name format (username / full name / nickname), privacy: show email/full name                                                   | v1    |                                                                                                                           |
| Rate limits, max payload size                                                                                                          | v1    |                                                                                                                           |
| Toggles: custom emoji, GIF picker, link previews + domain allow/block list                                                             | v1.x  |                                                                                                                           |
| Bots: allow creation, disable bots when owner is deactivated                                                                           | v1.x  |                                                                                                                           |
| Data retention details: keep pinned messages, separate file vs. message retention, job schedule                                        | v1.x  |                                                                                                                           |
| Content reporting workflow: reviewers, reasons, notify reporter, hide reported content during review                                   | v1.x  | Extends "Report message"                                                                                                  |
| Custom profile attributes (admin-defined fields)                                                                                       | v1.x  |                                                                                                                           |
| Statistics (users, channels, activity – metadata only), site health page                                                               | v1.x  |                                                                                                                           |
| Compliance archive, export, legal hold                                                                                                 | later | Built into the protocol from day one (policy field, reserved archive credential); runs on the vault node. See e2ee.md §11 |

Permission names (modelled on Mattermost, adapted): `create_post`, `add_reaction`, `remove_others_reactions`,
`edit_others_posts`, `delete_others_posts`, `use_channel_mentions`, `use_group_mentions`,
`manage_bookmarks`, `manage_members`, `manage_channel_roles`, `manage_channel_properties`,
`convert_channel_privacy`, `create_dm`, `create_group_dm`, `create_channel`, `create_user_group`,
`manage_user_group_members`, `invite_guest`, `promote_guest`, `demote_to_guest`, `create_emoji`,
`delete_others_emoji`, `manage_webhooks`, `create_bot`, `use_slash_commands`, `create_share_link`,
`read_audit_log`, `manage_branding`. Effective permission = granted at instance/space scope **and** not removed at
channel scope. Role and scheme assignments are signed like admin actions ([docs/admin.md](docs/admin.md) §2.3).

## 14. Integrations

| Feature                                                                                                                                                                                                                                                                    | Prio  | Notes                                    |
|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|------------------------------------------|
| Bot SDK (Rust); bots are clients with their own device keys                                                                                                                                                                                                                | v1.x  | Run on trusted machines                  |
| Incoming webhooks (Git, CI, monitoring) via `teamcraft-bridge`                                                                                                                                                                                                             | v1    | Slack/Mattermost-compatible format       |
| Slash commands for bots                                                                                                                                                                                                                                                    | later |                                          |
| Built-in slash commands: `/away` `/online` `/dnd` `/offline` `/status`, `/topic` `/purpose` `/rename`, `/join` `/open` `/leave` `/kick`, `/invite`, `/msg` `/groupmsg`, `/mute`, `/me` `/shrug` `/code`, `/search`, `/settings`, `/shortcuts`, `/help`, `/remind`, `/meet` | v1    | All run client-side                      |
| Interactive messages for bots (buttons, menus, cards)                                                                                                                                                                                                                      | v1.x  | Used by the `#hotline` bot ("Take call") |
| Webhook display name + icon override                                                                                                                                                                                                                                       | v1.x  |                                          |
| CalCraft integration                                                                                                                                                                                                                                                       | v1    |                                          |

## 15. Clients

| Client                          | Prio | Tech                                                                                                                                                                                                                                          |
|---------------------------------|------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Desktop Windows + macOS + Linux | v1   | Tauri 2 + Leptos; Rust core for crypto, media, storage. Windows (10/11, x64 and arm64): WebView2, Credential Manager, WASAPI, Media Foundation cameras, Windows Graphics Capture, MSI + NSIS installers (unsigned until a certificate exists) |
| Web (members + guests)          | v1   | Same Leptos app, WASM; WebTransport + WebCodecs                                                                                                                                                                                               |
| Android                         | v1   | Native shell (Kotlin) + Rust core via UniFFI; FCM + UnifiedPush, Jetpack Telecom, Android Auto                                                                                                                                                |
| iOS                             | v1   | Native shell (Swift) + Rust core via UniFFI; Notification Service Extension, CallKit                                                                                                                                                          |

Mobile is native-shell rather than Tauri mobile because the Notification Service Extension, CallKit
and Android Auto need native targets. All logic lives in the shared Rust core.

General client features (v1): offline mode with outbox, dark/light theme, German + English,
keyboard shortcuts, accessibility (screen reader labels, focus order), multiple accounts later.

---

## Roadmap

Each milestone ends with something usable. M0–M10 make up v1.

| #       | Milestone                               | Content                                                                                                                                                                                                        |
|---------|-----------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **M0**  | Foundation                              | Workspace, repo rules, CI, `cargo deny`, proto types, Postgres schema, docker-compose (Keycloak, Postgres, S3), dev certificates                                                                               |
| **M1**  | Accounts and structure                  | OIDC + local accounts, devices, spaces, channels, DMs, memberships, permission schemes incl. guests, gateway (WebSocket), presence. Plaintext messages *only in dev* as a stepping stone                       |
| **M2**  | E2EE core                               | Identity + device keys, cross-signing, OIDC attestation, MLS groups, content keys, encrypted messages and attachments, device approval + QR linking. UniFFI bindings start here so the core stays mobile-ready |
| **M3**  | Backup and recovery                     | Key bag, passkey PRF wrapping, passphrase fallback, org recovery (2-of-N)                                                                                                                                      |
| **M4**  | Desktop + web client v0                 | Leptos UI: sidebar, conversations, composer, threads, reactions, edits, files, search (shared encrypted index segments), desktop notifications                                                                 |
| **M5**  | Threads, status, notifications complete | Threads inbox, DND/OOO/custom status, notification rules, priority/acknowledgement, Web Push                                                                                                                   |
| **M6**  | Calls                                   | WebTransport SFU, Opus, DAVE, 1:1 and group calls, huddles, screen share, video effects (blur, backgrounds)                                                                                                    |
| **M7**  | Meetings                                | CalCraft integration, iMIP invites, browser guest join with lobby, meeting chat, "in a meeting" status                                                                                                         |
| **M8**  | Mobile                                  | Android + iOS shells: chat, E2EE push (NSE / FCM / UnifiedPush), calls (CallKit / Jetpack Telecom), Android Auto, CarPlay                                                                                      |
| **M9**  | Admin, guests, integration channels     | Admin panel, deprovisioning, audit log, guest expiry, retention, `teamcraft-bridge` + incoming webhooks, log view                                                                                              |
| **M10** | Company rollout (v1)                    | Hardening, fuzzing, external security review, packaging (dmg, Flatpak/AppImage/rpm/deb, App Store, Play Store closed testing), migration guide                                                                 |
| **M11** | Bots and share links (v1.x)             | Bot SDK, interactive messages, file share links, scheduled/recurring messages                                                                                                                                  |
| **M12** | Telephony (v1.x)                        | SIP spike with the first provider, then `teamcraft-pbx`: voice agent adapters, queues, take/transfer hotline calls in the app, mobile forwarding                                                               |

## Decisions

- Org recovery planned for v1, configurable in the admin panel (default on for members, off for guests)
- The OIDC provider runs on a separate server and attests identity keys (e2ee.md §3.1)
- "Share history" is chosen when adding a member: default on (admin setting); guests always from join
- `admin_self_join` defaults to `visible`
- Apple Developer and Google Play accounts exist

