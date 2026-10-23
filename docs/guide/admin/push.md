# Push notifications

Push wakes devices with an **encrypted payload** sealed with the device's push key: the push service
(browser vendor, Google, Apple) sees only that something arrived. Routing honors mute, mentions,
do-not-disturb and active devices ([docs/notifications.md](../../notifications.md)).

## Status

| Provider                   | Server                                                                        | Client                                                                                           |
|----------------------------|-------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------|
| Web Push (VAPID, RFC 8291) | Implemented                                                                   | Service worker of the web client; shows a generic notification until it can open sealed payloads |
| FCM (Android)              | Implemented (HTTP v1, service account, or via the push gateway)               | App receiver exists (flavor `full`)                                                              |
| UnifiedPush                | Implemented (Web Push encryption to the distributor)                          | App receiver exists                                                                              |
| APNs (iOS)                 | Implemented (HTTP/2, `.p8` key or via the push gateway; calls as alerts)      | iOS app with notification service extension                                                      |

**Store apps** (App Store, Play Store) are signed by their publisher, so their pushes can only be sent with the
publisher's credentials. Set `[push.gateway] url` to the publisher's push gateway and your server
sends APNs and FCM wake-ups through it; the gateway only sees device tokens and ciphertext
([Push gateway](../../push-gateway.md)). Builds you sign yourself use `[push.apns]` / `[push.fcm]`
directly instead.

Each provider is on once its credentials are set (or a gateway is configured); the admin console (Instance → Push providers) shows
which are configured, the last delivery and the last error. Credentials for APNs, FCM and UnifiedPush are
described in [Operations](../../operations.md#push-notifications).

## Desktop attention and works councils

While somebody looks at a desktop or web session, their phone pushes wait for the grace delay
(`[notifications] mobile_push_delay_secs`, default 2 minutes) and go out only if the message is still
unread. For that the sessions tell the server whether the user is at the desktop: transitions only,
"away" after a minute, kept in memory, never stored or logged, never which window or what input.
That still is information about when people work. Where a works council or an employee-monitoring
rule must agree first, decide before rollout:

- Users can turn it off themselves (Settings → Notifications → "Use desktop activity to hold phone
  notifications").
- Admins turn it off for everybody with `[notifications] is_desktop_attention_enabled = false`. The
  apps then report nothing, and a connected, active desktop holds phone pushes back as presence did
  before (no extra data).

Details: [docs/e2ee.md §7](../../e2ee.md), [docs/notifications.md](../../notifications.md).

## Delivery health and test pushes

- Users send themselves a test push from the app's notification diagnostics.
- Admins see per user and device how many pushes were sent, failed, retried, withdrawn, suppressed by
  DND or held for the desktop: `GET /api/v1/admin/users/{id}/push-health` (counts only, since the
  server started).

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

Other keys: `ttl_secs` (3600), `request_timeout_secs`, `max_concurrent_sends` (32 per pool),
`max_concurrent_sends_per_user` (4).
`allow_private_endpoints = true` is for development only.

Rotating the VAPID key makes every browser subscribe again: expect a short gap.
