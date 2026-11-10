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
device-waiting-notes-body = Your notes open here once another of your devices is online. If you no longer have it, remove it under Settings > Devices: this device then sets up your notes again on its own. Earlier notes stay readable.
left-in-group-unknown = Someone
group-refused = The server doesn't let this device read the encrypted group of this conversation. New messages may be missing.
waiting-for-admission = Adding you to this channel. The app of a member who is online adds you, usually within seconds. Only messages from when you joined on will be readable.
waiting-for-admission-no-member-online = No member of this channel is online right now. You are added as soon as one is; this app keeps the request, also after a restart. Only messages from when you joined on will be readable.
waiting-for-admission-not-yet = You haven't been added to this channel yet: no member's app added you, maybe because they refused the request. This app asks again on its own. Only messages from when you joined on will be readable.
import-in-progress = History is being imported. The import account { $name } can read this conversation until the import is finished.
read-by-outsider = { $name } is no participant of this conversation but can read it.
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
contact-attested = Your organization's sign-in confirmed this key belongs to them.
contact-not-attested = Your organization's sign-in hasn't confirmed this key. This app took it as the server handed it out; compare the number to be sure.

## Settings: display, sidebar, language, privacy, devices, calls
contact-verified-short = Verified

## First-run key setup
enroll-data-lost = TeamCraft's data on this device is gone: the browser deleted it, for example to free up space, or it could not be read and was set aside. Unlock your keys again with your recovery kit, or link this device from another device.
enroll-device-name = Name of this device
enroll-recover = Unlock
enroll-working = Working…
enroll-failed = The keys could not be created.
enroll-recover-failed = The recovery key or passphrase did not unlock your keys, or the server could not be reached.
enroll-secret-label = Recovery key or passphrase
enroll-secret-hint = The key from your recovery kit (8 groups of 4 characters), or your passphrase if you set one up before recovery kits existed.
enroll-key-typo = This looks like a recovery key, but a character is wrong. Compare it with your recovery kit.
enroll-kit-read = The recovery key was read from the file.
enroll-restore-device = Restore on this computer

## Recovery kit (e2ee.md §6.5)
kit-setup-intro = TeamCraft made a recovery key for you. Save the recovery kit: print it or keep it in your password manager. You only need it if you lose all your devices.
kit-key-label = Recovery key
kit-save-hint = The kit is a file with your key, a QR code and what to do with it. It is made on this device and never sent to the server.
kit-downloaded = Downloaded. Find it in your downloads.
kit-download-failed = The recovery kit could not be downloaded. Write the key down instead, then continue.
kit-confirm-intro-short = To make sure you kept it, type the group asked for below from your saved kit, or open the file.
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
link-new-title = Link this device
link-new-compare = On the other device, type this code to approve. Do not approve if you did not start this.
link-new-failed = Linking did not work.
link-new-retry = Try again
approve-link-retry = Try again
link-new-no-device = No other device signed in? Go back and use your recovery kit or ask an admin for organization recovery.
approve-link-title = Link a new device
approve-link-intro = Enter the code the new computer shows.
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
approve-link-phone-intro = Setting up a new phone? In TeamCraft on the phone, choose Link with another device, then Scan code, and point it at this code.
link-code-valid = This code works for { $time } more.
approve-link-phone-expired = This code ran out before a phone scanned it.
approve-link-phone-new-code = Show a new code
approve-link-phone-other-ways = The new phone can also ask for approval: this device then shows a prompt.
approve-link-enter-instead = The new device is a computer: enter its code
approve-link-show-instead = Show a code for a phone instead
error-link-for-new-device = This code is for the new phone: scan it there with Scan code.
error-link-for-signed-in-device = This code comes from a new device: scan it with a device that is already signed in.

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

## Guided first run on a device (#1701)
kit-download-again = Download again
kit-wrote-down = Can't download? I wrote the key down on paper
kit-check-missing = Type group { $number } from your saved kit, or open the kit file.
kit-check-group-ok = That's right.
kit-check-or = or
enroll-secret-missing = Open your kit file, or type the key from it.
onboarding-step = Step { $current } of { $total }
onboarding-back = Back
onboarding-continue = Continue
onboarding-start = Let's start
onboarding-recommended = Recommended
onboarding-intro-title = Good to have you, { $name }
onboarding-intro-lead = Before you start, TeamCraft protects your messages on this device. It takes about two minutes.
onboarding-intro-locked = Your messages are locked
onboarding-intro-locked-text = Only your own devices can read them. Not the server, not your IT department.
onboarding-intro-kit = You get a recovery kit
onboarding-intro-kit-text = A file with your personal key. It is the spare key to your messages, like the spare key to your home.
onboarding-intro-keep = Keep it safe, don't lose it
onboarding-intro-keep-text = If you ever lose all your devices, the kit is the only way back to your messages. Nobody can make you a new one.
onboarding-save-title = Save your recovery kit
onboarding-save-lead = Download the kit and keep it where you keep important papers or passwords: print it, or put it in your password manager.
onboarding-save-new-key = You started this before. For your safety this is a new key: a kit you saved earlier does not work. Delete it and save this one.
onboarding-check-title = Check your recovery kit
onboarding-check-lead = Make sure you really have it: look at your saved kit and type the group asked for below, or open the file.
onboarding-finish-title = Keep your kit safe
onboarding-finish-lead = Almost done. Remember these three things, then finish the setup.
onboarding-finish-where = Keep it somewhere safe
onboarding-finish-where-text = With your important papers, or in your password manager. Not only on this device.
onboarding-finish-share = Never send it
onboarding-finish-share-text = Not by e-mail, not in a chat, not even in TeamCraft. Whoever has it can read your messages.
onboarding-finish-ask = Nobody will ask you for it
onboarding-finish-ask-text = Not TeamCraft, not your IT department. Anyone who asks wants to trick you.
onboarding-finish = Finish setup
onboarding-device-hint = You see this name later in your list of devices.
onboarding-unlock-title = Unlock your messages here
onboarding-unlock-lead = Your account already has a key. To read your messages on this device, bring the key here. Choose how:
onboarding-way-device-text = This computer kept your key. One click and you're done.
onboarding-way-link = Use another device
onboarding-way-link-text = Approve this one on your phone or another computer where you're signed in. The quickest way.
onboarding-way-kit = Use my recovery kit
onboarding-way-kit-text = Open the kit file or type the key from it. For when no other device is at hand.
onboarding-kit-title = Unlock with your recovery kit
onboarding-kit-lead = Open the kit file you saved when you set up your first device. You can also type the key printed in it.
onboarding-link-open = Open TeamCraft on a device where you're signed in.
onboarding-link-scan = Go to Settings, Devices, Link a device, and scan this code (on a computer, type it in).
onboarding-link-compare = Compare the number both devices show, then approve.
onboarding-link-no-camera = Can't scan? Your other devices can show a prompt instead.
onboarding-way-it = Ask my IT department
onboarding-way-it-text = No other device and no kit? Your IT department can help.
onboarding-it-title = Ask your IT department
onboarding-it-lead = Tell your IT department that you lost all your devices and your recovery kit. They can reset your account: you then start with new keys, and your old messages are lost.
onboarding-it-code = If your organization set up recovery, your IT department may ask you for a code to get your messages back: sign in with the TeamCraft app on your phone and choose “Ask my IT department” there.
kit-download-short = Download kit
