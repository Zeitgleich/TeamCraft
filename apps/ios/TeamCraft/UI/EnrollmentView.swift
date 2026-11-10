import SwiftUI
import UniformTypeIdentifiers

/// First run on this device, as a guided sequence (issue #1701): a new identity protected by a
/// recovery kit (explain, save, check, keep it safe; e2ee.md §6.5.4), or, when the account already
/// has keys, a choice of how to bring them here: linking from another device first (quickest,
/// history comes along), then the recovery kit or the organization's admins.
struct EnrollmentView: View {
  let hasExistingIdentity: Bool
  let failure: AppFailure?

  var body: some View {
    if hasExistingIdentity {
      RestoreFlowView(failure: failure)
    } else {
      NewKitFlowView(failure: failure)
    }
  }
}

/// A recovery key's groups and their length (teamcraft-crypto's `RECOVERY_KEY_GROUPS`, e2ee.md §6.5.1).
private let recoveryKeyGroups = 8
private let recoveryKeyGroupLength = 4
/// A kit is a few kilobytes; a bigger file isn't read.
private let maxKitFileBytes = 1024 * 1024

/// The first device of an account, one thing per step. The key lives in the core's draft, never in
/// saved state: after the app was closed a new draft is a new key, so the flow resumes at saving the
/// kit and says that a kit saved before doesn't work.
private struct NewKitFlowView: View {
  @Environment(AppModel.self) private var model
  let failure: AppFailure?

  private enum Step: Int, Comparable {
    case intro, save, check, finish
    static func < (lhs: Step, rhs: Step) -> Bool { lhs.rawValue < rhs.rawValue }
  }
  private static let steps = 4

  @SceneStorage("onboarding.newKitStep") private var storedStep = Step.intro.rawValue
  @SceneStorage("onboarding.keyShown") private var wasKeyShown = false
  @State private var kit = RecoveryKitDraft()
  /// The key was shown before this draft existed: the flow was restarted.
  @State private var wasRestarted = false
  @State private var hasCheckedRestart = false
  @State private var isKeyConfirmed = false

  private var step: Step {
    let stored = Step(rawValue: storedStep) ?? .intro
    if stored > .save, !isKeyConfirmed, wasRestarted { return .save }
    if stored == .finish, !isKeyConfirmed { return .check }
    return stored
  }

  var body: some View {
    Group {
      switch step {
      case .intro: intro
      case .save:
        SaveKitStep(kit: kit, wasRestarted: wasRestarted, onBack: { go(.intro) }, onContinue: { go(.check) })
      case .check:
        CheckKitStep(kit: kit, onBack: { go(.save) }) {
          isKeyConfirmed = true
          go(.finish)
        }
      case .finish: finish
      }
    }
    .onAppear {
      guard !hasCheckedRestart else { return }
      hasCheckedRestart = true
      wasRestarted = wasKeyShown
    }
    .onChange(of: step) { _, step in
      if step == .save { wasKeyShown = true }
    }
  }

  private var intro: some View {
    StepPage(step: 1, steps: Self.steps, title: "onboarding_intro_title", lead: "onboarding_intro_lead") {
      StepPoint(systemImage: "lock.fill", title: "onboarding_intro_locked", text: "onboarding_intro_locked_text")
      StepPoint(systemImage: "key.fill", title: "onboarding_intro_kit", text: "onboarding_intro_kit_text")
      StepPoint(systemImage: "checkmark.shield.fill", title: "onboarding_intro_keep", text: "onboarding_intro_keep_text")
    } actions: {
      Button("onboarding_start") { go(.save) }
        .buttonStyle(PrimaryButtonStyle())
      SignOutButton()
    }
  }

