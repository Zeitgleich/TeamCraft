import SwiftUI
import UIKit

/// Shows the lock screen or the app switcher cover in a window of its own, above everything the
/// app presents: sheets, Quick Look previews, the call screen. An overlay on the root view would
/// sit underneath those.
@MainActor
final class ShieldWindow {
  enum Mode: Equatable {
    case lock
    case cover
  }

  private var maybeWindow: UIWindow?
  private var maybeMode: Mode?

  /// Shows `mode` (`nil` hides the window). The same mode again keeps the window as it is, so the
  /// lock screen doesn't ask again on every phase change.
  func show(_ maybeMode: Mode?, lock: AppLock) {
    guard maybeMode != self.maybeMode else { return }
    self.maybeMode = maybeMode
    guard let mode = maybeMode else {
      maybeWindow?.isHidden = true
      maybeWindow?.rootViewController = nil
      return
    }
    guard let window = maybeWindow ?? makeWindow() else { return }
    if mode == .lock {
      // The keyboard lives in a window of its own; dismissed, it can't show text over the lock.
      UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }
    let host: UIViewController
    switch mode {
    case .lock: host = UIHostingController(rootView: LockView(lock: lock))
    case .cover: host = UIHostingController(rootView: PrivacyCover())
    }
    // VoiceOver reads only this window, not the chats underneath.
    host.view.accessibilityViewIsModal = true
    window.rootViewController = host
    window.isHidden = false
  }

  private func makeWindow() -> UIWindow? {
    let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    guard let scene = scenes.first(where: { $0.activationState != .unattached }) ?? scenes.first else { return nil }
    let window = UIWindow(windowScene: scene)
    window.windowLevel = .alert + 1
    maybeWindow = window
    return window
  }
}
