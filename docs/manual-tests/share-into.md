# Sharing into TeamCraft from other apps

Manual checks for #208 (and #1037 for the iOS share sheet's picker). Everywhere: nothing is uploaded before you picked a conversation and pressed
send, and the picker offers only conversations you may post in. Use a neutral test file; a video of a
few GB for the large-file checks.

## Android

- [ ] Share a photo from the gallery: the picker opens, the notes and recent conversations on top; a
      pick opens the composer with the photo attached.
- [ ] Share a page from a browser: the composer has the title above the address.
- [ ] Direct share: a recent conversation in the system sheet opens its composer without the picker.
- [ ] Share a large video, wait for the picker, kill the app (`adb shell am kill` with it in the
      background, or swipe it away), open it again: the picker comes back with the video.
- [ ] Rotate the phone after picking: the picker does not open again.
- [ ] Open the app from Recents after a finished share: the share does not come back.
- [ ] Share while signed out: a notice says it is kept; after the sign-in the picker opens. "Discard"
      removes it.
- [ ] Share with the app lock on: the lock screen says it is kept; after unlocking the picker opens.
- [ ] Share eleven files: ten arrive, the picker says one was left out.
- [ ] Sign out with a share waiting, sign in: it is gone.

## iOS

Needs a signed build: the extension and the app talk through the App Group.

- [ ] The share sheet offers TeamCraft for photos, videos, files, a Safari page and selected text.
- [ ] Share a photo: the sheet says it was saved for TeamCraft, and a notification appears. A tap on it
      (or opening the app) shows the picker; a pick opens the composer with the photo.
- [ ] Share a video of a few GB: no crash of the extension; the composer gets it.
- [ ] Share a Safari page: the composer has the address.
- [ ] Share with the app signed out: the notice says it is kept; after the sign-in the picker opens.
- [ ] Share with the app lock on: the picker shows only after unlocking.
- [ ] Decline notifications for the app: the sheet still says to open TeamCraft.
- [ ] Sign out with a share waiting: it is gone after the next sign-in.
- [ ] Offline: pick a conversation and send; the message waits in the outbox and goes out later.

Picker in the share sheet (#1037). Open the app once after installing, so it lists the conversations
for the extension.

- [ ] Share a photo: the sheet lists "Notes" (bookmark) and the recent conversations on top, then all
      by name, with a search field and a message field. Pick a conversation: the sheet says which one
      opens, the notification does not name it. Opening the app (from the notification or the icon)
      shows that conversation with the photo and the message in the composer, without the app's
      picker. Nothing is sent before you press send.
- [ ] Search finds a channel by its name and by its space's name.
- [ ] "Choose in TeamCraft": the app's picker asks, as before.
- [ ] "Cancel" while a big video is still copying: nothing waits in the app afterwards.
- [ ] Share a video of a few GB and pick a conversation while it copies: the sheet waits for the copy,
      the extension is not killed, the composer gets the video.
- [ ] Kill the app (swipe it away), share and pick, then start the app: it opens the conversation.
- [ ] Flight mode: share and pick, start the app, send: the message waits in the outbox and goes out
      once online, once.
- [ ] Leave the picked conversation (or have yourself removed) before opening the app: the app's
      picker asks and says the conversation can no longer take it.
- [ ] App lock on, then share: the sheet shows no names, only the old card ("open TeamCraft to
      choose"); the picker shows after unlocking.
- [ ] Sign out, share: no names in the sheet. Sign in as somebody else: the old list is not shown.

## Desktop

- [ ] Linux: drag a file over 1 GB from the file manager onto a conversation: it attaches at once,
      memory of the webview stays flat, and it sends.
- [ ] Linux: TeamCraft is not listed under "Open with" in the file manager, and the task bar shows its
      own icon.
- [ ] Linux, Dolphin: right click on several files > "Send to TeamCraft": the window comes forward with
      the picker. Same on a file with spaces and quotes in its name, and with the app closed.
- [ ] Linux, Files (Scripts), Nemo, Caja, Thunar (after a restart): the same entry does the same.
      Thunar's other custom actions are unchanged.
- [ ] Linux: Settings > Preferences > "Show "Send to TeamCraft" in the file manager" off: every entry
      is gone from the menus and the folders (`grep -rl X-TeamCraft-Generated ~/.local/share ~/.config`
      finds nothing); a file of your own with the same name is still there. On again: back.
- [ ] Linux: change the language (`LANG=de_DE.UTF-8`), start the app: the Nautilus/Caja script is called
      "An TeamCraft senden" and the old one is gone.
- [ ] Linux, AppImage: move the file, start it: the entries run the new path. Flatpak: the entry
      works with a file in the home folder (the document portal).
- [ ] `teamcraft-desktop --share -- a.pdf b.png` from a terminal, app closed: the app starts and offers
      the picker after the sign-in. With "start minimized" on, the window shows.
- [ ] Windows (NSIS): Explorer > Send to > TeamCraft with several files. The entry goes with the
      uninstaller.
- [ ] macOS: Finder > Open With > TeamCraft, and files dropped on the dock icon. TeamCraft is not the
      default app of anything.
- [ ] Close the picker: a notice offers "Choose" and "Discard".
- [ ] A file above the instance's upload limit: an error names it and the limit, before any upload.
- [ ] Pick a conversation while offline and send: the message waits in the outbox.

## Web (installed app, Chrome or Edge on Android, ChromeOS or Windows)

The manifest declares the share target (#1324). Safari has no share target at all.

- [ ] Install the app, reopen it once so the new service worker runs, then share a picture from
      another app: TeamCraft is offered, opens, and shows the picker with the file's name and size.
- [ ] Share a page: the composer has its title and address.
- [ ] Close the app with the picker open and open it again: the share is still there. The network tab
      never shows the file going to `/share-target`.
- [ ] Share while signed out: the notice says it is kept; after the sign-in the picker opens.
- [ ] A form on another site that posts to `/share-target` with its referrer hidden: the picker says it
      came from outside and lists the files; after five waiting shares the next says it failed.
- [ ] With the service worker unregistered, `curl -i -X POST https://<host>/share-target`: `303` to
      `/?share-failed`, from the proxy without the body being buffered; the page shows the notice.
- [ ] Paste a picture and a copied file into the composer: both attach.
