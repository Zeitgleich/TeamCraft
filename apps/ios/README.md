# TeamCraft for iOS

A thin SwiftUI shell (iOS 17+, iPhone and iPad) over the Rust client core (ARCHITECTURE.md §8,
PLAN.md M8). All encryption, sync, storage and call media live in `teamcraft-core` /
`teamcraft-media`, reached through the UniFFI bindings of `crates/teamcraft-ffi` (the same ones the
Android app uses). The app does login, keys in the Keychain, screens, push and CallKit.

Status: written without a Mac; CI builds and unit-tests it on a macOS runner, nobody has run it on a
device yet. Use the [manual test checklist](#manual-test-checklist) for the first run. **Untested on a
device, and not testable in CI:** the camera and video path (needs a camera and a call partner), the
photo and document pickers, Quick Look, the background prefetch task and CallKit's video flag.

## Layout

| Path                   | What                                                                                                                             |
|------------------------|----------------------------------------------------------------------------------------------------------------------------------|
| `project.yml`          | XcodeGen project: app, notification extension, unit tests                                                                        |
| `Config/Base.xcconfig` | Server, OIDC client, App Group and keychain group; local overrides in `Local.xcconfig`                                           |
| `scripts/`             | `build-xcframework.sh` (Rust core + Swift bindings), `check-strings.py`                                                          |
| `TeamCraft/App`        | `@main`, `AppDelegate` (APNs token, notification taps and actions)                                                               |
| `TeamCraft/Auth`       | OIDC login: `ASWebAuthenticationSession`, PKCE                                                                                   |
| `TeamCraft/Model`      | `AppModel` (state, the one entry to the core), conversation sections, chat actions                                               |
| `TeamCraft/Calls`      | `CallManager`: CallKit provider, PushKit forward, mute / hold / hang up; camera (`CameraStreamer`, `H264Encoder`), remote video  |
| `TeamCraft/UI`         | SwiftUI screens                                                                                                                  |
| `TeamCraft/Design`     | Design tokens (same values as the desktop UI and the Android theme)                                                              |
| `TeamCraft/Resources`  | String Catalogs (en, de), asset catalog (app icon, launch screen colour and logo)                                                |
| `Shared/`              | Compiled into the app **and** the extension: keychain, App Group, process lock, OIDC session, core factory, notification content |
| `NotificationService/` | The Notification Service Extension                                                                                               |
| `TeamCraftTests/`      | Unit tests (PKCE, OIDC URLs, push payload, lock, notification text, sections, wire models, search filters, Annex B)              |
| `Generated/`           | `TeamCraftFFI.xcframework` and `Swift/teamcraft_ffi.swift` (not in git)                                                          |

## Build

Needs a Mac with Xcode 16+, Rust (rustup), `cmake` (libopus) and XcodeGen:

```sh
brew install xcodegen cmake
cd apps/ios
make core        # Rust core for iOS (device + simulator) + Swift bindings; slow the first time
make build       # xcodegen + xcodebuild for a simulator
make test        # unit tests
```

Or open Xcode: `make generate && open TeamCraft.xcodeproj` (run `make core` once first).
`scripts/build-xcframework.sh --debug` builds the dev profile (faster, large); `--device-only`
skips the simulator slices (archives).

The XCFramework holds the static library of `teamcraft-ffi` for `aarch64-apple-ios`,
`aarch64-apple-ios-sim` and `x86_64-apple-ios` (the two simulator slices merged with `lipo`). The Swift
bindings are generated from the library by `teamcraft-uniffi-bindgen` and compiled into both the app and
the extension (each links the static library; they are separate processes).

### Configuration

`Config/Base.xcconfig` has the development defaults; put your own in `Config/Local.xcconfig`
(untracked), which it includes:

| Setting                          | Meaning                                                                                                       |
|----------------------------------|---------------------------------------------------------------------------------------------------------------|
| `TEAMCRAFT_SERVER_URL`           | TeamCraft server                                                                                              |
| `TEAMCRAFT_OIDC_ISSUER`          | OIDC issuer (Keycloak realm URL)                                                                              |
| `TEAMCRAFT_OIDC_CLIENT_ID`       | Public OIDC client with PKCE                                                                                  |
| `TEAMCRAFT_OIDC_REDIRECT_SCHEME` | Redirect URI is `<scheme>:/oauth/callback`; the client must allow it                                          |
| `DEVELOPMENT_TEAM`               | Your Apple team (signed builds)                                                                               |
| `APP_GROUP_ID`                   | App Group shared by app and extension (change it with your bundle IDs)                                        |
| `NSE_ENTITLEMENTS`               | `NotificationService/NotificationService.Filtering.entitlements` once Apple granted the filtering entitlement |

Bundle IDs are `de.zeitgleich.teamcraft` (app), `de.zeitgleich.teamcraft.notifications`,
`de.zeitgleich.teamcraft.Tests`. Change them for your Apple account in `project.yml` together with
`APP_GROUP_ID` and `KEYCHAIN_ACCESS_GROUP`.

## How it works

### Data and keys

- One Rust engine per signed-in account owns the sealed SQLite store (docs/e2ee.md §9.2), the sync loop
  and the calls. Calls into it are `async`; the core tells the app to re-read through `EventListener`.
- The 256-bit **database key** is generated on first sign-in and kept in the Keychain
  (`AfterFirstUnlockThisDeviceOnly`, shared access group), so the extension can open the store while
  the phone is locked. The **OIDC session** (access and refresh token) is in the Keychain too;
  `SessionVault` refreshes it and hands the core access tokens.
- Database and lock file live in the **App Group container**. Without the entitlement (an unsigned
  simulator build) the app falls back to its own Application Support folder and the Keychain's default
  group; push then only works inside the app.

### Login and enrollment

OIDC authorization code + PKCE in an `ASWebAuthenticationSession` (single sign-on with Safari's
cookies), then the core enrolls the device: a new identity with a recovery passphrase, or recovery
with the passphrase, **device linking** (this phone shows a code and QR code; an existing device
approves it in *Settings → Link a device*, both compare a short code), or **recovery by the
organization's admins**. Same flows as the Android app.

### Push (docs/e2ee.md §7)

1. After enrollment the app asks for the notification permission and registers with APNs. The device
   token (lowercase hex) goes to the server with `PushService.apns` and a fresh push key.
2. The server sends an APNs notification with `mutable-content: 1`, a generic alert and the sealed
   wake-up under the custom key `w` (base64url, no padding, XChaCha20-Poly1305 with the push key).
3. The extension opens the wake-up, takes the **cross-process lock** (`flock` on a file in the App
   Group; the app holds it while it is in the foreground, so only one process syncs), opens the core
   with the shared database key, fetches and decrypts the message (`notificationForPush`) and
   rewrites the notification: sender and preview, thread grouping, reply and mark-as-read actions,
   and an `INSendMessageIntent` so the system shows it as a communication notification. It posts a
   Darwin notification so the app reloads. The extension never touches MLS state.
4. If the app holds the lock, there is no database key, or time runs out, the generic alert stays.

The extension has the same limit as every extension (about 24 MB of memory, 30 seconds): **measure it
on a device** with Xcode's memory gauge, a first-run risk because it opens the whole core.

Until Apple grants `com.apple.developer.usernotifications.filtering` (we request it, docs/e2ee.md §7)
an extension cannot suppress a notification: the server should push only what is shown, and ring
wake-ups show the generic alert next to CallKit's screen.

### Calls (docs/e2ee.md §8)

`CallManager` is the `CXProvider` delegate and the core's `CallListener`:

- **Incoming, app running:** the gateway rings (`onIncomingCall`), the app reports the call to CallKit.
- **Incoming, app asleep:** the server pushes a wake-up of kind `call`; the extension decrypts it and
  calls `CXProvider.reportNewIncomingVoIPPushPayload`, which wakes the app into
  `pushRegistry(_:didReceiveIncomingPushWith:)` (a `PKPushRegistry` exists only for this forward) and
  from there into CallKit. Background modes `voip` and `audio` are set for this.
- Answering joins in the core **after CallKit activated the audio session** (`didActivate`); the media
  runs in Rust (`cpal` / CoreAudio) on `AVAudioSession` category `playAndRecord`, mode `voiceChat`.
- Mute, hold (the core mutes and stops playing, `setHeld`), end and outgoing calls go through `CXCallController`,
  so the lock screen, headset buttons, the call list and **CarPlay** (system call UI) all work.
  CarPlay needs no extra entitlement for calls.

### Files and link previews

The composer's **+** menu attaches photos and videos (`PhotosPicker`, exported to a file, never loaded
into memory) and any file (Files app). Picks are copied into the app's `uploads` folder (not in backups,
removed after a week and on sign-out) and sent as one `send_message` action with `source: path`; the core
encrypts and uploads them in chunks. Messages show their files with thumbnail, size and **upload /
download progress** (re-read once a second while something moves); a tap opens a downloaded file in
Quick Look or starts the download first. *Files* in a chat's toolbar is the gallery of all its files.
Link previews are drawn from what the **sender's** core fetched (the receiving phone never contacts the
site); only `http(s)` links are opened. Files over the server's limit are refused before copying.

