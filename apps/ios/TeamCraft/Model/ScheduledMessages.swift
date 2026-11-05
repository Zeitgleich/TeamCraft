import BackgroundTasks
import Foundation
import SwiftUI

/// The workspace's rule for "send later" (docs/scheduled-messages.md).
struct SchedulePolicy: Decodable, Equatable, Sendable {
  let isEnabled: Bool
  let maxHorizonDays: Int
  let lateLimitHours: Int
}

/// A message waiting for its time. The core decides `status`; the app only words it.
struct ScheduledMessage: Decodable, Equatable, Identifiable, Sendable {
  let id: String
  let conversation: String
  let conversationName: String?
  let body: String
  let dueAt: Int64
  let status: ScheduledStatus

  /// The status as shown: one that was waiting when it was read is due once its time passed.
  func shownStatus(nowMs: Int64) -> ScheduledStatus {
    status == .waiting && dueAt <= nowMs ? .due : status
  }

  /// Whether the message waits for the user to do something.
  var needsAttention: Bool {
    switch status {
    case .missed, .policyOff, .unavailable: return true
    case .waiting, .due, .unknown: return false
    }
  }
}

/// `ScheduledStatus` of the core, with what the lists say about it.
enum ScheduledStatus: String, Decodable, Equatable, Sendable {
  case waiting
  case due
  case missed
  case policyOff = "policy_off"
  case unavailable
  case unknown

  init(from decoder: Decoder) throws {
    let value = try decoder.singleValueContainer().decode(String.self)
    self = ScheduledStatus(rawValue: value) ?? .unknown
  }

  var text: LocalizedStringKey {
    switch self {
    case .waiting: return "scheduled_status_waiting"
    case .due: return "scheduled_status_due"
    case .missed: return "scheduled_status_missed"
    case .policyOff: return "scheduled_status_policy_off"
    case .unavailable: return "scheduled_status_unavailable"
    case .unknown: return "scheduled_status_unknown"
    }
  }
}

/// The list, the rule and the server's clock as this device knows it.
struct ScheduledOverview: Decodable, Equatable, Sendable {
  let messages: [ScheduledMessage]
  let policy: SchedulePolicy
  let serverNow: Int64

  /// The next time a device of this user has something to send, for the background task.
  var maybeNextDueAt: Int64? {
    messages.filter { $0.status == .waiting || $0.status == .due }.map(\.dueAt).min()
  }
}

/// The fields of editing a scheduled message, top to bottom.
enum ScheduleField: CaseIterable, Hashable {
  case body, time
}

enum ScheduleProblem: FieldProblem {
  case empty, tooLong, inThePast, tooFarAhead

  var message: LocalizedStringKey {
    switch self {
    case .empty: return "schedule_error_empty"
    case .tooLong: return "schedule_error_long"
    case .inThePast: return "schedule_error_past"
    case .tooFarAhead: return "schedule_error_far"
    }
  }
}

/// The times "send later" offers and what it refuses, as desktop and web do (teamcraft-client-api
/// `scheduled`). Pure, so the rules are tested.
enum ScheduleTimes {
  /// Limit of the core; a form that passes this is taken.
  static let maxBodyBytes = 8 * 1024
  private static let hourMs: Int64 = 3_600_000
  private static let dayMs: Int64 = 24 * hourMs
  private static let morningHour = 8

  /// Counts from the server's clock, so a wrong device clock doesn't shift it.
  static func inOneHour(serverNowMs: Int64) -> Int64 { serverNowMs + hourMs }

  static func tomorrowMorning(nowMs: Int64, calendar: Calendar) -> Int64? {
    let now = date(ms: nowMs)
    guard let tomorrow = calendar.date(byAdding: .day, value: 1, to: now) else { return nil }
    return calendar.date(bySettingHour: morningHour, minute: 0, second: 0, of: tomorrow).map(ms)
  }

  /// The next Monday's morning; on a Monday, a week ahead.
  static func mondayMorning(nowMs: Int64, calendar: Calendar) -> Int64? {
    // Only a weekday to match: the start of the next Monday, whatever the time now.
    calendar.nextDate(after: date(ms: nowMs), matching: DateComponents(weekday: 2), matchingPolicy: .nextTime)
      .flatMap { calendar.date(bySettingHour: morningHour, minute: 0, second: 0, of: $0) }
      .map(ms)
  }

  static func timeProblem(dueAtMs: Int64, overview: ScheduledOverview) -> ScheduleProblem? {
    if dueAtMs <= overview.serverNow { return .inThePast }
    if dueAtMs > overview.serverNow + Int64(overview.policy.maxHorizonDays) * dayMs { return .tooFarAhead }
    return nil
  }

  static func errors(body: String, dueAtMs: Int64, overview: ScheduledOverview) -> FieldErrors<
    ScheduleField, ScheduleProblem
  > {
    let bodyProblem: ScheduleProblem?
    if body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
      bodyProblem = .empty
    } else if body.utf8.count > maxBodyBytes {
      bodyProblem = .tooLong
    } else {
      bodyProblem = nil
    }
    return FieldErrors([.body: bodyProblem, .time: timeProblem(dueAtMs: dueAtMs, overview: overview)])
  }

  static func date(ms: Int64) -> Date { Date(timeIntervalSince1970: TimeInterval(ms) / 1000) }

  static func ms(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }
}

/// Wakes the app for the next scheduled message, so a phone in a pocket sends it without being
/// opened. Best effort: iOS decides when a `BGProcessingTask` runs, possibly much later, and not at
/// all after the user force-quit the app. The lists say that a message goes out when a device of the
/// user is online at or after its time; this makes the phone one of them more often.
enum ScheduledSendTask {
  static let identifier = "de.zeitgleich.teamcraft.scheduled-send"

  /// Must run before the app finished launching.
  static func register() {
    BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: nil) { task in
      guard let processing = task as? BGProcessingTask else {
        task.setTaskCompleted(success: false)
        return
      }
      let work = Task {
        let isDone = await AppModel.shared.runScheduledSend()
        processing.setTaskCompleted(success: isDone)
        await AppModel.shared.planScheduledSend()
      }
      processing.expirationHandler = { work.cancel() }
    }
  }

  /// Asks for a run at or after `dueAtMs`. Fails quietly where the system doesn't allow it
  /// (simulator); the next foreground sends it then.
  static func schedule(dueAtMs: Int64) {
    let request = BGProcessingTaskRequest(identifier: identifier)
    request.requiresNetworkConnectivity = true
    request.requiresExternalPower = false
    request.earliestBeginDate = ScheduleTimes.date(ms: dueAtMs)
    try? BGTaskScheduler.shared.submit(request)
  }

  static func cancel() {
    BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: identifier)
  }
}
