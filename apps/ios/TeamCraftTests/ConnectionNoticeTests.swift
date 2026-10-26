import XCTest

@testable import TeamCraft

final class ConnectionNoticeTests: XCTestCase {
  private let start = Date(timeIntervalSince1970: 1_800_000_000)

  func testQuickReconnectShowsNoBanner() {
    var grace = ConnectionGrace()
    grace.observe(isDown: true, now: start)
    XCTAssertNil(notice(isOnline: true, .connecting, grace, at: 9))
    grace.observe(isDown: false, now: start.addingTimeInterval(9))
    XCTAssertNil(notice(isOnline: true, .connected, grace, at: 30))
  }

  func testBannerAfterTheGracePeriod() {
    var grace = ConnectionGrace()
    grace.observe(isDown: true, now: start)
    XCTAssertEqual(notice(isOnline: true, .connecting, grace, at: 10), .connecting)
    let retryAtMs = Int64((start.timeIntervalSince1970 + 15) * 1000)
    XCTAssertEqual(
      notice(isOnline: true, .offline(retryAtMs: retryAtMs, maybeInterception: nil), grace, at: 12),
      .unreachable(retryInSeconds: 3, maybeInterception: nil))
  }

  func testDeviceOfflineWinsAfterTheGracePeriod() {
    var grace = ConnectionGrace()
    grace.observe(isDown: true, now: start)
    XCTAssertNil(notice(isOnline: false, .connecting, grace, at: 5))
    XCTAssertEqual(notice(isOnline: false, .connecting, grace, at: 11), .deviceOffline)
    XCTAssertEqual(notice(isOnline: false, nil, grace, at: 11), .deviceOffline)
  }

  func testSignInRequiredShowsAtOnce() {
    XCTAssertEqual(notice(isOnline: true, .signInRequired, ConnectionGrace(), at: 0), .signInRequired)
  }

  func testComingBackStartsTheGracePeriodAgain() {
    var grace = ConnectionGrace()
    grace.observe(isDown: true, now: start)
    grace.appReturned(now: start.addingTimeInterval(600))
    XCTAssertNil(notice(isOnline: true, .connecting, grace, at: 605))
    XCTAssertEqual(notice(isOnline: true, .connecting, grace, at: 610), .connecting)
  }

  func testStayingDownKeepsTheFirstMoment() {
    var grace = ConnectionGrace()
    grace.observe(isDown: true, now: start)
    grace.observe(isDown: true, now: start.addingTimeInterval(8))
    XCTAssertEqual(grace.maybeDownSince, start)
  }

  func testAClockThatWentBackCountsAsJustNow() {
    var grace = ConnectionGrace()
    grace.observe(isDown: true, now: start)
    XCTAssertFalse(grace.isPastGrace(now: start.addingTimeInterval(-3600)))
  }

  func testOnlyADownConnectionNeedsAClock() {
    XCTAssertFalse(ConnectionNotice.isDown(isOnline: true, connection: .connected))
    XCTAssertFalse(ConnectionNotice.isDown(isOnline: true, connection: nil))
    XCTAssertTrue(ConnectionNotice.isDown(isOnline: true, connection: .connecting))
    XCTAssertTrue(ConnectionNotice.isDown(isOnline: false, connection: .connected))
  }

  func testPushUpkeepRunsDailyAndPausesAfterAnAttempt() {
    XCTAssertTrue(PushUpkeepSchedule.isRefreshDue(lastCheckedAt: nil, now: start))
    XCTAssertFalse(PushUpkeepSchedule.isRefreshDue(lastCheckedAt: start, now: start.addingTimeInterval(3600)))
    XCTAssertTrue(PushUpkeepSchedule.isRefreshDue(lastCheckedAt: start, now: start.addingTimeInterval(86_400)))
    XCTAssertTrue(PushUpkeepSchedule.isRefreshDue(lastCheckedAt: start, now: start.addingTimeInterval(-60)))
    XCTAssertFalse(PushUpkeepSchedule.mayAttempt(lastAttemptAt: start, now: start.addingTimeInterval(60)))
    XCTAssertTrue(PushUpkeepSchedule.mayAttempt(lastAttemptAt: start, now: start.addingTimeInterval(600)))
  }

  private func notice(
    isOnline: Bool, _ connection: ConnectionState?, _ grace: ConnectionGrace, at seconds: TimeInterval
  ) -> ConnectionNotice? {
    ConnectionNotice.current(
      isOnline: isOnline, connection: connection, grace: grace, now: start.addingTimeInterval(seconds))
  }
}
