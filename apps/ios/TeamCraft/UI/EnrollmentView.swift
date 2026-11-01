import SwiftUI

/// First run on this device: a new identity with a recovery passphrase, or, when the account
/// already has keys, linking from another device first (quickest, history comes along), then the
/// passphrase or the organization's admins.
struct EnrollmentView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  let hasExistingIdentity: Bool
  let failure: AppFailure?

  @State private var passphrase = ""
  @State private var repeated = ""
  /// A submit was refused or sent: from then on the fields' errors show and follow the input.
  @State private var hasTried = false
  /// A wrong passphrase is about the one typed; once the user edits it, it no longer applies.
  @State private var isFailureEdited = false
  @FocusState private var maybeFocus: NewPassphraseField?
  @AccessibilityFocusState private var maybeAccessibilityFocus: NewPassphraseField?

  var body: some View {
    ScrollViewReader { proxy in
      ScrollView {
        VStack(alignment: .leading, spacing: Theme.spacing) {
          Text("enroll_title").font(.title.bold()).accessibilityAddTraits(.isHeader)
          if hasExistingIdentity { restore(proxy) } else { newKeys(proxy) }
          Button("sign_out", role: .destructive) { Task { await model.signOut() } }
            .padding(.top, 16)
        }
        .padding(24)
      }
      // The core refused the passphrase: the field takes the focus, so it can be retyped.
      .onChange(of: model.isBusy) { _, isBusy in
        if !isBusy, failure == .wrongPassphrase { focus(.passphrase, proxy: proxy) }
      }
    }
  }

  @ViewBuilder
  private func newKeys(_ proxy: ScrollViewProxy) -> some View {
    let errors = shownNewErrors
    Text("enroll_new_hint").foregroundStyle(Theme.textMuted)
    PassphraseField(
      title: "passphrase", text: $passphrase, contentType: .newPassword, field: .passphrase,
      maybeError: errors[.passphrase]?.message, focus: $maybeFocus,
      accessibilityFocus: $maybeAccessibilityFocus)
    PassphraseField(
      title: "passphrase_repeat", text: $repeated, contentType: .newPassword, field: .repeated,
      maybeError: errors[.repeated]?.message, focus: $maybeFocus,
      accessibilityFocus: $maybeAccessibilityFocus
    ) { submit(proxy) }
    Text("passphrase_rule \(PassphraseRules.minimumLength)").font(.footnote).foregroundStyle(Theme.textMuted)
    // The device's or the server's failure belongs to no field: it sits next to the button.
    if let failure, !model.isBusy { Text(failure.message).foregroundStyle(Theme.danger) }
    Button { submit(proxy) } label: { busyLabel("enroll_button") }
      .buttonStyle(PrimaryButtonStyle())
      .disabled(model.isBusy)
  }

  @ViewBuilder
  private func restore(_ proxy: ScrollViewProxy) -> some View {
    let maybeError = shownRestoreError
    Text("restore_link_hint").foregroundStyle(Theme.textMuted)
    Button("link_instead") { model.startLinking() }
      .buttonStyle(PrimaryButtonStyle())
      .disabled(model.isBusy)
    Divider().padding(.vertical, 8)
    Text("restore_passphrase_hint").foregroundStyle(Theme.textMuted)
    PassphraseField(
      title: "passphrase", text: $passphrase, contentType: .password, field: .passphrase,
      maybeError: maybeError?.message, focus: $maybeFocus, accessibilityFocus: $maybeAccessibilityFocus
    ) { submit(proxy) }
    .onChange(of: passphrase) { isFailureEdited = true }
    if let failure, failure != .wrongPassphrase, !model.isBusy {
      Text(failure.message).foregroundStyle(Theme.danger)
    }
    Button { submit(proxy) } label: { busyLabel("recover_button") }
      .buttonStyle(.bordered)
      .frame(maxWidth: .infinity)
      .disabled(model.isBusy)
    Button("recover_with_admins") { Task { await model.startRecovery() } }
      .disabled(model.isBusy)
  }

  /// While the key derivation runs (a few seconds) the button says so.
  @ViewBuilder
  private func busyLabel(_ title: LocalizedStringKey) -> some View {
    if model.isBusy {
      HStack(spacing: 8) {
        ProgressView()
        Text("enroll_working")
      }
    } else {
      Text(title)
    }
  }

  private var shownNewErrors: FieldErrors<NewPassphraseField, PassphraseProblem> {
    hasTried ? PassphraseRules.errors(passphrase: passphrase, repeated: repeated) : FieldErrors()
  }

  /// The passphrase's own problem once tried, else the core's "wrong passphrase" until edited.
  private var shownRestoreError: PassphraseProblem? {
    if hasTried, let problem = PassphraseRules.problem(passphrase: passphrase) { return problem }
    guard failure == .wrongPassphrase, !isFailureEdited, !model.isBusy else { return nil }
    return .wrong
  }

  private func submit(_ proxy: ScrollViewProxy) {
    guard !model.isBusy else { return }
    hasTried = true
    let maybeFirst: NewPassphraseField? =
      hasExistingIdentity
      ? (PassphraseRules.problem(passphrase: passphrase) == nil ? nil : .passphrase)
      : PassphraseRules.errors(passphrase: passphrase, repeated: repeated).maybeFirst
    if let maybeFirst {
      focus(maybeFirst, proxy: proxy)
      return
    }
    isFailureEdited = false
    Task { await model.enroll(passphrase: passphrase) }
  }

  private func focus(_ field: NewPassphraseField, proxy: ScrollViewProxy) {
    maybeFocus = field
    maybeAccessibilityFocus = field
    scrollToMiddle(field, in: proxy, reduceMotion: reduceMotion)
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
    ScrollView {
      VStack(spacing: Theme.spacing) {
        Text("link_new_title").font(.title.bold()).accessibilityAddTraits(.isHeader)
        if let shortCode = progress.maybeShortCode {
          // Connected: only the comparison matters now.
          Text("link_compare").foregroundStyle(Theme.textMuted)
          Text(shortCode)
            .font(.system(.largeTitle, design: .monospaced).bold())
            .accessibilityLabel(Text(verbatim: shortCode.map(String.init).joined(separator: " ")))
          Text("link_confirm_on_other").foregroundStyle(Theme.textMuted)
          ProgressView()
        } else {
          if let notice = progress.maybeNotice {
            Text(notice).font(.footnote).foregroundStyle(Theme.danger).frame(maxWidth: .infinity, alignment: .leading)
          }
          Text("link_phone_intro").foregroundStyle(Theme.textMuted).frame(maxWidth: .infinity, alignment: .leading)
          // The prompt on the other devices needs the request's key at the server; scanning does not.
          if askState == .asked {
            Text("link_asked").foregroundStyle(Theme.textMuted)
            ProgressView("link_waiting")
          } else {
            Button("link_ask") {
              askState = .asking
              Task { askState = await model.askOtherDevices() ? .asked : .failed }
            }
            .buttonStyle(PrimaryButtonStyle())
            .disabled(askState == .asking)
            Text("link_ask_hint").font(.footnote).foregroundStyle(Theme.textMuted)
            if askState == .failed {
              Text("link_ask_failed").font(.footnote).foregroundStyle(Theme.danger)
            }
          }
          Divider()
          Button("link_scan_code", systemImage: "qrcode.viewfinder") { isScanning = true }
            .buttonStyle(.bordered)
          Text("link_scan_code_hint").font(.footnote).foregroundStyle(Theme.textMuted)
          Divider()
          Text("link_no_other_device").font(.footnote).foregroundStyle(Theme.textMuted)
        }
        Button("cancel") { model.cancelLinking() }.padding(.top, 16)
      }
      .padding(24)
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

/// New device, organization recovery: the admins approve the code shown here; then the user sets a
/// new passphrase (e2ee.md §6.3.1).
struct OrgRecoveryView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  let progress: RecoveryProgress
  let failure: AppFailure?

  @State private var passphrase = ""
  @State private var repeated = ""
  @State private var hasTried = false
  @FocusState private var maybeFocus: NewPassphraseField?
  @AccessibilityFocusState private var maybeAccessibilityFocus: NewPassphraseField?

  var body: some View {
    let errors: FieldErrors<NewPassphraseField, PassphraseProblem> =
      hasTried ? PassphraseRules.errors(passphrase: passphrase, repeated: repeated) : FieldErrors()
    ScrollViewReader { proxy in
      ScrollView {
        VStack(alignment: .leading, spacing: Theme.spacing) {
          Text("recovery_title").font(.title.bold()).accessibilityAddTraits(.isHeader)
          Text("recovery_hint").foregroundStyle(Theme.textMuted)
          Text(progress.verificationCode)
            .font(.system(.title, design: .monospaced).bold())
            .frame(maxWidth: .infinity)
            .padding()
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
          Text("recovery_new_passphrase").foregroundStyle(Theme.textMuted)
          PassphraseField(
            title: "passphrase", text: $passphrase, contentType: .newPassword, field: .passphrase,
            maybeError: errors[.passphrase]?.message, focus: $maybeFocus,
            accessibilityFocus: $maybeAccessibilityFocus)
          PassphraseField(
            title: "passphrase_repeat", text: $repeated, contentType: .newPassword, field: .repeated,
            maybeError: errors[.repeated]?.message, focus: $maybeFocus,
            accessibilityFocus: $maybeAccessibilityFocus
          ) { finish(proxy) }
          if let failure, !model.isBusy { Text(failure.message).foregroundStyle(Theme.danger) }
          Button("recovery_finish") { finish(proxy) }
            .buttonStyle(PrimaryButtonStyle())
            .disabled(model.isBusy)
          Button("cancel") { model.cancelRecovery() }
        }
        .padding(24)
      }
    }
  }

  private func finish(_ proxy: ScrollViewProxy) {
    guard !model.isBusy else { return }
    hasTried = true
    if let first = PassphraseRules.errors(passphrase: passphrase, repeated: repeated).maybeFirst {
      maybeFocus = first
      maybeAccessibilityFocus = first
      scrollToMiddle(first, in: proxy, reduceMotion: reduceMotion)
      return
    }
    Task { await model.finishRecovery(newPassphrase: passphrase) }
  }
}
