# TeamCraft UI – English (source language)
# General words, the app's frame (menus, window, tray, about, updates) and app-wide states.

## General
skip-to-content = Skip to main content
cancel = Cancel
save = Save
close = Close

## Compact layout (phones): bottom navigation, back, menus; calls in the browser
back = Back
history-back = Back
history-forward = Forward

## Sidebar
catching-up = Catching up…
catching-up-detail = Messages from while you were away are still loading.
key-catch-up = Getting the keys for older messages
key-catch-up-detail = Messages fill in when one of your other devices or a member comes online.
key-catch-up-progress = { $count ->
    [one] { $caught_up } of { $count } conversation caught up
   *[other] { $caught_up } of { $count } conversations caught up
  }

## Messages
badge-bot = BOT
imported-from-synology = Synology Chat
imported-from-other = another chat
imported-until = Imported from { $source } up to here
imported-title = Imported from { $source }
badge-guest = GUEST
badge-organization = Organization

## Sign-in
data-failed-title = Your data could not be loaded
data-failed-body = The server answered, but your spaces and conversations could not be read. Try again.
data-retry = Try again

## Settings: display, sidebar, language, privacy, devices, calls
language-en = English
language-de = Deutsch

## First-run key setup
# ---- Meetings ----
bundle-rejected = The server offered a version of this app that is not signed by the release key. You keep running the previous version. Do not enter secrets until your administrator has checked this.
bundle-unsigned = This web app is not signed by a release key, so nothing checks that the server sends the real code.
about-title = About
about-bundle-verified = Web app version { $version }, verified against release key { $key }.
about-bundle-unsigned = Web app: not signed (development build). Nothing checks the code the server sends.
about-bundle-unsupported = Web app: this browser cannot verify release signatures; the code is not checked.
about-bundle-rejected = Web app: a new version was refused ({ $reason }); the previous verified version is running.
about-bundle-desktop = This app does not take its code from the server.
about-trust-note = The web app trusts whoever serves it: a server that replaces the app can read your messages when you open it. The signed-release check catches changed files, not a server that replaces the checker itself. For the strongest protection use the desktop or mobile app, which carry their own code.
about-notices = Third-party notices

# Message actions
webview-download-saved = { $name } saved to Downloads
webview-download-saved-unnamed = Saved to Downloads
webview-download-failed = The download didn’t finish.
badge-pinned = Pinned
badge-saved = Saved

# Files and link previews
route-unavailable = This page isn't available. It may have been removed, or you may not have access to it.

# Workspace management
create = Create
confirm = Confirm
badge-external = External
badge-archived = Archived
badge-private = Private

# Profiles, people and groups (v1 REST UI)
weekday-mon = Mon
weekday-tue = Tue
weekday-wed = Wed
weekday-thu = Thu
weekday-fri = Fri
weekday-sat = Sat
weekday-sun = Sun

## Daily-use improvements (docs/ux-backlog.md)
offline-banner = You are offline. The app reconnects on its own when the network is back.
connection-banner-connecting = Connecting to the server…
connection-banner-unreachable = The server can't be reached. Messages wait until it is back. Next attempt in { $seconds } s.
connection-banner-captive-portal = This network wants you to sign in first (a login page answered instead of the server). Open a web page to sign in; messages wait. Next attempt in { $seconds } s.
connection-banner-untrusted-certificate = The server's certificate isn't trusted. On a network that inspects encrypted traffic, use another network or ask your IT to install the company certificate on this device; if it happens everywhere, the server's certificate may be broken: tell your administrator. Next attempt in { $seconds } s.
connection-hint-offline = Offline
connection-hint-connecting = Connecting…
connection-hint-reconnecting = Reconnecting…
connection-hint-slow = Slow connection
connection-banner-retry = Try now
connection-banner-sign-in = Your session has expired. Messages wait until you sign in again.
connection-banner-sign-in-action = Sign in again
connection-banner-session-ended = You were signed out at your organization's sign-in or on another device. Messages wait until you sign in again.
connection-banner-session-timed-out = Your session reached the time limit your organization set. Messages wait until you sign in again.
storage-quota-exceeded = Your browser has no storage space left for TeamCraft. New messages and keys are not saved on this device and are lost if you reload. Free up space, then keep this tab open: TeamCraft saves them as soon as it can.
storage-closed = A newer version of TeamCraft was opened in another tab. Reload this tab to keep working; until then, changes here are not saved.
storage-not-saved = TeamCraft can't save data in this browser. Changes are lost if you reload this tab.
storage-reload = Reload
release-ready = A new version of TeamCraft is ready.
release-too-old = This version of TeamCraft is no longer supported by the server (version { $version } or newer is needed).
release-reload = Reload now
release-switch-failed = The new version has not started yet. Try again in a moment.
undo = Undo
tray-show = Show window
tray-quit = Quit
tray-tooltip-unread = { $name }: { $count } unread
menubar-about = About { $name }
menubar-services = Services
menubar-hide = Hide { $name }
menubar-hide-others = Hide Others
menubar-show-all = Show All
menubar-quit = Quit { $name }
menubar-file = File
menubar-close-window = Close Window
menubar-edit = Edit
menubar-undo = Undo
menubar-redo = Redo
menubar-cut = Cut
menubar-copy = Copy
menubar-paste = Paste
menubar-select-all = Select All
menubar-view = View
menubar-fullscreen = Toggle Full Screen
menubar-window = Window
menubar-minimize = Minimize
menubar-zoom = Zoom
menubar-help = Help

