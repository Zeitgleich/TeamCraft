# TeamCraft UI – English (source language)
# Calls: the call screen and pre-join, incoming calls, huddles, screen sharing and effects.

## Compact layout (phones): bottom navigation, back, menus; calls in the browser
calls-browser-title = Calls aren't available here
calls-browser-body = Call from the TeamCraft desktop or mobile app. Your conversations are there too.
calls-browser-download = Get the app
calls-unsupported-title = This browser can't make calls
calls-unsupported-body = Calls need a current Chrome, Edge, Firefox or Safari. Update your browser, switch to one of these, or call from the TeamCraft app.
calls-other-tab-title = Calls run in your other tab
calls-other-tab-body = TeamCraft is open in another tab of this browser, and calls run there. Make and take calls in that tab, or close it and reload this one.

## Calls and huddles
call-start-audio = Start audio call
call-start-video = Start video call
huddle-start = Start a huddle
huddle-join = Join huddle
huddle-leave = Leave huddle
huddle-in-progress = Huddle in progress
huddle-on-other-device = You are in this call on another device
huddle-join-here = Join here too
huddle-move-here = Move call here
huddle-leave-everywhere = Leave on all devices
huddle-bar-label = Huddle
call-title-direct = Call with { $name }
call-title-group = Call in { $name }
call-title-huddle = Huddle in { $name }
call-title-unnamed = Call
call-title-huddle-unnamed = Huddle
call-open = Open call
call-in-progress = In a call
call-pill-label = Call in progress
call-controls-label = Call controls
call-leave = Leave
call-minimize = Back to chat
call-add-people = Add people
call-add-chosen = People to add
call-already-in = Already in the call
call-add-hint = The call moves into a new meeting room with everybody in it. The direct messages stay as they are; the people you add don't see them.
call-add = Add to the call
call-meeting-title = Call
call-move-offer = { $name } moved the call into a meeting room and added { $added }.
call-move-join = Join the meeting room
call-effects = Background effects
effects-system-hint = macOS can apply its own video effects (Portrait, background, Studio Light) on top. If one of them is on, choose "None" here so they don't add up.
effects-system-open = Open macOS video effects
call-layout-gallery = Gallery view
call-layout-speaker = Active speaker view
call-hand-raise = Raise hand
call-hand-lower = Lower hand
call-participants = { $count ->
    [one] { $count } person
   *[other] { $count } people
  }
call-elapsed = Call time { $time }
call-encrypted = End-to-end encrypted
call-encrypted-detail = Only people in this call can see and hear it. Compare the code to check.
call-verification-code = Code { $code }
control-mic-mute = Mute microphone
control-mic-unmute = Unmute microphone
control-camera-off = Turn camera off
control-camera-on = Turn camera on
control-camera-flip = Switch camera
control-camera-choose = Choose camera
call-camera-fallback = { $failed } couldn't be opened, so { $used } is used instead.
share-start = Share screen
share-stop = Stop sharing
share-options = Share your screen
share-sound = Share sound
share-sound-hint = What your computer plays, such as a video. The sound of this call is left out.
share-motion = Prioritize smooth motion
share-motion-hint = For video, games and scrolling: 60 frames per second at up to 1080p. Off: sharpest text, up to 1440p at 30 frames per second.
share-choose = Choose what to share
share-sound-on = With sound
share-sound-unavailable = Sound can't be shared on this system
share-banner-self = You are sharing your screen
share-banner-remote = { $name } is sharing their screen
share-screen-label = Screen shared by { $name }
tile-you = You
tile-muted = muted
tile-hand = hand raised
tile-speaking = speaking
tile-reconnecting = reconnecting
video-stalled = Video paused: weak connection

