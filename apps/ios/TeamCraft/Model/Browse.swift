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

  /// The one-to-one conversation with a person, if there is one.
  func directConversation(with userId: String) -> String? {
    conversations?.first { $0.kind == "Direct" && ($0.memberIds ?? []).contains(userId) }?.id
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
