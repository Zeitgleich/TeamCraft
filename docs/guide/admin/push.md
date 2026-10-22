# Push notifications

Push wakes devices with an **encrypted payload** sealed with the device's push key: the push service
(browser vendor, Google, Apple) sees only that something arrived. Routing honors mute, mentions,
do-not-disturb and active devices ([docs/notifications.md](../../notifications.md)).

## Status

| Provider                   | Server                                                     | Client                                                                                           |
|----------------------------|------------------------------------------------------------|--------------------------------------------------------------------------------------------------|
| Web Push (VAPID, RFC 8291) | Implemented                                                | Service worker of the web client; shows a generic notification until it can open sealed payloads |
| FCM (Android)              | **Stub**: logs "push provider is a stub, nothing was sent" | App receiver exists (flavor `full`)                                                              |
| UnifiedPush                | **Stub**                                                   | App receiver exists                                                                              |
| APNs (iOS)                 | **Stub**                                                   | iOS app: coming soon                                                                             |

So today only Web Push is delivered by the server. Android devices get messages while the app is
connected; background wake-ups need the server's FCM/UnifiedPush senders (coming soon).

## Set up Web Push

1. Generate the key (32 random bytes, base64url):

   ```sh
   openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
   ```

2. Set it as `TEAMCRAFT_PUSH__VAPID_PRIVATE_KEY` (secret). Set `[push] vapid_subject` to a contact
   (`mailto:admin@example.com`).
3. Start the server. The log line `Web Push enabled (applicationServerKey)` shows the **public key**
   that browsers subscribe with.
4. Serve the web client over HTTPS (service workers and push require it).

Other keys: `ttl_secs` (3600), `request_timeout_secs`, `max_concurrent_sends` (32).
`allow_private_endpoints = true` is for development only.

Rotating the VAPID key makes every browser subscribe again: expect a short gap.
