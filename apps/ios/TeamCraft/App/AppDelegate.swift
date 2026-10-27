import UIKit
import UserNotifications

/// Push registration, notification taps and actions, and the PushKit registry that receives the
/// extension's call wake-up (the SwiftUI lifecycle has no hooks for these).
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
  func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
  ) -> Bool {
    UNUserNotificationCenter.current().delegate = self
    NotificationCategories.register(in: UNUserNotificationCenter.current())
    // Before anything else: a call push that launches the app is delivered to this registry.
    AppModel.shared.calls.start()
    // Background tasks must be registered before launch finishes.
    SegmentPrefetchTask.register()
    return true
  }

  func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
    Task { await AppModel.shared.didRegister(deviceToken: deviceToken) }
  }

  func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
    // Simulators and unsigned builds have no APNs; the app works without push.
  }

  /// Silent (`content-available`) pushes: a `dismiss` wake-up after something was read or a ring
  /// ended on another device (notifications.md, "Mobile push delivery").
  func application(
    _ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any]
  ) async -> UIBackgroundFetchResult {
    await AppModel.shared.handleBackgroundPush(userInfo)
  }

  /// The app is in the foreground: show a banner unless that conversation is on screen; a push the
  /// extension couldn't open (the app held the store) is opened by the app.
  func userNotificationCenter(
    _ center: UNUserNotificationCenter, willPresent notification: UNNotification
  ) async -> UNNotificationPresentationOptions {
    await AppModel.shared.presentation(for: notification)
  }

  func userNotificationCenter(
    _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
  ) async {
    let info = response.notification.request.content.userInfo
    guard let conversationId = info[NotificationCategories.conversationKey] as? String else { return }
    let reply = (response as? UNTextInputNotificationResponse)?.userText
    let tag = info[NotificationAuthenticator.tagKey] as? String
    await AppModel.shared.handle(
      notificationAction: response.actionIdentifier, conversationId: conversationId, tag: tag,
      replyText: reply, maybeThreadId: info[NotificationKeys.thread] as? String,
      notificationId: response.notification.request.identifier)
  }
}
