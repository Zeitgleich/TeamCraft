import Foundation

/// How an opened push is shown (notifications.md, "Mobile push delivery"). Decided from the core's
/// flags only, so the extension and the app (foreground) behave the same and the rules are
/// unit-tested without the core.
enum PushPresentation: Equatable {
  /// A message notification that alerts: sound unless sent silently, time-sensitive when urgent.
  case message
  /// The message is shown without sound or banner: muted, DND or shown before, and iOS cannot drop
  /// a notification without the filtering entitlement.
  case quietMessage
  /// An incoming call handed to CallKit (`reportNewIncomingVoIPPushPayload`, needs the filtering
  /// entitlement).
  case callKit
  /// An incoming call shown as a time-sensitive alert with answer and decline (no filtering
  /// entitlement, so CallKit can't be reached from the extension).
  case callAlert
  /// The diagnostics' test push: a short confirmation.
  case test
  /// Nothing; only possible with the filtering entitlement (an empty content is dropped).
  case drop
  /// Nothing to show, but iOS shows something without the filtering entitlement: a quiet generic
  /// notification (dismiss or an unknown kind that arrived as an alert).
  case quietGeneric

  static func of(_ flags: PushFlags, canFilter: Bool) -> PushPresentation {
    switch flags.kind {
    case .message:
      if flags.isDuplicate || flags.maybeSuppression != nil { return canFilter ? .drop : .quietMessage }
      return .message
    case .call:
      if flags.isDuplicate || flags.maybeSuppression != nil { return canFilter ? .drop : .quietGeneric }
      return canFilter ? .callKit : .callAlert
    case .test:
      return .test
    case .dismiss, .unknown:
      return canFilter ? .drop : .quietGeneric
    }
  }

  /// Earlier notifications of the same message are removed before this one is delivered, so a
  /// message is listed once (an urgent repeat replaces and alerts again; a duplicate without the
  /// filtering entitlement replaces quietly).
  var replacesSameMessage: Bool {
    self == .message || self == .quietMessage
  }
}

/// The flags of an opened push the presentation depends on (from `PushNotification`).
struct PushFlags: Equatable {
  var kind: PushNotificationKind
  var isDuplicate = false
  var maybeSuppression: NotificationSuppression?
  var isUrgent = false
  var isSilent = false
  var isMention = false
  var maybeBadge: UInt32?
}

extension PushFlags {
  init(_ notification: PushNotification) {
    self.init(
      kind: notification.kind, isDuplicate: notification.isDuplicate,
      maybeSuppression: notification.maybeSuppression, isUrgent: notification.isUrgent,
      isSilent: notification.isSilent, isMention: notification.isMention, maybeBadge: notification.maybeBadge)
  }
}

/// The keys the app writes into a notification's `userInfo`: only what the core decrypted, so a
/// delivered notification can be matched against later dismissals and repeats.
enum NotificationKeys {
  static let kind = "kind"
  static let conversation = NotificationCategories.conversationKey
  static let message = "message_id"
  static let thread = "thread_id"
  static let call = "call_id"

  static let messageKind = "message"
  static let callKind = "call"
  static let testKind = "test"

  /// `userInfo` of a notification the app built; `maybeTag` is `NotificationAuthenticator`'s tag.
  static func userInfo(
    kind: String, conversationId: String, maybeTag: String?, maybeMessageId: String? = nil,
    maybeThreadId: String? = nil, maybeCallId: String? = nil
  ) -> [AnyHashable: Any] {
    var info: [AnyHashable: Any] = [Self.kind: kind, conversation: conversationId]
    if let maybeTag { info[NotificationAuthenticator.tagKey] = maybeTag }
    if let maybeMessageId { info[message] = maybeMessageId }
    if let maybeThreadId { info[thread] = maybeThreadId }
    if let maybeCallId { info[call] = maybeCallId }
    return info
  }
}

/// What a delivered notification carries, read back from its `userInfo`.
struct DeliveredPush: Equatable {
  let identifier: String
  let maybeKind: String?
  let maybeConversationId: String?
  let maybeThreadId: String?
  let maybeMessageId: String?
  let maybeCallId: String?

