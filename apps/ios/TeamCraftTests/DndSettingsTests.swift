import XCTest

@testable import TeamCraft

final class DndScheduleCodingTests: XCTestCase {
  private let serverJSON = """
    {"utc_offset_minutes":60,"time_zone":"Europe/Berlin","windows":[\
    {"weekdays":["mon","tue","wed","thu","fri"],"start_minute":1320,"end_minute":420},\
    {"weekdays":["sat","sun","holiday"],"start_minute":0,"end_minute":600}],\
    "allow_user_ids":["7206104748613181440"],"allows_urgent":true}
    """

  func testDecodesTheServersSchedule() throws {
    let schedule = try JSONDecoder().decode(DndSchedule.self, from: Data(serverJSON.utf8))
    XCTAssertEqual(schedule.utcOffsetMinutes, 60)
    XCTAssertEqual(schedule.maybeTimeZone, "Europe/Berlin")
    XCTAssertEqual(schedule.windows.count, 2)
    XCTAssertEqual(schedule.windows[0].weekdays, [.monday, .tuesday, .wednesday, .thursday, .friday])
    XCTAssertEqual(schedule.windows[0].startMinute, 22 * 60)
    XCTAssertTrue(schedule.windows[0].isOvernight)
    // An unknown weekday is skipped, not fatal.
    XCTAssertEqual(schedule.windows[1].weekdays, [.saturday, .sunday])
    XCTAssertEqual(schedule.allowUserIds, ["7206104748613181440"])
    XCTAssertTrue(schedule.allowsUrgent)
  }

  func testEncodesSnakeCaseWithMondayFirstWeekdays() throws {
    let schedule = DndSchedule(
      utcOffsetMinutes: -300, maybeTimeZone: nil,
      windows: [DndWindow(weekdays: [.sunday, .monday], startMinute: 60, endMinute: 120)],
      allowUserIds: [], allowsUrgent: false)
    let object = try XCTUnwrap(
      JSONSerialization.jsonObject(with: JSONEncoder().encode(schedule)) as? [String: Any])
    XCTAssertEqual(object["utc_offset_minutes"] as? Int, -300)
    XCTAssertNil(object["time_zone"])
    XCTAssertEqual(object["allows_urgent"] as? Bool, false)
    let window = try XCTUnwrap((object["windows"] as? [[String: Any]])?.first)
    XCTAssertEqual(window["weekdays"] as? [String], ["mon", "sun"])
    XCTAssertEqual(window["start_minute"] as? Int, 60)
    XCTAssertEqual(window["end_minute"] as? Int, 120)
  }

  func testRoundTrips() throws {
    let schedule = try JSONDecoder().decode(DndSchedule.self, from: Data(serverJSON.utf8))
    let again = try JSONDecoder().decode(DndSchedule.self, from: JSONEncoder().encode(schedule))
    XCTAssertEqual(again, schedule)
  }

  func testStampingSetsTheTimeZoneAndTodaysOffset() throws {
    let berlin = try XCTUnwrap(TimeZone(identifier: "Europe/Berlin"))
    // 2026-07-15 noon UTC: summer time.
    var schedule = DndSchedule.empty
    schedule.windows.append(.weeknights)
    let stamped = schedule.stamped(timeZone: berlin, at: Date(timeIntervalSince1970: 1_784_116_800))
    XCTAssertEqual(stamped.maybeTimeZone, "Europe/Berlin")
    XCTAssertEqual(stamped.utcOffsetMinutes, 120)
  }

  func testWithoutWindowsNoTimeZoneIsSent() throws {
    let berlin = try XCTUnwrap(TimeZone(identifier: "Europe/Berlin"))
    let stamped = DndSchedule.empty.stamped(timeZone: berlin, at: Date(timeIntervalSince1970: 1_784_116_800))
    XCTAssertNil(stamped.maybeTimeZone)
    XCTAssertEqual(stamped.utcOffsetMinutes, 0)
  }

  func testAtMostFourteenRanges() {
    var schedule = DndSchedule.empty
    for _ in 0..<DndSchedule.maxWindows { schedule.windows.append(.weeknights) }
    XCTAssertFalse(schedule.canAddWindow)
    schedule.windows.removeLast()
    XCTAssertTrue(schedule.canAddWindow)
  }

  func testReplacingTheScheduleKeepsTheOtherSettings() throws {
    let settings = """
      {"auto_follow_mentioned_threads":true,"push_mode":"always","send_read_receipts":false,"future_field":3}
      """
    let schedule = DndSchedule(
      utcOffsetMinutes: 0, maybeTimeZone: "UTC", windows: [.weeknights], allowUserIds: ["5"], allowsUrgent: true)
    let json = try XCTUnwrap(NotificationSettingsDocument.replacingSchedule(in: settings, with: schedule))
    let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    XCTAssertEqual(object["push_mode"] as? String, "always")
    XCTAssertEqual(object["send_read_receipts"] as? Bool, false)
    XCTAssertEqual(object["future_field"] as? Int, 3)
    XCTAssertEqual(NotificationSettingsDocument.schedule(in: json), schedule)
    let cleared = try XCTUnwrap(NotificationSettingsDocument.replacingSchedule(in: json, with: nil))
    XCTAssertNil(NotificationSettingsDocument.schedule(in: cleared))
  }

