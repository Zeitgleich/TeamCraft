import Foundation
import MetricKit
import Sentry

/// Kills by the system that no in-process handler sees (#2179): RunningBoard ending a suspended app
/// that held a file lock in the App Group container (0xdead10cc), the watchdog (0x8badf00d) and the
/// like arrive as SIGKILL. sentry-cocoa's MetricKit integration (`enableMetricKit`) sends CPU,
/// disk-write and hang diagnostics, never `MXCrashDiagnostic` (its own crash handler covers the
/// crashes it can catch), so these are forwarded here, once MetricKit delivers them on a later
/// launch (docs/crash-reports.md, "iOS"). Only while crash reports are on; the report carries the
/// namespace and code parsed from the termination reason, never its free text.
final class SystemKillReports: NSObject, MXMetricManagerSubscriber {
  static let shared = SystemKillReports()

  /// Call once at launch, after the crash reports.
  func install() {
    MXMetricManager.shared.add(self)
  }

  func didReceive(_ payloads: [MXDiagnosticPayload]) {
    guard SentrySDK.isEnabled else { return }
    for payload in payloads {
      for diagnostic in payload.crashDiagnostics ?? [] {
        guard
          let kill = SystemKill(signal: diagnostic.signal?.intValue, terminationReason: diagnostic.terminationReason)
        else { continue }
        SentrySDK.capture(event: kill.event(at: payload.timeStampEnd, appBuild: diagnostic.metaData.applicationBuildVersion))
      }
    }
  }
}

/// A crash diagnostic of a kill by the system (SIGKILL), reduced to what a report may carry.
struct SystemKill: Equatable {
  /// The termination reason's namespace (`RUNNINGBOARD`, `FRONTBOARD`), when it names one.
  let maybeNamespace: String?
  /// The termination reason's code (`0xdead10cc`), lowercased, when it names one.
  let maybeCode: String?

  /// `nil` for anything but SIGKILL: sentry-cocoa reports the other crashes itself.
  init?(signal: Int?, terminationReason: String?) {
    guard signal == Int(SIGKILL) else { return nil }
    maybeNamespace = terminationReason.flatMap { Self.firstGroup(of: #"Namespace ([A-Z_]{1,32})"#, in: $0) }
    maybeCode = terminationReason.flatMap { Self.firstGroup(of: #"Code (0x[0-9A-Fa-f]{1,16})"#, in: $0) }?
      .lowercased()
  }

  /// What the code means, for the report's title.
  var meaning: String {
    switch maybeCode {
    case "0xdead10cc": return "held a file lock in the shared container while suspended"
    case "0x8badf00d": return "watchdog timeout"
    case "0xc00010ff": return "thermal pressure"
    case "0xbaadca11": return "no call reported for a VoIP push"
    default: return "killed by the system"
    }
  }

  var summary: String {
    "\(maybeNamespace ?? "unknown") \(maybeCode ?? "no code"): \(meaning)"
  }

  /// A fatal, unhandled event, so `CrashEventScrub` treats it as a crash (always sent).
  func event(at date: Date, appBuild: String) -> Event {
    let event = Event(level: .fatal)
    let exception = Exception(value: summary, type: "SystemKill")
    let mechanism = Mechanism(type: "MetricKit")
    mechanism.handled = false
    exception.mechanism = mechanism
    event.exceptions = [exception]
    event.timestamp = date
    var tags = ["source": "metrickit", "app_build": appBuild]
    if let code = maybeCode { tags["termination_code"] = code }
    if let namespace = maybeNamespace { tags["termination_namespace"] = namespace }
    event.tags = tags
    return event
  }

  private static func firstGroup(of pattern: String, in text: String) -> String? {
    guard let regex = try? NSRegularExpression(pattern: pattern),
      let match = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)),
      let range = Range(match.range(at: 1), in: text)
    else { return nil }
    return String(text[range])
  }
}
