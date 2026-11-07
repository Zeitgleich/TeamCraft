import Foundation

/// A channel's members section (issue #1262), as on desktop and web: what each mark says. What is
/// marked and refused is the core's rule (`teamcraft_client_api::role_review`); the app only words it.
enum ChannelMembers {
  /// The badge of a role; a member, or a role this version doesn't know, has none.
  static func roleKey(_ role: ChannelMemberRole) -> String? {
    switch role {
    case .admin: return "channel_role_admin"
    case .moderator: return "channel_role_moderator"
    case .member, .other: return nil
    }
  }

  /// The mark on a reviewed row's key; a checked key has none.
  static func keyMarkKey(_ mark: RoleKeyMarkItem) -> String? {
    switch mark {
    case .checked: return nil
    case .changed: return "channel_roles_review_key_changed"
    case .uncheckable: return "channel_roles_review_key_uncheckable"
    }
  }

  /// Why the list can't be confirmed.
  static func refusalKey(_ refusal: RoleReviewRefusalItem) -> String {
    switch refusal {
    case .unknownPerson: return "channel_roles_review_unknown_refused"
    case .uncheckableKey: return "channel_roles_review_uncheckable_refused"
    case .noAdmin: return "channel_roles_review_no_admin"
    }
  }

  /// What confirming says afterwards; `nil` once it is done (the panel then shows the new roles).
  static func outcomeKey(_ outcome: ReviewedRolesOutcome) -> String? {
    switch outcome {
    case .confirmed: return nil
    case .changed: return "channel_roles_review_changed"
    case .refused: return "error_generic"
    }
  }

  /// Who confirming takes a role from, by name; somebody nobody can name is `unknown`.
  static func droppedNames(_ dropped: [String?], unknown: String) -> String {
    dropped.map { $0 ?? unknown }.joined(separator: ", ")
  }
}

extension AppModel {
  /// The channel's members section; `nil` when the core can't answer.
  func channelMembers(_ conversationId: String) async -> ChannelMembersInfo? {
    try? await core().channelMembers(conversationId: conversationId)
  }

  /// The members section opened: the members and signed roles are read afresh, as on desktop and web.
  func refreshChannelMembers(_ conversationId: String) async {
    try? await core().refreshChannelMembers(conversationId: conversationId)
  }

  /// Confirms exactly the roles of `review` as the admin opened it; nothing is signed if the log
  /// moved. A failure to reach the core counts as refused.
  func signReviewedChannelRoles(_ conversationId: String, review: ChannelRolesReviewItem) async -> ReviewedRolesOutcome {
    (try? await core().signReviewedChannelRoles(
      conversationId: conversationId, roles: review.rows, atCursor: review.logCursor)) ?? .refused
  }
}