  private var finish: some View {
    StepPage(step: 4, steps: Self.steps, title: "onboarding_finish_title", lead: "onboarding_finish_lead") {
      StepPoint(systemImage: "doc.text.fill", title: "onboarding_finish_where", text: "onboarding_finish_where_text")
      StepPoint(systemImage: "lock.fill", title: "onboarding_finish_share", text: "onboarding_finish_share_text")
      StepPoint(systemImage: "checkmark.shield.fill", title: "onboarding_finish_ask", text: "onboarding_finish_ask_text")
      // The device's or the server's failure belongs to no field: it sits above the buttons.
      if let failure, !model.isBusy { Text(failure.message).foregroundStyle(Theme.danger) }
    } actions: {
      Button { Task { await model.enroll(recoveryKit: kit) } } label: { BusyLabel(title: "onboarding_finish") }
        .buttonStyle(PrimaryButtonStyle())
        .disabled(model.isBusy)
      Button("onboarding_back") { go(.check) }
        .disabled(model.isBusy)
    }
  }

  private func go(_ to: Step) {
    storedStep = to.rawValue
  }
}

/// Step 2: the key, and saving the kit file with the system's file picker; once saved, the main
/// action moves on.
struct SaveKitStep: View {
  @Environment(AppModel.self) private var model
  let kit: RecoveryKitDraft
  let wasRestarted: Bool
  let onBack: () -> Void
  let onContinue: () -> Void

  /// `nil`: not tried yet; the outcome of the last save otherwise.
  @State private var maybeSaved: Bool?
  @State private var isExporting = false
  @State private var maybeDocument: KitDocument?

  var body: some View {
    StepPage(step: 2, steps: 4, title: "onboarding_save_title", lead: "onboarding_save_lead") {
      if wasRestarted {
        Text("onboarding_save_new_key").foregroundStyle(Theme.danger)
      }
      KitKeyCard(groups: kit.groups())
      switch maybeSaved {
      case .some(true):
        HStack {
          Text("kit_saved").frame(maxWidth: .infinity, alignment: .leading)
          Button("kit_save_again") { save() }
        }
      case .some(false):
        Text("kit_save_failed").foregroundStyle(Theme.danger)
      case .none:
        Text("kit_save_hint").foregroundStyle(Theme.textMuted)
      }
    } actions: {
      if maybeSaved == true {
        Button("onboarding_continue", action: onContinue)
          .buttonStyle(PrimaryButtonStyle())
      } else {
        Button { save() } label: { Label("kit_save", systemImage: "square.and.arrow.down") }
          .buttonStyle(PrimaryButtonStyle())
        Button("kit_wrote_down", action: onContinue)
      }
      Button("onboarding_back", action: onBack)
    }
    .fileExporter(
      isPresented: $isExporting, document: maybeDocument, contentType: .html,
      defaultFilename: recoveryKitFileName(serverHost: model.maybeServer?.address.host ?? "")
    ) { result in
      switch result {
      case .success: maybeSaved = true
      case .failure(let error):
        AppLog.failure("saving the recovery kit", error)
        maybeSaved = false
      }
      maybeDocument = nil
    }
  }

  private func save() {
    maybeDocument = KitDocument(html: kit.kitHtml(texts: recoveryKitTexts(serverHost: model.maybeServer?.address.host ?? "")))
    isExporting = true
  }
}

/// Step 3: one group of the key (chosen at random, the key is hidden here) or the saved file. A
/// refused check says what to do at the field and focuses it (forms.md).
struct CheckKitStep: View {
  let kit: RecoveryKitDraft
  let onBack: () -> Void
  let onConfirmed: () -> Void

  /// Not a secret: which group the user types back, so they look at their saved kit.
  @State private var groupIndex = Int.random(in: 0..<recoveryKeyGroups)
  @State private var typed = ""
  @State private var hasTried = false
  @State private var maybeFileCheck: KitFileCheck?
  @State private var isImporting = false
  @FocusState private var isFieldFocused: Bool
  @AccessibilityFocusState private var isFieldAccessibilityFocused: Bool

