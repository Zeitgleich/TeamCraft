import Foundation
import OSLog
import UserNotifications

/// The lobby of a meeting in its call (#1859), as desktop and web have it: who waits, for the
/// organizer and the co-hosts only (the core answers `nil` to anybody else), and their "Admit" and
/// "Deny". The core admits: it tells the server and lets the person's devices into the meeting
/// chat's encryption on this device's own decision (e2ee.md §4.6, "Meeting guests"). Also the
/// reminder after the organizer left from CallKit with people still waiting, and its handover.

/// How a decision in the lobby went.
enum LobbyDeciding: Equatable {
  case idle
  case deciding(userId: String)
  /// Whose decision failed and why, as a string key.
  case failed(name: String, reason: String)
}

/// A handover the reminder asked for: the meeting chat whose organizer left.
struct LobbyHandoverRequest: Identifiable, Equatable {
  let conversationId: String
  var id: String { conversationId }
}

/// Whether a guest of `lobby` is ready to be admitted but its key code isn't made yet.
func isWaitingForKeyCode(_ lobby: MeetingLobby) -> Bool {
  lobby.people.contains { person in
    if case .guest(let maybeKeyCode) = person.status { return maybeKeyCode == nil }
    return false
  }
}

extension AppModel {
  private static let lobbyLog = Logger(subsystem: "de.zeitgleich.teamcraft", category: "calls")
  /// The reminder's identifier per meeting chat: one at a time, replaced by a newer one.
  private static func lobbyReminderId(_ conversationId: String) -> String { "lobby-left-\(conversationId)" }

  /// The lobby of the meeting whose chat is `conversationId` if the user hosts it; `nil` otherwise
  /// or when it can't be read.
  func meetingLobby(conversationId: String) async -> MeetingLobby? {
    do {
      return try await core().meetingLobby(conversationId: conversationId)
    } catch {
      Self.lobbyLog.warning("the meeting's lobby could not be read: \(String(describing: error), privacy: .public)")
      return nil
    }
  }

  /// "Admit" or "Deny" on `person`; `.idle` when it went through.
  func decideLobbyEntry(conversationId: String, person: LobbyPerson, decision: LobbyDecision) async -> LobbyDeciding {
    do {
      let outcome = try await core().decideLobbyEntry(
        conversationId: conversationId, userId: person.userId, decision: decision)
      if outcome == .noLongerWaiting {
        return .failed(name: person.displayName, reason: "call_lobby_no_longer_waiting")
      }
      return .idle
    } catch {
      Self.lobbyLog.warning("a lobby decision failed: \(String(describing: error), privacy: .public)")
      let isOffline = CoreFailure(error) == .network
      return .failed(name: person.displayName, reason: isOffline ? "error_network" : "call_lobby_admit_failed")
    }
  }

  /// After the user ended a meeting's call in CallKit, where no question can be asked: if they
  /// organize it, people wait in its lobby and nobody else can let them in, a notification says so
  /// and offers "Hand over". The core decides, as for the in-call leave question.
  func remindOfLobby(conversationId: String) async {
    guard let left = try? await core().lobbyLeftWaiting(conversationId: conversationId) else { return }
    let waiting = Int(left.waiting)
    let content = UNMutableNotificationContent()
    // One and several as separate strings: the titles don't name the number.
    let title = left.meetingTitle
    switch (title.isEmpty, waiting == 1) {
    case (true, true): content.title = String(localized: "lobby_left_title_unnamed_one")
    case (true, false): content.title = String(localized: "lobby_left_title_unnamed_other")
    case (false, true): content.title = String(localized: "lobby_left_title_one \(title)")
    case (false, false): content.title = String(localized: "lobby_left_title_other \(title)")
    }
    content.body = String(localized: "lobby_left_body \(waiting)")
    content.categoryIdentifier = NotificationCategories.lobbyLeftCategory
    content.threadIdentifier = NotificationKeys.lobbyLeftKind
    content.sound = .default
    var info: [AnyHashable: Any] = [
      NotificationKeys.kind: NotificationKeys.lobbyLeftKind,
      NotificationKeys.conversation: conversationId,
    ]
    // The app's own: the tag is over the reminder's identifier, which the notification extension
    // never tags (it tags conversation IDs), so neither a server alert with this category nor a
    // decrypted push of any kind passes `handleLobbyReminder`'s check.
    if let tag = NotificationAuthenticator().tag(for: Self.lobbyReminderId(conversationId)) {
      info[NotificationAuthenticator.tagKey] = tag
    }
    content.userInfo = info
    let request = UNNotificationRequest(
      identifier: Self.lobbyReminderId(conversationId), content: content, trigger: nil)
    do {
      try await UNUserNotificationCenter.current().add(request)
    } catch {
      Self.lobbyLog.warning("the lobby reminder could not be posted: \(String(describing: error), privacy: .public)")
    }
  }

  /// "Hand over" on the reminder, or a tap on it: the app opens the choice of whom to hand the
  /// meeting of `conversationId` to. Only for a reminder this app posted (its tag over the
  /// reminder's identifier).
  func handleLobbyReminder(conversationId: String, tag: String?, notificationId: String?) {
    guard NotificationAuthenticator().isValid(tag: tag, for: Self.lobbyReminderId(conversationId)) else { return }
    if let notificationId {
      UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [notificationId])
    }
    maybeLobbyHandover = LobbyHandoverRequest(conversationId: conversationId)
  }

  /// Who could take the meeting of `conversationId` over now, read afresh: `nil` when nobody waits
  /// any more or nobody is left in the call; throws when it can't be read.
  func lobbyHandoverChoice(conversationId: String) async throws -> (
    meetingTitle: String, waiting: Int, candidates: [CallPerson]
  )? {
    guard let left = try await core().lobbyLeftWaiting(conversationId: conversationId) else { return nil }
    guard let json = try? await core().snapshot(), let people = Wire.decode(CallPeopleSnapshot.self, from: json) else {
      return nil
    }
    let candidates = Array(handoverCandidates(left.candidateUserIds, people: people).prefix(maxHandoverChoices))
    return candidates.isEmpty ? nil : (left.meetingTitle, Int(left.waiting), candidates)
  }

  /// The reminder of `conversationId` is dealt with.
  func removeLobbyReminder(conversationId: String) {
    UNUserNotificationCenter.current().removeDeliveredNotifications(
      withIdentifiers: [Self.lobbyReminderId(conversationId)])
  }
}
