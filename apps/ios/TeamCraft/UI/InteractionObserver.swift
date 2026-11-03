import SwiftUI
import UIKit
import UIKit.UIGestureRecognizerSubclass

/// Tells `onTouch` about every touch that begins anywhere in the window, without taking part in it:
/// buttons, scrolling and other gestures work exactly as before. For presence (issue #472).
struct InteractionObserver: UIViewRepresentable {
  let onTouch: () -> Void

  func makeUIView(context: Context) -> InstallerView {
    InstallerView(onTouch: onTouch)
  }

  func updateUIView(_ view: InstallerView, context: Context) {
    view.recognizer.onTouch = onTouch
  }

  /// An invisible view that puts the recognizer on the window it lands in.
  final class InstallerView: UIView {
    let recognizer: TouchObserver

    init(onTouch: @escaping () -> Void) {
      recognizer = TouchObserver(onTouch: onTouch)
      super.init(frame: .zero)
      isUserInteractionEnabled = false
      isAccessibilityElement = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
      nil
    }

    override func didMoveToWindow() {
      super.didMoveToWindow()
      recognizer.view?.removeGestureRecognizer(recognizer)
      window?.addGestureRecognizer(recognizer)
    }
  }
}

/// Reports a touch as it begins and fails at once, so it never delays, cancels or wins against
/// another gesture.
final class TouchObserver: UIGestureRecognizer, UIGestureRecognizerDelegate {
  var onTouch: () -> Void

  init(onTouch: @escaping () -> Void) {
    self.onTouch = onTouch
    super.init(target: nil, action: nil)
    cancelsTouchesInView = false
    delaysTouchesBegan = false
    delaysTouchesEnded = false
    delegate = self
  }

  override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
    onTouch()
    state = .failed
  }

  func gestureRecognizer(
    _ gestureRecognizer: UIGestureRecognizer,
    shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer
  ) -> Bool {
    true
  }
}
