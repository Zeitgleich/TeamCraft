import SwiftUI

/// How long pull to refresh waits for the connection, and the least it shows its spinner (#2006).
private let refreshWait: Duration = .seconds(8)
private let refreshMinimum: Duration = .milliseconds(600)

extension AppModel {
  /// Pull to refresh: the gateway checks its connection now instead of after its backoff, which
  /// also catches up what was missed. `false` when the connection didn't come up in time or needs
  /// a sign-in; the caller says so.
  func refreshConnection() async -> Bool {
    let start = ContinuousClock.now
    networkChanged()
    var isUp = connection?.isConnected == true
    while !isUp, ContinuousClock.now - start < refreshWait {
      if case .signInRequired = connection { break }
      if case .noRoleLeft = connection { break }
      try? await Task.sleep(for: .milliseconds(150))
      isUp = connection?.isConnected == true
    }
    let shown = ContinuousClock.now - start
    if shown < refreshMinimum { try? await Task.sleep(for: refreshMinimum - shown) }
    return isUp
  }
}

/// `refreshable` that forces a reconnect and catch-up, and says when it didn't work.
private struct RefreshesConnection: ViewModifier {
  @Environment(AppModel.self) private var model
  @State private var isFailed = false

  func body(content: Content) -> some View {
    content
      .refreshable { isFailed = !(await model.refreshConnection()) }
      .alert("refresh_failed", isPresented: $isFailed) { Button("done", role: .cancel) {} }
  }
}

extension View {
  func refreshesConnection() -> some View { modifier(RefreshesConnection()) }
}