## Stats for nerds (#938): an overlay with technical details on every video of the call
stats-title = Stats for nerds
stats-copy = Copy stats
stats-copied = Stats copied
stats-sent-camera = Your camera, sent
stats-sent-screen = Your screen, sent
stats-received = Received video (stream { $stream })
stats-resolution = Resolution
stats-frame-rate = Frame rate
stats-bitrate = Bitrate
stats-codec = Codec
stats-encoder = Encoder
stats-layers = Simulcast layers
stats-layer = Simulcast layer
stats-loss = Packet loss
stats-network = Network
stats-frames = Frames
stats-encoder-hardware = { $name } (hardware)
stats-encoder-software = { $name } (software)
stats-encoder-browser = { $name } (chosen by the browser)
stats-value-sent-resolution = { $sent } (capture { $capture })
stats-value-sent-frame-rate = { $fps } fps (capture { $capture })
stats-value-received-frame-rate = { $fps } fps (drawn { $drawn })
stats-value-sent-bitrate = { $rate } (estimate { $estimate })
stats-value-sent-layers = { $active } (target { $target })
stats-value-received-layer = { $layer } of { $offered } (asked ≤ { $max })
stats-value-uplink-loss = { $loss } (uplink)
stats-value-network = RTT { $rtt } · jitter { $jitter }
stats-value-frames = dropped { $dropped } · black { $black }
stats-waiting = Waiting for the first numbers…
# The chat next to the video (#936)
call-chat = Chat
call-chat-label = Chat of the call
call-chat-show = Show chat
call-chat-hide = Hide chat
call-chat-show-unread = { $count ->
    [one] Show chat, { $count } unread message
   *[other] Show chat, { $count } unread messages
  }
call-chat-toast-open = New message from { $name }. Show chat
call-chat-toast-files = Sent files

## Pre-join screen
prejoin-quick-controls = Microphone and camera
prejoin-direct-hint = Check your devices before you start the call.
prejoin-nobody = Nobody has joined yet.
prejoin-people-in-call = { $count ->
    [one] { $count } person is in the call.
   *[other] { $count } people are in the call.
  }
prejoin-camera = Camera
prejoin-microphone = Microphone
prejoin-speaker = Speaker
prejoin-mic-level = Microphone level
prejoin-test-speaker = Test speaker
prejoin-testing-speaker = Playing…
prejoin-effects = Background
prejoin-effects-hint = Effects are applied on your device before your video is sent.
prejoin-mirror = Mirror my video
prejoin-join = Join now
prejoin-camera-off = Camera off
prejoin-camera-at-join = Your camera turns on when you join
prejoin-camera-starting = Starting camera…
prejoin-camera-failed = Camera unavailable
prejoin-no-camera = No camera found
prejoin-no-microphone = No microphone found
prejoin-camera-blocked = Camera blocked
prejoin-microphone-blocked = Microphone blocked
prejoin-no-camera-hint = Connect a camera to turn it on.
prejoin-no-microphone-hint = Connect a microphone to turn it on.
prejoin-camera-blocked-hint = Allow the camera for this site in the browser (the icon in the address bar).
prejoin-microphone-blocked-hint = Allow the microphone for this site in the browser (the icon in the address bar).
effect-none = None
effect-blur-light = Light blur
effect-blur-strong = Strong blur
effect-group-photos = Photos
effect-meeting-room = Meeting room
effect-open-plan-office = Open-plan office
effect-home-office = Home office
effect-minimal-office = Minimal office
effect-living-room = Living room
effect-bright-room = Bright room
effect-library-shelves = Library shelves
effect-loft = Loft
effect-sofa-beige = Beige sofa
effect-white-panels = White panels
effect-lounge = Lounge
effect-grey-lounge = Grey lounge
effect-orange-wall = Orange wall
effect-library-aisle = Library aisle
effect-empty-room = Empty room
effect-lake-morning = Lake in the morning
effect-group-illustrations = Illustrations
effect-office = Office
effect-bookshelf = Bookshelf
effect-sunrise = Sunrise
effect-forest = Forest
effect-more = More backgrounds
effect-fewer = Fewer backgrounds
effect-upload = Upload image
effect-upload-too-large = This picture is larger than { $limit }.
effect-upload-unreadable = The picture could not be read.
effect-delete = Delete background { $name }

## Incoming calls
incoming-title = Incoming call
incoming-audio = { $name } is calling you
incoming-video = { $name } is video calling you
incoming-decline = Decline
incoming-accept-audio = Accept with audio
incoming-accept-video = Accept with video

## Settings: display, sidebar, language, privacy, devices, calls
calls-devices = Default devices
calls-devices-hint = Used when you join a call. You can still change them before joining.
calls-system-default = System default
calls-joining = When joining
calls-start-muted = Join with the microphone off
calls-start-camera-off = Join with the camera off
calls-audio = Audio
calls-noise = Noise suppression
calls-echo = Echo cancellation
calls-ptt = Push to talk
calls-ptt-hint = Hold a key to talk instead of an open microphone.
calls-status-sounds = Call status sounds
calls-status-sounds-hint = A soft tone repeats while your connection is lost, and short tones tell when it is back or the call ended. Only you hear them.
calls-status-sounds-all = Mine and others' connections
calls-status-sounds-own-only = Only my connection
calls-status-sounds-off = Off
calls-background = Default background
calls-background-hint = Applied on your device before your video is sent.