  private var isGroupRight: Bool { kit.isGroup(index: UInt32(groupIndex), typed: typed) }
  private var isConfirmed: Bool { isGroupRight || maybeFileCheck == .matches }
  private var maybeFieldError: LocalizedStringKey? {
    if isConfirmed { return nil }
    if typed.trimmingCharacters(in: .whitespaces).count >= recoveryKeyGroupLength { return "kit_check_wrong \(groupIndex + 1)" }
    if hasTried { return "kit_check_missing \(groupIndex + 1)" }
    return nil
  }

  var body: some View {
    StepPage(step: 3, steps: 4, title: "onboarding_check_title", lead: "onboarding_check_lead") {
      VStack(alignment: .leading, spacing: 6) {
        Text("kit_check_group \(groupIndex + 1)").font(.app(.subheadline).bold())
        TextField("", text: $typed)
          .font(.system(.title2, design: .monospaced))
          .textInputAutocapitalization(.characters)
          .autocorrectionDisabled()
          .submitLabel(.done)
          .onSubmit { submit() }
          .focused($isFieldFocused)
          .accessibilityFocused($isFieldAccessibilityFocused)
          .accessibilityLabel(Text("kit_check_group \(groupIndex + 1)"))
          .fieldError(maybeFieldError)
          .padding(.horizontal, 8)
          .padding(.vertical, 10)
          .fieldEdge(isError: maybeFieldError != nil, cornerRadius: 6)
        FieldMessage(maybeError: maybeFieldError)
        if isGroupRight { Text("kit_check_group_ok").font(.app(.footnote)).foregroundStyle(Theme.textMuted) }
      }
      Button { isImporting = true } label: { Label("kit_open", systemImage: "doc") }
        .buttonStyle(.bordered)
      switch maybeFileCheck {
      case .some(.matches): Text("kit_check_file_ok")
      case .some(.otherKey): Text("kit_check_file_other").foregroundStyle(Theme.danger)
      case .some(.noKey): Text("kit_file_no_key").foregroundStyle(Theme.danger)
      case .none: EmptyView()
      }
    } actions: {
      Button("onboarding_continue") { submit() }
        .buttonStyle(PrimaryButtonStyle())
      Button("kit_check_show_again", action: onBack)
    }
    .fileImporter(isPresented: $isImporting, allowedContentTypes: [.html, .plainText]) { result in
      maybeFileCheck = readKitText(result).map { kit.checkFile(text: $0) } ?? .noKey
    }
  }

  private func submit() {
    hasTried = true
    if isConfirmed {
      onConfirmed()
    } else {
      isFieldFocused = true
      isFieldAccessibilityFocused = true
    }
  }
}

