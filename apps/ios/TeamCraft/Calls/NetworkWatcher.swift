import Foundation
import Network

/// Reports changes of the device's network path to the call (Wi-Fi to cellular and back, new
/// addresses), so the core checks that its connection followed and rejoins at once otherwise
/// (ARCHITECTURE.md §6.5).
final class NetworkWatcher: @unchecked Sendable {
  private let monitor = NWPathMonitor()
  private let queue = DispatchQueue(label: "de.zeitgleich.teamcraft.network")
  /// Only touched on `queue`.
  private var maybeLastPath: NWPath?

  func start(onChange: @escaping @Sendable () -> Void) {
    monitor.pathUpdateHandler = { [weak self] path in
      guard let self else { return }
      defer { self.maybeLastPath = path }
      // The first update describes the network the call started on.
      guard let last = self.maybeLastPath, last != path else { return }
      onChange()
    }
    monitor.start(queue: queue)
  }

  func stop() {
    monitor.cancel()
  }
}
