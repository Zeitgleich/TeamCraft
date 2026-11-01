import Foundation

/// What the long-press menus of the chat list need beyond the list itself (#1105). Whether
/// leaving is offered is the core's rule; nothing here decides it again.
extension AppModel {
  func conversationLeaveChoice(_ conversationId: String) async -> LeaveChoice {
    (try? await core().conversationLeaveChoice(conversationId: conversationId)) ?? .hidden
  }

  func spaceMenu(_ spaceId: String) async -> SpaceMenuItem? {
    try? await core().spaceMenu(spaceId: spaceId)
  }

  /// `{origin}/c/{id}`, or `nil` where the server has no web address.
  func shareLink(forConversation conversationId: String) -> String? {
    guard let origin = maybeServer?.address.origin else { return nil }
    return conversationLink(origin: origin, conversationId: conversationId)
  }

  /// Each of these answers why the core refused (nil when it took the action).
  func leaveChannel(_ conversationId: String) async -> AppFailure? {
    await performReporting(.leaveChannel(conversationId: conversationId))
  }

  func markSpaceRead(_ spaceId: String) async -> AppFailure? {
    await reporting { try await core().markSpaceRead(spaceId: spaceId) }
  }

  func leaveSpace(_ spaceId: String) async -> AppFailure? {
    await reporting { try await core().leaveSpace(spaceId: spaceId) }
  }

  /// Mutes the space until `date` (`nil` unmutes); its level stays. Its channels follow it.
  func setSpaceMuted(_ spaceId: String, until maybeDate: Date?) async -> Bool {
    let path = "/spaces/\(spaceId)/notification-settings"
    guard let current = try? await core().apiRequest(method: "GET", path: path, maybeBody: nil),
      let body = SpaceMute.body(current: current, mutedUntil: maybeDate)
    else { return false }
    return (try? await core().apiRequest(method: "PUT", path: path, maybeBody: body)) != nil
  }

  /// Sets the conversation's level (`all`, `mentions`, `none`).
  func setConversationLevel(_ conversationId: String, level: String) async -> Bool {
    let path = "/conversations/\(conversationId)/notification-settings"
    guard let current = try? await core().apiRequest(method: "GET", path: path, maybeBody: nil),
      let body = ConversationMute.body(current: current, level: level)
    else { return false }
    return (try? await core().apiRequest(method: "PUT", path: path, maybeBody: body)) != nil
  }

  private func reporting(_ block: () async throws -> Void) async -> AppFailure? {
    do {
      try await block()
      return nil
    } catch {
      return AppFailure(CoreFailure(error))
    }
  }
}

/// A space's notification settings (`GET/PUT /spaces/{id}/notification-settings`).
enum SpaceMute {
  /// The settings with a new mute time (`nil`: unmuted); the level stays as it is.
  static func body(current json: String, mutedUntil maybeDate: Date?) -> AppFailure? {
    guard var object = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] else {
      return nil
    }
    if let date = maybeDate {
      object["muted_until"] = UnixMillis.from(date)
    } else {
      object.removeValue(forKey: "muted_until")
    }
    return jsonText(object)
  }
}
