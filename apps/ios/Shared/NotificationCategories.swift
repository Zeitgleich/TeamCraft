import UserNotifications

/// The actions on a notification: reply from the lock screen and mark as read on messages (like the
/// Android notification), answer and decline on an incoming-call alert, approve and decline on an
/// admin's request to join. The extension puts the category on what it shows; the app on the
/// requests it notifies about itself.
enum NotificationCategories {
  static let messageCategory = "message"
  static let callCategory = "call"
  static let replyAction = "reply"
  static let markReadAction = "mark_read"
  static let meetingCategory = "meeting"
  static let joinMeetingAction = "join_meeting"
  static let answerCallAction = "answer_call"
  static let declineCallAction = "decline_call"
  /// An admin's request to join a channel the user owns (issue #799): Approve and Decline, or
  /// Decline only when the verified roster doesn't list them as an admin.
  static let joinRequestCategory = "join_request"
  static let joinRequestDeclineOnlyCategory = "join_request_decline_only"
  static let approveJoinAction = "approve_join"
  static let declineJoinAction = "decline_join"
  /// The organizer left a meeting's call from CallKit while people still wait in its lobby
  /// (#1859): "Hand over" opens the choice of whom to hand the meeting to.
  static let lobbyLeftCategory = "lobby_left"
  static let handOverAction = "hand_over"
  /// Key of the conversation ID in a notification's `userInfo`.
  static let conversationKey = "conversation_id"

  static func register(in center: UNUserNotificationCenter) {
    let reply = UNTextInputNotificationAction(
      identifier: replyAction,
      title: String(localized: "notification_reply"),
      // Unlock first: a reply sends as the user.
      options: [.authenticationRequired],
      textInputButtonTitle: String(localized: "send"),
      textInputPlaceholder: String(localized: "composer_hint"))
    let markRead = UNNotificationAction(
      identifier: markReadAction, title: String(localized: "notification_mark_read"), options: [])
    let message = UNNotificationCategory(
      identifier: messageCategory, actions: [reply, markRead], intentIdentifiers: [],
      options: [])
    // Answer opens the app, which joins the call (the microphone needs the app in the foreground).
    let answer = UNNotificationAction(
      identifier: answerCallAction, title: String(localized: "notification_answer_call"),
      options: [.foreground, .authenticationRequired])
    let decline = UNNotificationAction(
      identifier: declineCallAction, title: String(localized: "notification_decline_call"),
      options: [.destructive])
    let call = UNNotificationCategory(
      identifier: callCategory, actions: [answer, decline], intentIdentifiers: [], options: [])
    // A meeting's reminder or start notice: Join opens the app like Answer (the microphone needs
    // the app in the foreground). Clearing the notice reports it to the app (no permission needed),
    // which tells the server so the user's other phones take theirs down.
    let join = UNNotificationAction(
      identifier: joinMeetingAction, title: String(localized: "notification_join_meeting"),
      options: [.foreground, .authenticationRequired])
    let meeting = UNNotificationCategory(
      identifier: meetingCategory, actions: [join], intentIdentifiers: [], options: [.customDismissAction])
    // Letting somebody into a channel needs the unlocked phone; the app decides in the background.
    let approveJoin = UNNotificationAction(
      identifier: approveJoinAction, title: String(localized: "join_request_approve"),
      options: [.authenticationRequired])
    let declineJoin = UNNotificationAction(
      identifier: declineJoinAction, title: String(localized: "join_request_decline"),
      options: [.authenticationRequired])
    let joinRequest = UNNotificationCategory(
      identifier: joinRequestCategory, actions: [approveJoin, declineJoin], intentIdentifiers: [], options: [])
    let joinRequestDeclineOnly = UNNotificationCategory(
      identifier: joinRequestDeclineOnlyCategory, actions: [declineJoin], intentIdentifiers: [], options: [])
    // Handing a meeting over signs with the user's key: unlock first, then the app asks to whom.
    let handOver = UNNotificationAction(
      identifier: handOverAction, title: String(localized: "lobby_left_hand_over"),
      options: [.foreground, .authenticationRequired])
    let lobbyLeft = UNNotificationCategory(
      identifier: lobbyLeftCategory, actions: [handOver], intentIdentifiers: [], options: [])
    center.setNotificationCategories([message, call, meeting, joinRequest, joinRequestDeclineOnly, lobbyLeft])
  }
}
