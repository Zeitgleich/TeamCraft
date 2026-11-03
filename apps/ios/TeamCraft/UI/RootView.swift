import SwiftUI

/// The top level: server entry, sign-in, enrollment or the chats, by the account's state.
struct RootView: View {
  @Environment(AppModel.self) private var model

  var body: some View {
    ZStack {
      Theme.background.ignoresSafeArea()
      switch model.phase {
      case .starting:
        // The loading mark below stands in for this screen.
        EmptyView()
      case .needsServer(let address, let failure):
        ServerEntryView(address: address, failure: failure)
      case .confirmServer(let checked):
        ServerConfirmView(checked: checked)
      case .signedOut(let failure):
        SignInView(failure: failure)
      case .needsEnrollment(let hasExistingIdentity, let failure):
        EnrollmentView(hasExistingIdentity: hasExistingIdentity, failure: failure)
      case .linking(let progress):
        // A fresh attempt forgets an earlier "asked".
        LinkThisDeviceView(progress: progress).id(progress.attempt)
      case .recovering(let progress, let failure):
        OrgRecoveryView(progress: progress, failure: failure)
      case .ready(let account):
        ConversationListView(account: account)
          // A new device asks to be linked: a card on every signed-in device (docs/e2ee.md §5).
          .overlay(alignment: .top) { LinkPromptOverlay() }
      case .failed(let failure):
        FailureView(failure: failure)
      case .storeUnreadable(let problem, let failure):
        StoreRecoveryView(problem: problem, failure: failure)
      }
      // The animated mark while the core starts, handing over to whatever screen comes next (#463).
      LoadingMarkOverlay(isLoading: isStarting)
    }
    .overlay { if model.isBusy { BusyOverlay() } }
    // Every touch counts for presence; the core reports it at most every 30 s (issue #472).
    .background { InteractionObserver { model.reportActivity(.interaction) } }
    .fullScreenCover(item: callBinding) { call in
      CallScreen(call: call)
    }
  }

  private var isStarting: Bool {
    if case .starting = model.phase { return true }
    return false
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
