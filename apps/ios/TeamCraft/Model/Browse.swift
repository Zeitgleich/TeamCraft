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
    /// Version of the space's picture; without one the space shows its letter (#1526).
    let maybeImageVersion: String?

    /// Whether the user may add or invite members, or invite guests, here (#1337).
    let canAddMembers: Bool?
    let canInviteGuests: Bool?

    /// "Create channel" is offered: the core's word, or offered when an older core didn't say.
    var offersChannelCreation: Bool { canCreateChannels ?? true }
    /// "Invite people" is offered: the user may invite anybody here.
    var canInvite: Bool { canAddMembers == true || canInviteGuests == true }
  }

  struct Conversation: Decodable, Sendable {
    let id: String
    /// `Public`, `Private`, `Direct`, `Group` or `Integration`.
    let kind: String
    let memberIds: [String]?
    /// The channel permissions the server says the user holds here; nil when it didn't say.
    let maybePermissions: [String]?

    /// "Add people" shows: the server says the user may manage the channel's members (#1534).
    var canAddMembers: Bool {
      kind != "Direct" && kind != "Group" && (maybePermissions ?? []).contains("manage_members")
    }
  }

  let me: Person
  let spaces: [Space]?
  let conversations: [Conversation]?

  /// Guests browse no channels (the server refuses them, as on desktop): they only find people.
  var canBrowseChannels: Bool { me.isGuest != true }

  /// The DM (one person) or group DM (several) with exactly these people and the user, if there is
  /// one.
  func directConversation(with userIds: [String]) -> String? {
    let wanted = Set(userIds).subtracting([me.id])
    let kind = wanted.count == 1 ? "Direct" : "Group"
    return conversations?.first {
      $0.kind == kind && Set($0.memberIds ?? []).subtracting([me.id]) == wanted
    }?.id
  }

  /// Whom "Add people" in a DM or group DM call doesn't offer: the conversation's members and the
  /// user (those not in the call join it from the DM).
  func notAddableToCall(in conversationId: String) -> Set<String> {
    Set(members(of: conversationId)).union([me.id])
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
  /// How the user gets in (`open`, `as_admin`, `needs_approval`, `requested`); absent from older
  /// cores, which listed public channels only.
  let join: String?

  var id: String { conversation }
  /// `nil` for a way in this version doesn't know: nothing to press.
  var maybeJoin: BrowseJoin? { BrowseJoin(wire: join) }
}
