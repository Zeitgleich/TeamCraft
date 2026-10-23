import Foundation

/// The weekly do-not-disturb schedule as the server stores it (`dnd_schedule` of
/// `GET/PUT /me/notification-settings`, notifications.md "Mobile push delivery"): several ranges,
/// evaluated in the user's IANA time zone (the fixed offset is the fallback), with overrides for
/// selected people and, optionally, urgent messages. The overrides apply to quick DND too.
struct DndSchedule: Equatable {
  /// The server accepts at most this many ranges.
  static let maxWindows = 14

  var utcOffsetMinutes: Int
  var maybeTimeZone: String?
  var windows: [DndWindow]
  var allowUserIds: [String]
  var allowsUrgent: Bool

  static let empty = DndSchedule(
    utcOffsetMinutes: 0, maybeTimeZone: nil, windows: [], allowUserIds: [], allowsUrgent: false)

  /// Saved with the device's time zone and its current offset, so the server follows DST. The
  /// zone names a place much more precisely than an offset: it is only sent while there are
  /// windows to evaluate.
  func stamped(timeZone: TimeZone, at date: Date) -> DndSchedule {
    var stamped = self
    guard !windows.isEmpty else {
      stamped.maybeTimeZone = nil
      stamped.utcOffsetMinutes = 0
      return stamped
    }
    stamped.maybeTimeZone = timeZone.identifier
    stamped.utcOffsetMinutes = timeZone.secondsFromGMT(for: date) / 60
    return stamped
  }

  var canAddWindow: Bool { windows.count < Self.maxWindows }
}

/// One range: on the given weekdays from `startMinute` to `endMinute` (minutes after local
/// midnight). An end before the start runs past midnight into the next day.
struct DndWindow: Equatable, Identifiable {
  /// Only for lists on screen; not sent.
  var id = UUID()
  var weekdays: [Weekday]
  var startMinute: Int
  var endMinute: Int

  /// Weeknights 22:00 to 07:00, the range a new entry starts with.
  static var weeknights: DndWindow {
    DndWindow(weekdays: [.monday, .tuesday, .wednesday, .thursday, .friday], startMinute: 22 * 60, endMinute: 7 * 60)
  }

  var isOvernight: Bool { endMinute < startMinute }

  static func == (lhs: DndWindow, rhs: DndWindow) -> Bool {
    lhs.weekdays == rhs.weekdays && lhs.startMinute == rhs.startMinute && lhs.endMinute == rhs.endMinute
  }
}

/// Weekdays as the server names them (`mon` … `sun`), in Monday-first order.
enum Weekday: String, CaseIterable, Identifiable {
  case monday = "mon"
  case tuesday = "tue"
  case wednesday = "wed"
  case thursday = "thu"
  case friday = "fri"
  case saturday = "sat"
  case sunday = "sun"

  var id: String { rawValue }

  /// `Calendar`'s weekday number (1 = Sunday).
  var calendarWeekday: Int {
    switch self {
    case .sunday: return 1
    case .monday: return 2
    case .tuesday: return 3
    case .wednesday: return 4
    case .thursday: return 5
    case .friday: return 6
    case .saturday: return 7
    }
  }

  /// The localized short name ("Mon", "Mo.").
  func shortName(calendar: Calendar = .current) -> String {
    calendar.shortWeekdaySymbols[calendarWeekday - 1]
  }

  /// Monday-first, without duplicates.
  static func sorted(_ days: some Sequence<Weekday>) -> [Weekday] {
    let set = Set(days)
    return allCases.filter { set.contains($0) }
  }
}

extension DndSchedule: Codable {
  private enum CodingKeys: String, CodingKey {
    case utcOffsetMinutes = "utc_offset_minutes"
    case maybeTimeZone = "time_zone"
    case windows
    case allowUserIds = "allow_user_ids"
    case allowsUrgent = "allows_urgent"
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    utcOffsetMinutes = try container.decodeIfPresent(Int.self, forKey: .utcOffsetMinutes) ?? 0
    maybeTimeZone = try container.decodeIfPresent(String.self, forKey: .maybeTimeZone)
    windows = try container.decodeIfPresent([DndWindow].self, forKey: .windows) ?? []
    allowUserIds = try container.decodeIfPresent([String].self, forKey: .allowUserIds) ?? []
    allowsUrgent = try container.decodeIfPresent(Bool.self, forKey: .allowsUrgent) ?? false
  }

  func encode(to encoder: Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(utcOffsetMinutes, forKey: .utcOffsetMinutes)
    try container.encodeIfPresent(maybeTimeZone, forKey: .maybeTimeZone)
    try container.encode(windows, forKey: .windows)
    try container.encode(allowUserIds, forKey: .allowUserIds)
    try container.encode(allowsUrgent, forKey: .allowsUrgent)
  }
}

extension DndWindow: Codable {
  private enum CodingKeys: String, CodingKey {
    case weekdays
    case startMinute = "start_minute"
    case endMinute = "end_minute"
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    // A weekday name this version doesn't know is skipped rather than failing the schedule.
    let names = try container.decode([String].self, forKey: .weekdays)
    weekdays = Weekday.sorted(names.compactMap(Weekday.init(rawValue:)))
    startMinute = try container.decode(Int.self, forKey: .startMinute)
    endMinute = try container.decode(Int.self, forKey: .endMinute)
  }

