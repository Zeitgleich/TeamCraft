# Getting started

How to sign in and set up your first device. Where a step is marked *coming soon*, the design
exists (see [docs/e2ee.md](../../e2ee.md)) but the current build does not do it yet.

## Which app

| Client  | Status                                                                                     |
|---------|--------------------------------------------------------------------------------------------|
| Desktop | Linux, macOS (Tauri). The most complete client: encrypted chat, calls, meetings, settings  |
| Android | Chat list, conversations, threads, composer, push notifications. No calls yet              |
| Web     | Served by your server at its address. Currently read-only chat; guests use it for meetings |
| iOS     | Coming soon                                                                                |

Your admin gives you the server address and tells you which client to install.

## Sign in

1. Open the app and choose **Sign in**. Your company login page opens in the browser.
2. Sign in as usual (password, second factor, whatever your company uses) and return to the app.
3. If the app says *Your account has no access to TeamCraft*, you signed in but have no TeamCraft
   role yet. Ask an admin to assign one, then sign in again.

TeamCraft never sees your company password; the login is handled by your company's identity provider.

## Set up your first device

The first time, the app asks you to **create your encryption keys**:

1. Choose a **recovery passphrase** of at least 12 characters and repeat it.
2. Give this device a **name** (for example "Work laptop").
3. Choose **Create keys**.

Your messages are end-to-end encrypted: only your devices can read them, not even the server can.
The passphrase protects a backup of your keys on the server. You need it to restore your messages on
a new device.

> **Write the passphrase down and keep it safe.** TeamCraft cannot recover it for you. If you lose
> the passphrase *and* all your devices, only an organization recovery by two admins can help,
> and only if your admin turned it on (see [Security basics](security-basics.md)).

## Passkey (coming soon)

The design replaces the passphrase as the primary recovery with a passkey (Face ID, Windows Hello,
fingerprint). The first-device screen shows the step, but creating a passkey is not wired yet.
Use the passphrase.

## Signing in on another device

- **Already have keys** (you signed in on a second computer): the app detects it and asks for your
  **recovery passphrase** instead of creating new keys. Enter it and choose **Unlock**.
- **Better:** link the new device to a device you already use, no passphrase needed. See
  [Linking another device](linking-devices.md).

## Next

[Spaces and channels](spaces-and-channels.md), [Messages and threads](messages-and-threads.md).
