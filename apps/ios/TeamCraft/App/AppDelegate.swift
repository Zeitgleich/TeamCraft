import UIKit
import UserNotifications

/// Push registration, notification taps and actions, and the PushKit registry that receives the
/// extension's call wake-up (the SwiftUI lifecycle has no hooks for these).
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
  func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
  ) -> Bool {
    // First, so a crash during the rest of the launch is reported (the last known setting).
    CrashReports.shared.install()
    // Kills by the system (0xdead10cc) arrive through MetricKit on a later launch (#2179).
    SystemKillReports.shared.install()
    // Performance data: the signed-in start until the chat list shows the core's rows (#1521).
    let entry =
      launchOptions?[.remoteNotification] != nil ? "notification" : launchOptions?[.url] != nil ? "link" : "launcher"
    Perf.beginStart(entry: entry)
    UNUserNotificationCenter.current().delegate = self
    // DM titles, members and notifications name deleted accounts in the user's language (#1066).
    setDeletedUserName(name: String(localized: "user_deleted_name"))
    NotificationCategories.register(in: UNUserNotificationCenter.current())
    // Before anything else: a call push that launches the app is delivered to this registry.
    AppModel.shared.calls.start()
    // Background tasks must be registered before launch finishes.
    SegmentPrefetchTask.register()
    ScheduledSealTask.register()
    QuickActions.shared.publish()
    return true
  }

  /// The scene's delegate takes the app icon's quick actions (#2061); SwiftUI still makes the window.
  func application(
    _ application: UIApplication, configurationForConnecting connectingSceneSession: UISceneSession,
    options: UIScene.ConnectionOptions
  ) -> UISceneConfiguration {
    let configuration = UISceneConfiguration(name: nil, sessionRole: connectingSceneSession.role)
    configuration.delegateClass = QuickActionSceneDelegate.self
    return configuration
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

  /// A tap or an action on a notification. An action (reply, mark as read, approve) runs in the
  /// background: as a background job, so the store opens for it and closes after it (#2179).
  func userNotificationCenter(
    _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
  ) async {
    await AppModel.shared.inBackgroundJob("Notification action") { await handle(response) }
  }

  private func handle(_ response: UNNotificationResponse) async {
    let info = response.notification.request.content.userInfo
    // Approve or Decline of an admin's request to join; a tap opens the channel like a message.
    if info[NotificationKeys.kind] as? String == NotificationKeys.joinRequestKind,
      response.actionIdentifier == NotificationCategories.approveJoinAction
        || response.actionIdentifier == NotificationCategories.declineJoinAction
    {
      await AppModel.shared.handle(
        joinRequestAction: response.actionIdentifier, tag: info[NotificationAuthenticator.tagKey] as? String,
        notificationId: response.notification.request.identifier)
      return
    }
    // A meeting's notice dealt with here (Join, or cleared): the server takes it down on the user's
    // other phones. Clearing is nothing more than that; Join goes on to open the call.
    if info[NotificationKeys.kind] as? String == NotificationKeys.meetingKind,
      response.actionIdentifier == NotificationCategories.joinMeetingAction
        || response.actionIdentifier == UNNotificationDismissActionIdentifier,
      let meetingId = info[NotificationKeys.meetingId] as? String,
      let startsAt = (info[NotificationKeys.meetingStartsAt] as? String).flatMap(Int64.init)
    {
      await AppModel.shared.reportMeetingNoticeDealtWith(meetingId: meetingId, startsAtMs: startsAt)
    }
    if response.actionIdentifier == UNNotificationDismissActionIdentifier { return }
    // The lobby reminder after leaving from CallKit (#1859): "Hand over" and a tap open the choice.
    // Routed by the category the app sets on its own reminder, never by a payload's kind, and
    // acted on only with the app's tag over the reminder (`handleLobbyReminder`).
    if response.notification.request.content.categoryIdentifier == NotificationCategories.lobbyLeftCategory {
      if let conversationId = info[NotificationCategories.conversationKey] as? String {
        AppModel.shared.handleLobbyReminder(
          conversationId: conversationId, tag: info[NotificationAuthenticator.tagKey] as? String,
          notificationId: response.notification.request.identifier)
      }
      return
    }
    guard let conversationId = info[NotificationCategories.conversationKey] as? String else { return }
    let reply = (response as? UNTextInputNotificationResponse)?.userText
    let tag = info[NotificationAuthenticator.tagKey] as? String
    await AppModel.shared.handle(
      notificationAction: response.actionIdentifier, conversationId: conversationId, tag: tag,
      replyText: reply, maybeThreadId: info[NotificationKeys.thread] as? String,
      notificationId: response.notification.request.identifier,
      maybeMessageId: info[NotificationKeys.message] as? String)
  }
}
