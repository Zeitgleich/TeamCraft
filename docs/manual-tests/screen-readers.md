# Manual test: screen readers and keyboard only

What automated checks can't show: what a screen reader actually says, and whether a journey can be
finished without a mouse or without sight. Run it before a minor release on at least one row of each
platform in [accessibility.md](../accessibility.md), "Test matrix", and after changes to the message
list, the composer, calls or navigation. Target and known issues: [accessibility.md](../accessibility.md).

Write down for every run: date, app version, platform and OS version, screen reader and its version,
browser (web). Mark each item ✅, ❌ (with what was heard or what got stuck) or "n/a". File what
fails with the `ux` label and add it to accessibility.md.

## 0. Setup

- Two accounts, **A** (the tester) and **B** (sends messages and calls). A shares a channel and a DM
  with B; the channel has some history (more than a screen) and a thread.
- Desktop/web: the mouse put away. Mobile: the screen reader on, the screen curtain on if you can.

## 1. Sign-in and linking

1. Sign in, link a device (enter or scan the code), set up the recovery kit. Every field has a name;
   a wrong entry is read at its field; the focus moves to the first error.

## 2. Reading

1. Open the channel from the sidebar. The channel's name is heard.
2. Move through the messages (arrow keys on desktop, swipe on mobile). Each message says who, when and
   what, once; scrolling back through history reads nothing on its own.
3. B sends a message. A hears it once ("B: …"), also while scrolled up ("1 new message").
4. Open the thread, read it, close it: the focus returns to the message.

## 3. Writing

1. Write a message with an @mention and an emoji from the suggestions: the number of suggestions and
   the current one are heard; Enter picks it.
2. Send, edit and delete an own message from the keyboard (desktop: Shift+F10 on the message;
   mobile: the actions rotor or custom actions). After delete the focus stays in the list.
3. React, forward and save a message without a mouse or a long press.
4. Record a voice message: the timer isn't read every second; "Recording" is heard on the button.

## 4. Calls

1. B calls A: A answers without a mouse.
2. In the call, step through the participants one by one; each says their name and state.
3. Wait 10 s with video on: the controls stay reachable with the screen reader on.
4. Mute, unmute, switch the camera, leave. B starts a recording: A hears it.

## 5. Settings and admin

1. Change a notification setting of a conversation: every switch and radio button says its name.
2. Admin console (an admin account): move through the navigation and one table.

## 6. Display

1. 200 % text size (desktop zoom, Android font scale, iOS Dynamic Type at an accessibility size): the
   chat list, a conversation, the composer and the call screen show everything, nothing cut off.
2. Reduced motion on: no slide or spring animations, animated images still.
3. High contrast / Increase Contrast: text and controls stay readable.