/// An account with keys: choose how to bring them here, then the recovery kit (linking and the
/// admins continue on their own screens, step 2 there too).
private struct RestoreFlowView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  let failure: AppFailure?

  private enum Step: Int { case choose, kit, it }
  private static let steps = 2

  @SceneStorage("onboarding.restoreStep") private var storedStep = Step.choose.rawValue
  @State private var secret = ""
  @State private var hasTried = false
  /// A refused key is about the one typed; once the user edits it, it no longer applies.
  @State private var isFailureEdited = false
  @State private var isImporting = false
  @State private var maybeFileNote: LocalizedStringKey?
  @FocusState private var maybeFocus: NewPassphraseField?
  @AccessibilityFocusState private var maybeAccessibilityFocus: NewPassphraseField?

  private var step: Step {
    // A refused key brings the user back to its field.
    failure == .wrongPassphrase ? .kit : (Step(rawValue: storedStep) ?? .choose)
  }

  var body: some View {
    switch step {
    case .choose: choose
    case .kit: unlock
    case .it: askTheItDepartment
    }
  }

  private var choose: some View {
    StepPage(step: 1, steps: Self.steps, title: "onboarding_unlock_title", lead: "onboarding_unlock_lead") {
      VStack(spacing: 8) {
        ChoiceRow(
          systemImage: "laptopcomputer.and.iphone", title: "onboarding_way_link", text: "onboarding_way_link_text",
          isRecommended: true
        ) { model.startLinking() }
        ChoiceRow(systemImage: "key.fill", title: "onboarding_way_kit", text: "onboarding_way_kit_text") {
          storedStep = Step.kit.rawValue
        }
        ChoiceRow(systemImage: "person.2.fill", title: "onboarding_way_admins", text: "onboarding_way_admins_text") {
          storedStep = Step.it.rawValue
        }
      }
      .disabled(model.isBusy)
      // A refused key belongs to the kit step, "recovery not set up" to the IT step.
      if let failure, failure != .wrongPassphrase, failure != .orgRecoveryUnavailable, !model.isBusy {
        Text(failure.message).foregroundStyle(Theme.danger)
      }
    } actions: {
      SignOutButton()
    }
  }

  private var unlock: some View {
    ScrollViewReader { proxy in
      StepPage(step: 2, steps: Self.steps, title: "onboarding_kit_title", lead: "onboarding_kit_lead") {
        Button { isImporting = true } label: { Label("kit_open", systemImage: "doc") }
          .buttonStyle(.bordered)
          .disabled(model.isBusy)
        if let maybeFileNote { Text(maybeFileNote).font(.app(.footnote)) }
        PassphraseField(
          title: "kit_secret_label", text: $secret, contentType: .password, field: .passphrase,
          maybeError: shownError, focus: $maybeFocus, accessibilityFocus: $maybeAccessibilityFocus
        ) { submit(proxy) }
        .onChange(of: secret) { isFailureEdited = true }
        Text("kit_secret_hint").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
        if let failure, failure != .wrongPassphrase, !model.isBusy {
          Text(failure.message).foregroundStyle(Theme.danger)
        }
      } actions: {
        Button { submit(proxy) } label: { BusyLabel(title: "onboarding_unlock") }
          .buttonStyle(PrimaryButtonStyle())
          .disabled(model.isBusy)
        Button("onboarding_back") { storedStep = Step.choose.rawValue }
          .disabled(model.isBusy)
      }
      // The core refused the key: the field takes the focus, so it can be retyped.
      .onChange(of: model.isBusy) { _, isBusy in
        if !isBusy, failure == .wrongPassphrase { focus(proxy) }
      }
      .fileImporter(isPresented: $isImporting, allowedContentTypes: [.html, .plainText]) { result in
        if let key = readKitText(result).flatMap({ recoveryKeyInFile(text: $0) }) {
          secret = key
          maybeFileNote = "kit_read"
        } else {
          maybeFileNote = "kit_file_no_key"
        }
      }
    }
  }

  /// No other device and no kit: what to tell the IT department. Not a self-service: the IT
  /// department resets the account, or (if the organization set up recovery) asks for the code this
  /// device shows (e2ee.md §6.3.1); without recovery set up, asking for the code says so.
  private var askTheItDepartment: some View {
    StepPage(step: 2, steps: Self.steps, title: "onboarding_it_title", lead: "onboarding_it_lead") {
      Text("onboarding_it_code").foregroundStyle(Theme.textMuted)
      Button("onboarding_it_show_code") { Task { await model.startRecovery() } }
        .buttonStyle(.bordered)
        .disabled(model.isBusy)
      if let failure, failure != .wrongPassphrase, !model.isBusy {
        Text(failure.message).foregroundStyle(Theme.danger)
      }
    } actions: {
      Button("onboarding_back") { storedStep = Step.choose.rawValue }
        .disabled(model.isBusy)
    }
  }

  /// Nothing given once tried, else the core's refusal until edited.
  private var shownError: LocalizedStringKey? {
    if hasTried, secret.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "kit_secret_missing" }
    guard failure == .wrongPassphrase, !isFailureEdited, !model.isBusy else { return nil }
    return PassphraseProblem.wrong.message
  }

  private func submit(_ proxy: ScrollViewProxy) {
    guard !model.isBusy else { return }
    hasTried = true
    let trimmed = secret.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.isEmpty {
      focus(proxy)
      return
    }
    isFailureEdited = false
    // A legacy passphrase works in the same field: the core tries a text that reads as a recovery
    // key as one (e2ee.md §6.5.5).
    Task { await model.enroll(passphrase: trimmed) }
  }

  private func focus(_ proxy: ScrollViewProxy) {
    maybeFocus = .passphrase
    maybeAccessibilityFocus = .passphrase
    scrollToMiddle(NewPassphraseField.passphrase, in: proxy, reduceMotion: reduceMotion)
  }
}

