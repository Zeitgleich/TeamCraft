import XCTest

@testable import TeamCraft

final class ScheduledMessagesTests: XCTestCase {
  private static let berlin: Calendar = {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "Europe/Berlin") ?? .gmt
    return calendar
  }()

  private func ms(_ year: Int, _ month: Int, _ day: Int, _ hour: Int) -> Int64 {
    let components = DateComponents(year: year, month: month, day: day, hour: hour)
    return ScheduleTimes.ms(Self.berlin.date(from: components) ?? Date())
  }

  private func overview(serverNow: Int64, horizonDays: Int = 365) -> ScheduledOverview {
    ScheduledOverview(
      messages: [],
      policy: SchedulePolicy(isEnabled: true, maxHorizonDays: horizonDays, lateLimitHours: 24),
      serverNow: serverNow)
  }

  func testInOneHourCountsFromTheServersClock() {
    XCTAssertEqual(ScheduleTimes.inOneHour(serverNowMs: 500), 3_600_500)
  }

  func testTomorrowMorningIsTheNextDayAtEight() {
    let wednesdayNoon = ms(2026, 11, 4, 12)
    XCTAssertEqual(
      ScheduleTimes.tomorrowMorning(nowMs: wednesdayNoon, calendar: Self.berlin), ms(2026, 11, 5, 8))
  }

  func testMondayMorningOnAMondayIsAWeekAhead() {
    XCTAssertEqual(
      ScheduleTimes.mondayMorning(nowMs: ms(2026, 11, 2, 7), calendar: Self.berlin), ms(2026, 11, 9, 8))
    XCTAssertEqual(
      ScheduleTimes.mondayMorning(nowMs: ms(2026, 11, 4, 12), calendar: Self.berlin), ms(2026, 11, 9, 8))
  }

  func testEveryProblemShowsAtItsField() {
    let errors = ScheduleTimes.errors(body: "  ", dueAtMs: 5, overview: overview(serverNow: 5))
    XCTAssertEqual(errors[.body], .empty)
    XCTAssertEqual(errors[.time], .inThePast)
    XCTAssertEqual(errors.maybeFirst, .body)
  }

  func testTheHorizonEndsExactlyAtItsDay() {
    let day: Int64 = 86_400_000
    let rule = overview(serverNow: 0, horizonDays: 2)
    XCTAssertNil(ScheduleTimes.timeProblem(dueAtMs: 2 * day, overview: rule))
    XCTAssertEqual(ScheduleTimes.timeProblem(dueAtMs: 2 * day + 1, overview: rule), .tooFarAhead)
  }

  func testTheCoresAnswerDecodes() throws {
    let answer = Wire.decode(
      ChatAnswer.self,
      from: """
        {"answer":"scheduled_messages","overview":{"messages":[{"id":"ab","conversation":"7",
        "conversation_name":"general","maybe_thread":null,"body":"later","due_at":5,"status":"on_the_moon",
        "priority":"normal"}],"policy":{"is_enabled":true,"max_horizon_days":30,"late_limit_hours":24},
        "server_now":3}}
        """)
    let overview = try XCTUnwrap(answer?.overview)
    XCTAssertEqual(overview.policy.maxHorizonDays, 30)
    XCTAssertEqual(overview.messages.first?.status, .unknown)
    XCTAssertEqual(overview.serverNow, 3)
  }

  func testAWaitingMessageShowsAsDueOnceItsTimePassed() {
    let message = ScheduledMessage(
      id: "ab", conversation: "7", conversationName: nil, body: "later", dueAt: 10, status: .waiting)
    XCTAssertEqual(message.shownStatus(nowMs: 9), .waiting)
    XCTAssertEqual(message.shownStatus(nowMs: 10), .due)
    XCTAssertFalse(message.needsAttention)
  }

  func testTheActionsHaveTheCoresShape() {
    XCTAssertEqual(ChatAction.cancelScheduled(id: "ab").json, #"{"action":"cancel_scheduled","id":"ab"}"#)
    XCTAssertEqual(ChatAction.sendScheduledNow(id: "ab").json, #"{"action":"send_scheduled_now","id":"ab"}"#)
    XCTAssertEqual(
      ChatAction.updateScheduled(id: "ab", body: "x", dueAtMs: 5).json,
      #"{"action":"update_scheduled","body":"x","due_at":5,"id":"ab"}"#)
    XCTAssertEqual(ChatQuery.scheduledMessages.json, #"{"kind":"scheduled_messages"}"#)
  }
}
