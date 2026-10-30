# TeamCraft UI – English (source language)
# Setting up this device: keys, recovery, linking and approving devices, key changes.

## Device approval, linking and key changes
approval-title = New login
approval-question = Someone signed in on { $device }. Approve this device?
approval-code-label = Code { $code }
approval-compare = Compare this code with the one on the new device. Approve only if they match.
approval-location = Near
approval-warning = If you did not just sign in, choose "Not me". Approving gives the new device access to your messages.
approval-approve = Approve
approval-deny = Not me
approval-approved = Device approved. It is receiving your keys now.
approval-denied = Request denied. Review your devices in Settings if you are unsure.
link-title = Link this device
link-qr-label = QR code to link this device
link-qr-note = Demo code, not a real login.
link-step-open = On a device you are already signed in on, open Settings and choose Devices.
link-step-scan = Choose Link a new device and scan this code.
link-step-code = Check that both screens show the same code, then approve.
link-waiting = Waiting for approval…
link-done = Device linked. Your messages are loading.
link-alternative = No camera? Sign in here and approve the code on your other device instead.
link-simulate = Simulate approval
device-waiting-title = This device isn't fully set up for this conversation yet
device-waiting-body = You can read what your other devices could. New messages may stay locked until one of your other devices or a member of this conversation is online.
left-in-group-unknown = Someone
group-refused = The server doesn't let this device read the encrypted group of this conversation. New messages may be missing.
waiting-for-admission = Waiting to be let into this channel. A member's app lets you in when it's online, and this app asks again on its own. Only messages from when you joined on will be readable.
never-set-up = { $name } never set up TeamCraft. { $name } will see this conversation if they sign in on a device later.
waiting-first-device = { $name } hasn't finished setting up TeamCraft yet. You can write anyway: { $name } will see your messages once setup is done.
devices-unverified = { $name } can't be added to the encryption: none of their devices could be verified. Ask { $name } to sign in again; a new device is tried by itself.
waiting-key-package = { $name } can't be added to the encryption yet because none of their devices has keys left on the server. This is fixed once one of their devices is online again.
key-changed-title = { $name }'s security key changed
key-changed-body = This normally happens only after a full account reset. Check with { $name } through another channel before you share anything sensitive.
key-verify = Compare keys
key-accept = Accept new key
key-verify-title = Compare { $name }'s key
key-verify-body = Read the new key out loud to the person, for example in a call, and check that it matches what they see in their own security settings.
key-new = New key
key-old = Previous key
key-mark-verified = Mark as verified

## Sign-in
welcome-title = Welcome, { $name }
welcome-subtitle = This is the first device of your account. Two short steps protect your messages.
welcome-step-keys = Keys are created on this device
welcome-step-keys-hint = Your encryption keys never leave your devices. The server only sees encrypted data.
welcome-step-passkey = Protect your key backup with a passkey
welcome-step-passkey-hint = A passkey (Face ID, fingerprint or a security key) lets you restore your keys on a new device.
welcome-create-passkey = Create passkey
welcome-continue = Continue
welcome-demo-note = Key creation and passkeys are not connected yet; continuing only opens the app.

## Search
contact-verify-title = Verify { $name }
contact-verify-body = Compare this number with the one { $name } sees in their own settings, for example in a call. If the numbers are identical, nobody sits between you.
contact-verified = Verified: you compared this number.
contact-not-verified = Not verified yet.
contact-unmark = Remove verification

## Settings: display, sidebar, language, privacy, devices, calls
contact-verified-short = Verified

## First-run key setup
enroll-title = Set up this device, { $name }
enroll-new-subtitle = Create your encryption keys. Only you can read your messages; not even the server can. First save your recovery kit: it brings your messages back if you ever lose all your devices.
enroll-recover-subtitle = Your account already has encryption keys. Open your recovery kit or type its key to unlock them on this device.
enroll-data-lost = TeamCraft's data on this device is gone: the browser deleted it, for example to free up space, or it could not be read and was set aside. Unlock your keys again with your recovery kit, or link this device from another device.
enroll-device-name = Name of this device
enroll-create = Create keys
enroll-recover = Unlock
enroll-working = Working…
enroll-failed = The keys could not be created.
enroll-recover-failed = The recovery key or passphrase did not unlock your keys, or the server could not be reached.
enroll-secret-label = Recovery key or passphrase
enroll-secret-hint = The key from your recovery kit (8 groups of 4 characters), or your passphrase if you set one up before recovery kits existed.
enroll-key-typo = This looks like a recovery key, but a character is wrong. Compare it with your recovery kit.
enroll-kit-read = The recovery key was read from the file.
enroll-restore-device = Restore on this computer
enroll-restore-device-hint = This computer kept a key for your account in its keychain, so you don't need your recovery kit here.

