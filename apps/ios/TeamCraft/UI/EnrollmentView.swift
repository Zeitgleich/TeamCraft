import SwiftUI

/// First run on this device: a new identity with a recovery passphrase, or, when the account
/// already has keys, linking from another device first (quickest, history comes along), then the
/// passphrase or the organization's admins.
struct EnrollmentView: View {
  @Environment(AppModel.self) private var model
  let hasExistingIdentity: Bool
  let failure: AppFailure?

  @State private var passphrase = ""
  @State private var repeated = ""

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: Theme.spacing) {
        Text("enroll_title").font(.title.bold()).accessibilityAddTraits(.isHeader)
        if hasExistingIdentity { restore } else { newKeys }
        Button("sign_out", role: .destructive) { Task { await model.signOut() } }
          .padding(.top, 16)
      }
      .padding(24)
    }
  }

  @ViewBuilder
  private var newKeys: some View {
    Text("enroll_new_hint").foregroundStyle(Theme.textMuted)
    PassphraseField(title: "passphrase", text: $passphrase, contentType: .newPassword)
    PassphraseField(title: "passphrase_repeat", text: $repeated, contentType: .newPassword) { submit() }
    Text("passphrase_rule \(PassphraseRules.minimumLength)").font(.footnote).foregroundStyle(Theme.textMuted)
    if let problemText {
      Text(problemText).font(.footnote).foregroundStyle(Theme.danger)
    }
    if let failure { Text(failure.message).foregroundStyle(Theme.danger) }
    Button { submit() } label: { busyLabel("enroll_button") }
      .buttonStyle(PrimaryButtonStyle())
      .disabled(!isValid || model.isBusy)
  }

  @ViewBuilder
  private var restore: some View {
    Text("restore_link_hint").foregroundStyle(Theme.textMuted)
    Button("link_instead") { Task { await model.startLinking() } }
      .buttonStyle(PrimaryButtonStyle())
      .disabled(model.isBusy)
    Divider().padding(.vertical, 8)
    Text("restore_passphrase_hint").foregroundStyle(Theme.textMuted)
    PassphraseField(title: "passphrase", text: $passphrase, contentType: .password) { submit() }
    if let failure { Text(failure.message).foregroundStyle(Theme.danger) }
    Button { submit() } label: { busyLabel("recover_button") }
      .buttonStyle(.bordered)
      .frame(maxWidth: .infinity)
      .disabled(!isValid || model.isBusy)
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

  private func submit() {
    guard isValid, !model.isBusy else { return }
    Task { await model.enroll(passphrase: passphrase) }
  }

  private var isValid: Bool {
    hasExistingIdentity
      ? !passphrase.isEmpty : PassphraseRules.problem(passphrase: passphrase, repeated: repeated) == nil
  }

  private var problemText: LocalizedStringKey? {
    guard !hasExistingIdentity, !passphrase.isEmpty else { return nil }
    switch PassphraseRules.problem(passphrase: passphrase, repeated: repeated) {
    case .tooShort: return "passphrase_too_short"
    case .mismatch: return repeated.isEmpty ? nil : "passphrases_differ"
    case nil: return nil
    }
  }
}

/// A passphrase field with a show/hide button. The last field of a form submits on Return.
struct PassphraseField: View {
  let title: LocalizedStringKey
  @Binding var text: String
  let contentType: UITextContentType
  let maybeOnSubmit: (() -> Void)?

  @State private var isVisible = false

  init(
    title: LocalizedStringKey, text: Binding<String>, contentType: UITextContentType,
    onSubmit: (() -> Void)? = nil
  ) {
    self.title = title
    _text = text
    self.contentType = contentType
    maybeOnSubmit = onSubmit
  }

  var body: some View {
    HStack {
      Group {
        if isVisible { TextField(title, text: $text) } else { SecureField(title, text: $text) }
      }
      .textContentType(contentType)
      .textInputAutocapitalization(.never)
      .autocorrectionDisabled()
      .submitLabel(maybeOnSubmit == nil ? .next : .done)
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
    .overlay(RoundedRectangle(cornerRadius: 6).stroke(Theme.controlBorder))
  }
}

/// New device: shows the code (and a QR code) another device of the user approves, then the short
/// code both compare (e2ee.md §5).
struct LinkThisDeviceView: View {
  @Environment(AppModel.self) private var model
  let progress: LinkProgress

  /// Not kept across screens: asking hands the code's key to the server and is a fresh choice.
  @State private var askState = AskState.idle

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
          Text("link_steps_phone").foregroundStyle(Theme.textMuted).frame(maxWidth: .infinity, alignment: .leading)
          QRCodeView(text: progress.code).frame(width: 220, height: 220)
          Text("link_steps_computer").foregroundStyle(Theme.textMuted).frame(maxWidth: .infinity, alignment: .leading)
          Text(progress.code)
            .font(.system(.footnote, design: .monospaced))
            .textSelection(.enabled)
            .padding()
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
          if askState == .asked {
            Text("link_asked").foregroundStyle(Theme.textMuted)
          } else {
            Text("link_ask_hint").font(.footnote).foregroundStyle(Theme.textMuted)
            Button("link_ask") {
              askState = .asking
              Task { askState = await model.askOtherDevices() ? .asked : .failed }
            }
            .buttonStyle(.bordered)
            .disabled(askState == .asking)
            if askState == .failed {
              Text("link_ask_failed").font(.footnote).foregroundStyle(Theme.danger)
            }
          }
          ProgressView("link_waiting")
        }
        Button("cancel") { model.cancelLinking() }.padding(.top, 16)
      }
      .padding(24)
    }
  }
}

/// New device, organization recovery: the admins approve the code shown here; then the user sets a
/// new passphrase (e2ee.md §6.3.1).
struct OrgRecoveryView: View {
  @Environment(AppModel.self) private var model
  let progress: RecoveryProgress
  let failure: AppFailure?

  @State private var passphrase = ""
  @State private var repeated = ""

  var body: some View {
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
        PassphraseField(title: "passphrase", text: $passphrase, contentType: .newPassword)
        PassphraseField(title: "passphrase_repeat", text: $repeated, contentType: .newPassword)
        if let failure { Text(failure.message).foregroundStyle(Theme.danger) }
        Button("recovery_finish") { Task { await model.finishRecovery(newPassphrase: passphrase) } }
          .buttonStyle(PrimaryButtonStyle())
          .disabled(PassphraseRules.problem(passphrase: passphrase, repeated: repeated) != nil || model.isBusy)
        Button("cancel") { model.cancelRecovery() }
      }
      .padding(24)
    }
  }
}
