import SwiftUI
import UIKit
import UserNotifications

/// Why notifications don't arrive, at a glance (notifications.md, "Diagnostics"): the system
/// permission and its settings, the registration, background refresh, DND, what this device saw of
/// pushes, and a test push that goes through the server, the push service and the extension end to
/// end.
struct NotificationDiagnosticsView: View {
  @Environment(AppModel.self) private var model
  let account: Account

  /// How long the screen waits for the test push to come back.
  private static let testWaitSeconds = 60

  @State private var maybeSettings: SystemNotificationSettings?
  @State private var backgroundRefresh = UIBackgroundRefreshStatus.available
  @State private var maybeDiagnostics: PushDiagnosticsInfo?
  @State private var maybePresence: String?
  @State private var maybeSchedule: DndSchedule?
  @State private var test = TestState.idle

  enum TestState: Equatable {
    case idle, sending
    case sent(outcome: TestPushOutcome, maybeProvider: String?, sentAtMs: Int64)
    case arrived(maybeProvider: String?, delayMs: Int64)
    case notArrived(maybeProvider: String?)
    case failed
  }

  var body: some View {
    Form {
      Section("diagnostics_system") {
        row("diagnostics_permission", value: permissionText, isOk: maybeSettings?.authorizationStatus == .authorized)
        row("diagnostics_alerts", value: settingText(maybeSettings?.alertSetting), isOk: maybeSettings?.alertSetting == .enabled)
        row("diagnostics_sounds", value: settingText(maybeSettings?.soundSetting), isOk: maybeSettings?.soundSetting == .enabled)
        row("diagnostics_badges", value: settingText(maybeSettings?.badgeSetting), isOk: maybeSettings?.badgeSetting == .enabled)
        row(
          "diagnostics_time_sensitive", value: settingText(maybeSettings?.timeSensitiveSetting),
          isOk: maybeSettings?.timeSensitiveSetting == .enabled)
        row("diagnostics_background_refresh", value: backgroundRefreshText, isOk: backgroundRefresh == .available)
        Button("diagnostics_open_settings") {
          if let url = URL(string: UIApplication.openNotificationSettingsURLString) { UIApplication.shared.open(url) }
        }
      }
      Section {
        row(
          "diagnostics_registration",
          value: model.isPushTokenRegistered ? "diagnostics_registered" : "diagnostics_not_registered",
          isOk: model.isPushTokenRegistered)
        LabeledContent("diagnostics_provider") { Text(providerText) }
      } header: {
        Text("diagnostics_server")
      } footer: {
        Text("diagnostics_provider_hint")
      }
      Section("diagnostics_dnd") {
        LabeledContent("diagnostics_presence") { Text(presenceText) }
        if let until = QuickDndMemory.until {
          LabeledContent("diagnostics_dnd_until") { Text(until.formatted(date: .abbreviated, time: .shortened)) }
        }
        LabeledContent("diagnostics_schedule") { Text(scheduleText) }
      }
      Section("diagnostics_received") {
        LabeledContent("diagnostics_received_count") { Text(String(maybeDiagnostics?.receivedCount ?? 0)) }
        LabeledContent("diagnostics_last_received") { Text(lastReceivedText) }
        LabeledContent("diagnostics_last_kind") { Text(lastKindText) }
        LabeledContent("diagnostics_last_delay") { Text(delayText(maybeDiagnostics?.maybeLastDelayMs)) }
      }
      Section {
        Button("diagnostics_send_test") { Task { await sendTest() } }
          .disabled(test == .sending || isWaitingForTest)
        testStatus
      } footer: {
        Text("diagnostics_test_hint")
      }
    }
    .navigationTitle("diagnostics_title")
    .navigationBarTitleDisplayMode(.inline)
    .task { await refresh() }
    .refreshable { await refresh() }
  }

  // MARK: Rows

