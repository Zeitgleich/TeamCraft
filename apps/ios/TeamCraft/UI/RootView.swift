import SwiftUI

/// The top level: sign-in, enrollment or the chats, by the account's state.
struct RootView: View {
  @Environment(AppModel.self) private var model

  var body: some View {
    ZStack {
      Theme.background.ignoresSafeArea()
      switch model.phase {
      case .starting:
        ProgressView("loading")
      case .signedOut(let failure):
        SignInView(failure: failure)
      case .needsEnrollment(let hasExistingIdentity, let failure):
        EnrollmentView(hasExistingIdentity: hasExistingIdentity, failure: failure)
      case .linking(let progress):
        LinkThisDeviceView(progress: progress)
      case .recovering(let progress, let failure):
        OrgRecoveryView(progress: progress, failure: failure)
      case .ready(let account):
        ConversationListView(account: account)
      case .failed(let failure):
        FailureView(failure: failure)
      }
    }
    .overlay { if model.isBusy { BusyOverlay() } }
    .fullScreenCover(item: callBinding) { call in
      CallScreen(call: call)
    }
  }

  private var callBinding: Binding<ActiveCall?> {
    Binding(get: { model.calls.activeCall }, set: { _ in })
  }
}

struct FailureView: View {
  @Environment(AppModel.self) private var model
  let failure: AppFailure

  var body: some View {
    VStack(spacing: Theme.spacing) {
      Text(failure.message).multilineTextAlignment(.center)
      Button("retry") { Task { await model.retry() } }.buttonStyle(PrimaryButtonStyle())
    }
    .padding()
  }
}

struct BusyOverlay: View {
  var body: some View {
    ProgressView().padding(24).background(.regularMaterial).clipShape(RoundedRectangle(cornerRadius: Theme.radius))
  }
}