/// The key in large monospace groups that wrap as whole groups; VoiceOver reads it group by group.
private struct KitKeyCard: View {
  let groups: [String]

  var body: some View {
    // Not selectable: the key stays out of clipboard histories (save the kit or write it down).
    LazyVGrid(columns: [GridItem(.adaptive(minimum: 72), spacing: 12, alignment: .leading)], alignment: .leading, spacing: 8) {
      ForEach(Array(groups.enumerated()), id: \.offset) { _, group in
        Text(verbatim: group).font(.system(.title2, design: .monospaced).weight(.semibold))
      }
    }
    .padding(16)
    .frame(maxWidth: .infinity, alignment: .leading)
    .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radius))
    .accessibilityElement(children: .ignore)
    .accessibilityLabel(
      Text("kit_key_label") + Text(verbatim: ": " + groups.map { $0.map(String.init).joined(separator: " ") }.joined(separator: ", ")))
  }
}

/// The kit page as a document for the system's "Save to Files" sheet.
private struct KitDocument: FileDocument {
  static let readableContentTypes: [UTType] = [.html]
  let html: String

  init(html: String) {
    self.html = html
  }

  init(configuration: ReadConfiguration) throws {
    html = String(decoding: configuration.file.regularFileContents ?? Data(), as: UTF8.self)
  }

  func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
    FileWrapper(regularFileWithContents: Data(html.utf8))
  }
}

/// The text of a file picked in the system's file picker (a saved kit); `nil` when it can't be
/// read or is too big to be a kit.
private func readKitText(_ result: Result<URL, Error>) -> String? {
  guard case .success(let url) = result else { return nil }
  let isScoped = url.startAccessingSecurityScopedResource()
  defer { if isScoped { url.stopAccessingSecurityScopedResource() } }
  guard let data = try? Data(contentsOf: url), data.count <= maxKitFileBytes else { return nil }
  return String(decoding: data, as: UTF8.self)
}

/// The kit page's words in the app's language (the same as the web's kit, e2ee.md §6.5.3).
private func recoveryKitTexts(serverHost: String) -> RecoveryKitTexts {
  let date = Date().formatted(date: .long, time: .omitted)
  return RecoveryKitTexts(
    language: Locale.current.language.languageCode?.identifier ?? "en",
    title: String(localized: "kit_file_title"),
    accountLine: String(localized: "kit_file_account \(serverHost) \(date)"),
    intro: String(localized: "kit_file_intro"),
    keyLabel: String(localized: "kit_key_label"),
    keepTitle: String(localized: "kit_file_keep_title"),
    keep: [
      String(localized: "kit_file_keep_print"),
      String(localized: "kit_file_keep_share"),
      String(localized: "kit_file_keep_anyone"),
    ],
    useTitle: String(localized: "kit_file_use_title"),
    useText: String(localized: "kit_file_use"),
    lostTitle: String(localized: "kit_file_lost_title"),
    lostText: String(localized: "kit_file_lost"),
    neverAsked: String(localized: "kit_file_never")
  )
}