  private func row(_ title: LocalizedStringKey, value: LocalizedStringKey, isOk: Bool) -> some View {
    LabeledContent {
      Label {
        Text(value)
      } icon: {
        Image(systemName: isOk ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
          .foregroundStyle(isOk ? Theme.accent : Theme.danger)
      }
    } label: {
      Text(title)
    }
  }

  @ViewBuilder
  private var testStatus: some View {
    switch test {
    case .idle:
      EmptyView()
    case .sending:
      ProgressView()
    case .sent(let outcome, _, _):
      if outcome == .sent {
        HStack {
          ProgressView()
          Text("diagnostics_test_waiting")
        }
      } else {
        Label(Self.outcomeText(outcome), systemImage: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
      }
    case .arrived(_, let delayMs):
      Label("diagnostics_test_arrived \(delayText(delayMs))", systemImage: "checkmark.circle.fill")
        .foregroundStyle(Theme.accent)
    case .notArrived:
      Label("diagnostics_test_not_arrived", systemImage: "exclamationmark.triangle.fill")
        .foregroundStyle(Theme.danger)
    case .failed:
      Label("diagnostics_test_failed", systemImage: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
    }
  }

  private var isWaitingForTest: Bool {
    if case .sent(.sent, _, _) = test { return true }
    return false
  }

  // MARK: Texts

  private var permissionText: LocalizedStringKey {
    guard let status = maybeSettings?.authorizationStatus else { return "diagnostics_unknown" }
    switch status {
    case .authorized: return "diagnostics_allowed"
    case .provisional, .ephemeral: return "diagnostics_provisional"
    case .denied: return "diagnostics_denied"
    case .notDetermined: return "diagnostics_not_asked"
    @unknown default: return "diagnostics_unknown"
    }
  }

  private func settingText(_ setting: UNNotificationSetting?) -> LocalizedStringKey {
    guard let setting else { return "diagnostics_unknown" }
    switch setting {
    case .enabled: return "diagnostics_on"
    case .disabled: return "diagnostics_off"
    case .notSupported: return "diagnostics_not_supported"
    @unknown default: return "diagnostics_unknown"
    }
  }

  private var backgroundRefreshText: LocalizedStringKey {
    switch backgroundRefresh {
    case .available: return "diagnostics_on"
    case .denied: return "diagnostics_off"
    case .restricted: return "diagnostics_restricted"
    @unknown default: return "diagnostics_unknown"
    }
  }

  private var providerText: String {
    let maybeProvider: String?
    switch test {
    case .sent(_, let provider, _), .arrived(let provider, _), .notArrived(let provider): maybeProvider = provider
    case .idle, .sending, .failed: maybeProvider = nil
    }
    guard let provider = maybeProvider else { return String(localized: "diagnostics_provider_after_test") }
    switch provider {
    case "apns": return String(localized: "diagnostics_provider_apns")
    case "fcm": return String(localized: "diagnostics_provider_fcm")
    case "unifiedpush": return String(localized: "diagnostics_provider_unifiedpush")
    case "webpush": return String(localized: "diagnostics_provider_webpush")
    default: return provider
    }
  }

  private var presenceText: String {
    switch maybePresence {
    case "online": return String(localized: "presence_online")
    case "away": return String(localized: "presence_away")
    case "dnd": return String(localized: "presence_dnd")
    case "offline": return String(localized: "presence_offline")
    default: return String(localized: "diagnostics_unknown")
    }
  }

  private var scheduleText: String {
    guard let schedule = maybeSchedule, !schedule.windows.isEmpty else { return String(localized: "diagnostics_schedule_none") }
    return String(localized: "diagnostics_schedule_ranges \(schedule.windows.count)")
  }

  private var lastReceivedText: String {
    guard let millis = maybeDiagnostics?.maybeLastReceivedAtMs else { return String(localized: "diagnostics_never") }
    return UnixMillis.date(millis).formatted(date: .abbreviated, time: .standard)
  }

  private var lastKindText: String {
    guard let kind = maybeDiagnostics?.maybeLastKind else { return "–" }
    switch kind {
    case .message: return String(localized: "diagnostics_kind_message")
    case .call: return String(localized: "diagnostics_kind_call")
    case .dismiss: return String(localized: "diagnostics_kind_dismiss")
    case .test: return String(localized: "diagnostics_kind_test")
    case .unknown: return String(localized: "diagnostics_unknown")
    }
  }

  private func delayText(_ maybeMillis: Int64?) -> String {
    guard let millis = maybeMillis else { return "–" }
    let seconds = Double(max(0, millis)) / 1000
    return seconds.formatted(.number.precision(.fractionLength(1))) + " s"
  }

  static func outcomeText(_ outcome: TestPushOutcome) -> LocalizedStringKey {
    switch outcome {
    case .sent: return "diagnostics_outcome_sent"
    case .notRegistered: return "diagnostics_outcome_not_registered"
    case .notConfigured: return "diagnostics_outcome_not_configured"
    case .unavailable: return "diagnostics_outcome_unavailable"
    case .rejected: return "diagnostics_outcome_rejected"
    case .gone: return "diagnostics_outcome_gone"
    case .unknown: return "diagnostics_outcome_unknown"
    }
  }

  // MARK: Loading

  private func refresh() async {
    maybeSettings = SystemNotificationSettings(await UNUserNotificationCenter.current().notificationSettings())
    backgroundRefresh = UIApplication.shared.backgroundRefreshStatus
    maybeDiagnostics = await model.pushDiagnostics()
    maybePresence = await model.ownPresenceState(userId: account.userId)
    if let json = await model.notificationSettingsJSON() {
      maybeSchedule = NotificationSettingsDocument.schedule(in: json)
    }
  }

  /// Sends the test, then watches the core's diagnostics until the push came back through the
  /// extension (or the app) or the wait is over.
  private func sendTest() async {
    test = .sending
    guard let result = await model.sendTestPush() else {
      test = .failed
      return
    }
    test = .sent(outcome: result.outcome, maybeProvider: result.maybeProvider, sentAtMs: result.sentAtMs)
    guard result.outcome == .sent else { return }
    for _ in 0..<Self.testWaitSeconds {
      try? await Task.sleep(for: .seconds(1))
      guard let diagnostics = await model.pushDiagnostics() else { continue }
      maybeDiagnostics = diagnostics
      if let arrivedAt = diagnostics.maybeLastTestAtMs, arrivedAt >= result.sentAtMs {
        test = .arrived(maybeProvider: result.maybeProvider, delayMs: arrivedAt - result.sentAtMs)
        return
      }
    }
    test = .notArrived(maybeProvider: result.maybeProvider)
  }
}

/// The parts of `UNNotificationSettings` the screen shows (a value, so it can live in `@State`).
struct SystemNotificationSettings: Equatable {
  let authorizationStatus: UNAuthorizationStatus
  let alertSetting: UNNotificationSetting
  let soundSetting: UNNotificationSetting
  let badgeSetting: UNNotificationSetting
  let timeSensitiveSetting: UNNotificationSetting

  init(_ settings: UNNotificationSettings) {
    authorizationStatus = settings.authorizationStatus
    alertSetting = settings.alertSetting
    soundSetting = settings.soundSetting
    badgeSetting = settings.badgeSetting
    timeSensitiveSetting = settings.timeSensitiveSetting
  }
}
