import Darwin
import Foundation
// The hybrid-SDK envelope API (`SentrySDK.internal.envelope`): the core builds trace envelopes.
@_spi(Private) import Sentry

/// Performance data of the app's own operations (issue #1521, docs/crash-reports.md "Performance
/// data"): the signed-in start, a channel switch, a send until the server took it, a reconnect and a
/// conversation's catch-up. Named as on every platform (`perfMeasureName` from the core), sent by
/// sentry-cocoa at the instance's trace rate, which also decides whether a measure runs at all.
/// Each measure gets the core's jobs that overlapped it as steps (`perfJobSpans`): a slow chat open
/// shows what it waited behind.
///
/// A measure that starts in one place and ends in another is kept by a local key (a conversation's
/// ID), which never leaves the device.
@MainActor
enum Perf {
  /// Measures that may run side by side at most; a further one drops the oldest.
  private static let maxPending = 16
  private static var pending: [(measure: PerfMeasure, key: String, transaction: any Span)] = []
  private static var isSdkRunning = false

  /// sentry-cocoa runs (crash reports on, the user allowed them); set by `CrashReports`.
  static func setRunning(_ running: Bool) {
    isSdkRunning = running
    if !running { pending.removeAll() }
  }

  static func begin(_ measure: PerfMeasure, key: String = "", startedAt: Date = .now) {
    cancel(measure, key: key)
    guard isSdkRunning else { return }
    let name = perfMeasureName(measure: measure)
    let transaction = SentrySDK.startTransaction(name: name.name, operation: name.op)
    // Left out by the trace rate: nothing to finish or send.
    guard transaction.sampled == .yes else { return }
    transaction.startTimestamp = startedAt
    if pending.count >= maxPending { pending.removeFirst() }
    pending.append((measure, key, transaction))
  }

  static func isRunning(_ measure: PerfMeasure, key: String = "") -> Bool {
    pending.contains { $0.measure == measure && $0.key == key }
  }

  static func tag(_ measure: PerfMeasure, key: String = "", name: String, value: String) {
    pending.first { $0.measure == measure && $0.key == key }?.transaction.setTag(value: value, key: name)
  }

  /// Ends it now, with the core's jobs that overlapped it as its steps.
  static func end(_ measure: PerfMeasure, key: String = "", isOk: Bool = true) {
    guard let index = pending.firstIndex(where: { $0.measure == measure && $0.key == key }) else { return }
    let transaction = pending.remove(at: index).transaction
    let startMs = (transaction.startTimestamp ?? .now).timeIntervalSince1970 * 1_000
    let endMs = Date.now.timeIntervalSince1970 * 1_000
    for job in perfJobSpans(startMs: startMs, endMs: endMs) { addJob(job, to: transaction) }
    transaction.finish(status: isOk ? .ok : .unavailable)
  }

  /// Dropped unsent: the user moved on before it ended.
  static func cancel(_ measure: PerfMeasure, key: String = "") {
    guard let index = pending.firstIndex(where: { $0.measure == measure && $0.key == key }) else { return }
    let transaction = pending.remove(at: index).transaction
    // sentry-cocoa can't drop a started transaction: `beforeSend` drops it by this tag.
    transaction.setTag(value: "1", key: droppedTag)
    transaction.finish(status: .cancelled)
  }

  /// The signed-in start (`start.channel_list`): from the process start until the chat list shows
  /// the core's rows, tagged by how the app was opened (`launcher`, `notification`, `link`). Called
  /// once, at launch.
  static func beginStart(entry: String) {
    begin(.startToChannelList, startedAt: processStartDate() ?? .now)
    tag(.startToChannelList, name: "start", value: "cold")
    tag(.startToChannelList, name: "entry", value: entry)
  }

  /// The chat list shows rows read from the core: the start is done.
  static func chatListShown() { end(.startToChannelList) }

  /// The core's connection changed: a reconnect runs from connecting again until connected.
  static func connectionChanged(from previous: ConnectionState?, to current: ConnectionState?) {
    switch current {
    case .connecting:
      if let previous, !isConnecting(previous) { begin(.resync) }
    case .connected:
      end(.resync)
    case .slow:
      tag(.resync, name: "link", value: "slow")
      end(.resync)
    case .offline:
      end(.resync, isOk: false)
    default:
      cancel(.resync)
    }
  }

  nonisolated static let droppedTag = "teamcraft.dropped"

  private static func isConnecting(_ state: ConnectionState) -> Bool {
    if case .connecting = state { return true }
    return false
  }

  /// A core job as two steps: its wait in the queue (`queue.wait`) and its run.
  private static func addJob(_ job: PerfJobSpan, to transaction: any Span) {
    // A span of the core's trace layer names its own operation (`http.client`); a queue job doesn't.
    let op = job.op.isEmpty ? jobOperation(job.kind) : job.op
    if job.startedAtMs - job.enqueuedAtMs >= 1 {
      let wait = transaction.startChild(operation: "queue.wait", description: job.name)
      wait.startTimestamp = date(job.enqueuedAtMs)
      wait.setData(value: Int(job.queueDepth), key: "teamcraft.queue_depth")
      wait.setData(value: op, key: "teamcraft.queue")
      wait.finish()
      wait.timestamp = date(job.startedAtMs)
    }
    let run = transaction.startChild(operation: op, description: job.name)
    run.startTimestamp = date(job.startedAtMs)
    run.setData(value: Int(job.queueDepth), key: "teamcraft.queue_depth")
    run.finish()
    run.timestamp = date(job.doneAtMs)
  }

