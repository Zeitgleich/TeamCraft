import Foundation
import Network
import Observation

/// Whether the device has a usable network path, updated live. Every change after the first (Wi-Fi
/// to cellular, a VPN, airplane mode off) is reported to the core, which checks its gateway
/// connection at once instead of waiting for its own detection and backoff. Whether the server is
/// reachable is the core's connection state (`AppModel.connection`). The proxy can change with the
/// path, so the core learns it first (issue #157).
@Observable
final class NetworkMonitor {
  static let shared = NetworkMonitor()

  private(set) var isOnline = true
  @ObservationIgnored private let monitor = NWPathMonitor()
  @ObservationIgnored private var hasFirstPath = false

  private init() {
    monitor.pathUpdateHandler = { [weak self] path in
      let isOnline = path.status == .satisfied
      SystemProxy.report()
      Task { @MainActor in self?.update(isOnline: isOnline) }
    }
    monitor.start(queue: DispatchQueue(label: "network-monitor"))
  }

  @MainActor
  private func update(isOnline: Bool) {
    self.isOnline = isOnline
    // The first path is the state at start, not a change.
    AppModel.shared.networkPathChanged(isFirstPath: !hasFirstPath)
    hasFirstPath = true
  }
}
