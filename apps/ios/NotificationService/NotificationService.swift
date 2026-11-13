import CallKit
import Foundation
import UserNotifications
import os

/// Rewrites the generic APNs alert into what the push means (docs/e2ee.md §7, notifications.md
/// "Mobile push delivery"): opens the sealed wake-up with the device's push key, fetches and
/// decrypts the message from the shared store through the Rust core, and shows it with sender,
/// preview, badge and the right interruption level. Calls go to CallKit (with the filtering
/// entitlement) or show as an incoming-call alert; muted, DND and already shown messages stay quiet.
/// The extension never saves MLS state; it takes the cross-process lock so the app and the
/// extension don't sync at the same time. A push it can't read from the store and the one fetched
/// message (a new conversation, keys that changed, a sender's new device) is caught up on a probe
/// that is thrown away: in memory only, and nothing sent to the server (issue #839, e2ee.md §7.2).
///
/// Memory: extensions have a small limit (about 24 MB). The core opens the store with small caches
/// and reads only the records of the notified message (issue #155; measured in docs/performance.md).
/// The probe needs a few MB more, so it runs only with that much to spare; otherwise the push shows
/// what the store and the fetched message give.
final class NotificationService: UNNotificationServiceExtension {
  /// How long to wait for the app, which holds the lock while it is in the foreground.
  private static let lockTimeout: TimeInterval = 8
  /// What must be free for the probe: its writes (at most 4 MB in the core) and a second look at
  /// the MLS state while the first one is freed.
  private static let probeMemoryBytes = 12 * 1024 * 1024
  /// Info.plist key (from `NSE_NOTIFICATION_FILTERING`): the extension is signed with
  /// `com.apple.developer.usernotifications.filtering`, so an empty content shows nothing.
  private static let filteringKey = "TCNotificationFiltering"

  private let delivery = DeliveryGate()
  private var maybeOriginal: UNNotificationContent?
  private var maybeHandler: ((UNNotificationContent) -> Void)?

  override func didReceive(
    _ request: UNNotificationRequest, withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
  ) {
    maybeOriginal = request.content
    maybeHandler = contentHandler
    let original = request.content
    Task {
      let content = await Self.process(original: original)
      deliver(content)
    }
  }

  /// About 30 seconds passed: show the best we have. The sealed wake-up stays, so an app in the
  /// foreground can still open it.
  override func serviceExtensionTimeWillExpire() {
    guard let original = maybeOriginal else { return }
    deliver(Self.builder.generic(from: original, keepsSealed: true))
  }

  private func deliver(_ content: UNNotificationContent) {
    guard delivery.claim(), let handler = maybeHandler else { return }
    handler(content)
  }

  private static var builder: NotificationContentBuilder {
    NotificationContentBuilder(strings: .localized)
  }

  private static var canFilter: Bool {
    let value = Bundle.main.object(forInfoDictionaryKey: filteringKey) as? String
    return value?.uppercased() == "YES"
  }

  private static func process(original: UNNotificationContent) async -> UNNotificationContent {
    // A DM with a deleted account is named in the user's language (#1066).
    setDeletedUserName(name: String(localized: "user_deleted_name"))
    guard let sealed = PushPayload.sealedBytes(from: original.userInfo),
      let notification = await decrypt(sealed)
    else {
      return builder.generic(from: original, keepsSealed: true)
    }
    let flags = PushFlags(notification)
    // App lock on: the lock screen must not show what the locked app hides. Calls still ring.
    if SharedContainer().isAppLockOn && notification.kind == .message
      && !(flags.isDuplicate || flags.maybeSuppression != nil)
    {
      await DeliveredNotifications.setBadge(flags.maybeBadge)
      return builder.generic(from: original)
    }
    let presentation = PushPresentation.of(flags, canFilter: canFilter)
    switch presentation {
    case .message, .quietMessage:
      await DeliveredNotifications.removeEarlier(of: notification)
      let (content, maybeIntent) = builder.message(for: notification, presentation: presentation, base: original)
      return await builder.communicationStyled(
        content, intent: maybeIntent, presentation: presentation, flags: flags)
    case .callKit:
      await reportCall(notification)
      await DeliveredNotifications.setBadge(flags.maybeBadge)
      return UNNotificationContent()
    case .callAlert:
      return builder.callAlert(for: notification, base: original)
    case .meeting:
      return builder.meeting(for: notification, base: original)
    case .meetingHost:
      return builder.meetingHost(for: notification, base: original)
    case .test:
      return builder.test(for: notification, base: original)
    case .drop:
      if notification.kind == .dismiss { await DeliveredNotifications.apply(dismiss: notification) }
      await DeliveredNotifications.setBadge(flags.maybeBadge)
      return UNNotificationContent()
    case .quietGeneric:
      if notification.kind == .dismiss { await DeliveredNotifications.apply(dismiss: notification) }
      return builder.quietGeneric(for: flags, base: original)
    }
  }

  /// Opens the core under the cross-process lock; `nil` if not signed in, locked out or failed.
  /// A store that still needs a layout step fails here too: the extension doesn't migrate it (no
  /// `VACUUM` under its limits, #1198) and the app does on its next start.
  private static func decrypt(_ sealed: Data) async -> PushNotification? {
    let container = SharedContainer()
    guard let account = container.activeAccount else { return nil }
    let lock = ProcessLock(fileURL: container.lockFileURL)
    let hasLock = await Task.detached { lock.acquire(timeout: lockTimeout) }.value
    guard hasLock else { return nil }
    defer { lock.release() }
    do {
      let core = try CoreFactory().open(
        account: account, createKey: false, isMemoryConstrained: true, runsUpkeep: false,
        listener: ClosureEventListener {})
      // Never advances the account's state: the app alone does (MLS, cursors), or the two
      // processes would fork it. The probe catches up on a copy in memory and sends nothing
      // (docs/e2ee.md §7.2).
      let offset = PushClock.utcOffsetMinutes()
      let notification: PushNotification
      if hasMemoryForProbe {
        notification = try await core.notificationProbeForPush(payload: sealed, utcOffsetMinutes: offset)
      } else {
        notification = try await core.notificationPreviewForPush(payload: sealed, utcOffsetMinutes: offset)
      }
      // The app reloads what the extension stored.
      StoreChangeNotifier().post()
      return notification
    } catch {
      return nil
    }
  }

  /// Whether the extension has the memory the probe takes to spare (`os_proc_available_memory`,
  /// what is left before the system ends it).
  private static var hasMemoryForProbe: Bool {
    os_proc_available_memory() >= probeMemoryBytes
  }

  /// Wakes the app into CallKit (`reportNewIncomingVoIPPushPayload`, iOS 14.5+, needs the filtering
  /// entitlement); the app's PushKit registry receives the payload and reports the call.
  private static func reportCall(_ notification: PushNotification) async {
    let name = notification.maybeSenderName ?? notification.maybeConversationName ?? String(localized: "app_name")
    var payload: [AnyHashable: Any] = [
      CallForward.conversationKey: notification.conversationId,
      CallForward.callerNameKey: name,
    ]
    if let callId = notification.maybeCallId { payload[CallForward.callIdKey] = callId }
    await withCheckedContinuation { continuation in
      CXProvider.reportNewIncomingVoIPPushPayload(payload) { _ in continuation.resume() }
    }
  }
}

/// Delivers exactly once: the work and the expiry handler race.
private final class DeliveryGate: @unchecked Sendable {
  private let lock = NSLock()
  private var isClaimed = false

  func claim() -> Bool {
    lock.lock()
    defer { lock.unlock() }
    if isClaimed { return false }
    isClaimed = true
    return true
  }
}
