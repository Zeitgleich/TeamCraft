import SwiftUI

/// The local data can't be read (issue #414): what happened and the ways out, as on desktop and web.
/// Damaged data is set aside and the device set up again (after a confirmation; nothing is
/// deleted); data from a newer version first asks to update and try again.
struct StoreRecoveryView: View {
  @Environment(AppModel.self) private var model
  let problem: StoreProblem
  let failure: AppFailure?

  @State private var isConfirmingReset = false

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: Theme.spacing) {
        Image(systemName: "externaldrive.badge.exclamationmark")
          .font(.largeTitle)
          .foregroundStyle(Theme.danger)
          .accessibilityHidden(true)
        Text(title)
          .font(.title.bold())
          .accessibilityAddTraits(.isHeader)
        Text(message).foregroundStyle(Theme.textMuted)
        if let failure { Text(failure.message).foregroundStyle(Theme.danger) }
        // The likely way out leads: setting up again for damaged data, updating for newer data.
        if isNewer {
          Button("retry") { Task { await model.retryStore() } }
            .buttonStyle(PrimaryButtonStyle())
            .disabled(model.isBusy)
          resetButton.buttonStyle(.bordered).frame(maxWidth: .infinity)
        } else {
          resetButton.buttonStyle(PrimaryButtonStyle())
          Button("retry") { Task { await model.retryStore() } }
            .buttonStyle(.bordered)
            .frame(maxWidth: .infinity)
            .disabled(model.isBusy)
        }
        Button("sign_out", role: .destructive) { Task { await model.signOut() } }
          .padding(.top, 16)
          .disabled(model.isBusy)
      }
      .padding(24)
    }
    .alert("store_reset_confirm_title", isPresented: $isConfirmingReset) {
      Button("store_reset_confirm_button", role: .destructive) { Task { await model.resetStore() } }
      Button("cancel", role: .cancel) {}
    } message: {
      Text("store_reset_confirm_message")
    }
  }

  private var resetButton: some View {
    Button("store_reset") { isConfirmingReset = true }.disabled(model.isBusy)
  }

  private var isNewer: Bool { problem == .newerVersion }

  private var title: LocalizedStringKey { isNewer ? "store_newer_title" : "store_damaged_title" }

  private var message: LocalizedStringKey { isNewer ? "store_newer_body" : "store_damaged_body" }
}
