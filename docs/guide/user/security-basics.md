# Security basics

## What is protected

Messages, attachments, reactions and thread contents are end-to-end encrypted with MLS. Admins and
the people who run the server cannot read them. The server can see **metadata**: who is in which
conversation, when messages are sent, and your presence and status.

## Your keys and your devices

- Each device has its own keys. **Settings > Devices and security** shows them and your own identity
  fingerprint.
- Lost a phone? Remove it there or ask an admin to remove it.
- Lost all devices? Sign in on a new one and enter your **recovery passphrase**
  ([Getting started](getting-started.md)). If you lost that too, an admin can start an
  **organization recovery** if your company enabled it: it needs two admins to approve and a
  confirmation code. Guests are excluded by default. After a recovery your backup key is rotated.
- Your identity key is vouched for by your company login, so a malicious server cannot swap it
  for another one. Device lists are signed, so a new device only counts after you or another of your
  devices approved it.

## Verify a contact (safety numbers)

To be sure you talk to the right person, compare keys:

1. Open the person's profile or the key banner and choose **Compare keys**.
2. Read the number out loud to them, for example in a call, and check that it matches what they see
   in their own security settings.
3. Choose **Mark as verified**.

The mark names the key it was made for, so a changed key is never shown as verified.

## The key-change banner

> **{Name}'s security key changed.**

This normally happens only after a full account reset (new keys, for example after a recovery). Check
with the person through another channel before sharing anything sensitive. Then:

- **Compare keys** to verify the new key, or
- **Accept new key** if you are satisfied. This clears the old verification.

Until you accept, TeamCraft does not trust the new key.

## Admins in private channels

Admins cannot read channels they are not in. They can add *themselves* to a private channel, but
this is never silent: every member's app shows "… joined this channel as a workspace admin",
worked out from the encrypted membership change itself, so nobody can fake or hide it. The
workspace policy `admin_self_join` decides whether that is allowed (`visible` by default) or
disabled.

## The web app

The web app is delivered by your server. It checks a **signed release** before it updates itself,
and *Settings > About* shows the version and the release key it was verified against. That catches
changed files, but a server that replaces the checker itself could still serve you a modified app.
For the strongest protection use the desktop or mobile app, which carry their own code.

## Status

The key-change banner and safety numbers are computed from the client core's data. Verification
marks are stored on the device. Comparing keys by QR code and passkey recovery are coming soon.
