import Foundation

/// Co-hosts and handing a meeting over in its call (#1720; docs/meetings.md, "Hosts"). The core
/// signs the host list with the user's identity key and publishes it; nothing here decides who may
/// host: the core's rules, as on Android, desktop and web.
extension AppModel {
  /// Who hosts the meeting whose chat is `conversationId`; `nil` outside a meeting's call.
  func meetingHosts(conversationId: String) async -> MeetingHosts? {
    (try? await core().meetingHosts(conversationId: conversationId)) ?? nil
  }

  /// How the last change of the hosts went on this device; `nil` before one.
  func meetingHostsChange(conversationId: String) async -> HostChangeStatus? {
    guard let text = (try? await core().meetingHostsChange(conversationId: conversationId)) ?? nil
    else {
      return nil
    }
    return HostChangeStatus(text)
  }

  /// "Make co-host", "Remove as co-host" or "Hand over host role" (the organizer only).
  func changeMeetingHost(conversationId: String, userId: String, action: MeetingHostAction) async {
    try? await core().changeMeetingHost(
      conversationId: conversationId, userId: userId, action: action)
  }

  /// The people in the call who could host: known members, no guests, no bots, not the user.
  func hostCandidates(participants: [String]) async -> [PeopleSnapshot.Person] {
    guard let json = try? await core().snapshot(),
      let people = Wire.decode(PeopleSnapshot.self, from: json)
    else {
      return []
    }
    let known = Dictionary(
      (people.users ?? []).map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
    return
      participants
      .filter { $0 != people.me.id }
      .compactMap { known[$0] }
      .filter { $0.isGuest != true && $0.isBot != true }
  }

  /// What to ask when the organizer leaves the call: people wait in the lobby, nobody else hosts
  /// and somebody in the call could take over (the core's rule). `nil`: just leave; a failed read
  /// leaves too, since the user asked to go.
  func leaveOffer(hosts: MeetingHosts, conversationId: String, participants: [String]) async
    -> LeaveOffer?
  {
    let candidates = await hostCandidates(participants: participants)
    guard !candidates.isEmpty,
      let json = try? await core().apiRequest(
        method: "GET", path: "/meetings/\(hosts.meetingId)/lobby", maybeBody: nil),
      let lobby = Wire.decode(LobbyList.self, from: json)
    else { return nil }
    let waiting = lobby.entries.count
    let isOffered =
      (try? await core().offersMeetingHandover(
        conversationId: conversationId, waiting: UInt32(waiting), candidateIds: candidates.map(\.id)
      )) ?? false
    return isOffered
      ? LeaveOffer(waiting: waiting, candidates: Array(candidates.prefix(Self.maxHandoverChoices)))
      : nil
  }

  /// Up to this many people are offered to hand the meeting to, as on the other platforms.
  private static let maxHandoverChoices = 5
}

/// Leaving would leave `waiting` people in the lobby without a host: whom to hand the meeting to.
struct LeaveOffer: Identifiable {
  let waiting: Int
  let candidates: [PeopleSnapshot.Person]
  var id: String { candidates.map(\.id).joined(separator: ",") }
}

/// How the last change of the hosts went, as the core says it.
enum HostChangeStatus: Equatable {
  case saving
  case saved
  /// The core's words for the failure (shown under a translated lead-in).
  case failed(String)

  init(_ text: String) {
    switch text {
    case "saving": self = .saving
    case "saved": self = .saved
    default: self = .failed(text)
    }
  }
}

/// `GET /meetings/{id}/lobby`: only how many wait matters here.
private struct LobbyList: Decodable {
  struct Entry: Decodable {}
  let entries: [Entry]
}
