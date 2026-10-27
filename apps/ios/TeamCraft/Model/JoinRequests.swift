import Foundation
import SwiftUI
import UserNotifications

/// Admins asking to join a private channel under `admin_self_join = owner_approval` (admin.md
/// §2.4.1, issue #799). The core decides what matters: which requests wait for this user, whether
/// Approve is offered (only for an admin of the fresh verified roster), and approving is its own
/// signed MLS add. These are the app's plain rules around it, unit-tested without the core.
enum JoinRequests {
  /// One request: an admin and a channel. Its buttons wait while its decision runs.
  static func key(_ request: JoinRequestItem) -> String {
    "\(request.conversationId):\(request.userId)"
  }

  /// The requests for one channel, for its conversation screen.
  static func requests(in conversationId: String, of requests: [JoinRequestItem]) -> [JoinRequestItem] {
    requests.filter { $0.conversationId == conversationId }
  }

  /// What to tell the user when `step` changed nothing; `nil` when it went through.
  static func note(after step: JoinStep, outcome: JoinRequestOutcome) -> JoinNote? {
    switch outcome {
    case .done: return nil
    case .gone: return .gone
    // Asking: this user's own role isn't in a fresh signed roster; approving: the requester's isn't.
    case .notAnAdmin: return step == .ask ? .notAnAdminYourself : .requesterNotAdmin
    case .tooSoon: return .tooSoon
    case .notApprovable: return .notApprovable
    }
  }
}

/// One action on a request.
enum JoinStep: Equatable {
  case ask, withdraw, approve, decline

  /// Runs on `core`; `userId` only for the owner's decisions. Approving shares `history`: what a
  /// self-join gets unless the owner explicitly chose the earlier history too.
  func run(
    on core: TeamCraft, conversationId: String, userId: String, history: JoinHistory = .fromJoin
  ) async throws -> JoinRequestOutcome {
    switch self {
    case .ask: return try await core.requestAdminJoin(conversationId: conversationId)
    case .withdraw: return try await core.withdrawAdminJoin(conversationId: conversationId)
    case .approve:
      return try await core.approveAdminJoin(conversationId: conversationId, userId: userId, history: history)
    case .decline: return try await core.declineAdminJoin(conversationId: conversationId, userId: userId)
    }
  }
}

/// Why an action on a request didn't go through, as the alert says it.
enum JoinNote: Equatable {
  /// Decided on another device or by another owner, withdrawn, or expired.
  case gone
  /// Approving: the fresh signed roster doesn't list the person as an admin.
  case requesterNotAdmin
  /// Asking: this device has no fresh signed roster that lists the user as an admin.
  case notAnAdminYourself
  /// Asking again soon after a decline or a withdrawal.
  case tooSoon
  /// Approving: the verified policy or the channel doesn't allow it on this device.
  case notApprovable
  case failed(AppFailure)

  var message: LocalizedStringKey {
    switch self {
    case .gone: return "join_request_gone"
    case .requesterNotAdmin: return "join_request_not_admin"
    case .notAnAdminYourself: return "join_ask_not_admin"
    case .tooSoon: return "join_ask_too_soon"
    case .notApprovable: return "join_request_not_approvable"
    case .failed(let failure): return failure.message
    }
  }

  /// For a notification, which can't show a `LocalizedStringKey`.
  var notificationText: String {
    switch self {
    case .gone: return String(localized: "join_request_gone")
    case .requesterNotAdmin: return String(localized: "join_request_not_admin")
    case .notAnAdminYourself: return String(localized: "join_ask_not_admin")
    case .tooSoon: return String(localized: "join_ask_too_soon")
    case .notApprovable: return String(localized: "join_request_not_approvable")
    case .failed: return String(localized: "join_request_failed")
    }
  }
}

/// How the channel browser offers a channel the user is not in (the core's `BrowseJoin`).
enum BrowseJoin: Equatable {
  /// A public channel: anyone in the space joins.
  case open
  /// A private channel a workspace admin joins by themselves; that happens on desktop and web.
  case asAdmin
  /// A private channel whose owners decide: "Ask to join".
  case needsApproval
  /// Asked already: "Requested" with "Withdraw".
  case requested

  /// The core's wire value; absent (an older core) is a public channel, one this version doesn't
  /// know offers nothing to press.
  init?(wire maybeWire: String?) {
    switch maybeWire ?? "open" {
    case "open": self = .open
    case "as_admin": self = .asAdmin
    case "needs_approval": self = .needsApproval
    case "requested": self = .requested
    default: return nil
    }
  }
}

/// The local notification of a request (issue #799): posted while the app runs, one per request
/// (its identifier is the core's tag, so a newer request of the same admin for the same channel
/// replaces it), withdrawn once the request is gone.
struct JoinRequestNotice: Equatable {
  let identifier: String
  let title: String
  let body: String
  /// `NotificationCategories.joinRequestCategory`, `…DeclineOnlyCategory` or none (no actions).
  let categoryIdentifier: String
  /// Shown again after a decision from the notification didn't go through: no sound.
  let isQuiet: Bool

  /// Names and actions only where the user's privacy choices allow them: with the app lock on or
  /// nothing shown, the notification says only that somebody asks, and the decision is made in the
  /// app. Approve only for an admin of the fresh verified roster. `maybeNote`: why a decision from
  /// the notification didn't go through.
  init(_ request: JoinRequestItem, showsNames: Bool, maybeNote: JoinNote? = nil) {
    identifier = request.notificationTag
    isQuiet = maybeNote != nil
    guard showsNames else {
      title = String(localized: "join_request_notification_hidden_title")
      body = String(localized: "join_request_notification_hidden_text")
      categoryIdentifier = ""
      return
    }
    let channel = "#" + request.conversationName
    title =
      request.conversationName.isEmpty
      ? String(localized: "join_request_notification_title_unnamed \(request.userLabel)")
      : String(localized: "join_request_notification_title \(request.userLabel) \(channel)")
    switch request.standing {
    case .approvable:
      body = maybeNote?.notificationText ?? String(localized: "join_request_notification_text")
      categoryIdentifier = NotificationCategories.joinRequestCategory
    case .notAnAdmin:
      body = maybeNote?.notificationText ?? String(localized: "join_request_not_admin")
      categoryIdentifier = NotificationCategories.joinRequestDeclineOnlyCategory
    }
  }

  /// The request to hand to the notification center. The tag over the request's identifier marks
  /// it as this app's own: the server could show an alert with the same category, and its actions
  /// are refused without the tag.
  func request(for item: JoinRequestItem) -> UNNotificationRequest {
    let content = UNMutableNotificationContent()
    content.title = title
    content.body = body
    content.categoryIdentifier = categoryIdentifier
    // Grouped on their own: reading the channel doesn't remove them, as it does its messages.
    content.threadIdentifier = NotificationKeys.joinRequestKind
    content.sound = isQuiet ? nil : .default
    var info: [AnyHashable: Any] = [
      NotificationKeys.kind: NotificationKeys.joinRequestKind,
      NotificationKeys.conversation: item.conversationId,
      NotificationKeys.joinRequestUser: item.userId,
    ]
    if let tag = NotificationAuthenticator().tag(for: identifier) {
      info[NotificationAuthenticator.tagKey] = tag
    }
    content.userInfo = info
    return UNNotificationRequest(identifier: identifier, content: content, trigger: nil)
  }
}