## Desktop updates (docs/updates.md)
update-state-idle = TeamCraft is up to date.
update-state-unchecked = Not checked for updates yet.
update-state-checking = Looking for a new version…
update-state-downloading = Downloading TeamCraft { $version }…
update-state-downloading-percent = Downloading TeamCraft { $version }… { $percent } %
update-state-installing = Installing TeamCraft { $version }…
update-installing = Installing { $version }…
update-downloading = Downloading…
update-downloading-percent = Downloading… { $percent } %
update-waiting-authorization = Waiting for authorization…
update-waiting-authorization-hint = Confirm in the system’s password dialog to install TeamCraft { $version }.
update-state-failed = The last update check failed: { $message }
update-off-policy = Your organization manages updates of this app.
update-off-machine = Updates are turned off on this computer.
update-off-preference = TeamCraft does not look for new versions.
update-off-not_configured = This build cannot update itself. New versions are on the releases page.
update-off-unpackaged = Development build: no updates.
update-ready = TeamCraft { $version } is ready.
update-ready-banner = TeamCraft { $version } is ready — restart to update.
update-ready-installed = TeamCraft { $version } is installed — restart to use the new version.
update-restart-short = Restart
update-available = TeamCraft { $version } is available.
update-restart = Restart to update
update-restart-after-call = Restart after the call
update-download = Download
update-later = Later
update-whats-new = What’s new
update-package-manager = Update it with your package manager:
update-package-kit = Update now
update-flatpak = Update it with Flatpak or your software center:
update-copy-command = Copy
update-command-copied = Copied
update-download-package = Download the new package
update-download-appimage = Download the new AppImage
update-add-repository = Add the repository for automatic updates
update-dismiss = Dismiss
update-too-old = This version of TeamCraft is too old for your organization. Please update to { $version } or later.
update-too-old-self = The update is on its way; restart when TeamCraft says it is ready.
update-too-old-elsewhere = Your IT department or software center provides the update.

## Window controls of the desktop app's own title bar
window-controls = Window
window-minimize = Minimize
window-maximize = Maximize
window-restore = Restore
window-close = Close

## Failed actions (an error toast) and the panic overlay
error-details = Details
error-generic = That didn't work. Try again; if it keeps failing, copy the details for your admin.
area-load-failed = This part of the app could not be loaded. Check your connection, then try again. If it still fails, reload the app: a new version may have come out.
area-reload = Reload app
error-network = The server can't be reached. Check your connection and try again.
error-notification-not-allowed = Notifications are blocked for this app. Allow them in your browser's or system's settings, then try again.
error-notification-unsupported = This browser can't show notifications.
error-notification-refused = The system didn't show the notification. Check its notification settings, such as focus or do-not-disturb modes.
error-captive-portal = This network wants you to sign in first. Open a web page to sign in, then try again.
error-untrusted-certificate = The server's certificate isn't trusted. On a network that inspects encrypted traffic, use another network or ask your IT to install the company certificate on this device; if it happens everywhere, the server's certificate may be broken: tell your administrator.
error-not-space-member = This person isn't in the space yet. Invite them to the space first, then add them to the channel.
error-forbidden = You don't have permission for this. Ask an admin of the space or workspace.
error-signed-out = Your sign-in has expired. Sign in again, then try once more.
error-account-deactivated = Your account is deactivated. Ask an admin to reactivate it.
error-rate-limited = Too many requests at once. Wait a moment and try again.
error-too-large = That is too large for the server. Try something smaller.
error-server-trouble = The server has a problem right now. Try again in a few minutes.
error-conflict = Someone changed this at the same time. Look at the current state and try again.
error-not-found = This doesn't exist any more. It may have been deleted or moved.
panic-title = Something went wrong
panic-body = The app ran into an error and has to be reloaded. What you were typing just now may be lost.
panic-reload = Reload
panic-details-label = Technical details for your admin (version and where the error happened)

## One active tab (#744)
tab-elsewhere-title = TeamCraft is already open in another tab
tab-elsewhere-body = Use it here instead? The other tab becomes inactive.
tab-moved-title = TeamCraft was opened in another tab
tab-moved-body = This tab is inactive now. Use it here to continue in this tab.
tab-call-title = A call is running in the other tab
tab-call-body = Using TeamCraft here ends the call there.
tab-timed-out = The other tab didn’t respond. Try again.
tab-use-here = Use here
tab-end-call-use-here = End call and use here
tab-cancel = Cancel
tab-close = Close
tab-close-hint = You can close this tab now.

# Errors at a form field (forms.md)
form-error-name = Enter a name.
