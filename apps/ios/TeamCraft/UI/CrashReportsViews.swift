import SwiftUI

/// Settings → Privacy and security: "Send crash reports", only while the instance has them on
/// (docs/crash-reports.md, "Settings"). Mandatory on the instance: on, disabled, and it says so.
struct CrashReportsSection: View {
  @State private var reports = CrashReports.shared

  var body: some View {
    if let status = reports.status {
      Section {
        Toggle("crash_reports_send", isOn: binding(allowsOptOut: status.allowsOptOut))
          .disabled(!status.allowsOptOut)
      } header: {
        Text("crash_reports_title")
      } footer: {
        Text(status.allowsOptOut ? LocalizedStringKey("crash_reports_send_hint") : LocalizedStringKey("crash_reports_required_hint"))
      }
    }
  }

  private func binding(allowsOptOut: Bool) -> Binding<Bool> {
    Binding(
      get: { reports.isAllowedByUser || !allowsOptOut },
      set: { reports.setAllowedByUser($0) })
  }
}

extension View {
  /// Once, on the first start with crash reports on, when the instance wants people told.
  func crashReportsNotice(onSettings: @escaping () -> Void) -> some View {
    modifier(CrashReportsNotice(onSettings: onSettings))
  }
}

private struct CrashReportsNotice: ViewModifier {
  let onSettings: () -> Void
  @State private var reports = CrashReports.shared

  func body(content: Content) -> some View {
    content.alert(
      "crash_reports_title",
      isPresented: Binding(get: { reports.isNoticeDue }, set: { if !$0 { reports.markNoticeSeen() } })
    ) {
      Button("crash_reports_notice_ok", role: .cancel) { reports.markNoticeSeen() }
      Button("crash_reports_notice_settings") {
        reports.markNoticeSeen()
        onSettings()
      }
    } message: {
      Text("crash_reports_notice")
    }
  }
}