  func testMalformedDocumentsAreRefused() {
    XCTAssertNil(NotificationSettingsDocument.replacingSchedule(in: "[]", with: .empty))
    XCTAssertNil(NotificationSettingsDocument.replacingSchedule(in: "", with: .empty))
    XCTAssertNil(NotificationSettingsDocument.schedule(in: "{\"dnd_schedule\":{\"windows\":[{\"weekdays\":1}]}}"))
  }
}

final class QuickDndTests: XCTestCase {
  private var calendar: Calendar {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "Europe/Berlin") ?? .gmt
    return calendar
  }

  func testOneHourEndsAnHourFromNow() {
    let now = date(2026, 3, 10, 14, 30)
    XCTAssertEqual(QuickDnd.oneHour.endDate(now: now, calendar: calendar), date(2026, 3, 10, 15, 30))
  }

  func testUntilMorningInTheEveningEndsTomorrowAtEight() {
    XCTAssertEqual(
      QuickDnd.untilMorning.endDate(now: date(2026, 3, 10, 21, 0), calendar: calendar), date(2026, 3, 11, 8, 0))
  }

  func testUntilMorningAtNightEndsThisMorningAtEight() {
    XCTAssertEqual(
      QuickDnd.untilMorning.endDate(now: date(2026, 3, 11, 2, 0), calendar: calendar), date(2026, 3, 11, 8, 0))
  }

  func testUntilMorningAcrossTheDaylightSavingChange() {
    // Summer time starts on 2026-03-29 at 02:00 in Berlin: 08:00 local is 06:00 UTC.
    let end = QuickDnd.untilMorning.endDate(now: date(2026, 3, 28, 23, 0), calendar: calendar)
    XCTAssertEqual(end, date(2026, 3, 29, 8, 0))
    XCTAssertEqual(end.timeIntervalSince1970, 1_774_764_000)
  }

  func testCustomEndsWhenChosen() {
    let chosen = date(2026, 4, 1, 9, 15)
    XCTAssertEqual(QuickDnd.until(chosen).endDate(now: date(2026, 3, 10, 9, 0), calendar: calendar), chosen)
  }

  func testRequestBodies() throws {
    let body = QuickDnd.requestBody(until: Date(timeIntervalSince1970: 1_774_764_000))
    let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(body.utf8)) as? [String: Any])
    XCTAssertEqual(object["manual"] as? String, "dnd")
    XCTAssertEqual((object["until"] as? NSNumber)?.int64Value, 1_774_764_000_000)
    XCTAssertEqual(QuickDnd.endBody, "{\"manual\":\"auto\"}")
  }

  func testMutePresets() {
    let now = date(2026, 3, 10, 21, 0)
    XCTAssertEqual(MutePreset.oneHour.mutedUntil(now: now, calendar: calendar), date(2026, 3, 10, 22, 0))
    XCTAssertEqual(MutePreset.eightHours.mutedUntil(now: now, calendar: calendar), date(2026, 3, 11, 5, 0))
    XCTAssertEqual(MutePreset.untilMorning.mutedUntil(now: now, calendar: calendar), date(2026, 3, 11, 8, 0))
    XCTAssertEqual(MutePreset.oneWeek.mutedUntil(now: now, calendar: calendar), date(2026, 3, 17, 21, 0))
  }

  func testMutingKeepsTheLevelAndStopsInheriting() throws {
    let current = "{\"level\":\"mentions\",\"is_inherited\":true}"
    let until = Date(timeIntervalSince1970: 1_774_764_000)
    let body = try XCTUnwrap(ConversationMute.body(current: current, mutedUntil: until))
    let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(body.utf8)) as? [String: Any])
    XCTAssertEqual(object["level"] as? String, "mentions")
    XCTAssertEqual(object["is_inherited"] as? Bool, false)
    XCTAssertEqual((object["muted_until"] as? NSNumber)?.int64Value, 1_774_764_000_000)
    XCTAssertEqual(ConversationMute.mutedUntil(in: body, now: until.addingTimeInterval(-60)), until)
    XCTAssertNil(ConversationMute.mutedUntil(in: body, now: until.addingTimeInterval(60)))
    let unmuted = try XCTUnwrap(ConversationMute.body(current: body, mutedUntil: nil))
    XCTAssertNil(ConversationMute.mutedUntil(in: unmuted, now: .distantPast))
  }

  func testMinutesOfTheDayRoundTrip() {
    let day = date(2026, 3, 10, 0, 0)
    for minute in [0, 7 * 60, 22 * 60 + 30, 23 * 60 + 59] {
      let picked = MinuteOfDay.date(minute, on: day, calendar: calendar)
      XCTAssertEqual(MinuteOfDay.minute(of: picked, calendar: calendar), minute)
    }
  }

  private func date(_ year: Int, _ month: Int, _ day: Int, _ hour: Int, _ minute: Int) -> Date {
    calendar.date(from: DateComponents(year: year, month: month, day: day, hour: hour, minute: minute)) ?? .distantPast
  }
}
