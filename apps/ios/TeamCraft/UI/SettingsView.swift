import SwiftUI

struct SettingsView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let account: Account

  @AppStorage("presence") private var presenceRaw = PresenceChoice.online.rawValue
  @State private var statusEmoji = ""
  @State private var statusText = ""
  @State private var statusExpiry = StatusExpiryValue.never

  var body: some View {
    NavigationStack {
      Form {
        Section {
          HStack(spacing: 12) {
            AvatarView(name: account.displayName, size: 44)
            Text(account.displayName).font(.headline)
          }
        }
        Section("presence_title") {
          Picker("presence_title", selection: $presenceRaw) {
            ForEach(PresenceChoice.allCases) { choice in Text(choice.title).tag(choice.rawValue) }
          }
          .pickerStyle(.inline)
          .labelsHidden()
          .onChange(of: presenceRaw) { _, raw in
            if let choice = PresenceChoice(rawValue: raw) { Task { await model.setPresence(choice.presence) } }
          }
        }
        Section("custom_status_title") {
          TextField("custom_status_emoji", text: $statusEmoji)
          TextField("custom_status_text", text: $statusText)
          Picker("custom_status_expiry", selection: $statusExpiry) {
            ForEach(StatusExpiryValue.allCases) { expiry in Text(expiry.title).tag(expiry) }
          }
          Button("custom_status_save") {
            let status = CustomStatusValue(emoji: statusEmoji, text: statusText, expiry: statusExpiry)
            Task { await model.perform(.setCustomStatus(status)) }
          }
          .disabled(statusEmoji.isEmpty && statusText.isEmpty)
          Button("custom_status_clear", role: .destructive) {
            statusEmoji = ""
            statusText = ""
            Task { await model.perform(.setCustomStatus(nil)) }
          }
        }
        Section {
          NavigationLink("approve_title") { ApproveLinkView() }
        } header: {
          Text("devices_title")
        }
        Section {
          Text(account.identityFingerprint)
            .font(.system(.footnote, design: .monospaced))
            .textSelection(.enabled)
        } header: {
          Text("fingerprint_title")
        } footer: {
          Text("fingerprint_hint")
        }
        Section("push_title") {
          switch model.pushStatus {
          case .enabled: Label("push_registered", systemImage: "checkmark.circle")
          case .denied:
            Button("push_denied_open_settings") {
              if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
            }
          case .unknown: Button("push_enable") { Task { await model.enablePush() } }
          }
        }
        Section {
          Button("sign_out", role: .destructive) {
            Task {
              await model.signOut()
              dismiss()
            }
          }
        }
      }
      .navigationTitle("settings_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
      }
    }
  }
}

/// What the user can pick as presence (the core's four states).
enum PresenceChoice: String, CaseIterable, Identifiable {
  case online, away, doNotDisturb, offline

  var id: String { rawValue }

  var presence: Presence {
    switch self {
    case .online: return .online
    case .away: return .away
    case .doNotDisturb: return .doNotDisturb
    case .offline: return .offline
    }
  }

  var title: LocalizedStringKey {
    switch self {
    case .online: return "presence_online"
    case .away: return "presence_away"
    case .doNotDisturb: return "presence_dnd"
    case .offline: return "presence_offline"
    }
  }
}

extension StatusExpiryValue {
  var title: LocalizedStringKey {
    switch self {
    case .never: return "expiry_never"
    case .thirtyMinutes: return "expiry_30_minutes"
    case .oneHour: return "expiry_1_hour"
    case .today: return "expiry_today"
    case .thisWeek: return "expiry_this_week"
    }
  }
}

/// Existing device: approves a new device by the code it shows (e2ee.md §5).
struct ApproveLinkView: View {
  @Environment(AppModel.self) private var model

  private enum Step {
    case enterCode, compare(String), done, failed
  }

  @State private var code = ""
  @State private var step = Step.enterCode

  var body: some View {
    Form {
      switch step {
      case .enterCode:
        Section {
          Text("approve_hint").foregroundStyle(Theme.textMuted)
          TextField("approve_code", text: $code, axis: .vertical)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .font(.system(.body, design: .monospaced))
          Button("approve_start") { Task { await approve() } }
            .disabled(code.trimmingCharacters(in: .whitespaces).isEmpty)
        }
      case .compare(let shortCode):
        Section {
          Text("approve_compare")
          Text(shortCode).font(.system(size: 36, weight: .bold, design: .monospaced))
          Button("approve_confirm") { Task { await confirm() } }
        }
      case .done:
        Section { Label("approve_done", systemImage: "checkmark.circle") }
      case .failed:
        Section {
          Text("error_generic").foregroundStyle(Theme.danger)
          Button("retry") { step = .enterCode }
        }
      }
    }
    .navigationTitle("approve_title")
    .navigationBarTitleDisplayMode(.inline)
  }

  private func approve() async {
    do { step = .compare(try await model.approveLink(code: code)) } catch { step = .failed }
  }

  private func confirm() async {
    do {
      try await model.confirmLink()
      step = .done
    } catch { step = .failed }
  }
}
