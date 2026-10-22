import BackgroundTasks
import Foundation
import Network

/// Downloads the shared search index segments in the background (docs/e2ee.md §10.1), so old
/// history is searchable without waiting. iOS runs a `BGProcessingTask` when the device is charging
/// and online; whether the current network qualifies is the core's setting (`SegmentPrefetchMode`,
/// by default only unmetered networks), the download itself is the core's too.
enum SegmentPrefetchTask {
  static let identifier = "de.zeitgleich.teamcraft.segment-prefetch"

  /// Must run before the app finished launching.
  static func register() {
    BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: nil) { task in
      guard let processing = task as? BGProcessingTask else {
        task.setTaskCompleted(success: false)
        return
      }
      let completion = Completion(processing)
      let work = Task {
        let isDone = await AppModel.shared.runSegmentPrefetch()
        completion.finish(success: isDone)
        // The next run, whatever the outcome.
        schedule()
      }
      processing.expirationHandler = {
        work.cancel()
        completion.finish(success: false)
      }
    }
  }

  /// Asks for a run in the next hours. Fails quietly where the system doesn't allow it (simulator).
  static func schedule() {
    let request = BGProcessingTaskRequest(identifier: identifier)
    request.requiresNetworkConnectivity = true
    request.requiresExternalPower = true
    request.earliestBeginDate = Date(timeIntervalSinceNow: 4 * 3600)
    try? BGTaskScheduler.shared.submit(request)
  }

  static func cancel() {
    BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: identifier)
  }
}

/// Completes the task once: the work and the expiry handler race.
private final class Completion: @unchecked Sendable {
  private let lock = NSLock()
  private var maybeTask: BGTask?

  init(_ task: BGTask) {
    maybeTask = task
  }

  func finish(success: Bool) {
    lock.lock()
    let task = maybeTask
    maybeTask = nil
    lock.unlock()
    task?.setTaskCompleted(success: success)
  }
}

/// What kind of network the device is on right now.
enum NetworkPath {
  /// Whether the current network is metered (cellular, personal hotspot, Low Data Mode). With no
  /// answer within a few seconds it counts as metered: better to skip than to burn mobile data.
  static func isMetered() async -> Bool {
    await withCheckedContinuation { continuation in
      let monitor = NWPathMonitor()
      let gate = OneShot(continuation)
      monitor.pathUpdateHandler = { path in
        monitor.cancel()
        gate.resume(path.isExpensive || path.isConstrained)
      }
      monitor.start(queue: DispatchQueue(label: "de.zeitgleich.teamcraft.network-path"))
      DispatchQueue.global().asyncAfter(deadline: .now() + 3) {
        monitor.cancel()
        gate.resume(true)
      }
    }
  }
}

/// Resumes a continuation exactly once.
private final class OneShot: @unchecked Sendable {
  private let lock = NSLock()
  private var maybeContinuation: CheckedContinuation<Bool, Never>?

  init(_ continuation: CheckedContinuation<Bool, Never>) {
    maybeContinuation = continuation
  }

  func resume(_ value: Bool) {
    lock.lock()
    let continuation = maybeContinuation
    maybeContinuation = nil
    lock.unlock()
    continuation?.resume(returning: value)
  }
}