/// One step of the guided first run: the step indicator, the heading and a plain lead on top, the
/// content scrolling below, and the actions pinned at the bottom (the main one full width, the
/// other one plain below it), as Apple's own setup screens do.
struct StepPage<Content: View, Actions: View>: View {
  let step: Int
  let steps: Int
  let title: LocalizedStringKey
  let lead: LocalizedStringKey?
  @ViewBuilder let content: Content
  @ViewBuilder let actions: Actions

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: 20) {
        StepProgress(step: step, steps: steps)
        Text(title).font(.app(.title).bold()).accessibilityAddTraits(.isHeader)
        if let lead { Text(lead).foregroundStyle(Theme.textMuted) }
        content
      }
      .padding(24)
      .frame(maxWidth: 560, alignment: .leading)
      .frame(maxWidth: .infinity)
    }
    .safeAreaInset(edge: .bottom) {
      VStack(spacing: 12) { actions }
        .padding(.horizontal, 24)
        .padding(.vertical, 12)
        .frame(maxWidth: 560)
        .frame(maxWidth: .infinity)
        .background(.bar)
    }
  }
}

/// "Step 2 of 3" under a segmented bar; VoiceOver reads the text, the bar is decoration.
struct StepProgress: View {
  let step: Int
  let steps: Int

  var body: some View {
    VStack(alignment: .leading, spacing: 6) {
      HStack(spacing: 4) {
        ForEach(1...steps, id: \.self) { index in
          Capsule().fill(index <= step ? Theme.accent : Theme.border).frame(height: 4)
        }
      }
      .frame(maxWidth: 160)
      .accessibilityHidden(true)
      Text("onboarding_step \(step) \(steps)").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
    }
  }
}

/// One explained point of a step: a symbol in a tinted tile, a short title and a plain sentence.
struct StepPoint: View {
  let systemImage: String
  let title: LocalizedStringKey
  let text: LocalizedStringKey

  var body: some View {
    HStack(alignment: .top, spacing: 14) {
      StepSymbol(systemImage: systemImage)
      VStack(alignment: .leading, spacing: 2) {
        Text(title).font(.app(.headline))
        Text(text).foregroundStyle(Theme.textMuted)
      }
    }
    .accessibilityElement(children: .combine)
  }
}

/// A way to choose, as a full-width row: symbol, title, one line of why, and a chevron.
private struct ChoiceRow: View {
  let systemImage: String
  let title: LocalizedStringKey
  let text: LocalizedStringKey
  var isRecommended = false
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      HStack(spacing: 14) {
        StepSymbol(systemImage: systemImage)
        VStack(alignment: .leading, spacing: 2) {
          if isRecommended {
            Text("onboarding_recommended").font(.app(.caption).bold()).foregroundStyle(Theme.accent)
          }
          Text(title).font(.app(.headline)).foregroundStyle(Theme.text)
          Text(text).font(.app(.subheadline)).foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        Image(systemName: "chevron.right").foregroundStyle(Theme.textMuted).accessibilityHidden(true)
      }
      .padding(14)
      .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radius))
      .overlay(
        RoundedRectangle(cornerRadius: Theme.radius)
          .stroke(isRecommended ? Theme.accent.opacity(0.5) : Theme.border, lineWidth: 1)
      )
      .contentShape(RoundedRectangle(cornerRadius: Theme.radius))
    }
    .buttonStyle(.plain)
  }
}

private struct StepSymbol: View {
  let systemImage: String

  var body: some View {
    Image(systemName: systemImage)
      .font(.app(.body))
      .foregroundStyle(Theme.accent)
      .frame(width: 36, height: 36)
      .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: 10))
      .accessibilityHidden(true)
  }
}

/// The way out of the setup: sign out (another account, or later).
private struct SignOutButton: View {
  @Environment(AppModel.self) private var model

  var body: some View {
    Button("sign_out", role: .destructive) { Task { await model.signOut() } }
      .disabled(model.isBusy)
  }
}

/// While the key derivation runs (a few seconds) the main button says so.
struct BusyLabel: View {
  @Environment(AppModel.self) private var model
  let title: LocalizedStringKey

