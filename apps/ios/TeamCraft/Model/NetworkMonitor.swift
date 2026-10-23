import Foundation
import Network
import Observation

/// Whether the device has a usable network path, updated live. Messages written while offline wait
/// in the core's outbox; the banner tells the user why nothing new arrives.
@Observable
final class NetworkMonitor {
  static let shared = NetworkMonitor()

  private(set) var isOnline = true
  @ObservationIgnored private let monitor = NWPathMonitor()

  private init() {
    monitor.pathUpdateHandler = { [weak self] path in
      let isOnline = path.status == .satisfied
      DispatchQueue.main.async { self?.isOnline = isOnline }
    }
    monitor.start(queue: DispatchQueue(label: "network-monitor"))
  }
}