  func encode(to encoder: Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(Weekday.sorted(weekdays).map(\.rawValue), forKey: .weekdays)
    try container.encode(startMinute, forKey: .startMinute)
    try container.encode(endMinute, forKey: .endMinute)
  }
}

/// Reads and replaces the schedule inside the whole settings document. `PUT` replaces every field,
/// so the other fields (and ones this version doesn't know) are kept as they came.
enum NotificationSettingsDocument {
  static let scheduleKey = "dnd_schedule"

  static func schedule(in json: String) -> DndSchedule? {
    guard let object = dictionary(json), let schedule = object[scheduleKey],
      let data = try? JSONSerialization.data(withJSONObject: schedule)
    else { return nil }
    return try? JSONDecoder().decode(DndSchedule.self, from: data)
  }

  /// The document with `schedule` in place (`nil` removes it); `nil` if `json` isn't an object.
  static func replacingSchedule(in json: String, with maybeSchedule: DndSchedule?) -> String? {
    guard var object = dictionary(json) else { return nil }
    if let schedule = maybeSchedule {
      guard let data = try? JSONEncoder().encode(schedule),
        let encoded = try? JSONSerialization.jsonObject(with: data)
      else { return nil }
      object[scheduleKey] = encoded
    } else {
      object.removeValue(forKey: scheduleKey)
    }
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return nil }
    return String(decoding: data, as: UTF8.self)
  }

  private static func dictionary(_ json: String) -> [String: Any]? {
    (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any]
  }
}

/// Quick do-not-disturb through presence `until` (`PUT /me/presence`).
enum QuickDnd: Equatable {
  case oneHour
  /// Until the next 08:00 (tomorrow morning, or this morning when it is still night).
  case untilMorning
  case until(Date)

  static let morningHour = 8

  func endDate(now: Date, calendar: Calendar) -> Date {
    switch self {
    case .oneHour:
      return now.addingTimeInterval(3600)
    case .untilMorning:
      return Self.nextMorning(after: now, calendar: calendar)
    case .until(let date):
      return date
    }
  }

  /// The next 08:00 strictly after `now` in the calendar's time zone.
  static func nextMorning(after now: Date, calendar: Calendar) -> Date {
    let components = DateComponents(hour: morningHour, minute: 0, second: 0)
    return calendar.nextDate(after: now, matching: components, matchingPolicy: .nextTime) ?? now.addingTimeInterval(
      12 * 3600)
  }

  /// `{"manual":"dnd","until":<unix ms>}`.
  static func requestBody(until date: Date) -> String {
    jsonText(["manual": "dnd", "until": UnixMillis.from(date)])
  }

  /// Back to automatic presence: DND ends now.
  static var endBody: String {
    jsonText(["manual": "auto"])
  }
}

/// How long a conversation stays muted (`muted_until` of its notification settings).
enum MutePreset: String, CaseIterable, Identifiable {
  case oneHour, eightHours, untilMorning, oneWeek

  var id: String { rawValue }

  func mutedUntil(now: Date, calendar: Calendar) -> Date {
    switch self {
    case .oneHour: return now.addingTimeInterval(3600)
    case .eightHours: return now.addingTimeInterval(8 * 3600)
    case .untilMorning: return QuickDnd.nextMorning(after: now, calendar: calendar)
    case .oneWeek: return calendar.date(byAdding: .day, value: 7, to: now) ?? now.addingTimeInterval(7 * 86400)
    }
  }
}

/// A conversation's notification settings (`GET/PUT /conversations/{id}/notification-settings`).
enum ConversationMute {
  /// The settings with a new mute time (`nil`: unmuted); level kept, no longer inherited from the
  /// space (the user chose for this conversation). `nil` if `json` isn't an object.
  static func body(current json: String, mutedUntil maybeDate: Date?) -> String? {
    guard var object = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] else {
      return nil
    }
    if object["level"] == nil { object["level"] = "all" }
    object["is_inherited"] = false
    if let date = maybeDate {
      object["muted_until"] = UnixMillis.from(date)
    } else {
      object.removeValue(forKey: "muted_until")
    }
    return jsonText(object)
  }

  /// When the conversation is muted until, if that is still ahead.
  static func mutedUntil(in json: String, now: Date) -> Date? {
    guard let object = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
      let millis = (object["muted_until"] as? NSNumber)?.int64Value
    else { return nil }
    let date = UnixMillis.date(millis)
    return date > now ? date : nil
  }
}

/// Unix milliseconds, the server's time format.
enum UnixMillis {
  static func from(_ date: Date) -> Int64 {
    Int64((date.timeIntervalSince1970 * 1000).rounded())
  }

  static func date(_ millis: Int64) -> Date {
    Date(timeIntervalSince1970: Double(millis) / 1000)
  }
}

/// Minutes after midnight ↔ a time of day for the time pickers.
enum MinuteOfDay {
  static func date(_ minute: Int, on day: Date, calendar: Calendar) -> Date {
    let start = calendar.startOfDay(for: day)
    return calendar.date(bySettingHour: minute / 60, minute: minute % 60, second: 0, of: start) ?? start
  }

  static func minute(of date: Date, calendar: Calendar) -> Int {
    let parts = calendar.dateComponents([.hour, .minute], from: date)
    return (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
  }
}
