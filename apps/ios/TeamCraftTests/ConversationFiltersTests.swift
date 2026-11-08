import XCTest

@testable import TeamCraft

final class ConversationFiltersTests: XCTestCase {
  private let berlin = TimeZone(identifier: "Europe/Berlin")!
  private let summer = Date(timeIntervalSince1970: 1_784_116_800)  // July 2026

  private func object(_ json: String?) throws -> [String: Any] {
    let text = try XCTUnwrap(json)
    return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any])
  }

  // MARK: Reading

  func testAbsentFieldsAreTheDefaults() throws {
    let filters = try XCTUnwrap(ConversationFilters.read(in: "{\"level\":\"all\",\"is_inherited\":false}"))
    XCTAssertEqual(filters, .initial)
  }

  func testReadsTheServersFilters() throws {
    let json = """
      {"level":"mentions","is_inherited":false,"channel_mentions":"notify",\
      "quiet_hours":{"utc_offset_minutes":60,"time_zone":"Europe/Berlin","windows":[\
      {"weekdays":["sat","sun","holiday"],"start_minute":600,"end_minute":1200}]}}
      """
    let filters = try XCTUnwrap(ConversationFilters.read(in: json))
    XCTAssertEqual(filters.channelMentions, .known(.notify))
    XCTAssertEqual(filters.quietWindows.count, 1)
    // A weekday from a newer version is skipped, not fatal.
    XCTAssertEqual(filters.quietWindows[0].weekdays, [.saturday, .sunday])
    XCTAssertEqual(filters.quietWindows[0].startMinute, 600)
    XCTAssertEqual(filters.quietWindows[0].endMinute, 1200)
  }

  func testAnUnknownChannelMentionsValueIsToleratedAndShownAsNoChoice() throws {
    let filters = try XCTUnwrap(ConversationFilters.read(in: "{\"level\":\"all\",\"channel_mentions\":\"only_admins\"}"))
    XCTAssertEqual(filters.channelMentions, .unknown("only_admins"))
    XCTAssertNil(filters.channelMentions.maybeChoice)
  }

  func testDocumentThatIsNoObjectIsRefused() {
    XCTAssertNil(ConversationFilters.read(in: "[]"))
    XCTAssertNil(ConversationFilters.initial.body(applyingTo: "nope", timeZone: berlin, at: summer))
  }

  // MARK: Writing

  func testWritesTheChoiceAndStampedQuietHoursKeepingTheRest() throws {
    var filters = ConversationFilters.initial
    filters.channelMentions = .known(.ignore)
    filters.quietWindows = [DndWindow(weekdays: [.sunday, .monday], startMinute: 60, endMinute: 120)]
    let current = "{\"level\":\"mentions\",\"is_inherited\":true,\"muted_until\":1774764000000}"
    let sent = try object(filters.body(applyingTo: current, timeZone: berlin, at: summer))
    XCTAssertEqual(sent["channel_mentions"] as? String, "ignore")
    XCTAssertEqual(sent["level"] as? String, "mentions")
    XCTAssertEqual(sent["is_inherited"] as? Bool, true)
    XCTAssertEqual((sent["muted_until"] as? NSNumber)?.int64Value, 1_774_764_000_000)
    let quiet = try XCTUnwrap(sent["quiet_hours"] as? [String: Any])
    XCTAssertEqual(quiet["time_zone"] as? String, "Europe/Berlin")
    XCTAssertEqual(quiet["utc_offset_minutes"] as? Int, 120)
    let window = try XCTUnwrap((quiet["windows"] as? [[String: Any]])?.first)
    XCTAssertEqual(window["weekdays"] as? [String], ["mon", "sun"])
    XCTAssertEqual(window["start_minute"] as? Int, 60)
    XCTAssertEqual(window["end_minute"] as? Int, 120)
  }

  func testTheDefaultChoiceIsSentAsDefaultNotLeftOut() throws {
    var filters = ConversationFilters.initial
    filters.channelMentions = .known(.followGeneral)
    let sent = try object(filters.body(applyingTo: "{\"level\":\"all\",\"channel_mentions\":\"notify\"}", timeZone: berlin, at: summer))
    XCTAssertEqual(sent["channel_mentions"] as? String, "default")
  }

  func testAnUnknownChannelMentionsValueIsNeverSentBack() throws {
    let current = "{\"level\":\"all\",\"channel_mentions\":\"only_admins\"}"
    let loaded = try XCTUnwrap(ConversationFilters.read(in: current))
    let sent = try object(loaded.body(applyingTo: current, timeZone: berlin, at: summer))
    XCTAssertNil(sent["channel_mentions"])
    // Picking a choice replaces it.
    var chosen = loaded
    chosen.channelMentions = .known(.notify)
    let replaced = try object(chosen.body(applyingTo: current, timeZone: berlin, at: summer))
    XCTAssertEqual(replaced["channel_mentions"] as? String, "notify")
  }

  func testLevelAndMuteWritesNeverSendAnUnknownValueBack() throws {
    let current = "{\"level\":\"all\",\"channel_mentions\":\"only_admins\",\"quiet_hours\":{\"utc_offset_minutes\":0,\"windows\":[]}}"
    let muted = try object(ConversationMute.body(current: current, mutedUntil: summer))
    XCTAssertNil(muted["channel_mentions"])
    let level = try object(ConversationMute.body(current: current, level: "none"))
    XCTAssertNil(level["channel_mentions"])
    // A known value stays.
    let known = try object(ConversationMute.body(current: "{\"level\":\"all\",\"channel_mentions\":\"notify\"}", level: "none"))
    XCTAssertEqual(known["channel_mentions"] as? String, "notify")
  }

  func testNoWindowsRemoveExistingQuietHours() throws {
    let current = """
      {"level":"all","quiet_hours":{"utc_offset_minutes":60,"time_zone":"Europe/Berlin","windows":[\
      {"weekdays":["mon"],"start_minute":0,"end_minute":60}]}}
      """
    var filters = try XCTUnwrap(ConversationFilters.read(in: current))
    filters.quietWindows = []
    let sent = try object(filters.body(applyingTo: current, timeZone: berlin, at: summer))
    let quiet = try XCTUnwrap(sent["quiet_hours"] as? [String: Any])
    XCTAssertEqual((quiet["windows"] as? [Any])?.count, 0)
    XCTAssertNil(quiet["time_zone"])
  }

  func testNoWindowsAndNoneBeforeLeaveTheFieldOut() throws {
    let sent = try object(ConversationFilters.initial.body(applyingTo: "{\"level\":\"all\"}", timeZone: berlin, at: summer))
    XCTAssertNil(sent["quiet_hours"])
  }

  func testQuietHoursRoundTrip() throws {
    var quiet = QuietHours(windows: [DndWindow.weeknights]).stamped(timeZone: berlin, at: summer)
    XCTAssertEqual(quiet.maybeTimeZone, "Europe/Berlin")
    let again = try JSONDecoder().decode(QuietHours.self, from: JSONEncoder().encode(quiet))
    XCTAssertEqual(again, quiet)
    quiet.windows = []
    XCTAssertNil(quiet.stamped(timeZone: berlin, at: summer).maybeTimeZone)
  }

  // MARK: Validation

  func testARangeNeedsADayAndDifferentTimes() {
    let noDays = DndWindow(weekdays: [], startMinute: 60, endMinute: 120)
    let same = DndWindow(weekdays: [.monday], startMinute: 300, endMinute: 300)
    let fine = DndWindow.weeknights
    let overnight = DndWindow(weekdays: [.friday], startMinute: 23 * 60, endMinute: 60)
    let problems = QuietRangeProblem.problems(in: [fine, noDays, same, overnight])
    XCTAssertEqual(problems[noDays.id], .noDays)
    XCTAssertEqual(problems[same.id], .sameTime)
    XCTAssertNil(problems[fine.id])
    XCTAssertNil(problems[overnight.id])
    XCTAssertEqual(problems.count, 2)
  }

  func testTheFirstInvalidRangeIsTheTopmost() {
    let fine = DndWindow.weeknights
    let same = DndWindow(weekdays: [.monday], startMinute: 300, endMinute: 300)
    let noDays = DndWindow(weekdays: [], startMinute: 60, endMinute: 120)
    XCTAssertEqual(QuietRangeProblem.firstInvalid(in: [fine, same, noDays]), same.id)
    XCTAssertNil(QuietRangeProblem.firstInvalid(in: [fine]))
    XCTAssertNil(QuietRangeProblem.firstInvalid(in: []))
  }

  func testAtMostFourteenRanges() {
    var filters = ConversationFilters.initial
    for _ in 0..<DndSchedule.maxWindows { filters.quietWindows.append(.weeknights) }
    XCTAssertFalse(filters.canAddWindow)
  }
}
