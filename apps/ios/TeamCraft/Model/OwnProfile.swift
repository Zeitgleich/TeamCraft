import Foundation

/// The member's own profile (`/me/profile`) as the server sent it. The update replaces the whole
/// profile, so a change to the name sends the other fields back as they are (issue #408).
enum OwnProfileDocument {
  private static let kept: Set<String> = ["title", "department", "phone", "time_zone", "working_hours"]

  /// The workspace policy lets people replace the name from the identity provider. Older servers
  /// say nothing: no.
  static func canOverrideDisplayName(in json: String) -> Bool {
    dictionary(json)?["can_override_display_name"] as? Bool ?? false
  }

  /// A picture shows for the member (their own or the identity provider's).
  static func hasAvatar(in json: String) -> Bool {
    dictionary(json)?["has_avatar"] as? Bool ?? false
  }

  /// The member may set or remove their own picture: not while the workspace shows the identity
  /// provider's picture without overrides (issue #2071). Older servers say nothing: yes.
  static func canChangeAvatar(in json: String) -> Bool {
    dictionary(json)?["can_change_avatar"] as? Bool ?? true
  }

  /// Changes with every new picture; the shown picture is read again when it does.
  static func avatarVersion(in json: String) -> String? {
    dictionary(json)?["avatar_version"] as? String
  }

  /// The name the member chose, or `nil` while the sign-in name is shown.
  static func displayNameOverride(in json: String) -> String? {
    dictionary(json)?["display_name_override"] as? String
  }

  /// The body of `PUT /me/profile` with `maybeName` as the name (`nil`: the sign-in name).
  static func updateBody(from json: String, name maybeName: String?) -> String? {
    guard var object = dictionary(json) else { return nil }
    object = object.filter { kept.contains($0.key) }
    if let name = maybeName { object["display_name"] = name }
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return nil }
    return String(decoding: data, as: UTF8.self)
  }

  /// What is wrong with the typed name, if anything: blank, or with control characters.
  /// The server checks the length, which is a setting.
  static func problem(typed: String) -> TextProblem? {
    let trimmed = typed.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.isEmpty { return .missing("profile_name_empty") }
    if trimmed.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) {
      return .rejected("profile_name_invalid")
    }
    return nil
  }

  private static func dictionary(_ json: String) -> [String: Any]? {
    (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any]
  }
}

extension AppModel {
  /// The profile document, or `nil` offline.
  func ownProfileJSON() async -> String? {
    try? await core().apiRequest(method: "GET", path: "/me/profile", maybeBody: nil)
  }

  /// The member's own picture as image data, or `nil` without one (or offline).
  func ownAvatarData(userId: String) async -> Data? {
    try? await core().avatarPicture(userId: userId, maxBytes: Self.maxAvatarBytes)
  }

  /// Makes `png` (a square the app cropped) the member's own picture; the server re-encodes it.
  /// Answers what to say below the picture when it failed.
  func setOwnAvatar(png: Data) async -> TextProblem? {
    do {
      _ = try await core().setOwnAvatar(png: png)
      return nil
    } catch {
      return avatarProblem(error)
    }
  }

  /// Removes the member's own picture; what to say when it failed.
  func removeOwnAvatar() async -> TextProblem? {
    do {
      _ = try await core().apiRequest(method: "DELETE", path: "/me/avatar", maybeBody: nil)
      return nil
    } catch {
      return avatarProblem(error)
    }
  }

  /// A refusal of the server (not a picture, or the policy changed meanwhile) is worded for the
  /// picture; a lost connection and the like as everywhere.
  private func avatarProblem(_ error: Error) -> TextProblem {
    switch CoreFailure(error) {
    case .other: return .rejected("profile_picture_rejected")
    case let failure: return .refused(AppFailure(failure))
    }
  }

  private static let maxAvatarBytes: UInt32 = 2 * 1024 * 1024

  /// Saves `maybeName` as the member's name (`nil`: the sign-in name). Answers what to say at the
  /// field when it failed: the server's refusal of the name (too long, or the policy changed
  /// meanwhile) is worded for the name, a lost connection and the like as everywhere.
  func setDisplayName(profileJSON: String, to maybeName: String?) async -> TextProblem? {
    guard let body = OwnProfileDocument.updateBody(from: profileJSON, name: maybeName) else {
      return .refused(.generic)
    }
    do {
      _ = try await core().apiRequest(method: "PUT", path: "/me/profile", maybeBody: body)
      return nil
    } catch {
      switch CoreFailure(error) {
      case .other: return .rejected("profile_name_rejected")
      case let failure: return .refused(AppFailure(failure))
      }
    }
  }
}
