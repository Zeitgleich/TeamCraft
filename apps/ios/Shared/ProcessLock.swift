import Darwin
import Foundation

/// Only one process syncs with the server and writes the local store at a time (docs/e2ee.md §7,
/// like Threema's `ProcessCoordinator`): the app while it is in the foreground, the notification
/// extension while it handles a push. An advisory `flock` on a file in the App Group; the kernel
/// releases it if the holder dies.
final class ProcessLock: @unchecked Sendable {
  private let fileURL: URL
  private var descriptor: Int32 = -1

  init(fileURL: URL) {
    self.fileURL = fileURL
  }

  deinit {
    release()
  }

  var isHeld: Bool { descriptor >= 0 }

  /// Takes the lock, polling until `timeout` has passed. `false`: someone else holds it.
  func acquire(timeout: TimeInterval) -> Bool {
    if isHeld { return true }
    let opened = open(fileURL.path, O_CREAT | O_RDWR, 0o600)
    guard opened >= 0 else { return false }
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
      if flock(opened, LOCK_EX | LOCK_NB) == 0 {
        descriptor = opened
        return true
      }
      Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    close(opened)
    return false
  }

  func release() {
    guard descriptor >= 0 else { return }
    flock(descriptor, LOCK_UN)
    close(descriptor)
    descriptor = -1
  }
}

/// Tells the other process that the shared store changed (Darwin notifications cross processes and
/// carry no payload): the extension after it stored a message, the app to reload.
final class StoreChangeNotifier {
  static let name = "com.example.teamcraft.store-changed"

  private var handler: (() -> Void)?

  /// Calls `handler` (on the main queue) when the *other* process posts. Replaces an earlier handler.
  func observe(_ handler: @escaping () -> Void) {
    self.handler = handler
    let center = CFNotificationCenterGetDarwinNotifyCenter()
    CFNotificationCenterAddObserver(
      center,
      Unmanaged.passUnretained(self).toOpaque(),
      { _, observer, _, _, _ in
        guard let observer else { return }
        let notifier = Unmanaged<StoreChangeNotifier>.fromOpaque(observer).takeUnretainedValue()
        DispatchQueue.main.async { notifier.handler?() }
      },
      Self.name as CFString,
      nil,
      .deliverImmediately
    )
  }

  func post() {
    CFNotificationCenterPostNotification(
      CFNotificationCenterGetDarwinNotifyCenter(), CFNotificationName(Self.name as CFString), nil, nil, true
    )
  }

  deinit {
    CFNotificationCenterRemoveEveryObserver(
      CFNotificationCenterGetDarwinNotifyCenter(), Unmanaged.passUnretained(self).toOpaque()
    )
  }
}
