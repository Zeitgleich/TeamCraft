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
  @State private var maybeStatusProblem: TextProblem?
  @State private var isSavingStatus = false
  @FocusState private var isStatusFocused: Bool
  @AccessibilityFocusState private var isStatusAccessibilityFocused: Bool

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
          DiagnosticsExportRow()
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
      LabeledContent("custom_status_emoji_label") {
        TextField("custom_status_emoji", text: $statusEmoji)
          .multilineTextAlignment(.trailing)
      }
      VStack(alignment: .leading, spacing: 6) {
        LabeledContent("custom_status_text_label") {
          TextField("custom_status_text", text: $statusText)
            .multilineTextAlignment(.trailing)
            .focused($isStatusFocused)
            .accessibilityFocused($isStatusAccessibilityFocused)
            .fieldError(maybeStatusProblem?.message)
            .onChange(of: statusText) { maybeStatusProblem = nil }
        }
        FieldMessage(maybeError: maybeStatusProblem?.message)
      }
      Picker("custom_status_expiry", selection: $statusExpiry) {
        ForEach(StatusExpiryValue.allCases) { expiry in Text(expiry.title).tag(expiry) }
      }
      Button("custom_status_save") { Task { await saveStatus() } }
        .disabled(isSavingStatus)
      Button("custom_status_clear", role: .destructive) {
        statusEmoji = ""
        statusText = ""
        maybeStatusProblem = nil
        Task { await model.perform(.setCustomStatus(nil)) }
      }
      .disabled(isSavingStatus)
    }
  }

  /// An empty status is said at the text field; so is the core's refusal, with the input kept.
  private func saveStatus() async {
    guard !isSavingStatus else { return }
    if statusEmoji.isEmpty, let problem = TextProblem.required(statusText, missing: "custom_status_missing") {
      showStatusProblem(problem)
      return
    }
    isSavingStatus = true
    let status = CustomStatusValue(emoji: statusEmoji, text: statusText, expiry: statusExpiry)
    if let failure = await model.performReporting(.setCustomStatus(status)) {
      showStatusProblem(.refused(failure))
    }
    isSavingStatus = false
  }

  private func showStatusProblem(_ problem: TextProblem) {
    maybeStatusProblem = problem
    isStatusFocused = true
    isStatusAccessibilityFocused = true
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

/// Typing indicator, call sounds, original pictures and the search download.
private struct ChatsSettingsPage: View {
  @Environment(AppModel.self) private var model
  @AppStorage("share_typing") private var isSharingTyping = true
  @AppStorage(CallStatusSoundsSetting.key) private var statusSounds = CallStatusSoundsSetting.all.rawValue
  @AppStorage(OriginalDownloadSetting.key) private var originalDownload = OriginalDownloadSetting.fallback.rawValue
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
        Picker("original_download_title", selection: $originalDownload) {
          Text("original_download_always").tag(OriginalDownloadSetting.always.rawValue)
          Text("original_download_unmetered").tag(OriginalDownloadSetting.unmeteredOnly.rawValue)
          Text("original_download_never").tag(OriginalDownloadSetting.never.rawValue)
        }
      } footer: {
        Text("original_download_hint")
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

/// The key backup, the security code, the app lock and the app switcher cover.
private struct SecuritySettingsPage: View {
  let account: Account
  @Environment(AppModel.self) private var model
  @State private var backup: BackupSnapshot.KeyBackup?

  var body: some View {
    Form {
      if let backup {
        KeyBackupSection(backup: backup)
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
      DeviceSecuritySection()
    }
    .navigationTitle("settings_security_title")
    .navigationBarTitleDisplayMode(.inline)
    .task { backup = await model.keyBackup() }
  }
}

/// The key backup in plain words (issue #410): stored or not, and how it is protected.
private struct KeyBackupSection: View {
  let backup: BackupSnapshot.KeyBackup

  var body: some View {
    Section {
      Label {
        Text(title)
      } icon: {
        Image(systemName: isOk ? "checkmark.shield" : "exclamationmark.shield")
          .foregroundStyle(isOk ? Color.green : Color.orange)
      }
      .accessibilityElement(children: .combine)
    } header: {
      Text("backup_title")
    } footer: {
      VStack(alignment: .leading, spacing: 4) {
        Text(detail)
        if backup.hasOrganization {
          Text("backup_organization")
        }
      }
    }
  }

  private var isOk: Bool { backup.isStored && backup.hasPassphrase }

  private var title: LocalizedStringKey {
    if !backup.isStored { return "backup_missing" }
    return backup.hasPassphrase ? "backup_ok" : "backup_no_recovery"
  }

  private var detail: String {
    if !backup.isStored { return String(localized: "backup_missing_hint") }
    if !backup.hasPassphrase { return String(localized: "backup_no_recovery_hint") }
    guard let date = backup.passphraseSetDate else { return String(localized: "backup_passphrase") }
    return String(format: String(localized: "backup_passphrase_set"), date.formatted(date: .abbreviated, time: .omitted))
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

/// Existing device: joins a new device by the code it shows (scanned or typed), then the user types the
/// short code the new device shows (e2ee.md §5). This device never shows that code.
struct ApproveLinkView: View {
  @Environment(AppModel.self) private var model

  private enum Step: Equatable {
    case enterCode, typeCode, done
  }

  @State private var code = ""
  @State private var step = Step.enterCode
  /// Why the last attempt ended (on the new device); not about the code typed here.
  @State private var maybeNotice: LocalizedStringKey?
  /// A missing code or the server's refusal of it, shown at the code field.
  @State private var maybeProblem: TextProblem?
  @State private var isWorking = false
  @State private var isScanning = false
  @FocusState private var isCodeFocused: Bool
  @AccessibilityFocusState private var isCodeAccessibilityFocused: Bool

  var body: some View {
    Form {
      switch step {
      case .enterCode:
        Section {
          if let notice = maybeNotice {
            Text(notice).font(.footnote).foregroundStyle(Theme.danger)
          }
          Text("approve_hint").foregroundStyle(Theme.textMuted)
          Button("approve_scan", systemImage: "qrcode.viewfinder") { isScanning = true }
            .disabled(isWorking)
          Text("approve_new_phone_hint").font(.footnote).foregroundStyle(Theme.textMuted)
          VStack(alignment: .leading, spacing: 6) {
            TextField("approve_code", text: $code, axis: .vertical)
              .textInputAutocapitalization(.never)
              .autocorrectionDisabled()
              .font(.system(.body, design: .monospaced))
              .focused($isCodeFocused)
              .accessibilityFocused($isCodeAccessibilityFocused)
              .fieldError(maybeProblem?.message)
              .onChange(of: code) { maybeProblem = nil }
            FieldMessage(maybeError: maybeProblem?.message)
          }
          Button("approve_start") { Task { await approveTyped() } }
            .disabled(isWorking)
        }
      case .typeCode:
        Section {
          LinkTypedCodeForm(
            onApproved: { step = .done },
            onEnded: { restart(with: "link_attempt_ended") },
            onCancel: {
              Task { await model.cancelLinkApproval() }
              restart(with: nil)
            })
        }
      case .done:
        Section { Label("approve_done", systemImage: "checkmark.circle") }
      }
    }
    .navigationTitle("approve_title")
    .navigationBarTitleDisplayMode(.inline)
    .sheet(isPresented: $isScanning) {
      ScanLinkCodeView { scanned in
        isScanning = false
        code = scanned
        Task { await approve(scanned) }
      }
    }
    .onDisappear {
      // Leaving the typed-code step: nothing is sent, the attempt is dropped.
      if step == .typeCode { Task { await model.cancelLinkApproval() } }
    }
  }

  /// A typed code: an empty field is said at the field, not sent.
  private func approveTyped() async {
    if let problem = TextProblem.required(code, missing: "approve_code_missing") {
      showAtField(problem)
      return
    }
    await approve(code)
  }

  /// Joins the new device whose code was scanned or typed; the typed-code step follows. The
  /// server's refusal of the code (expired, for the other device) goes to the code field.
  private func approve(_ text: String) async {
    guard !isWorking else { return }
    isWorking = true
    maybeNotice = nil
    maybeProblem = nil
    do {
      try await model.approveLink(code: text)
      step = .typeCode
    } catch {
      showAtField(.refused(AppFailure(CoreFailure(error))))
    }
    isWorking = false
  }

  private func showAtField(_ problem: TextProblem) {
    maybeProblem = problem
    isCodeFocused = true
    isCodeAccessibilityFocused = true
  }

  private func restart(with maybeMessage: LocalizedStringKey?) {
    code = ""
    maybeProblem = nil
    maybeNotice = maybeMessage
    step = .enterCode
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
  @State private var maybeKeywordProblem: TextProblem?
  @FocusState private var isKeywordFocused: Bool
  @AccessibilityFocusState private var isKeywordAccessibilityFocused: Bool

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
        VStack(alignment: .leading, spacing: 6) {
          HStack {
            TextField("rules_keyword_hint", text: $newKeyword)
              .textInputAutocapitalization(.never)
              .autocorrectionDisabled()
              .focused($isKeywordFocused)
              .accessibilityFocused($isKeywordAccessibilityFocused)
              .accessibilityLabel(Text("rules_keyword_label"))
              .fieldError(maybeKeywordProblem?.message)
              .onSubmit(addKeyword)
              .onChange(of: newKeyword) { maybeKeywordProblem = nil }
            Button("rules_keyword_add", action: addKeyword)
          }
          FieldMessage(maybeError: maybeKeywordProblem?.message)
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

  /// Adds the typed keyword; an empty one or one already in the list is said at the field.
  private func addKeyword() {
    if let problem = keywordProblem(newKeyword, existing: rules.keywords) {
      maybeKeywordProblem = problem
      isKeywordFocused = true
      isKeywordAccessibilityFocused = true
      return
    }
    rules.keywords.append(newKeyword.trimmingCharacters(in: .whitespacesAndNewlines))
    newKeyword = ""
  }
}
