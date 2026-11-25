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
