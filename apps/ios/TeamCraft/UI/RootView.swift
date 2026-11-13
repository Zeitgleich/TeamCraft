import SwiftUI

/// The top level: server entry, sign-in, enrollment or the chats, by the account's state.
struct RootView: View {
  @Environment(AppModel.self) private var model
  /// The interface font the views were built with (#1494). A font chosen in Settings applies when
  /// Settings closes: the screens are built again, which a change underneath the open sheet would
  /// close.
  @State private var appliedFont = AppFont.current

  var body: some View {
    ZStack {
      Theme.background.ignoresSafeArea()
      #if DEBUG
        if ChatListDemo.isOn {
          // Screenshots of the chat list with sample chats (#1786), whatever the account's state.
          ConversationListView(account: ChatListDemo.account)
        } else {
          phaseView
        }
      #else
        phaseView
      #endif
      // The animated mark while the core starts, handing over to whatever screen comes next (#463).
      LoadingMarkOverlay(isLoading: isStarting && !isDemo)
    }
    .font(.app(.body))
    .onReceive(NotificationCenter.default.publisher(for: AppFont.mayHaveChanged)) { _ in
      if AppFont.current != appliedFont { appliedFont = AppFont.current }
    }
    .overlay { if model.isBusy { BusyOverlay() } }
    .overlay(alignment: .top) {
      if let share = model.waitingShare, !isReady, !isStarting { ShareWaitingNotice(share: share) }
    }
    .sheet(item: shareToPick) { share in SharePickerView(share: share) }
    // "Hand over" on the lobby reminder after leaving a meeting's call from CallKit (#1859).
    .sheet(item: lobbyHandover) { request in LobbyHandoverSheet(request: request) }
    .alert(
      "share_other_account",
      isPresented: Binding(get: { model.isShareGoneForAccount }, set: { model.isShareGoneForAccount = $0 })
    ) {
      Button("done", role: .cancel) {}
    }
    .appLinkQuestions(model)
    .task(id: appLinkMoment) { await model.actOnAppLink() }
    // Every touch counts for presence; the core reports it at most every 30 s (issue #472).
    .background { InteractionObserver { model.reportActivity(.interaction) } }
    .fullScreenCover(item: callBinding) { call in
      CallScreen(call: call)
    }
  }

  /// Changes when a waiting link may get further: the start is over, or the account changed.
  private var appLinkMoment: String {
    switch model.phase {
    case .starting: "starting"
    case .ready: "ready \(model.currentServerOrigin ?? "")"
    default: "other \(model.currentServerOrigin ?? "")"
    }
  }

  /// The screen of the account's state.
  @ViewBuilder
  private var phaseView: some View {
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
        .id(appliedFont)
        // A new device asks to be linked: a card on every signed-in device (docs/e2ee.md §5).
        .overlay(alignment: .top) { LinkPromptOverlay() }
    case .failed(let failure):
      FailureView(failure: failure)
    case .storeUnreadable(let problem, let failure):
      StoreRecoveryView(problem: problem, failure: failure)
    }
  }

  /// The chat list with sample chats, for screenshots (Debug builds only, `ChatListDemo`).
  private var isDemo: Bool {
    #if DEBUG
      ChatListDemo.isOn
    #else
      false
    #endif
  }

  private var isReady: Bool {
    if case .ready = model.phase { return true }
    return false
  }

  /// A waiting share opens its picker in the signed-in app; the lock's shield covers it until then.
  private var shareToPick: Binding<StagedShare?> {
    Binding(
      get: { isReady ? model.waitingShare : nil },
      set: { _ in })
  }

  /// The handover choice, once the account is ready (the app lock is in front until then).
  private var lobbyHandover: Binding<LobbyHandoverRequest?> {
    Binding(
      get: { isReady ? model.maybeLobbyHandover : nil },
      set: { model.maybeLobbyHandover = $0 })
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
