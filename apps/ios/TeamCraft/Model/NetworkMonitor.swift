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
  /// Mobile data, a personal hotspot or Low Data Mode: the image viewer may leave originals for a
  /// tap then (Settings → Chats).
  private(set) var isMetered = false
  @ObservationIgnored private let monitor = NWPathMonitor()
  @ObservationIgnored private var hasFirstPath = false

  private init() {
    monitor.pathUpdateHandler = { [weak self] path in
      let isOnline = path.status == .satisfied
      let isMetered = path.isExpensive || path.isConstrained
      SystemProxy.report()
      Task { @MainActor in self?.update(isOnline: isOnline, isMetered: isMetered) }
    }
    monitor.start(queue: DispatchQueue(label: "network-monitor"))
  }

  @MainActor
  private func update(isOnline: Bool, isMetered: Bool) {
    self.isOnline = isOnline
    self.isMetered = isMetered
    // The first path is the state at start, not a change.
    AppModel.shared.networkPathChanged(isFirstPath: !hasFirstPath)
    hasFirstPath = true
  }
}