  var body: some View {
    if model.isBusy {
      HStack(spacing: 8) {
        ProgressView()
        Text("enroll_working")
      }
    } else {
      Text(title)
    }
  }
}

/// A passphrase field with a show/hide button, its error below it (forms.md). The last field of a
/// form submits on Return.
struct PassphraseField<Field: Hashable>: View {
  let title: LocalizedStringKey
  @Binding var text: String
  let contentType: UITextContentType
  let field: Field
  let maybeError: LocalizedStringKey?
  let focus: FocusState<Field?>.Binding
  let accessibilityFocus: AccessibilityFocusState<Field?>.Binding
  let maybeOnSubmit: (() -> Void)?

  @State private var isVisible = false

  init(
    title: LocalizedStringKey, text: Binding<String>, contentType: UITextContentType, field: Field,
    maybeError: LocalizedStringKey?, focus: FocusState<Field?>.Binding,
    accessibilityFocus: AccessibilityFocusState<Field?>.Binding, onSubmit: (() -> Void)? = nil
  ) {
    self.title = title
    _text = text
    self.contentType = contentType
    self.field = field
    self.maybeError = maybeError
    self.focus = focus
    self.accessibilityFocus = accessibilityFocus
    maybeOnSubmit = onSubmit
  }

  var body: some View {
    VStack(alignment: .leading, spacing: 6) {
      HStack {
        Group {
          if isVisible { TextField(title, text: $text) } else { SecureField(title, text: $text) }
        }
        .textContentType(contentType)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .submitLabel(maybeOnSubmit == nil ? .next : .done)
        .focused(focus, equals: field)
        .accessibilityFocused(accessibilityFocus, equals: field)
        .fieldError(maybeError)
        .onSubmit { maybeOnSubmit?() }
        Button {
          isVisible.toggle()
        } label: {
          Image(systemName: isVisible ? "eye.slash" : "eye")
            .frame(minWidth: 44, minHeight: 44)
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.textMuted)
        .accessibilityLabel(Text(isVisible ? "passphrase_hide" : "passphrase_show"))
      }
      .padding(.leading, 8)
      .fieldEdge(isError: maybeError != nil, cornerRadius: 6)
      FieldMessage(maybeError: maybeError)
    }
    .id(field)
  }
}

/// New phone: no QR code, since nobody could scan it (issue #998). The user approves this phone from
/// a prompt on a signed-in device, or scans the code a computer shows under "Link a new device";
/// either way the other device then asks for the number shown here (e2ee.md §5).
struct LinkThisDeviceView: View {
  @Environment(AppModel.self) private var model
  let progress: LinkProgress

  /// Not kept across attempts: asking hands the request's key to the server and is a fresh choice.
  @State private var askState = AskState.idle
  @State private var isScanning = false

  private enum AskState { case idle, asking, asked, failed }

  var body: some View {
    StepPage(
      step: 2, steps: 2, title: "link_new_title",
      lead: progress.maybeShortCode == nil ? "link_phone_intro" : "link_compare"
    ) {
      if let shortCode = progress.maybeShortCode {
        // Connected: only the comparison matters now.
        Text(shortCode)
          .font(.system(.largeTitle, design: .monospaced).bold())
          .accessibilityLabel(Text(verbatim: shortCode.map(String.init).joined(separator: " ")))
        Text("link_confirm_on_other").foregroundStyle(Theme.textMuted)
        ProgressView()
      } else {
        if let notice = progress.maybeNotice {
          Text(notice).font(.app(.footnote)).foregroundStyle(Theme.danger)
        }
        if askState == .asked {
          Text("link_asked").foregroundStyle(Theme.textMuted)
          ProgressView("link_waiting")
        } else {
          Text("link_ask_hint").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
          if askState == .failed {
            Text("link_ask_failed").font(.app(.footnote)).foregroundStyle(Theme.danger)
          }
        }
        Divider()
        Text("link_scan_code_hint").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
        Button("link_scan_code", systemImage: "qrcode.viewfinder") { isScanning = true }
          .buttonStyle(.bordered)
        Divider()
        Text("link_no_other_device").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
      }
    } actions: {
      // The prompt on the other devices needs the request's key at the server; scanning does not.
      if progress.maybeShortCode == nil, askState != .asked {
        Button("link_ask") {
          askState = .asking
          Task { askState = await model.askOtherDevices() ? .asked : .failed }
        }
        .buttonStyle(PrimaryButtonStyle())
        .disabled(askState == .asking)
      }
      Button("onboarding_back") { model.cancelLinking() }
    }
    .sheet(isPresented: $isScanning) {
      // Without a camera the prompt above is the way: it needs none.
      ScanLinkCodeView { scanned in
        isScanning = false
        model.linkScannedCode(scanned)
      }
    }
  }
}

