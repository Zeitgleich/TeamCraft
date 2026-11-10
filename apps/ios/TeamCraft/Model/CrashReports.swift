import Foundation
import Sentry
import os

/// Crash reports of the app (docs/crash-reports.md): runs sentry-cocoa only while the signed-in
/// server has crash reports on (its `server-info`) and the user didn't turn them off, or the
/// instance makes them mandatory. The last known setting is kept, so a crash early in the next
/// start is reported too. Stored in the app's own defaults (not the App Group).
@MainActor
@Observable
final class CrashReports {
  static let shared = CrashReports()
  private static let log = os.Logger(subsystem: "de.zeitgleich.teamcraft", category: "crash-reports")
  private static let configKey = "crash_reports.config"
  private static let optedOutKey = "crash_reports.opted_out"
  private static let installationKey = "crash_reports.installation_id"
  private static let noticeSeenKey = "crash_reports.notice_seen"
  /// Returns to the foreground closer together than this ask the server only once.
  private static let refreshInterval: TimeInterval = 60

  /// Reports are on for this instance: the privacy settings show the switch.
  private(set) var status: CrashReportsStatus?
  /// The user's switch (Settings → Privacy and security).
  private(set) var isAllowedByUser: Bool
  private(set) var isNoticeSeen: Bool

  @ObservationIgnored private let defaults: UserDefaults
  @ObservationIgnored private var maybeRunning: CrashSDKSettings?
  @ObservationIgnored private var maybeLastRefresh: Date?

  init(defaults: UserDefaults = .standard) {
    self.defaults = defaults
    isAllowedByUser = !defaults.bool(forKey: Self.optedOutKey)
    isNoticeSeen = defaults.bool(forKey: Self.noticeSeenKey)
    status = Self.storedConfig(in: defaults).map(CrashReportsStatus.init)
  }

  /// Whether the first-start notice is due: reports on, the instance wants it, not shown yet.
  var isNoticeDue: Bool { status?.showsNotice == true && !isNoticeSeen }

  /// At launch, before anything else can crash: the last known setting and choice.
  func install() { apply() }

  /// Asks the signed-in server for its current setting (`GET /api/v1/server-info`): after sign-in
  /// and when the app comes to the foreground, at most once a minute. Offline, or an answer that
  /// isn't the server's, keeps the last known setting.
  func refresh(server: StoredServer) async {
    if let last = maybeLastRefresh, Date().timeIntervalSince(last) < Self.refreshInterval { return }
    maybeLastRefresh = Date()
    guard let url = URL(string: server.address.serverInfoUrl),
      let answer = try? await ServerInfoClient().fetch(url), answer.status == 200
    else { return }
    let maybeConfig = crashReportsConfig(responseJson: String(decoding: answer.body, as: UTF8.self))
    update(maybeConfig.map(CrashConfig.init))
  }

  /// The signed-in server's current setting (`nil` while it has reports off).
  func update(_ maybeConfig: CrashConfig?) {
    if let config = maybeConfig, let data = try? JSONEncoder().encode(config) {
      defaults.set(data, forKey: Self.configKey)
    } else {
      defaults.removeObject(forKey: Self.configKey)
    }
    status = maybeConfig.map(CrashReportsStatus.init)
    apply()
  }

  /// The user turned the switch; turned on again, reports get a new installation ID.
  func setAllowedByUser(_ isAllowed: Bool) {
    if isAllowed && !isAllowedByUser { defaults.set(Self.newInstallationID(), forKey: Self.installationKey) }
    isAllowedByUser = isAllowed
    defaults.set(!isAllowed, forKey: Self.optedOutKey)
    apply()
  }

  func markNoticeSeen() {
    isNoticeSeen = true
    defaults.set(true, forKey: Self.noticeSeenKey)
  }

  /// The user left this server (signed out and changed the server): its setting no longer counts.
  func forgetServer() {
    maybeLastRefresh = nil
    update(nil)
  }

  /// On while the instance has reports on, unless the user opted out where that is allowed.
  nonisolated static func isActive(config: CrashConfig, isAllowedByUser: Bool) -> Bool {
    isAllowedByUser || !config.allowsOptOut
  }

  /// The OS version as the reports carry it (`17.4.1`).
  nonisolated static func osVersion() -> String {
    let version = ProcessInfo.processInfo.operatingSystemVersion
    return "\(version.majorVersion).\(version.minorVersion).\(version.patchVersion)"
  }

  /// 128 random bits as 32 hex digits; never derived from the account, device or instance.
  nonisolated static func newInstallationID() -> String {
    var generator = SystemRandomNumberGenerator()
    return (0..<16).map { _ in String(format: "%02x", UInt8.random(in: 0...255, using: &generator)) }.joined()
  }

