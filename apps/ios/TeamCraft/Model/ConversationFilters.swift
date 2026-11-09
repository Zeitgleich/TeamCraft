import Foundation

/// A conversation's two notification filters (`channel_mentions` and `quiet_hours` of
/// `GET/PUT /conversations/{id}/notification-settings`, notifications.md "Per-conversation
/// filters"). They are the conversation's own: the space's level never changes them.
struct ConversationFilters: Equatable {
  var channelMentions: ChannelMentionsSetting
  /// The quiet-hours ranges, the same rows as the DND schedule's. None: no quiet hours.
  var quietWindows: [DndWindow]

  static let initial = ConversationFilters(channelMentions: .known(.followGeneral), quietWindows: [])

  var canAddWindow: Bool { quietWindows.count < DndSchedule.maxWindows }

  /// The filters in a settings document; absent fields are the defaults.
  static func read(in json: String) -> ConversationFilters? {
    guard let object = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] else {
      return nil
    }
    return ConversationFilters(
      channelMentions: ChannelMentionsSetting(wire: object[ConversationMute.channelMentionsKey]),
      quietWindows: quietHours(in: object)?.windows ?? [])
  }

  /// The document to `PUT`: `current` (what the server has now) with the filters in place.
  /// A `channel_mentions` this version doesn't know is left out, so the server keeps it; quiet
  /// hours without ranges remove them, and are left out where there were none.
  func body(applyingTo current: String, timeZone: TimeZone, at date: Date) -> String? {
    guard var object = (try? JSONSerialization.jsonObject(with: Data(current.utf8))) as? [String: Any] else {
      return nil
    }
    let hadQuietHours = !(Self.quietHours(in: object)?.windows ?? []).isEmpty
    switch channelMentions {
    case .known(let choice): object[ConversationMute.channelMentionsKey] = choice.rawValue
    case .unknown: object.removeValue(forKey: ConversationMute.channelMentionsKey)
    }
    if quietWindows.isEmpty {
      if hadQuietHours {
        object[Self.quietHoursKey] = ["utc_offset_minutes": 0, "windows": [Any]()] as [String: Any]
      } else {
        object.removeValue(forKey: Self.quietHoursKey)
      }
    } else {
      let quiet = QuietHours(windows: quietWindows).stamped(timeZone: timeZone, at: date)
      guard let data = try? JSONEncoder().encode(quiet),
        let encoded = try? JSONSerialization.jsonObject(with: data)
      else { return nil }
      object[Self.quietHoursKey] = encoded
    }
    return jsonText(object)
  }

  static let quietHoursKey = "quiet_hours"

  private static func quietHours(in object: [String: Any]) -> QuietHours? {
    guard let value = object[quietHoursKey], let data = try? JSONSerialization.data(withJSONObject: value) else {
      return nil
    }
    return try? JSONDecoder().decode(QuietHours.self, from: data)
  }
}

/// Whether `@channel`, `@here` and `@all` count as a mention here.
enum ChannelMentionsChoice: String, CaseIterable, Identifiable {
  /// The member's general setting; the server doesn't push them.
  case followGeneral = "default"
  case notify
  case ignore

  var id: String { rawValue }
}

/// What the server has: one of the three choices, or a value from a newer version, which this
/// version shows as none of them and never sends back.
enum ChannelMentionsSetting: Equatable {
  case known(ChannelMentionsChoice)
  case unknown(String)

  /// From the document's value: absent is the default.
  init(wire maybeValue: Any?) {
    guard let value = maybeValue, !(value is NSNull) else {
      self = .known(.followGeneral)
      return
    }
    if let text = value as? String, let choice = ChannelMentionsChoice(rawValue: text) {
      self = .known(choice)
    } else {
      self = .unknown(String(describing: value))
    }
  }

  var maybeChoice: ChannelMentionsChoice? {
    if case .known(let choice) = self { return choice }
    return nil
  }
}

/// `quiet_hours`: weekly ranges like the DND schedule's, in the member's time zone.
struct QuietHours: Codable, Equatable {
  var utcOffsetMinutes = 0
  var maybeTimeZone: String?
  var windows: [DndWindow]

  private enum CodingKeys: String, CodingKey {
    case utcOffsetMinutes = "utc_offset_minutes"
    case maybeTimeZone = "time_zone"
    case windows
  }

  init(windows: [DndWindow]) {
    self.windows = windows
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    utcOffsetMinutes = try container.decodeIfPresent(Int.self, forKey: .utcOffsetMinutes) ?? 0
    maybeTimeZone = try container.decodeIfPresent(String.self, forKey: .maybeTimeZone)
    windows = try container.decodeIfPresent([DndWindow].self, forKey: .windows) ?? []
  }

  func encode(to encoder: Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(utcOffsetMinutes, forKey: .utcOffsetMinutes)
    try container.encodeIfPresent(maybeTimeZone, forKey: .maybeTimeZone)
    try container.encode(windows, forKey: .windows)
  }

  /// Saved with the device's zone and its offset at `date`, like the DND schedule.
  func stamped(timeZone: TimeZone, at date: Date) -> QuietHours {
    var stamped = self
    stamped.maybeTimeZone = windows.isEmpty ? nil : timeZone.identifier
    stamped.utcOffsetMinutes = windows.isEmpty ? 0 : timeZone.secondsFromGMT(for: date) / 60
    return stamped
  }
}

// MARK: Validation

/// Why a quiet-hours range can't be saved; its message sits at the range (forms.md).
enum QuietRangeProblem: Equatable {
  case noDays, sameTime
}

extension QuietRangeProblem {
  /// The problems of the ranges, by range, in the order of the list. A range is valid with at
  /// least one day and a start different from the end (an end before the start runs past midnight).
  static func problems(in windows: [DndWindow]) -> [UUID: QuietRangeProblem] {
    var found: [UUID: QuietRangeProblem] = [:]
    for window in windows {
      if window.weekdays.isEmpty {
        found[window.id] = .noDays
      } else if window.startMinute == window.endMinute {
        found[window.id] = .sameTime
      }
    }
    return found
  }

  /// The first range with a problem, the one a refused save focuses.
  static func firstInvalid(in windows: [DndWindow]) -> UUID? {
    let found = problems(in: windows)
    return windows.first { found[$0.id] != nil }?.id
  }
}
