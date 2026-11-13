import Foundation
import OSLog
import SwiftUI

/// Co-hosts and handing a meeting over in its call (#1720), as desktop and web do it
/// (docs/meetings.md, "Hosts"): who hosts the meeting of the call, for the tiles' "Host" and
/// "Co-host", the organizer's "Make co-host", "Remove as co-host" and "Hand over host role" (the
/// core signs the new host list, e2ee.md §4.6), and the question before the organizer leaves with
/// people waiting in the lobby and nobody else to let them in. The core decides; these are the
/// buttons.
enum HostBadge: Equatable {
  case host, coHost

  var label: LocalizedStringKey {
    switch self {
    case .host: return "call_tile_host"
    case .coHost: return "call_tile_co_host"
    }
  }

  /// `userId`'s part in the meeting `hosts` describes; `nil` for an ordinary participant or a call
  /// outside a meeting.
  init?(userId: String, hosts: MeetingHosts?) {
    guard let hosts else { return nil }
    if hosts.organizerUserId == userId {
      self = .host
    } else if hosts.coHostUserIds.contains(userId) {
      self = .coHost
    } else {
      return nil
    }
  }
}

/// Somebody in the call, as the host menu and the leave question show them.
struct CallPerson: Identifiable, Equatable, Decodable {
  let id: String
  let displayName: String
  let isGuest: Bool?
  let isBot: Bool?

  /// Guests and bots host nothing: the server refuses them.
  var mayHost: Bool { isGuest != true && isBot != true }
}

/// The people part of the core's snapshot, with what decides who may host.
struct CallPeopleSnapshot: Decodable {
  let me: CallPerson
  let users: [CallPerson]?

  /// The others among `participantIds` this device knows, once each and in their order.
  func others(_ participantIds: [String]) -> [CallPerson] {
    var seen = Set<String>()
    return participantIds.compactMap { id in
      guard id != me.id, seen.insert(id).inserted else { return nil }
      return users?.first { $0.id == id }
    }
  }
}

/// How many people the leave question offers to hand the meeting to, as on desktop.
let maxHandoverChoices = 5

/// The organizer leaves with `waiting` people in the lobby: whom the meeting can go to.
struct HandoverOffer: Identifiable, Equatable {
  let waiting: Int
  let candidates: [CallPerson]

  var id: String { candidates.map(\.id).joined(separator: ",") }
}

/// How the last change of the hosts on this device went.
enum HostsChange: Equatable {
  case idle, saving
  /// The core's words, or `nil` for a failure it gave none for.
  case failed(String?)
}

/// What the organizer's menu offers on `person`: "Make co-host" or "Remove as co-host", and "Hand
/// over host role". Nothing in anybody else's menu, nor on guests, bots or the organizer.
func hostActions(for person: CallPerson, hosts: MeetingHosts?) -> [MeetingHostAction] {
  guard let hosts, hosts.ownPart == .organizer, person.mayHost, person.id != hosts.organizerUserId else { return [] }
  return [hosts.coHostUserIds.contains(person.id) ? .removeCoHost : .makeCoHost, .handOver]
}

/// The others in the call who could take the meeting over: members, not guests or bots.
func handoverCandidates(_ participantIds: [String], people: CallPeopleSnapshot) -> [CallPerson] {
  people.others(participantIds).filter(\.mayHost)
}

/// How many wait in a meeting's lobby, from the server's `GET /meetings/{id}/lobby`; `nil` if unreadable.
func lobbyWaitingCount(_ json: String) -> Int? {
  guard let data = json.data(using: .utf8),
    let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
    let entries = object["entries"] as? [Any]
  else { return nil }
  return entries.count
}

extension AppModel {
  private static let hostsLog = Logger(subsystem: "de.zeitgleich.teamcraft", category: "calls")
  /// How long leaving waits for the handover check before it just leaves.
  private static let handoverTimeout: Duration = .seconds(4)

  /// Who hosts the meeting whose chat is `conversationId`; `nil` for a conversation of no meeting
  /// or when it can't be read.
  func meetingHosts(conversationId: String) async -> MeetingHosts? {
    do {
      return try await core().meetingHosts(conversationId: conversationId)
    } catch {
      Self.hostsLog.warning("the meeting's hosts could not be read: \(String(describing: error), privacy: .public)")
      return nil
    }
  }

  /// The others in the call, for the host menu; empty while the store can't be read.
  func callPeople(_ participantIds: [String]) async -> [CallPerson] {
    guard let json = try? await core().snapshot(), let people = Wire.decode(CallPeopleSnapshot.self, from: json) else {
      return []
    }
    return people.others(participantIds)
  }

  /// The organizer's menu on `userId`: the core signs and publishes the new hosts; this waits until
  /// it is saved or refused.
  func changeMeetingHost(conversationId: String, userId: String, action: MeetingHostAction) async -> HostsChange {
    do {
      try await core().changeMeetingHost(conversationId: conversationId, userId: userId, action: action)
      for _ in 0..<40 {
        switch try await core().meetingHostsChange(conversationId: conversationId) {
        case nil, "saving"?: try await Task.sleep(for: .milliseconds(250))
        case "saved"?: return .idle
        case let reason?: return .failed(reason)
        }
      }
      return .idle
    } catch is CancellationError {
      return .idle
    } catch {
      Self.hostsLog.warning("the meeting's hosts could not be changed: \(String(describing: error), privacy: .public)")
      return .failed(nil)
    }
  }

  /// "Hand over to … and leave": the core takes the signed handover on its own queue, which goes on
  /// after the call. A failure is only logged, as desktop and web leave at once too.
  func handOverBeforeLeaving(conversationId: String, userId: String) async {
    do {
      try await core().changeMeetingHost(conversationId: conversationId, userId: userId, action: .handOver)
    } catch {
      Self.hostsLog.warning("the meeting could not be handed over: \(String(describing: error), privacy: .public)")
    }
  }

  /// What to ask before the user leaves the call of `conversationId`: a handover when the user
  /// organizes its meeting, people wait in the lobby, nobody else hosts it and somebody here could
  /// (the core decides, as on desktop and web). `nil` to just leave, also when the check takes
  /// too long, so leaving never hangs on the network.
  func handoverOffer(conversationId: String, participantIds: [String], hosts: MeetingHosts) async -> HandoverOffer? {
    guard hosts.ownPart == .organizer else { return nil }
    return await withTaskGroup(of: HandoverOffer?.self) { group in
      group.addTask { @MainActor in
        await self.checkHandover(conversationId: conversationId, participantIds: participantIds, hosts: hosts)
      }
      group.addTask {
        try? await Task.sleep(for: Self.handoverTimeout)
        return nil
      }
      let first = await group.next() ?? nil
      group.cancelAll()
      return first
    }
  }

  private func checkHandover(conversationId: String, participantIds: [String], hosts: MeetingHosts) async -> HandoverOffer? {
    guard let json = try? await core().snapshot(), let people = Wire.decode(CallPeopleSnapshot.self, from: json) else {
      return nil
    }
    let candidates = handoverCandidates(participantIds, people: people)
    guard !candidates.isEmpty,
      let lobby = try? await core().apiRequest(method: "GET", path: "/meetings/\(hosts.meetingId)/lobby", maybeBody: nil),
      let waiting = lobbyWaitingCount(lobby), waiting > 0,
      (try? await core().offersMeetingHandover(
        conversationId: conversationId, waiting: UInt32(waiting), candidateIds: candidates.map(\.id))) == true
    else { return nil }
    return HandoverOffer(waiting: waiting, candidates: Array(candidates.prefix(maxHandoverChoices)))
  }
}
