import Foundation

/// The notification settings that live on the server (synced to every device and used by the push
/// routing): DND schedule and overrides, quick DND, per-conversation mutes. Plain REST through the
/// core (`apiRequest`, paths under `/api/v1`).
extension AppModel {
  /// The whole settings document (`GET /me/notification-settings`), or `nil` offline.
  func notificationSettingsJSON() async -> String? {
    try? await core().apiRequest(method: "GET", path: "/me/notification-settings", maybeBody: nil)
  }

  /// Replaces the schedule, keeping the other settings; stamped with this device's time zone.
  func saveDndSchedule(_ schedule: DndSchedule) async -> Bool {
    guard let current = await notificationSettingsJSON(),
      let body = NotificationSettingsDocument.replacingSchedule(
        in: current, with: schedule.stamped(timeZone: .current, at: Date()))
    else { return false }
    return await request("PUT", "/me/notification-settings", body: body) != nil
  }

  /// Quick DND until `date`; answers the end the server stored.
  func startDnd(until date: Date) async -> Date? {
    guard let answer = await request("PUT", "/me/presence", body: QuickDnd.requestBody(until: date)),
      let response = Wire.decode(PresenceAnswer.self, from: answer)
    else { return nil }
    let maybeUntil = response.until.map(UnixMillis.date)
    QuickDndMemory.until = maybeUntil
    return maybeUntil ?? date
  }

  func endDnd() async -> Bool {
    let isDone = await request("PUT", "/me/presence", body: QuickDnd.endBody) != nil
    if isDone { QuickDndMemory.until = nil }
    return isDone
  }

  /// The user's presence as others see it (`online`, `away`, `dnd`, `offline`).
  func ownPresenceState(userId: String) async -> String? {
    let path = "/presence?user_ids=\(Self.queryEscaped(userId))"
    guard let json = await request("GET", path, body: nil),
      let list = Wire.decode(PresenceList.self, from: json)
    else { return nil }
    return list.presences.first { $0.userId == userId }?.state
  }

  /// People for the DND allow list (`GET /users?query=`).
  func searchPeople(_ query: String) async -> [PersonSummary] {
    let path = "/users?query=\(Self.queryEscaped(query))"
    guard let json = await request("GET", path, body: nil) else { return [] }
    return Wire.decode(PeopleList.self, from: json)?.users ?? []
  }

  func person(_ userId: String) async -> PersonSummary? {
    guard let json = await request("GET", "/users/\(Self.queryEscaped(userId))", body: nil) else { return nil }
    return Wire.decode(PersonSummary.self, from: json)
  }

  /// When the conversation is muted until (still ahead), or `nil`.
  func conversationMutedUntil(_ conversationId: String) async -> Date? {
    guard let json = await request("GET", "/conversations/\(conversationId)/notification-settings", body: nil)
    else { return nil }
    return ConversationMute.mutedUntil(in: json, now: Date())
  }

  /// Mutes the conversation until `date` (`nil` unmutes).
  func setConversationMuted(_ conversationId: String, until maybeDate: Date?) async -> Bool {
    let path = "/conversations/\(conversationId)/notification-settings"
    guard let current = await request("GET", path, body: nil),
      let body = ConversationMute.body(current: current, mutedUntil: maybeDate)
    else { return false }
    return await request("PUT", path, body: body) != nil
  }

  // MARK: Diagnostics

  func pushDiagnostics() async -> PushDiagnosticsInfo? {
    try? await core().pushDiagnostics()
  }

  /// Asks the server for a test push to this device; `nil` if the server couldn't be asked.
  func sendTestPush() async -> TestPushResult? {
    try? await core().sendTestPush()
  }

  private func request(_ method: String, _ path: String, body: String?) async -> String? {
    try? await core().apiRequest(method: method, path: path, maybeBody: body)
  }

  private static func queryEscaped(_ value: String) -> String {
    var allowed = CharacterSet.urlQueryAllowed
    allowed.remove(charactersIn: "&=+?/#")
    return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
  }
}

/// The end of a quick DND this device set, for showing it (the server has no read of its own
/// manual presence yet). Per device and only a convenience.
enum QuickDndMemory {
  private static let key = "quickDndUntil"

  static var until: Date? {
    get {
      let millis = UserDefaults.standard.double(forKey: key)
      guard millis > 0 else { return nil }
      let date = Date(timeIntervalSince1970: millis / 1000)
      return date > Date() ? date : nil
    }
    set {
      if let date = newValue {
        UserDefaults.standard.set(Double(UnixMillis.from(date)), forKey: key)
      } else {
        UserDefaults.standard.removeObject(forKey: key)
      }
    }
  }
}

/// `SetPresenceResponse`.
struct PresenceAnswer: Decodable {
  let manual: String
  let until: Int64?
}

/// `ListPresenceResponse` (only what the app shows).
struct PresenceList: Decodable {
  struct Entry: Decodable {
    let userId: String
    let state: String
  }

  let presences: [Entry]
}

/// A person from the directory (`UserSummary`).
struct PersonSummary: Decodable, Identifiable, Equatable {
  let id: String
  let displayName: String
  let username: String?
}

struct PeopleList: Decodable {
  let users: [PersonSummary]
}