/// New device, organization recovery (e2ee.md §6.3.1), one thing per step: read the code to an
/// admin, save and check a new recovery kit (the admins saw the old backup key, so the account
/// moves to a new one protected by this kit), then finish once two admins approved. Finishing too
/// early says so and keeps the user on the step.
struct OrgRecoveryView: View {
  @Environment(AppModel.self) private var model
  let progress: RecoveryProgress
  let failure: AppFailure?

  private enum Step: Int, Comparable {
    case code, save, check, finish
    static func < (lhs: Step, rhs: Step) -> Bool { lhs.rawValue < rhs.rawValue }
  }

  @State private var storedStep = Step.code
  @State private var kit = RecoveryKitDraft()
  @State private var isKeyConfirmed = false

  /// The kit's key lives only in this draft: without a confirmed one the flow is at the kit again.
  private var step: Step { storedStep > .save && !isKeyConfirmed ? .save : storedStep }

  var body: some View {
    switch step {
    case .code: code
    case .save:
      SaveKitStep(kit: kit, wasRestarted: false, onBack: { storedStep = .code }, onContinue: { storedStep = .check })
    case .check:
      CheckKitStep(kit: kit, onBack: { storedStep = .save }) {
        isKeyConfirmed = true
        storedStep = .finish
      }
    case .finish: finish
    }
  }

  private var code: some View {
    StepPage(step: 1, steps: 4, title: "recovery_title", lead: "recovery_hint") {
      // Read aloud to an admin.
      Text(progress.verificationCode)
        .font(.system(.title, design: .monospaced).bold())
        .frame(maxWidth: .infinity)
        .padding()
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radius))
        .accessibilityLabel(Text(verbatim: progress.verificationCode.map(String.init).joined(separator: " ")))
      Text("org_recovery_next").foregroundStyle(Theme.textMuted)
    } actions: {
      Button("onboarding_continue") { storedStep = .save }
        .buttonStyle(PrimaryButtonStyle())
      Button("cancel") { model.cancelRecovery() }
    }
  }

  private var finish: some View {
    StepPage(step: 4, steps: 4, title: "org_recovery_finish_title", lead: "org_recovery_finish_lead") {
      StepPoint(systemImage: "doc.text.fill", title: "onboarding_finish_where", text: "onboarding_finish_where_text")
      StepPoint(systemImage: "lock.fill", title: "onboarding_finish_share", text: "onboarding_finish_share_text")
      // "Not approved yet" and the device's or the server's failures: above the buttons.
      if let failure, !model.isBusy { Text(failure.message).foregroundStyle(Theme.danger) }
    } actions: {
      Button { Task { await model.finishRecovery(recoveryKit: kit) } } label: { BusyLabel(title: "recovery_finish") }
        .buttonStyle(PrimaryButtonStyle())
        .disabled(model.isBusy)
      Button("onboarding_back") { storedStep = .check }
        .disabled(model.isBusy)
    }
  }
}
