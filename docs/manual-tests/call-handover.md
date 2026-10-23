# Manual test: calls across network changes on real phones

What the automated tests can't show: real radios, carrier NATs, the time Android and iOS take to
switch the default network, CallKit and Telecom during a switch. Run this before every release on at
least one Android phone and one iPhone, each with a SIM with mobile data. Target behaviour:
[ARCHITECTURE.md](../../ARCHITECTURE.md) §6.5.

Write down for every run: date, server and SFU version, app versions, phone model and OS version,
carrier, Wi-Fi network (home, office, guest). Mark each item ✅, ❌ (with what happened and how long
it took) or "n/a". For every switch note the **audio gap** (seconds without hearing the other side)
and whether the phone showed "Connection lost, reconnecting…".

## 0. Setup

- Production-like server and SFU (real certificate, SFU UDP 4433 reachable over IPv4 and IPv6).
  SFU logs at `info` so `participant reconnecting`/`participant joined … Resumed` lines are visible.
- Two accounts: **A** on the phone under test, **B** on a desktop app on a wired network.
- A and B share a DM. Wi-Fi and mobile data both on on the phone; mobile data allowed for the app.
- Something steady to listen to: B plays music into the microphone or holds a phone with a
  metronome next to it, so gaps are easy to hear and time.

Expected for every switch below unless the item says otherwise: the call goes on, B never sees A
leave, A's privacy code is the same afterwards for both (the group re-keyed; compare it).

## 1. Wi-Fi → LTE (connection migration)

- [ ] A calls B on Wi-Fi; both see the call secured and hear each other.
- [ ] Walk out of Wi-Fi range (or turn Wi-Fi off in the quick settings, not airplane mode). Gap
  ≤ 2 s; A shows **no** "reconnecting" when the connection migrated, B shows nothing either. SFU
  log: no `participant reconnecting` line.
- [ ] Same with the phone locked and the screen off (Android: Telecom call in the status bar; iOS:
  CallKit). Gap ≤ 2 s.
- [ ] Same during a video call (A's camera on): A's video freezes at most a few seconds and comes
  back sharp (a keyframe follows), audio continues.

## 2. LTE → Wi-Fi

- [ ] Start on mobile data, walk into a known Wi-Fi. Android and iOS switch the default network
  to Wi-Fi; the call continues. Gap ≤ 2 s. Note whether the phone used Wi-Fi afterwards (SFU log
  shows the new address or a resume).

## 3. Network gone for a while (rejoin)

- [ ] Airplane mode on for 10 s, then off. A shows "Connection lost, reconnecting…" within ~5 s;
  B sees A's tile dimmed with "reconnecting". After airplane mode is off the call resumes within a
  few seconds of the phone having data again; B never sees A leave. SFU log: `participant
  reconnecting`, then `participant joined … Resumed`.
- [ ] Airplane mode on for 45 s (longer than the 30 s grace period): A's call ends with "The call
  was interrupted." (Android) / the failure note (iOS) after about 30 s; B sees A leave then.
- [ ] Lift or underground with flaky coverage: note how often A shows "reconnecting" and whether the
  call survives.

## 4. Captive and blocked networks

- [ ] Switch to a Wi-Fi with a captive portal that isn't logged in: the phone keeps mobile data as
  default (Android) or the call rejoins over mobile data once the OS gives up on Wi-Fi. Note the gap.
- [ ] A Wi-Fi that blocks UDP (guest network or a firewall rule dropping UDP 4433): joining a call
  there fails after the connection timeout; an ongoing call switched onto it ends after the grace
  period. Expected today (no TCP fallback, ARCHITECTURE.md §6.6) – note the message the user sees.

## 5. Both sides moving

- [ ] A and B both on phones, both switch Wi-Fi → LTE within a few seconds of each other: the call
  continues for both; if both had to rejoin, both see "reconnecting" and get back together.

## 6. Platform specifics

- [ ] Android: during "reconnecting" the Telecom call stays active (no system "call ended"); Android
  Auto (if available) keeps showing the call.
- [ ] iOS: CallKit keeps the call during "reconnecting"; the lock screen call banner stays.
- [ ] Android with a VPN app (always-on VPN): switching networks inside the VPN – note the gap.
- [ ] Dual-SIM or eSIM switch of the data SIM mid-call (if available).

## 7. After the run

- [ ] SFU log has no panics or `signaling queue full` lines; no participant stayed listed after its
  call ended.
- [ ] Note everything that took longer than the targets above in the release checklist.
