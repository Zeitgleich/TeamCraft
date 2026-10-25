import SwiftUI

/// Settings as a hub, like the Android app: name, presence and status on top, then one page each
/// for notifications, chats, privacy and security and devices, and sign-out.
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
        customStatusSection
        Section {
          NavigationLink { NotificationsSettingsPage(account: account) } label: {
            SettingsEntry(title: "notifications_title", summary: "settings_notifications_summary", icon: "bell")
          }
          NavigationLink { ChatsSettingsPage() } label: {
            SettingsEntry(title: "settings_chats_title", summary: "settings_chats_summary", icon: "bubble.left.and.bubble.right")
          }
          NavigationLink { SecuritySettingsPage(account: account) } label: {
            SettingsEntry(title: "settings_security_title", summary: "settings_security_summary", icon: "lock.shield")
          }
          NavigationLink { ApproveLinkView() } label: {
            SettingsEntry(title: "devices_title", summary: "settings_devices_summary", icon: "iphone")
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

  private var customStatusSection: some View {
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
  }
}

/// One entry of the settings hub: what the page is and what it holds.
private struct SettingsEntry: View {
  let title: LocalizedStringKey
  let summary: LocalizedStringKey
  let icon: String

  var body: some View {
    Label {
      VStack(alignment: .leading, spacing: 2) {
        Text(title)
        Text(summary).font(.caption).foregroundStyle(Theme.textMuted)
      }
    } icon: {
      Image(systemName: icon)
    }
  }
}

/// Push, do not disturb, the mention rules and the diagnostics.
private struct NotificationsSettingsPage: View {
  @Environment(AppModel.self) private var model
  let account: Account

  var body: some View {
    Form {
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
        NavigationLink("notification_content_title") { NotificationContentView() }
        NavigationLink("dnd_title") { DndSettingsView() }
        NavigationLink("rules_title") { NotificationRulesView() }
        NavigationLink("diagnostics_title") { NotificationDiagnosticsView(account: account) }
      }
    }
    .navigationTitle("notifications_title")
    .navigationBarTitleDisplayMode(.inline)
  }
}

/// Typing indicator and the search download.
private struct ChatsSettingsPage: View {
  @Environment(AppModel.self) private var model
  @AppStorage("share_typing") private var isSharingTyping = true
  @AppStorage(CallStatusSoundsSetting.key) private var statusSounds = CallStatusSoundsSetting.all.rawValue
  @State private var prefetch = SegmentPrefetchMode.unmeteredOnly
  @State private var isPrefetchLoaded = false

  var body: some View {
    Form {
      Section {
        Toggle("share_typing", isOn: $isSharingTyping)
      }
      Section {
        Picker("call_status_sounds", selection: $statusSounds) {
          Text("call_status_sounds_all").tag(CallStatusSoundsSetting.all.rawValue)
          Text("call_status_sounds_own_only").tag(CallStatusSoundsSetting.ownOnly.rawValue)
          Text("call_status_sounds_off").tag(CallStatusSoundsSetting.off.rawValue)
        }
      } footer: {
        Text("call_status_sounds_hint")
      }
      Section {
        Picker("prefetch_title", selection: $prefetch) {
          Text("prefetch_off").tag(SegmentPrefetchMode.off)
          Text("prefetch_unmetered").tag(SegmentPrefetchMode.unmeteredOnly)
          Text("prefetch_always").tag(SegmentPrefetchMode.always)
        }
      } header: {
        Text("search_title")
      } footer: {
        Text("prefetch_hint")
      }
    }
    .task {
      prefetch = await model.segmentPrefetch()
      isPrefetchLoaded = true
    }
    .onChange(of: prefetch) { _, mode in
      if isPrefetchLoaded { Task { await model.setSegmentPrefetch(mode) } }
    }
    .navigationTitle("settings_chats_title")
    .navigationBarTitleDisplayMode(.inline)
  }
}

/// The security code, the app lock and the app switcher cover.
private struct SecuritySettingsPage: View {
  let account: Account

  var body: some View {
    Form {
      Section {
        Text(account.identityFingerprint)
          .font(.system(.footnote, design: .monospaced))
          .textSelection(.enabled)
      } header: {
        Text("fingerprint_title")
      } footer: {
        Text("fingerprint_hint")
      }
      DeviceSecuritySection()
    }
    .navigationTitle("settings_security_title")
    .navigationBarTitleDisplayMode(.inline)
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
          Text(shortCode).font(.system(.largeTitle, design: .monospaced).bold())
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

/// What counts as a mention besides `@username`: `@channel`, the first name and the user's own
/// keywords. The core saves the rules in the encrypted store; the notification extension applies
/// them to pushed messages too.
struct NotificationRulesView: View {
  @Environment(AppModel.self) private var model

  @State private var rules = MentionSettings(isFirstNameMention: true, doChannelMentionsNotify: true, keywords: [])
  @State private var isLoaded = false
  @State private var newKeyword = ""

  var body: some View {
    Form {
      Section {
        Toggle("rules_first_name", isOn: $rules.isFirstNameMention)
        Toggle("rules_channel", isOn: $rules.doChannelMentionsNotify)
      } footer: {
        Text("rules_hint")
      }
      Section("rules_keywords") {
        ForEach(rules.keywords, id: \.self) { keyword in Text(keyword) }
          .onDelete { offsets in rules.keywords.remove(atOffsets: offsets) }
        HStack {
          TextField("rules_keyword_hint", text: $newKeyword)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .onSubmit(addKeyword)
          Button("rules_keyword_add", action: addKeyword)
            .disabled(newKeyword.trimmingCharacters(in: .whitespaces).isEmpty)
        }
      }
    }
    .navigationTitle("rules_title")
    .navigationBarTitleDisplayMode(.inline)
    .task {
      rules = await model.mentionSettings()
      isLoaded = true
    }
    .onChange(of: rules) { _, updated in
      if isLoaded { Task { await model.setMentionSettings(updated) } }
    }
  }

  private func addKeyword() {
    let keyword = newKeyword.trimmingCharacters(in: .whitespacesAndNewlines)
    newKeyword = ""
    guard !keyword.isEmpty, !rules.keywords.contains(where: { $0.caseInsensitiveCompare(keyword) == .orderedSame })
    else { return }
    rules.keywords.append(keyword)
  }
}
