# v1 acceptance

The acceptance suite is the end-to-end suite in [tests/e2e](../tests/e2e/README.md): every scenario starts
the real `teamcraft-server` (plus SFU and bridge where needed) against the dev stack and drives it with
real `teamcraft-core` clients, plain REST or a headless browser. `scripts/e2e.sh` runs all of it. This
page lists what each scenario proves for the v1 sections of [PLAN.md](../PLAN.md) and what it does not.

| PLAN.md section       | Scenario (`tests/e2e/tests/suite/`)                                   | What it proves                                                                                                                               |
|-----------------------|-----------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------|
| §1 Workspace          | `accounts`, `devices`, `conversion`, `moderator`, `membership`        | Keycloak login and roles, enrollment, device linking, public↔private conversion, channel moderators, member removal rotates the keys         |
| §2 Messaging          | `messaging`, `membership`                                             | Encrypted channel and DM messages, threads, reactions; the server (and its log) never sees plaintext; outsiders can't read                   |
| §3 Threads            | `acceptance_threads`, `threads_policy`                                | Auto-follow (root author, mentioned), inbox, unread and mention counts, read position, manual follow, policy default and threads on/off      |
| §4 Presence           | `acceptance_presence`                                                 | Online from the gateway, do not disturb, appear offline, custom status with end, admin switches (custom statuses, presets, statuses off)     |
| §5 Notifications      | `acceptance_notifications`, `email_digest`                            | Level and mute per conversation, unread and mention counts, DND schedule, push registration (fake token), content-free mention e-mail digest |
| §6 Files              | `acceptance_files`                                                    | Encrypted upload/download, ciphertext in storage, files tab, share links with download cap, revocation, members only                         |
| §7 Search             | `acceptance_search`                                                   | Full-text search over decrypted messages with word forms and the filters; the server stores no plaintext                                     |
| §8 Calls (signalling) | `acceptance_calls`, `calls`                                           | Ring, start, roster update and end events through the gateway; members only; 1:1 call through the SFU with DAVE and equal privacy codes      |
| §9 Meetings           | `meetings`, `acceptance_meetings_web`                                 | Scheduling with iMIP mail (Mailpit), RSVP, guest link in a real browser, lobby, admission, cancellation                                      |
| §11 People            | `acceptance_people`, `display_names`                                  | Profile with time zone and working hours, directory (guests find nobody they share nothing with), user groups, display name policy           |
| §12 Guests            | `guests`, `guest_domains`, `mfa`                                      | Guest visibility, DMs only along shared conversations (also in groups), e-mail domain restriction, local accounts with TOTP                  |
| §13 Administration    | `admin`, `retention_limits`, `mfa`, `threads_policy`, `display_names` | Deactivation and audit, retention and its limit, signed policy options enforced by the server, require MFA                                   |
| §14 Integrations      | `bridge`                                                              | Webhooks and log lines arrive decrypted in an integration channel; bad input is refused                                                      |

## Known gaps

What the suite does **not** prove today, and why:

| Gap                                                          | Why                                                                                                         |
|--------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------|
| Browser sends a message that a second client receives        | Ignored test `web::a_message_written_in_the_web_client_reaches_a_second_client`: the web build is read-only |
| Push delivery (Web Push, APNs, FCM) and the content mode     | Only registration is exercised; dispatch is covered by the server's own tests with fakes                    |
| Keyword notifications, activity feed, "also send to channel" | Client-side features of the UI, not reachable through the core API                                          |
| Search from shared index segments for a new member           | Segments are published for closed months; the real clock can't close one inside a test                      |
| "In a meeting" status from CalCraft free/busy                | Needs the CalCraft container (heavy build), not part of the default dev stack run                           |
| Urgent messages, read receipts                               | Covered by the server's integration tests, no e2e scenario yet                                              |
| Telephony and the AI hotline (§10)                           | Not v1                                                                                                      |

## Bugs the suite found

| Bug                                                                                                                                           | Where                                                                       |
|-----------------------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------|
| `PUT /me/profile` accepts a time zone that is not an IANA name (ignored test `acceptance_people::a_time_zone_that_does_not_exist_is_refused`) | `crates/teamcraft-server` profile service                                   |
| Enrollment answered `500 internal server error` once or twice in about 40 parallel runs                                                       | Not reproduced alone; the harness now prints the server log when it happens |