## First-run key setup
# ---- Meetings ----
call-connecting = Connecting…
call-reconnecting = Connection lost, reconnecting…
control-deafen = Mute the call audio
control-undeafen = Unmute the call audio

## Activity, last seen and call history
calls-title = Calls
calls-filter = Filter calls
calls-all = All calls
calls-missed = Missed
calls-empty = Calls you make, take or miss show up here.
calls-empty-title = No calls yet
calls-direction-missed = Missed
calls-direction-incoming = Incoming
calls-direction-outgoing = Outgoing
calls-ongoing = Ongoing
calls-call-back = Call back
calls-join = Join

## Key names in keyboard shortcuts (the caps shown on a keyboard)
calls-unavailable = Calls aren't available here. Use your other TeamCraft tab or the TeamCraft app.

## Failed actions (an error toast) and the panic overlay
error-call-microphone-denied = The browser blocked the microphone. Allow it for this site (the icon in the address bar), then join again.
error-call-microphone-missing = No microphone was found. Connect one, then join again.
error-call-microphone-in-use = The microphone can't be opened. Close other apps that use it and try again.
error-call-camera-denied = The browser blocked the camera. Allow it for this site (the icon in the address bar), then turn the camera on again.
error-call-camera-missing = No camera was found. Connect one, then turn the camera on again.
error-call-camera-in-use = Another app is using the camera. Close that app, or choose another camera.
error-call-camera-unsupported = This camera's picture format isn't supported, so your video stays off. Try another camera.
error-call-camera-not-connected = The iPhone camera didn't connect: it was declined on the iPhone, or the iPhone is locked or too far away. Allow it on the iPhone, or choose another camera.
error-call-camera-unavailable = The camera can't be opened. Choose another camera, or try again.
error-call-screen-unavailable = Your screen can't be shared from here.
error-call-screen-denied = macOS doesn't let TeamCraft share your screen, even if TeamCraft is switched on there. In System Settings → Privacy & Security → Screen & System Audio Recording, select TeamCraft and remove it with −. Then quit and reopen TeamCraft and share again; allow it if macOS asks.
call-screen-settings-open = Open System Settings
error-call-screen-needs-click = Your browser didn't open the screen picker. Select Share screen again, then choose what to share.
error-call-camera-no-picture = Your camera turned on but sends no picture, so your video stays off. Check that no other app uses it, or choose another camera.
error-call-camera-stopped = Your camera stopped, so your video is off: it was disconnected, or the system or another app took it. Turn it on again, or choose another camera.
error-call-screen-no-picture = Your screen share started but sends no picture, so it was stopped. Try sharing again, or choose another screen or window.
error-call-camera-black = Your camera sends only black. Check that it isn't covered or closed with a privacy shutter, or choose another camera.
error-call-screen-black = Your screen share shows only black. If you just allowed screen recording, quit and reopen TeamCraft and share again. Protected videos also show black.
error-call-unsupported = This browser can't make calls. Use a current Chrome, Edge, Firefox or Safari, or the TeamCraft app.
error-call-device-limit = Your account is already in this call on as many devices as are allowed. Leave the call on one of them first, then join from this device.
error-call-server-unreachable = The call server can't be reached. The network may block calls (UDP); try another network or ask your admin.
error-call-background-required = Your organization requires a background behind your camera, which can't be added here. Backgrounds need WebGPU: turn your camera on in Chrome, Edge or Safari, or in the TeamCraft app.
error-call-effects-unavailable = The background effect couldn't start here, so your camera is on without it.
error-call-encryption-pending = This device can't join the call yet: it is still being let into the conversation's encryption. Keep TeamCraft open on another of your devices, or wait for a member to come online, then join again. For a meeting, its organizer may have to open it once first.
error-call-move-refused = The call wasn't moved: the meeting room doesn't match what was announced in the conversation. Stay in this call, or join the meeting room from the list if you trust it.
error-call-speakers-unavailable = The chosen speakers can't be used here. The test sound played on the default output.
error-call-speakers-missing = No speakers were found. Connect speakers or headphones and try again.
error-call-sound-blocked = The browser didn't let the page play sound. Click anywhere on the page, then try again.

## Settings: sections, search and rows (#327)
calls-noise-hint = Filters typing, fans and other background noise.
calls-echo-hint = Stops others from hearing themselves through your speakers.

# Files view and setup checklist (issue #301)
calls-loading = Loading calls
