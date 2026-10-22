import SwiftUI

/// First run on this device: a new identity with a recovery passphrase, or recovery when the
/// account already has keys (passphrase, another device, or the organization's admins).
struct EnrollmentView: View {
  @Environment(AppModel.self) private var model
  let hasExistingIdentity: Bool
  let failure: AppFailure?

  @State private var passphrase = ""
  @State private var repeated = ""

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: Theme.spacing) {
        Text("enroll_title").font(.title.bold())
        Text(hasExistingIdentity ? "enroll_recover_hint" : "enroll_new_hint")
          .foregroundStyle(Theme.textMuted)
        SecureField("passphrase", text: $passphrase)
          .textContentType(hasExistingIdentity ? .password : .newPassword)
          .textFieldStyle(.roundedBorder)
        if !hasExistingIdentity {
          SecureField("passphrase_repeat", text: $repeated)
            .textContentType(.newPassword)
            .textFieldStyle(.roundedBorder)
        }
        if let problemText { Text(problemText).font(.footnote).foregroundStyle(Theme.danger) }
        if let failure { Text(failure.message).foregroundStyle(Theme.danger) }
        Button(hasExistingIdentity ? "recover_button" : "enroll_button") {
          Task { await model.enroll(passphrase: passphrase) }
        }
        .buttonStyle(PrimaryButtonStyle())
        .disabled(!isValid || model.isBusy)
        if hasExistingIdentity {
          Divider().padding(.vertical, 8)
          Button("link_instead") { Task { await model.startLinking() } }
          Button("recover_with_admins") { Task { await model.startRecovery() } }
        }
        Button("sign_out", role: .destructive) { Task { await model.signOut() } }
          .padding(.top, 16)
      }
      .padding(24)
    }
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

/// New device: shows the code (and a QR code) another device of the user approves, then the short
/// code both compare (e2ee.md §5).
struct LinkThisDeviceView: View {
  @Environment(AppModel.self) private var model
  let progress: LinkProgress

  var body: some View {
    ScrollView {
      VStack(spacing: Theme.spacing) {
        Text("link_new_title").font(.title.bold())
        Text("link_new_hint").foregroundStyle(Theme.textMuted).multilineTextAlignment(.center)
        QRCodeView(text: progress.code).frame(width: 220, height: 220)
        Text(progress.code)
          .font(.system(.footnote, design: .monospaced))
          .textSelection(.enabled)
          .padding()
          .background(Theme.surface)
          .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
        if let shortCode = progress.maybeShortCode {
          Text("link_compare").foregroundStyle(Theme.textMuted)
          Text(shortCode).font(.system(size: 40, weight: .bold, design: .monospaced))
          Text("link_confirm_on_other").foregroundStyle(Theme.textMuted)
        } else {
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
        Text("recovery_title").font(.title.bold())
        Text("recovery_hint").foregroundStyle(Theme.textMuted)
        Text(progress.verificationCode)
          .font(.system(size: 32, weight: .bold, design: .monospaced))
          .frame(maxWidth: .infinity)
          .padding()
          .background(Theme.surface)
          .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
        Text("recovery_new_passphrase").foregroundStyle(Theme.textMuted)
        SecureField("passphrase", text: $passphrase).textContentType(.newPassword).textFieldStyle(.roundedBorder)
        SecureField("passphrase_repeat", text: $repeated).textContentType(.newPassword).textFieldStyle(.roundedBorder)
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
