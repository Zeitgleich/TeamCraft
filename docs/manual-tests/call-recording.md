# Manual test: call recording (issue #216)

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
- [ ] A sets "Everybody except guests": B's desktop app shows it; a guest in a meeting never does;
  the browser never does (it doesn't record yet).
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
- [ ] Quit the app while recording, start it again: no card, and the app's cache folder
  `call-recordings` is empty. The others saw the recording end when A's device left the call.
- [ ] Sign out after a recording without posting it: the take is gone.
- [ ] While recording and while the card waits, the file in `call-recordings` is not playable and
  doesn't start with `OggS` (it is encrypted, #2129); `strings` shows nothing readable. Posting still
  gives B a file that plays, and afterwards `call-recordings` is empty.

## 4. Logs

- [ ] The desktop log has "call recording started" and "call recording finished" with the length,
  never the file name or path. The SFU log shows nothing about recordings at `info`.
