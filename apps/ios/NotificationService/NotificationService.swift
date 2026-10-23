import CallKit
import Foundation
import UserNotifications

/// Rewrites the generic APNs alert into a notification with sender and preview (docs/e2ee.md §7,
/// the Threema model): opens the sealed wake-up with the device's push key, fetches and decrypts
/// the message from the shared store through the Rust core, and shows it. A call wake-up is handed
/// to CallKit instead. The extension never touches MLS state; it takes the cross-process lock so
/// the app and the extension don't sync at the same time.
///
/// Memory: extensions have a small limit (about 24 MB); the first device test must watch it.
final class NotificationService: UNNotificationServiceExtension {
  /// How long to wait for the app, which holds the lock while it is in the foreground.
  private static let lockTimeout: TimeInterval = 8

  private let delivery = DeliveryGate()
  private var maybeOriginal: UNNotificationContent?
  private var maybeHandler: ((UNNotificationContent) -> Void)?

  override func didReceive(
    _ request: UNNotificationRequest, withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
  ) {
    maybeOriginal = request.content
    maybeHandler = contentHandler
    let userInfo = request.content.userInfo
    let original = request.content
    Task {
      let content = await Self.process(userInfo: userInfo, original: original)
      deliver(content)
    }
  }

  /// About 30 seconds passed: show the best we have.
  override func serviceExtensionTimeWillExpire() {
    guard let original = maybeOriginal else { return }
    deliver(Self.builder.generic(from: original))
  }

  private func deliver(_ content: UNNotificationContent) {
    guard delivery.claim(), let handler = maybeHandler else { return }
    handler(content)
  }

  private static var builder: NotificationContentBuilder {
    NotificationContentBuilder(
      fallbackSender: String(localized: "app_name"),
      fallbackText: String(localized: "notification_new_message"),
      fileText: { String(localized: "notification_file \($0)") })
  }

  private static func process(userInfo: [AnyHashable: Any], original: UNNotificationContent) async
    -> UNNotificationContent
  {
    guard let sealed = PushPayload.sealedBytes(from: userInfo),
      let notification = await decrypt(sealed)
    else {
      return builder.generic(from: original)
    }
    // App lock on: the lock screen must not show what the locked app hides. Calls still ring.
    if SharedContainer().isAppLockOn && !notification.isCall {
      return builder.generic(from: original)
    }
    if notification.isCall {
      await reportCall(notification)
      // With the filtering entitlement an empty content shows nothing; without it the system
      // falls back to the generic alert, which is acceptable.
      return UNNotificationContent()
    }
    let (content, intent) = builder.content(for: notification, base: original)
    return await builder.communicationStyled(content, intent: intent)
  }

  /// Opens the core under the cross-process lock; `nil` if not signed in, locked out or failed.
  private static func decrypt(_ sealed: Data) async -> PushNotification? {
    let container = SharedContainer()
    guard let account = container.activeAccount else { return nil }
    let lock = ProcessLock(fileURL: container.lockFileURL)
    let hasLock = await Task.detached { lock.acquire(timeout: lockTimeout) }.value
    guard hasLock else { return nil }
    defer { lock.release() }
    do {
      let core = try CoreFactory().open(account: account, createKey: false, listener: ClosureEventListener {})
      let notification = try await core.notificationForPush(payload: sealed)
      // The app reloads what the extension stored.
      StoreChangeNotifier().post()
      return notification
    } catch {
      return nil
    }
  }

  /// Wakes the app into CallKit (`reportNewIncomingVoIPPushPayload`, iOS 14.5+); the app's PushKit
  /// registry receives the payload and reports the call.
  private static func reportCall(_ notification: PushNotification) async {
    let name = notification.maybeSenderName ?? notification.maybeConversationName ?? String(localized: "app_name")
    let payload: [AnyHashable: Any] = [
      "conversation_id": notification.conversationId,
      "caller_name": name,
    ]
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