## Recovery kit (e2ee.md §6.5)
kit-setup-intro = TeamCraft made a recovery key for you. Save the recovery kit: print it or keep it in your password manager. You only need it if you lose all your devices.
kit-key-label = Recovery key
kit-download = Download recovery kit
kit-save-hint = The kit is a file with your key, a QR code and what to do with it. It is made on this device and never sent to the server.
kit-downloaded = The recovery kit was downloaded. Print it or put it in your password manager, then continue.
kit-download-failed = The recovery kit could not be downloaded. Write the key down instead, then continue.
kit-saved-continue = I saved my recovery kit
kit-confirm-intro = To make sure you kept it, type group { $number } of the key from your saved kit, or open the file.
kit-confirm-group = Group { $number }
kit-confirm-wrong = That isn't group { $number }. Look at your saved recovery kit again.
kit-confirm-file-ok = This is your new recovery kit.
kit-confirm-file-other = This file holds another recovery key, not the one just made. Pick the kit you just downloaded.
kit-confirm-back = Show the key again
kit-open = Open recovery kit…
kit-file-no-key = No recovery key was found in this file. Pick the recovery kit TeamCraft saved for you.
kit-file-title = TeamCraft recovery kit
kit-file-account = Account: { $account } · Server: { $server } · Made on { $date }
kit-file-intro = This is the key to your encrypted messages. If you lose all your devices, it is the only way to read your messages again.
kit-file-keep-title = Keep it safe
kit-file-keep-print = Print this page and keep it somewhere safe, or keep the file in your password manager.
kit-file-keep-share = Don't send it by e-mail or chat, and don't store it in TeamCraft.
kit-file-keep-anyone = Anyone who has this key and can sign in as you can read your messages.
kit-file-use-title = When you need it
kit-file-use = Only when you set up a device and none of your other devices is at hand: sign in to TeamCraft, choose "Open recovery kit…" and pick this file, or type the key. If another of your devices is at hand, link the new one from it instead.
kit-file-lost-title = If it is lost or someone else saw it
kit-file-lost = Make a new recovery kit in TeamCraft under Settings, Security & privacy. This kit then stops working.
kit-file-never = TeamCraft and your IT department will never ask you for this key.

# ---- Meetings ----
enroll-link = Link with another device
link-new-title = Link this device
link-new-steps = On a device you are already signed in on, open TeamCraft, then Settings, Devices and security, Link a new device, and scan or enter this code there. The camera app alone cannot read it.
link-new-compare = On the other device, type this code to approve. Do not approve if you did not start this.
link-new-failed = Linking did not work.
link-new-back = Back
link-new-retry = Try again
approve-link-retry = Try again
link-new-no-device = No other device signed in? Go back and use your recovery kit or ask an admin for organization recovery.
approve-link-title = Link a new device
approve-link-intro = Enter the code the new device shows.
approve-link-code = Code of the new device
approve-link-hint = The code contains a one-time key; enter it only here.
approve-link-connect = Connect
approve-link-compare = Check that the new device shows the same code. Approve only if they match.
approve-link-sending = Sending your keys and waiting for the new device to confirm…
approve-link-failed = The device could not be linked.
recovery-title = Recovery passphrase
recovery-hint = Restores your keys on a new device. Changing it re-protects your key backup; the old passphrase stops working.
recovery-new = New recovery passphrase
recovery-confirm = Repeat the passphrase
recovery-length-hint = At least { $count } characters. You need it to restore your messages on a new device. If you lose it, we can't recover it for you.
recovery-save = Change passphrase
recovery-done = The recovery passphrase was changed. Write it down somewhere safe.
recovery-failed = The passphrase could not be changed.
approve-link-type-code = Type the short code that the new device shows. If it shows none, or you did not start this, do not approve.
approve-link-short = Short code from the new device
approve-link-wrong = That is not the code on the new device. Try again.

## Getting started (empty workspace)
device-kind-desktop = Desktop
device-kind-browser = Browser
passphrase-show = Show
passphrase-hide = Hide

## Key names in keyboard shortcuts (the caps shown on a keyboard)
link-unavailable = Linking a device needs this app's encryption, which is not running here. Reload the page or use the desktop app.

## Failed actions (an error toast) and the panic overlay
error-link-code-invalid = That isn't a TeamCraft link code. Scan the code the new device shows on its Link this device screen.
error-link-other-version = The other device runs a different version of TeamCraft. Update TeamCraft on both devices, then try again.
error-link-peer-gone = The new device is no longer waiting for this code. Show a new code there and scan it again.
error-link-timed-out = The other device stopped answering. Try again.
error-link-code-expired = Nobody used this code in time. Try again for a new code.
error-link-denied = A device of your account denied this request.
error-link-not-confirmed = The keys did not arrive on the new device. Link it again.
error-link-request-gone = This request is no longer open: another device took it up, or it expired.

## Settings: sections, search and rows (#327)
# Home pages (issue #585)
link-request-region = Requests to link a new device
link-request-title = New device wants to join your account
link-request-device = Says it is { $device } ({ $platform }). Not verified.
link-request-warning = Only continue if you are setting up this device right now. Otherwise deny it.
link-request-review = Review
link-request-deny = Deny
link-request-joining = Connecting to the new device…
link-request-type-code = Type the code that the new device shows. If it shows none, deny the request.
link-platform-android = Android
link-platform-ios = iPhone or iPad
link-platform-macos = Mac
link-platform-windows = Windows
link-platform-linux = Linux
link-platform-web = Web browser
link-platform-other = Unknown system
link-new-ask = Ask my other devices
link-new-ask-hint = Instead of scanning, your other devices show a prompt. The server then sees the key in this code; you still type the code shown here on the other device, so nothing is approved without you.
link-new-asked = Your other devices show a prompt now.
link-new-ask-failed = The other devices could not be asked.