  private func apply() {
    guard let config = Self.storedConfig(in: defaults), Self.isActive(config: config, isAllowedByUser: isAllowedByUser)
    else {
      stop()
      return
    }
    let release = "teamcraft@\(AppEnvironment.current.clientVersion)"
    start(CrashSDKSettings(config: config, release: release, installationID: installationID()))
  }

  private func start(_ settings: CrashSDKSettings) {
    guard maybeRunning != settings else { return }
    if maybeRunning != nil { SentrySDK.close() }
    SentrySDK.start { options in Self.configure(options, settings: settings) }
    // The core's slow jobs as sampled warnings, and the app's own measures (issue #1521).
    setSlowJobThresholdMs(thresholdMs: settings.config.slowJobMs)
    setPerfListener(listener: SlowJobReports())
    // The core's spans (docs/tracing.md): slow, failed and sampled traces as transactions.
    traceStart(
      traceRate: settings.config.traceRate,
      slowJobMs: settings.config.slowJobMs,
      environment: settings.config.environment,
      appVersion: String(settings.release.dropFirst("teamcraft@".count)),
      installationId: settings.installationID,
      osVersion: Self.osVersion()
    )
    Perf.setRunning(true)
    // The scope's user too, so every report path carries the installation ID and nothing else.
    SentrySDK.setUser(User(userId: settings.installationID))
    maybeRunning = settings
    Self.log.info("crash reports on")
  }

  private func stop() {
    guard maybeRunning != nil else { return }
    setSlowJobThresholdMs(thresholdMs: 0)
    traceStop()
    Perf.setRunning(false)
    SentrySDK.close()
    maybeRunning = nil
    Self.log.info("crash reports off")
  }

  /// Only what docs/crash-reports.md allows: no PII, screenshots, view hierarchy, replay, sessions
  /// or automatic breadcrumbs; every event passes `CrashEventScrub` before it is sent. Performance
  /// data (app start, screen loads, slow and frozen frames) is traced at the instance's trace rate
  /// (issue #1521), without network, file or Core Data spans, which would name URLs and files.
  private nonisolated static func configure(_ options: Options, settings: CrashSDKSettings) {
    options.dsn = settings.config.dsn
    options.releaseName = settings.release
    options.environment = settings.config.environment
    options.sendDefaultPii = false
    options.attachScreenshot = false
    options.attachViewHierarchy = false
    options.enableAutoBreadcrumbTracking = false
    options.enableNetworkBreadcrumbs = false
    options.enableCaptureFailedRequests = false
    // Performance data: off at a rate of 0, as from an older server.
    options.tracesSampleRate = settings.config.traceRate > 0 ? NSNumber(value: settings.config.traceRate) : nil
    options.enableAutoPerformanceTracing = settings.config.traceRate > 0
    options.enableUIViewControllerTracing = true
    options.enableUserInteractionTracing = false
    options.enableNetworkTracking = false
    options.enableFileIOTracing = false
    options.enableCoreDataTracing = false
    // No sessions: release health isn't used, and an app that doesn't crash sends nothing.
    options.enableAutoSessionTracking = false
    options.enableAppHangTracking = true
    options.enableMetricKit = true
    // Breadcrumbs come from the core's redacted log only, set in beforeSend.
    options.beforeBreadcrumb = { _ in nil }
    // Sampling of handled errors happens here: the SDK's own rate would sample crashes too.
    options.beforeSend = { event in
      CrashEventScrub.prepare(
        event, installationID: settings.installationID, sampleRate: settings.config.sampleRate,
        roll: Double.random(in: 0..<1), logLines: { recentLogLines() })
    }
  }

  private func installationID() -> String {
    if let stored = defaults.string(forKey: Self.installationKey) { return stored }
    let fresh = Self.newInstallationID()
    defaults.set(fresh, forKey: Self.installationKey)
    return fresh
  }

  private static func storedConfig(in defaults: UserDefaults) -> CrashConfig? {
    defaults.data(forKey: configKey).flatMap { try? JSONDecoder().decode(CrashConfig.self, from: $0) }
  }
}

/// The instance's crash reports setting, as the core checked it (`crashReportsConfig`).
struct CrashConfig: Codable, Equatable, Sendable {
  let dsn: String
  /// Share of handled errors sent, 0 to 1; crashes are always sent.
  let sampleRate: Double
  let environment: String
  let allowsOptOut: Bool
  let showsNotice: Bool
  /// Share of app starts and screen loads traced, 0 to 1; 0 when stored by an older version.
  let traceRate: Double
  /// A core job that waits or runs this long is reported as a sampled warning; 0: never.
  let slowJobMs: UInt64

