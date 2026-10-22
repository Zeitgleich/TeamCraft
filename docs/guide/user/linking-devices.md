# Linking another device

Link a new device to one you are already signed in on. The old device hands the keys over directly;
the server only relays encrypted data.

## On the new device

1. Sign in. On the key setup screen choose **Link with another device**.
2. The screen shows a **code** and, once the other device answers, a **short code**. Keep it open.

## On the device you already use

1. Open **Settings**, then **Devices and security**.
2. Choose **Link a new device** and enter the code from the new device. Choose **Connect**.
3. **Type the short code** the new device shows. If it shows none, or you did not start this,
   do not approve. A wrong code is refused, so a relay in the middle cannot slip in a different device.
4. The new device receives your keys and starts loading your messages.

The code contains a one-time key. Enter it only in your own TeamCraft app, never on a web page or
in a chat.

## If you did not link a device

When somebody signs in to your account on a new device, your other devices may ask
**Approve this device?** with a code. If you did not just sign in, choose **Not me**. Approving gives
the new device access to your messages.

## Removing a device

**Settings > Devices and security** lists the devices that can read your messages. Remove any you no
longer use or do not recognize. A removed device stops receiving new messages, and your other devices
rotate the channel keys so it cannot read what comes next. Admins can also remove a device (for
example after a lost phone); you see this in your security log.

## Status

| Part                                    | Status                                                       |
|-----------------------------------------|--------------------------------------------------------------|
| Link with code and short code (desktop) | Works                                                        |
| Android and iOS                         | The core supports linking; the Android screen is coming soon |
| Scanning a QR code instead of typing    | Coming soon                                                  |
