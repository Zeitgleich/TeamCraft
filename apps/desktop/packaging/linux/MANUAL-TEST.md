# Manual test: calls on Wayland (GNOME and KDE)

Screen sharing, the camera and push-to-talk depend on the desktop's portals, which only a real
session exercises fully. `portal-test/run.sh` covers the screen-share path headlessly (sway and
xdg-desktop-portal-wlr); this checklist covers GNOME (Mutter) and KDE Plasma (KWin) with real
dialogs. Run it with the Flatpak, and once with the `.deb`/`.rpm` (outside the sandbox, the same
portals are used).

## Setup

1. Build and run the Flatpak: `apps/desktop/packaging/linux/build-flatpak.sh --run`
   (or install the bundle: `build-flatpak.sh --bundle`, then
   `flatpak install --user ~/.cache/teamcraft-flatpak/teamcraft-desktop.flatpak`).
2. Check the session type: `echo $XDG_SESSION_TYPE` must print `wayland`.
3. Versions to note in the result: desktop (`gnome-shell --version` or `plasmashell --version`),
   `xdg-desktop-portal` (`/usr/libexec/xdg-desktop-portal --version`), PipeWire (`pipewire --version`),
   GPU.
4. Have a second participant (another machine, or the web client in a browser) in the same call
   to see what is sent.

## Checklist

Mark each line with pass/fail and a note. "Dialog" is what the desktop should show.

### Screen sharing

| #  | Step                                                                          | Expected                                                                                                                                                                               |
|----|-------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| S1 | Start a call, click "Share screen"                                            | Dialog: GNOME "Share Screen" with tabs Screens/Windows; KDE "Screen Sharing" list of screens and windows. The app is named TeamCraft (Flatpak) or shows the app id                         |
| S2 | Pick a whole monitor, confirm                                                 | The other participant sees the monitor within about 2 s; the cursor is visible in the shared image                                                                                     |
| S3 | Multi-monitor: stop, share again, pick the second monitor                     | Only the second monitor is shared, at its own resolution; mixed scaling (e.g. 100 % and 150 %) shows no stretching                                                                     |
| S4 | Share again                                                                   | GNOME offers "Remember this selection"/restores; KDE may skip the dialog (restore token). If restored, the same monitor is shared                                                       |
| S5 | Pick a single window (a text editor), confirm                                 | Only that window is visible to others; moving or resizing it keeps it shared; covering it with another window doesn't leak the other window                                            |
| S6 | Minimize the shared window                                                    | The stream pauses or shows the last frame; no crash; restoring it resumes                                                                                                              |
| S7 | Close the shared window                                                       | Sharing ends (or shows a frozen frame) and the call continues                                                                                                                          |
| S8 | Cancel the dialog                                                             | No error dialog; the share button returns to its idle state                                                                                                                            |
| S9 | Stop sharing from the desktop (GNOME: top-bar indicator; KDE: tray indicator) | Sharing stops in the app as well                                                                                                                                                       |
| S10| Share with camera on                                                          | Both streams reach the other participant; screen text stays readable                                                                                                                   |

### Camera

| #  | Step                                        | Expected                                                                                                                       |
|----|---------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------|
| C1 | Turn the camera on (first time)             | Dialog: "Allow TeamCraft to use the camera?" (GNOME); KDE asks through its permission dialog. Inside the Flatpak this is the only way in |
| C2 | Allow                                       | Self-view and the other participant see the camera within about 2 s                                                             |
| C3 | Turn it off and on again                    | No second dialog (the permission is remembered); `flatpak permission-show de.zeitgleich.teamcraft` lists `camera`                  |
| C4 | Deny (reset first: `flatpak permission-reset de.zeitgleich.teamcraft`) | The app reports that camera access was denied; the call continues                                    |
| C5 | Background blur on                          | Blur applies; CPU usage stays reasonable (note it)                                                                              |
| C6 | Unplug the camera during a call (USB)        | The camera stops; no crash                                                                                                      |

### Push-to-talk

| #  | Step                                                         | Expected                                                                                                       |
|----|--------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------|
| P1 | Settings: enable push-to-talk (default Ctrl+Shift+Space)     | Dialog: GNOME 48+ / KDE "Global Shortcuts" asks to bind "Push to talk"; the user may change the key             |
| P2 | Focus another app, hold the key                              | The microphone opens while held and closes on release; others hear only while held                              |
| P3 | Change the key in the desktop's settings (GNOME: Settings → Apps → TeamCraft → Global Shortcuts; KDE: System Settings → Shortcuts) | The new key works without restarting the app |
| P4 | X11 session (log in with "GNOME on Xorg" or "Plasma (X11)")  | No dialog; the shortcut works through the X11 grab                                                              |

### Rest of the sandbox

| #  | Step                                    | Expected                                                                                              |
|----|-----------------------------------------|-------------------------------------------------------------------------------------------------------|
| R1 | Sign in                                 | The system browser opens the login page (OpenURI portal); the app receives the result                  |
| R2 | Quit and restart                        | Still signed in (refresh token in GNOME Keyring / KWallet)                                             |
| R3 | Mention notification with the app in the background | A desktop notification appears                                                            |
| R4 | Attach a file                           | The file dialog is the desktop's (FileChooser portal); only the chosen file is visible to the app      |
| R5 | First video use                         | OpenH264 is downloaded once (`~/.var/app/de.zeitgleich.teamcraft/data/de.zeitgleich.teamcraft/codecs/`)    |
| R6 | Hardware encoding (AMD/Intel)           | `flatpak run --env=GST_DEBUG=va*:3 de.zeitgleich.teamcraft` shows the VA encoder in use; else software    |