  init(
    dsn: String, sampleRate: Double, environment: String, allowsOptOut: Bool, showsNotice: Bool,
    traceRate: Double = 0, slowJobMs: UInt64 = 1_000
  ) {
    self.dsn = dsn
    self.sampleRate = sampleRate
    self.environment = environment
    self.allowsOptOut = allowsOptOut
    self.showsNotice = showsNotice
    self.traceRate = traceRate
    self.slowJobMs = slowJobMs
  }

  init(_ config: CrashReportsConfig) {
    self.init(
      dsn: config.dsn, sampleRate: config.sampleRate, environment: config.environment,
      allowsOptOut: config.allowsOptOut, showsNotice: config.showsNotice, traceRate: config.traceRate,
      slowJobMs: config.slowJobMs)
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    dsn = try container.decode(String.self, forKey: .dsn)
    sampleRate = try container.decode(Double.self, forKey: .sampleRate)
    environment = try container.decode(String.self, forKey: .environment)
    allowsOptOut = try container.decode(Bool.self, forKey: .allowsOptOut)
    showsNotice = try container.decode(Bool.self, forKey: .showsNotice)
    traceRate = try container.decodeIfPresent(Double.self, forKey: .traceRate) ?? 0
    slowJobMs = try container.decodeIfPresent(UInt64.self, forKey: .slowJobMs) ?? 1_000
  }
}

/// What the privacy settings and the first-start notice need to know.
struct CrashReportsStatus: Equatable {
  let allowsOptOut: Bool
  let showsNotice: Bool

  init(_ config: CrashConfig) {
    allowsOptOut = config.allowsOptOut
    showsNotice = config.showsNotice
  }
}

/// What sentry-cocoa is started with.
struct CrashSDKSettings: Equatable, Sendable {
  let config: CrashConfig
  /// `teamcraft@<version>`, as the other apps report it.
  let release: String
  /// The random installation ID, the report's only `user` field.
  let installationID: String
}

/// sentry-cocoa's `beforeSend` (docs/crash-reports.md, "iOS"): samples handled errors, drops what a
/// report never carries (request, server name, extras, every user field but the installation ID),
/// scrubs exception and message texts (`CrashScrubber`) and puts the core's redacted log lines in
/// as the only breadcrumbs.
enum CrashEventScrub {
  /// The event to send, or `nil` when a handled error isn't in the sample.
  static func prepare(
    _ event: Event, installationID: String, sampleRate: Double, roll: Double, logLines: () -> [String]
  ) -> Event? {
    // sentry-cocoa hands transactions (performance data, sampled by the SDK's trace rate) to
    // `beforeSend` too: they keep their spans, lose what a report never carries.
    if event.type == "transaction" {
      // A measure the user moved on from (`Perf.cancel`) is dropped here.
      if event.tags?[Perf.droppedTag] != nil { return nil }
      return prepareTransaction(event, installationID: installationID)
    }
    guard isCrash(event) || roll < sampleRate else { return nil }
    event.request = nil
    event.serverName = nil
    event.extra = nil
    event.user = User(userId: installationID)
    for exception in event.exceptions ?? [] {
      if let value = exception.value as String? { exception.value = CrashScrubber.scrub(value) }
    }
    if let message = event.message {
      event.message = SentryMessage(formatted: CrashScrubber.scrub(message.formatted as String? ?? ""))
    }
    event.breadcrumbs = logLines().suffix(CrashScrubber.maxBreadcrumbs).map { line in
      let breadcrumb = Breadcrumb(level: .info, category: "log")
      breadcrumb.message = CrashScrubber.scrub(line, maxChars: CrashScrubber.maxBreadcrumbChars)
      return breadcrumb
    }
    return event
  }

  /// A transaction: no request, server name, extras or breadcrumbs, the installation ID as the only
  /// user field, tag values scrubbed. Its spans come only from app start, screen loads and frames
  /// (network, file and Core Data tracing are off), so they name no URL or file.
  static func prepareTransaction(_ event: Event, installationID: String) -> Event {
    event.request = nil
    event.serverName = nil
    event.extra = nil
    event.breadcrumbs = nil
    event.user = User(userId: installationID)
    event.tags = event.tags?.mapValues { CrashScrubber.redact($0) }
    return event
  }

  /// A crash or app hang (always sent), not a handled error (sampled).
  private static func isCrash(_ event: Event) -> Bool {
    event.level == .fatal
      || (event.exceptions ?? []).contains { $0.mechanism?.handled?.boolValue == false }
  }
}