  init(identifier: String, userInfo: [AnyHashable: Any]) {
    self.identifier = identifier
    maybeKind = userInfo[NotificationKeys.kind] as? String
    maybeConversationId = userInfo[NotificationKeys.conversation] as? String
    maybeThreadId = userInfo[NotificationKeys.thread] as? String
    maybeMessageId = userInfo[NotificationKeys.message] as? String
    maybeCallId = userInfo[NotificationKeys.call] as? String
  }

  /// Message notifications written before kinds were stored have a conversation but no kind.
  var isMessage: Bool {
    maybeKind == NotificationKeys.messageKind || (maybeKind == nil && maybeConversationId != nil)
  }

  var isCall: Bool { maybeKind == NotificationKeys.callKind }
}

/// A `dismiss` wake-up: the conversation (or one thread of it) was read up to a message on another
/// device, or a ring ended.
struct DismissTarget: Equatable {
  let conversationId: String
  var maybeThreadId: String?
  var maybeMessageId: String?
  var maybeCallId: String?
}

/// Which delivered notifications go when something was read or ended. Pure, unit-tested.
enum NotificationMatcher {
  /// For a dismiss with a call ID: that call's alert. Otherwise the message notifications of the
  /// conversation (only the named thread if one was read, else the top-level ones) whose message is
  /// not newer than the dismissed one.
  static func identifiers(_ delivered: [DeliveredPush], dismissing target: DismissTarget) -> [String] {
    if let callId = target.maybeCallId {
      return
        delivered
        .filter { $0.isCall && ($0.maybeCallId == callId || ($0.maybeCallId == nil && $0.maybeConversationId == target.conversationId)) }
        .map(\.identifier)
    }
    return
      delivered
      .filter { push in
        push.isMessage && push.maybeConversationId == target.conversationId
          && push.maybeThreadId == target.maybeThreadId
          && isAtOrBefore(push.maybeMessageId, target.maybeMessageId)
      }
      .map(\.identifier)
  }

  /// Notifications of the same message (replaced by a repeat).
  static func identifiers(_ delivered: [DeliveredPush], sameMessageAs messageId: String, in conversationId: String)
    -> [String]
  {
    delivered
      .filter { $0.isMessage && $0.maybeConversationId == conversationId && $0.maybeMessageId == messageId }
      .map(\.identifier)
  }

  /// Top-level message notifications of conversations the core reports fully read. Thread replies
  /// stay until their thread is read (a `dismiss` with the thread).
  static func identifiers(_ delivered: [DeliveredPush], readConversations: Set<String>) -> [String] {
    delivered
      .filter { push in
        guard push.isMessage, push.maybeThreadId == nil, let conversation = push.maybeConversationId else {
          return false
        }
        return readConversations.contains(conversation)
      }
      .map(\.identifier)
  }

  /// Snowflake IDs grow with time; compared as numbers. A notification without a message ID, or a
  /// dismiss without one, counts as covered.
  static func isAtOrBefore(_ maybeMessageId: String?, _ maybeReadUpTo: String?) -> Bool {
    guard let messageId = maybeMessageId, let readUpTo = maybeReadUpTo else { return true }
    guard let message = UInt64(messageId), let read = UInt64(readUpTo) else { return messageId == readUpTo }
    return message <= read
  }
}

/// What one conversation adds to the app icon badge.
struct UnreadSummary: Equatable {
  let isDirectMessage: Bool
  let unreadCount: UInt32
  let mentionCount: UInt32
}

/// The badge from local state, the same rule as the server's (conversations with unread mentions or
/// unread DMs); followed threads are only in the server's count, which the wake-ups carry.
enum BadgeCount {
  static func local(_ conversations: [UnreadSummary]) -> Int {
    conversations.filter { $0.mentionCount > 0 || ($0.isDirectMessage && $0.unreadCount > 0) }.count
  }
}

/// The device's offset from UTC, which the core's DND check needs.
enum PushClock {
  static func utcOffsetMinutes(timeZone: TimeZone = .current, at date: Date = Date()) -> Int16 {
    Int16(clamping: timeZone.secondsFromGMT(for: date) / 60)
  }
}

/// The payload the extension forwards to the app's PushKit registry for CallKit
/// (`reportNewIncomingVoIPPushPayload`). A VoIP push from the server carries the sealed wake-up
/// under `PushPayload.sealedKey` instead.
enum CallForward {
  static let conversationKey = "conversation_id"
  static let callerNameKey = "caller_name"
  static let callIdKey = "call_id"
}
