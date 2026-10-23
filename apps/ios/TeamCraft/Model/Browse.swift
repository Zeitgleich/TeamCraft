import Foundation

/// What Browse needs of the core's chat snapshot.
struct BrowseSnapshot: Decodable, Sendable {
  struct Person: Decodable, Identifiable, Sendable {
    let id: String
    let username: String
    let displayName: String
    let isBot: Bool?
  }

  struct Space: Decodable, Identifiable, Sendable {
    let id: String
    let name: String
  }

  struct Conversation: Decodable, Sendable {
    let id: String
    /// `Public`, `Private`, `Direct`, `Group` or `Integration`.
    let kind: String
    let memberIds: [String]?
  }

  let me: Person
  let users: [Person]?
  let spaces: [Space]?
  let conversations: [Conversation]?

  /// Everyone the user can message, without the user and bots.
  var people: [Person] {
    (users ?? []).filter { $0.id != me.id && $0.isBot != true }
      .sorted { $0.displayName.localizedCaseInsensitiveCompare($1.displayName) == .orderedAscending }
  }

  /// The DM (one person) or group DM (several) with exactly these people and the user, if there is
  /// one.
  func directConversation(with userIds: [String]) -> String? {
    let wanted = Set(userIds).subtracting([me.id])
    let kind = wanted.count == 1 ? "Direct" : "Group"
    return conversations?.first {
      $0.kind == kind && Set($0.memberIds ?? []).subtracting([me.id]) == wanted
    }?.id
  }
}

/// A channel of a space as Browse lists it (`browse_channels`).
struct BrowsableChannel: Decodable, Identifiable, Sendable {
  let conversation: String
  let name: String
  let topic: String?
  let isMember: Bool
  let isArchived: Bool?

  var id: String { conversation }
}
