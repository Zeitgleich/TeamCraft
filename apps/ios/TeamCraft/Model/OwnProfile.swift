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