  private static func date(_ unixMs: Double) -> Date { Date(timeIntervalSince1970: unixMs / 1_000) }

  /// When this process started (the kernel's record of it).
  private static func processStartDate() -> Date? {
    var info = kinfo_proc()
    var size = MemoryLayout<kinfo_proc>.stride
    var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
    guard sysctl(&mib, u_int(mib.count), &info, &size, nil, 0) == 0 else { return nil }
    let started = info.kp_proc.p_un.__p_starttime
    return Date(timeIntervalSince1970: TimeInterval(started.tv_sec) + TimeInterval(started.tv_usec) / 1_000_000)
  }
}

/// Which step a core job is (`core.read`, `core.engine`, `store.lock_wait`, `store.write_group`).
func jobOperation(_ kind: PerfJobKind) -> String {
  switch kind {
  case .readQueue: "core.read"
  case .engineQueue: "core.engine"
  case .storeLock: "store.lock_wait"
  case .writeGroup: "store.write_group"
  case .coreStep: "core.step"
  }
}

/// A conversation's catch-up (`conversation.catch_up`): from opening it, or the app coming back to
/// the foreground, until the newest server message is in the store and shown. Only runs while the
/// conversation is behind (a notified message the store lacks, or a catch-up running).
@MainActor
final class CatchUpMeasure {
  private let conversationId: String
  private let openedAt = Date.now
  private var isRunning = false
  private var isFirstLook = true
  private var maybeResumedAt: Date?

  init(conversationId: String) { self.conversationId = conversationId }

  /// The app came back to the foreground with this conversation open.
  func resumed() { if !isFirstLook { maybeResumedAt = .now } }

  func observe(isBehind: Bool, isFromPush: Bool, hasGaps: Bool) {
    if isBehind && !isRunning {
      let (startedAt, source): (Date, String) =
        if isFirstLook { (openedAt, isFromPush ? "push" : "open") }
        else if let resumed = maybeResumedAt { (resumed, "foreground") }
        else { (.now, "open") }
      Perf.begin(.catchUp, key: conversationId, startedAt: startedAt)
      Perf.tag(.catchUp, key: conversationId, name: "source", value: source)
      if Perf.isRunning(.resync) { Perf.tag(.catchUp, key: conversationId, name: "resync", value: "yes") }
      isRunning = true
    }
    if isRunning && hasGaps { Perf.tag(.catchUp, key: conversationId, name: "gap_fill", value: "yes") }
    if isRunning && !isBehind {
      Perf.end(.catchUp, key: conversationId)
      isRunning = false
    }
    isFirstLook = false
    maybeResumedAt = nil
  }

  /// The screen went away: a catch-up the user didn't wait for says nothing.
  func dispose() { Perf.cancel(.catchUp, key: conversationId) }
}

/// A core job that ran longer than the instance's threshold, or a thread held up that long by one
/// (`blockedBy`: one "lane blocked" event naming the job that held it, not one per waiter, #1759), as
/// a warning (sampled with the handled errors' rate in `beforeSend`), grouped per job or per
/// blocker. Names are functions in the code.
final class SlowJobReports: PerfListener, Sendable {
  func onSlowJob(job: PerfJobSpan) {
    let event = Event(level: .warning)
    let queue = jobOperation(job.kind)
    event.tags = [
      "job": job.name,
      "queue": queue,
      "wait_ms": String(Int(job.startedAtMs - job.enqueuedAtMs)),
      "run_ms": String(Int(job.doneAtMs - job.startedAtMs)),
      "queue_depth": String(job.queueDepth),
    ]
    if job.blockedBy.isEmpty {
      event.message = SentryMessage(formatted: "Slow core job: \(job.name)")
      event.fingerprint = ["slow-core-job", job.name]
      if job.waiters > 0 { event.tags?["waiters"] = String(job.waiters) }
    } else {
      // The waiter's own name varies; the cause groups the issue.
      let lane = job.kind == .engineQueue ? "Engine" : "Read"
      event.message = SentryMessage(formatted: "\(lane) lane blocked: \(job.blockedBy)")
      event.fingerprint = ["slow-core-job-blocked", queue, job.blockedBy]
      event.tags?["blocker"] = job.blockedBy
      event.tags?["blocker_run_ms"] = String(Int(job.blockerRunMs))
      event.tags?["waiters"] = String(job.waiters)
    }
    // Far below run_ms: the job waited (a lock, or the app suspended in the background).
    if let cpuMs = job.maybeCpuMs {
      event.tags?["cpu_ms"] = String(Int(cpuMs))
    }
    SentrySDK.capture(event: event)
  }

  /// Kept traces (docs/tracing.md) are envelopes the core built and scrubbed; the SDK sends them as
  /// they are, with its own queue, retries and rate limits. Off the core's thread.
  func onTracesKept() {
    DispatchQueue.global(qos: .utility).async {
      for data in takeTraceEnvelopes() {
        // An envelope the SDK can't read is only a trace not sent.
        if let envelope = SentrySDK.internal.envelope.deserialize(from: data) {
          SentrySDK.internal.envelope.capture(envelope)
        }
      }
    }
  }
}
