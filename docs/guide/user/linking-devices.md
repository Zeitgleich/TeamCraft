# Linking another device

Link a new device to one you are already signed in on. The old device hands the keys over directly;
the server only relays encrypted data.

## On the new device

1. Sign in. On the key setup screen choose **Link with another device**.
2. The screen shows a **code**. Keep it open.

## On the device you already use

1. Open **Settings**, then **Devices and security**.
2. Choose **Link a new device** and enter the code from the new device. Choose **Connect**.
3. Both screens show a short **comparison code**. Check that they are identical.
4. Approve. The new device receives your keys and starts loading your messages.

The code contains a one-time key. Enter it only in your own TeamCraft app, never on a web page or
in a chat.

## If you did not link a device

When somebody signs in to your account on a new device, your other devices may ask
**Approve this device?** with a code. If you did not just sign in, choose **Not me**. Approving gives
the new device access to your messages.

## Removing a device

**Settings > Devices and security** lists the devices that can read your messages. Remove any you no
longer use or do not recognize. A removed device stops receiving new messages. Admins can also
remove a device (for example after a lost phone); you see this in your security log.

## Status of this feature

| Part                                        | Status                                            |
|---------------------------------------------|---------------------------------------------------|
| Desktop: link with code and comparison code | Works                                             |
| QR code scanning                            | Coming soon (the code is typed in)                |
| Android: linking                            | In the shared core; the app screen is coming soon |
