import BackgroundTasks
import Foundation
import SwiftUI

/// The workspace's rule for "send later" (docs/scheduled-messages.md).
struct SchedulePolicy: Decodable, Equatable, Sendable {
  let isEnabled: Bool
  let maxHorizonDays: Int
  let maxPerUser: Int
}

/// A message waiting for its time. The core decides `status`; the app only words it.
struct ScheduledMessage: Decodable, Equatable, Identifiable, Sendable {
  let id: String
  let conversation: String
  let conversationName: String?
  let body: String
  let dueAt: Int64
  let status: ScheduledStatus
  /// When the server removes a hold nothing can solve any more.
  var maybeRemovesAt: Int64? = nil
  /// The conversation is the user's notes: `conversationName` is empty (#1385). Absent from older cores.
  var isNotesToSelf: Bool? = nil

  /// What the row calls the conversation; `nil` when this device doesn't know it.
  var listName: String? {
    if isNotesToSelf == true { return conversationListName("", isNotesToSelf: true) }
    guard let name = conversationName, !name.isEmpty else { return nil }
    return name
  }

  /// Gone from the server: only the text is left on the device, until the user removes it.
  var isRemoved: Bool { status == .removedByServer }

  /// Whether the message waits for the user to do something.
  var needsAttention: Bool {
    switch status {
    case .policyOff, .notAMember, .noPermission, .conversationGone, .threadGone, .removedByServer: return true
    case .waiting, .needsSealing, .stale, .unreadable, .unknown: return false
    }
  }

  /// Whether sending it can still make it go out.
  var canGoOut: Bool {
    switch status {
    case .waiting, .needsSealing, .stale, .policyOff: return true
    case .notAMember, .noPermission, .conversationGone, .threadGone, .unreadable, .removedByServer, .unknown: return false
    }
  }

  var canEdit: Bool { canGoOut && status != .policyOff }
}

/// `ScheduledStatus` of the core, with what the lists say about it.
enum ScheduledStatus: String, Decodable, Equatable, Sendable {
  case waiting
  case needsSealing = "needs_sealing"
  case stale
  case policyOff = "policy_off"
  case notAMember = "not_a_member"
  case noPermission = "no_permission"
  case conversationGone = "conversation_gone"
  case threadGone = "thread_gone"
  case unreadable
  case removedByServer = "removed_by_server"
  case unknown

  init(from decoder: Decoder) throws {
    let value = try decoder.singleValueContainer().decode(String.self)
    self = ScheduledStatus(rawValue: value) ?? .unknown
  }

  var text: LocalizedStringKey {
    switch self {
    case .waiting: return "scheduled_status_waiting"
    case .needsSealing: return "scheduled_status_needs_sealing"
    case .stale: return "scheduled_status_stale"
    case .policyOff: return "scheduled_status_policy_off"
    case .notAMember: return "scheduled_status_not_a_member"
    case .noPermission: return "scheduled_status_no_permission"
    case .conversationGone: return "scheduled_status_conversation_gone"
    case .threadGone: return "scheduled_status_thread_gone"
    case .unreadable: return "scheduled_status_unreadable"
    case .removedByServer: return "scheduled_status_removed"
    case .unknown: return "scheduled_status_unknown"
    }
  }
}

/// The list, the rule and the server's clock as this device knows it.
struct ScheduledOverview: Decodable, Equatable, Sendable {
  let messages: [ScheduledMessage]
  let policy: SchedulePolicy
  let serverNow: Int64
}

/// The fields of editing a scheduled message, top to bottom.
enum ScheduleField: CaseIterable, Hashable {
  case body, time
}

enum ScheduleProblem: FieldProblem {
  case empty, inThePast, tooFarAhead

  var message: LocalizedStringKey {
    switch self {
    case .empty: return "schedule_error_empty"
    case .inThePast: return "schedule_error_past"
    case .tooFarAhead: return "schedule_error_far"
    }
  }
}

/// The times "send later" offers and what it refuses, as desktop and web do (teamcraft-client-api
/// `scheduled`). Pure, so the rules are tested.
enum ScheduleTimes {
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
    } else {
      bodyProblem = nil
    }
    return FieldErrors([.body: bodyProblem, .time: timeProblem(dueAtMs: dueAtMs, overview: overview)])
  }

  static func date(ms: Int64) -> Date { Date(timeIntervalSince1970: TimeInterval(ms) / 1000) }

  static func ms(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }
}

/// When a silent push couldn't finish sealing a scheduled message again, a later try once the network
/// is there. iOS decides when a `BGProcessingTask` runs and may not run it at all, certainly not after
/// the user force-quit the app; the lists don't promise more (docs/scheduled-messages.md).
enum ScheduledSealTask {
  static let identifier = "de.zeitgleich.teamcraft.scheduled-seal"

  /// Must run before the app finished launching.
  static func register() {
    BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: nil) { task in
      guard let processing = task as? BGProcessingTask else {
        task.setTaskCompleted(success: false)
        return
      }
      let work = Task {
        processing.setTaskCompleted(success: await AppModel.shared.sealScheduledMessages())
      }
      processing.expirationHandler = { work.cancel() }
    }
  }

  /// Fails quietly where the system doesn't allow it (simulator).
  static func schedule() {
    let request = BGProcessingTaskRequest(identifier: identifier)
    request.requiresNetworkConnectivity = true
    request.requiresExternalPower = false
    try? BGTaskScheduler.shared.submit(request)
  }
}
