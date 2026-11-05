import SwiftUI

/// One call of the user's history (teamcraft-client-api `calls`).
struct CallHistoryItem: Decodable, Equatable, Identifiable, Sendable {
  let id: String
  let conversation: String
  /// `missed`, `incoming` or `outgoing`.
  let direction: String
  /// Unix milliseconds.
  let startedAt: Int64

  var startDate: Date { Date(timeIntervalSince1970: TimeInterval(startedAt) / 1000) }

  var directionTitle: LocalizedStringKey {
    switch direction {
    case "missed": return "space_call_missed"
    case "outgoing": return "space_call_outgoing"
    default: return "space_call_incoming"
    }
  }
}

/// Somebody in a space, as far as the user may see it.
struct SpaceMember: Decodable, Equatable, Identifiable, Sendable {
  let user: String
  let displayName: String
  var isGuest: Bool?
  var isBot: Bool?

  var id: String { user }
}

/// The member list of a space: `loading`, `hidden` (guests may not see it) or `loaded`.
struct SpaceRoster: Decodable, Equatable, Sendable {
  let state: String
  var members: [SpaceMember]?

  /// The people to list: by name, integrations' bot accounts left out.
  var people: [SpaceMember] {
    (members ?? [])
      .filter { $0.isBot != true }
      .sorted { $0.displayName.localizedCaseInsensitiveCompare($1.displayName) == .orderedAscending }
  }
}

/// A saved message, with what the list shows of it.
struct SavedItem: Decodable, Equatable, Identifiable, Sendable {
  struct SavedMessage: Decodable, Equatable, Sendable {
    let id: String
    let conversation: String
    let body: String
  }

  let message: SavedMessage
  let conversationName: String

  var id: String { message.id }
}

/// A space the user can open the views of.
struct SpaceChoice: Equatable, Identifiable, Sendable {
  let id: String
  let name: String
  /// Version of the space's picture; `nil`: no picture, the title stands alone (#284).
  var maybeImageVersion: String?
}

/// The views of one space, in the order of the sidebar on the desktop.
enum SpaceViewTab: String, CaseIterable, Identifiable, Sendable {
  case people, calls, meetings, saved, files

  var id: Self { self }

  var title: LocalizedStringKey {
    switch self {
    case .people: return "space_tab_people"
    case .calls: return "space_tab_calls"
    case .meetings: return "space_tab_meetings"
    case .saved: return "space_tab_saved"
    case .files: return "space_tab_files"
    }
  }
}

/// One choice per space the conversations are in, by name.
func spaceChoices(_ conversations: [ConversationItem]) -> [SpaceChoice] {
  var choices: [String: SpaceChoice] = [:]
  for conversation in conversations {
    if let id = conversation.maybeSpaceId, let name = conversation.maybeSpaceName {
      choices[id] = SpaceChoice(id: id, name: name, maybeImageVersion: conversation.maybeSpaceImageVersion)
    }
  }
  return choices.values
    .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
}

/// The meetings listed in a space: those scheduled in it.
func meetingsOfSpace(_ meetings: [Meeting], spaceId: String) -> [Meeting] {
  meetings.filter { $0.spaceId == spaceId }
}
