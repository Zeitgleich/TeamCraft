# TeamCraft UI – English (source language)
# Choosing a server and signing in.

## Sign-in
login-title = Sign in
login-subtitle = Use your company account to continue.
login-use-demo = Use demo data instead
login-use-other-account = Sign out and use another account
login-brand-links = About this service
login-welcome = Welcome to TeamCraft
login-welcome-brand = Welcome to { $brand }'s TeamCraft server
login-brand-support = Support
login-brand-privacy = Privacy
login-brand-terms = Terms
login-brand-imprint = Imprint
login-checking = Checking your session…
login-signing-in-title = Continue in your browser
login-signing-in = Finish signing in in the browser window that just opened.
login-no-role-title = Your account has no access to TeamCraft
login-no-role-body = You signed in, but your account has no TeamCraft role yet. Ask an admin to give you access, then sign in again.
login-deactivated-title = Your account is deactivated
login-deactivated-body = An admin deactivated your account. Ask an admin to reactivate it; your chats and devices are kept.
login-unreachable-title = Can't reach the server
login-unreachable-body = Check your connection and the server address, then try again.
login-cancelled-title = Sign-in was cancelled
login-cancelled-body = Nothing was changed. Sign in again when you are ready.
login-failed-title = Sign-in did not work
login-failed-body = Try again. If it keeps failing, give the details below to your admin.
server-entry-title = Enter your server address
server-entry-subtitle = Your company or team runs its own TeamCraft server. Ask your admin for its address if you don't know it.
server-entry-label = Server address
server-entry-hint = For example team.example.com. You can also paste an invitation or a message link.
server-entry-continue = Continue
server-entry-confirm-title = Is this your server?
server-entry-confirm-body = Continue only if you know this server. You will sign in with your company account next.
server-entry-confirm = Continue to sign-in
server-entry-other = Use another server
server-entry-current = Server: { $host }
server-entry-change = Change server
server-entry-provider = You will sign in at { $host }.
server-entry-provider-elsewhere = This server signs you in through { $host }, which is not part of its own domain. Continue only if your organization uses this sign-in service; otherwise someone could be impersonating your server.
server-entry-signed-in = Sign out before you change the server.
server-entry-empty = Enter the address of your server.
server-entry-invalid-address = This is not a valid server address.
server-entry-https-only = Only secure (https) server addresses are supported.
server-entry-has-credentials = Leave out user names and passwords; enter only the server address.
server-entry-not-teamcraft = No TeamCraft server answered at this address. Check the address with your admin.
server-entry-captive-portal = The network wants you to sign in first (for example hotel or train Wi-Fi). Open a web page in your browser, sign in to the network, then try again.
server-entry-unavailable = The server is having problems right now. Try again later.
server-entry-app-too-old = This version of the app is too old for this server. Update the app, then try again.
server-entry-server-too-old = This server runs an older TeamCraft version than this app supports. Ask your admin to update it.
server-entry-invalid-sign-in = The server's sign-in settings are not usable. Give the details below to your admin.
server-entry-redirect-not-allowed = This server is not set up for this app. Ask your admin to allow it.
server-entry-unreachable = Can't reach the server. Check your connection and the address, then try again.
server-entry-tls = The secure connection to the server failed. The network may be intercepting it, or the server's certificate is not valid.
server-entry-locked = Your organization has set up this app for one server only.
server-entry-not-checked = The server has to be checked again. Continue once more.
server-entry-not-saved = The server could not be saved on this device.
server-entry-failed = Something went wrong. Try again.
server-entry-unknown-host = There is no server at this address. Check the spelling.
server-entry-did-you-mean = Did you mean { $host }?
server-entry-check-alternatives = Check similar addresses
server-entry-no-alternative = No similar address found. Check the address with your admins.
server-entry-provider-label = Sign-in service
login-details = Technical details

## First-run key setup
login-core-failed-title = Your encrypted data could not be opened
login-core-failed-body = The local database or the system keystore is not available. Check that a keyring service (for example GNOME Keyring) is running, then try again.
login-core-stopped-title = TeamCraft hit an internal error
login-core-stopped-body = Your encrypted data is safe, but the app stopped unexpectedly. Try again or restart TeamCraft. If it keeps happening, please report it.
login-core-no-token-title = Sign-in is not complete yet
login-core-no-token-body = TeamCraft did not receive your sign-in in time to open your encrypted data. Try again, or sign out and sign in again.
login-store-damaged-title = The data on this device can't be read
login-store-damaged-body = TeamCraft's encrypted data on this device is damaged, or the key that opens it is gone. Your messages are not lost: set this device up again and unlock your keys with your recovery passphrase, or link it from another device. On a computer, the old data is kept aside, not deleted.
login-store-newer-title = This data is from a newer TeamCraft
login-store-newer-body = A newer version of TeamCraft changed the data on this device, and this version can't read it. Update TeamCraft or reload the page to get the newest version.
login-store-reset = Set up this device again
login-store-reset-confirm-desktop = Set this device up again? TeamCraft keeps the unreadable data beside the new data, so nothing is deleted, but this device starts empty and fills again from the server once you unlocked your keys.
login-store-reset-confirm-web = Set this browser up again? The unreadable data is deleted from this browser for good. Your messages stay on the server and come back once you unlocked your keys with your recovery passphrase or from another device.
login-store-reset-confirm-button = Yes, set up again
login-store-reset-cancel = Cancel
login-store-reload = Reload
login-core-retry = Try again
login-core-sign-out = Sign out
login-opening-core-title = Opening your encrypted data
login-opening-core = Connecting…

# Sign-in card: methods, local accounts (e-mail, password, one-time code), help
login-local-email = E-mail address
login-local-password = Password
login-local-code = One-time code
login-local-code-hint = The 6-digit code from your authenticator app.
login-local-refused = E-mail address, password or one-time code is wrong.
login-local-failed = Sign-in did not work
login-local-submit = Sign in
login-sso = Sign in with { $provider }
login-or = or
login-subtitle-local = Sign in with your e-mail address and password.
login-subtitle-choice = Choose how you want to sign in.
login-local-show-password = Show password
login-local-code-optional = One-time code (if you set one up)
login-local-disabled = Sign-in with e-mail and password is not available on this server.
login-local-rate-limited = Too many attempts. Wait a few minutes, then try again.
login-local-network = Can't reach the server. Check your connection, then try again.
login-local-working = Signing in…
login-help-link = Help with signing in

## Getting started (empty workspace)
# Runtime settings: brands, mail, calendar and instance settings in the admin console
login-organization = Operated by { $name }