### Meetings

*Meetings* (calendar icon in the chat list) lists the user's scheduled meetings (`GET /meetings`), sorted
by start. **Join** starts the meeting chat's call; the menu answers the invitation (`PUT
/meetings/{id}/rsvp`: accept, maybe, decline), shown with the current answer. Like the Android screen.

### Notification rules

*Settings → Mentions and keywords*: first name, `@channel` / `@here` / `@all` and own keywords
(`teamcraft_core::notify_rules` keeps them in the sealed store, the matching is `teamcraft_client_api::notify`). The extension reads them with the store, so a
pushed message that matches counts as a mention (`PushNotification.isMention`): it ranks higher in the
notification summary. The rules cannot stop the server from pushing, and an extension without the
filtering entitlement cannot suppress a notification (see Push).

### Search

The search field takes the core's query syntax; the filter button (top left of the chat list) builds
`from:`, `in:`, `after:`, `before:`, `has:file` and `has:link` for you. *Settings → Search → Download
search index* sets whether the core prefetches shared index segments (off / on Wi-Fi / any network). iOS
runs it as a `BGProcessingTask` (`de.zeitgleich.teamcraft.segment-prefetch`, needs power and network,
scheduled when the app goes to the background); the core checks the setting and the network's metered flag.
iOS decides when the task really runs, so old history may need a day of charging before it is searchable.

### Video calls

The phone icon with a camera starts a video call; *camera* in any call turns the camera on (permission
asked then), a *flip* button switches front and back. CallKit is told (`supportsVideo`, `hasVideo`).
The core has no platform camera on iOS, and Cisco's OpenH264 cannot be downloaded into an iOS app, so
the app does the codec work in system frameworks and hands **H.264 (Baseline, Annex B, SPS and PPS in
front of every keyframe)** to the call engine, which encrypts (DAVE) and forwards it:

- send: `AVCaptureSession` → `H264Encoder` (VideoToolbox, 720p portrait, 1.2 Mbit/s, keyframe every 2 s
  and when the SFU asks) → `CallSession.sendCameraFrame`;
- receive: `VideoListener.onRemoteVideoFrame` → `RemoteVideoRenderer` (Annex B → AVCC samples with the
  stream's parameter sets) → `AVSampleBufferDisplayLayer`, which decodes with the hardware decoder.

One simulcast layer, one remote track is shown (the first camera, else a screen share), camera tracks
only on the send side (no screen sharing from the phone). Other clients must offer H.264 (the desktop
client does). **Hold** (system call UI) silences the microphone, stops playing the others and pauses the
camera; the call stays joined (`CallSession.setHeld`).

### Server side

The server's APNs sender is still a stub (`crates/teamcraft-server/src/push/sender.rs`) and the
`call` wake-up kind is new (`PushKind::Call`): push and ring-in-the-background work only once the
server sends them. The in-app paths (foreground, gateway) do not need it.

## Signing and TestFlight

CI builds unsigned (`CODE_SIGNING_ALLOWED=NO`). For a device or TestFlight you need an Apple
Developer account:

1. Create the App IDs for the app and the extension and an App Group; enable the capabilities Push
   Notifications, App Groups, Keychain Sharing, Communication Notifications and Time Sensitive
   Notifications (all self-service). Request **Notification Filtering**
   (`com.apple.developer.usernotifications.filtering`) from Apple.
2. Set `DEVELOPMENT_TEAM`, the bundle IDs and `APP_GROUP_ID` / `KEYCHAIN_ACCESS_GROUP` (see above).
3. `APS_ENVIRONMENT = production` for archives (also automatic from the provisioning profile).
4. `scripts/build-xcframework.sh --device-only`, `xcodegen generate`, then *Product → Archive* in Xcode
   and upload to App Store Connect. `ITSAppUsesNonExemptEncryption` is `true`: the app has encryption
   beyond the OS (export compliance questions apply).

There is no signing, fastlane or upload automation yet.

## Manual test checklist

Run against the dev stack (`docker-compose.dev.yml`, README of the repo) or a real server. First on
a **device** (push, CallKit and memory only work there), then the simulator.

1. **Build and launch.** `make core && make generate`, run on the device. The app starts at the sign-in screen.
2. **Login.** *Sign in* opens the system browser sheet, log in, the sheet closes, the app shows *Set up
   this device*. Cancelling the sheet returns to sign-in without an error.
3. **Enrollment.** A fresh user: passphrase under 12 characters or mismatching is refused; a good one
   creates keys and shows the chat list. A user with keys: wrong passphrase shows the error, the right
   one restores.
4. **Linking.** On a second device (desktop) *Link a device* with the code or QR code from the phone;
   compare the short codes on both; the phone shows its chats. Also the reverse: approve the phone from *Settings → Link a device*.
5. **Chats.** Conversation list in sections (favorites, spaces, direct messages); unread and mention
   badges; open one, history loads, send a message (appears *Sending…*, then settles), receive one from
   another device; reply in a thread and open the thread; react (long press) and toggle; edit and delete
   own message; search finds messages from other chats; favorite toggle; presence and custom status change shows on the desktop.
6. **Push, app in background or killed.** Another user writes: a notification with the sender, the
   channel and the text arrives (not the generic alert). Long press: *Reply* sends, *Mark as read*
   clears the badge. Muted channels stay quiet. Check the memory gauge of the extension stays under 24 MB.
7. **Push, app in foreground.** Banner for other chats, none for the open chat; a tap opens the chat.
8. **Locked phone.** A push while the phone is locked after a first unlock still shows the text.
9. **Calls.** Start a call from a chat (the phone icon): the system shows an outgoing call, the in-call
   screen shows the privacy code, the other side hears audio both ways; mute from the screen and from the
   lock screen; hang up. Receive a call with the app open, in the background and killed (needs the server
   `call` wake-up): the system's incoming-call screen, answer joins; decline stops ringing on the caller.
   If you have CarPlay (or the simulator's CarPlay window): the call appears there, answer works.
10. **Sign out.** *Settings → Sign out* returns to sign-in; the Keychain items and the account are gone from the shared defaults; signing in again as the same user restores the chats.
11. **Files.** Attach a photo, a video and a PDF from the Files app (also two at once): chips show above the
    composer, sending shows the files with a progress bar; the other side (desktop) receives them. A file
    from the desktop shows *Tap to download*, a tap downloads and opens it in Quick Look; the gallery
    (folder icon) lists them. A file over the server's limit shows the "larger than the server accepts"
    alert. A message with a link shows a preview card when the sender's client made one.
12. **Meetings.** Schedule a meeting on the desktop and invite the phone's user: it shows in *Meetings*,
    *Maybe* / *Accepted* / *Declined* changes the answer on the desktop too, *Join* opens the call.
13. **Notification rules.** Add a keyword in *Settings → Mentions and keywords*, send it from another
    user while the app is killed: the notification arrives and is ranked as a mention in the summary
    (check with a muted-by-default channel). Turn *My first name* off and check that a message with the
    name no longer ranks higher.
14. **Search filters.** Search with `from`, `in`, a date range, *Has a file*, *Has a link* from the filter
    sheet and check that the results shrink; clear the filters.
15. **Search prefetch.** Set *Download search index* to *On any network*, background the app on the
    charger (Xcode: *Debug → Simulate Background Fetch* does not run processing tasks; use
    `e -l objc -- (void)[[BGTaskScheduler sharedScheduler] _simulateLaunchForTaskWithIdentifier:@"de.zeitgleich.teamcraft.segment-prefetch"]`
    in the debugger) and check that an old-month search finds hits afterwards.
16. **Video.** Start a video call from a chat (video icon) with a desktop user: both see each other
    (camera permission prompt on first use); *flip* switches the camera; turning the camera off and on
    again works; hold from the system UI freezes the picture and silences both ways; the call screen
    shows the remote picture in portrait and landscape of the other side. Join a call that already has
    video: the picture appears within a few seconds. Check CPU/heat over five minutes.
17. **German.** Switch the phone's language to German; screens, notification actions and the call
    screen are translated.

Report anything that fails with the Xcode console log (subsystem `de.zeitgleich.teamcraft` once added) and
whether it was a device or the simulator.

## Known gaps

- No admin screens (desktop and web only, as on Android). Meetings can be joined and answered, not
  scheduled, from the phone.
- Video: one camera track per phone, no screen sharing from the phone, no layout for more than one remote
  track (the first one is shown), portrait capture only. The camera and video path has only ever run in
  CI's compiler and unit tests (Annex B handling), never on a device.
- The keyword rules mark and rank notifications; they cannot suppress one without the filtering
  entitlement, and the server still decides what is pushed.
- Search prefetch keeps at most 512 MB of segments (newest months first), and iOS decides when a processing task runs.
- Push and ring-in-the-background need the server's APNs sender and the `call` wake-up.
- Swift 5 language mode (CI probes Swift 6 in a non-gating step; see the PR).
