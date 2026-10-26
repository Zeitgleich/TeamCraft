import Foundation

/// What Browse needs of the core's chat snapshot.
struct BrowseSnapshot: Decodable, Sendable {
  struct Person: Decodable, Identifiable, Sendable {
    let id: String
    let username: String
    let displayName: String
    let isBot: Bool?
    let isGuest: Bool?
  }

  struct Space: Decodable, Identifiable, Sendable {
    let id: String
    let name: String
    /// Whether the user may create channels here, as the core decided it (issue #758).
    let canCreateChannels: Bool?

    /// "Create channel" is offered: the core's word, or offered when an older core didn't say.
    var offersChannelCreation: Bool { canCreateChannels ?? true }
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

  /// Guests browse no channels (the server refuses them, as on desktop): they only find people.
  var canBrowseChannels: Bool { me.isGuest != true }

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

  /// The people "Add people" in a DM or group DM call offers: everyone the user can message who
  /// isn't in that conversation, matching `query` (name or username) when one is typed.
  func addablePeople(to conversationId: String, matching query: String) -> [Person] {
    let members = Set(members(of: conversationId))
    let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
    return people.filter { person in
      !members.contains(person.id)
        && (needle.isEmpty || person.displayName.localizedCaseInsensitiveContains(needle)
          || person.username.localizedCaseInsensitiveContains(needle))
    }
  }

  /// Whom "Add people" in a DM or group DM call starts the meeting room with (docs/meetings.md,
  /// "A call becomes a meeting"): the people in the call (`inCallUsers`, from the call's verified
  /// roster) and the user; the core keeps only those in the DM's encryption group. Added are the
  /// picked people who aren't among them.
  func callAddition(inCallUsers: [String], picked: [String]) -> CallAddition {
    let inCall = Array(Set(inCallUsers).union([me.id])).sorted()
    var added: [String] = []
    for user in picked where !inCall.contains(user) && !added.contains(user) { added.append(user) }
    return CallAddition(inCall: inCall, added: added)
  }

  private func members(of conversationId: String) -> [String] {
    conversations?.first { $0.id == conversationId }?.memberIds ?? []
  }
}

/// The two lists of `TeamCraft.addPeopleToCall`.
struct CallAddition: Equatable {
  let inCall: [String]
  let added: [String]
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
