# Manual test: call recording (issues #216, #2116, #2250)

What the automated tests can't show: the indicator, banner and tone on real screens, the file a
real call produces, and what people who join late see. Design: [e2ee.md](../e2ee.md) §8.3, admin
setting: [admin.md](../admin.md) §3.13.

Write down for every run: date, server and SFU version, app versions and platforms. Mark each item
✅, ❌ (with what happened) or "n/a".

## 0. Setup

- Three accounts: **A** (owner) on the desktop app, **B** (member) on the desktop app or in the
  browser, **C** (member) on an Android phone. A DM or channel all three are in.
- Administration → Security → Call recording: start with **Nobody** (the default).

## 1. Policy

- [ ] With "Nobody", no device shows a record button in a call.
- [ ] A sets "Owners and admins": A's desktop app shows "Record"; B's doesn't.
- [ ] A sets "Everybody except guests": B's desktop app and B in the browser (Chrome, Firefox,
  Safari 26) show it; a guest in a meeting never does.
- [ ] The longest recording can be set to 30 minutes to 8 hours; it is greyed out while recording
  is off.

## 2. Notice

- [ ] A starts recording. Within a second B and C see "Recorded by A": B in the header and on A's
  tile, C as a pill above the controls. B and C hear a short rising three-tone sound (also with
  the call's status sounds turned off in settings). A's header shows "Recording 0:00" counting up.
- [ ] B's banner says "A started recording this call" with "Leave" and "Continue". "Continue"
  closes the banner; the header indicator stays and can't be closed.
- [ ] D (another member) joins while the recording runs: the banner says "This call is being
  recorded by A" right after joining.
- [ ] A stops: the indicators go away on every device within a second.
- [ ] A starts again, then A's network drops for ~10 s (Wi-Fi off/on): the others keep seeing the
  recording, and after A is back it still shows (A re-announces it).
- [ ] A leaves the call while recording: the indicator goes away for the others; A gets the take.
- [ ] A screen reader (Orca, VoiceOver, TalkBack) reads the banner and the indicator.

## 3. The recording

- [ ] After stopping, A sees the "Call recording" card with length and size, also after the call
  ended. "Post to conversation" posts an `.ogg` file into the call's conversation; B can play it
  and hears everybody, A included (not while A was muted).
- [ ] "Delete" asks first, then the card goes and nothing is posted.
- [ ] With the longest recording set to 30 minutes (or a debug build with a short limit), the
  recording stops by itself, the others see it stop, and the card says it stopped at the limit.
- [ ] Quit the app while recording, start it again: the card says "A recording from <time> was not
  finished" (or "… is waiting" when the recording was stopped first), with "Post to conversation",
  "Save as file…" and "Delete". The others saw the recording end when A's device left the call.
- [ ] Kill the app while recording (`kill -9`, Task Manager → End task) after about a minute,
  start it again: the card says "was not finished"; its length is up to about 16 seconds shorter
  than what was recorded. "Post to conversation" posts an `.ogg` that B plays to its end.
- [ ] Kill it during two recordings in a row (start, kill, start the app, record again without
  answering the card, kill): at the next start the older recording is offered first, the newer
  one after the first is posted or deleted.
- [ ] Sign out with a recording left from a kill: at the next sign-in no card, and
  `call-recordings` is empty.
- [ ] Sign out after a recording without posting it: the take is gone.
- [ ] "Save as file…" asks first and warns that the file is unencrypted; "Save unencrypted" opens
  the system's save dialog (cancel: nothing happens). The saved `.ogg` plays in the system's
  player; the card stays, and "Post to conversation" still works afterwards.
- [ ] Save over an existing file on a nearly full disk (or a USB stick filled up): the error shows,
  the old file is unchanged and no `.….partial` file stays next to it.
- [ ] Administration → Security → Call recording: clear "Allow saving recordings as files". A's
  card no longer offers "Save as file…" (after the policy reached A's app).
- [ ] While recording and while the card waits, the file in `call-recordings` is not playable and
  doesn't start with `OggS` (it is encrypted, #2129); `strings` shows nothing readable. Posting still
  gives B a file that plays, and afterwards `call-recordings` is empty.

## 4. In the browser (#2114)

B records in the browser; repeat in Chrome, Firefox and Safari.

- [ ] B starts recording: A and C see "Recorded by B" and hear the rising tone; B's own browser
  plays the tone when A starts recording too.
- [ ] B records two minutes, talking and muting in between, then stops: the card shows the length
  and size (about 240 kB a minute). "Post to conversation" posts an `.ogg` that A plays: everybody,
  B included except while muted.
- [ ] DevTools → Application → Storage (Chrome) while recording: the origin private file system
  has `teamcraft-call-recordings/<page>/recording-0`, growing about every 16 seconds, never
  starting with `OggS`. After posting, the sealed file is gone (a `post-…` copy stays until
  sign-out).
- [ ] B switches microphone and speakers while recording: the recording goes on (a moment of
  silence at most).
- [ ] B puts the tab in the background for five minutes while recording: the recording has no gap.
- [ ] B reloads the tab while recording: A and C see the recording end when B's device leaves the
  call; after the reload there is no card, and the next call module start removes the old folder
  (Application → Storage).
- [ ] B records in a Firefox private window (no OPFS): it records in memory; with the longest
  recording set above two hours, it stops after two hours and the card says the browser can't keep
  more.
- [ ] B leaves the call while recording: A and C see it stop; B gets the card.
- [ ] B chooses "Save as file…", confirms the warning: the browser downloads
  `call-recording-<date>-<time>.ogg`, which plays; the card stays.
- [ ] B signs out with a take waiting: the take and B's folder are gone.
- [ ] Two tabs of B's account: the recording runs in the active tab; making the other tab active
  (the app moves) ends it and deletes the take.

## 5. Video (#2250)

Administration → Security → Call recording: "Everybody except guests" and "Record video too".

- [ ] The switch is greyed out while recording is off; B's console shows it after A signed it.
- [ ] A (desktop) records a call of A, B and C with B's camera and C's camera on, for about a
  minute, switching between "Gallery" and "Speaker" once. The card says "Video · 1:0x · …"; posted,
  it is an `.mp4` that plays in VLC and in a browser (and in the Windows media player) with
  picture and sound in sync. It shows the tiles as A's screen did, the layout switch included, A's
  own camera too.
- [ ] B shares a screen during the recording: the share is large in the file from then on, with
  the cameras beside it; it ends when B stops sharing.
- [ ] With "Ask everybody for consent": C declines. C's tile in the file is a grey tile with "Not
  recorded" (in A's language), never C's picture; after C allows it, C's picture appears.
- [ ] About 160 MB an hour: a ten-minute recording is roughly 25 to 30 MB.
- [ ] Kill A's app after about a minute of video: at the next start the card offers it as "not
  finished"; the posted `.mp4` plays to within a few seconds of the kill.
- [ ] B in the browser and C on the phone record the sound only (an `.ogg`), with the same setting.
- [ ] Cost on A's computer while recording a gallery of four cameras: CPU and memory (Task
  Manager, Activity Monitor, `top`) stay about where they are without video plus one encoder; the
  memory doesn't grow over an hour.

## 6. Logs

- [ ] The desktop log has "call recording started" and "call recording finished" with the length,
  never the file name or path. The SFU log shows nothing about recordings at `info`. A recording
  with video adds "call recording video started" (size and rate) and "call recording video ended"
  (pictures and frames dropped), and which encoder it used.
