import SwiftUI

/// When a vertical swipe closes a full-screen viewer (#1999): where a release would come to rest,
/// and how far the backdrop has faded. Shared by the picture and the video viewer; the Android
/// apps decide the same way.
enum SwipeDismiss {
  /// A drag that ends this far (of the viewer's height) from the start closes the viewer.
  static let dismissFraction: CGFloat = 0.3
  /// The backdrop is gone at this share of the viewer's height.
  static let fadeFraction: CGFloat = 0.5
  /// A scroll's deceleration: where a fling would stop.
  private static let decelerationRate: CGFloat = 0.998

  /// Where a fling of `velocity` (points per second) travels before it stops.
  static func projectedTravel(velocity: CGFloat) -> CGFloat {
    velocity / 1000 * decelerationRate / (1 - decelerationRate)
  }

  /// Whether a drag released at `offset` with `velocity` closes the viewer; `extent` is its height.
  static func shouldDismiss(offset: CGFloat, velocity: CGFloat, extent: CGFloat) -> Bool {
    let projected = offset + projectedTravel(velocity: velocity)
    // A fling back past the start is a change of mind, not a dismissal the other way.
    let isTurnedBack = offset != 0 && (projected < 0) != (offset < 0)
    return extent > 0 && !isTurnedBack && abs(projected) >= extent * dismissFraction
  }

  /// How much of the backdrop is gone at `offset`: 0 at rest, 1 when faded out.
  static func progress(offset: CGFloat, extent: CGFloat) -> Double {
    guard extent > 0 else { return 0 }
    return Double(min(max(abs(offset) / (extent * fadeFraction), 0), 1))
  }
}

extension View {
  /// Lets a vertical drag (up or down) carry the view with the finger and close the viewer: past a
  /// distance or with a fast fling `onDismiss` runs, otherwise it springs back. Horizontal drags
  /// (paging, scrubbing) are left alone, and so is everything while `isEnabled` is false (a zoomed
  /// picture pans). Reduced motion: no spring, it closes at once or snaps back.
  func swipeDismiss(
    offset: Binding<CGFloat>, extent: CGFloat, isEnabled: Bool, onDismiss: @escaping () -> Void
  ) -> some View {
    modifier(SwipeDismissModifier(offset: offset, extent: extent, isEnabled: isEnabled, onDismiss: onDismiss))
  }
}

private struct SwipeDismissModifier: ViewModifier {
  @Environment(\.accessibilityReduceMotion) private var isMotionReduced
  @Binding var offset: CGFloat
  let extent: CGFloat
  let isEnabled: Bool
  let onDismiss: () -> Void
  /// Decided on the first movement of a drag: vertical is ours, anything else is not.
  @State private var isVertical: Bool?

  func body(content: Content) -> some View {
    content.simultaneousGesture(
      DragGesture(minimumDistance: 10)
        .onChanged { value in
          guard isEnabled else { return }
          if isVertical == nil {
            isVertical = abs(value.translation.height) > abs(value.translation.width)
          }
          if isVertical == true { offset = value.translation.height }
        }
        .onEnded { value in
          defer { isVertical = nil }
          guard isVertical == true else { return }
          settle(velocity: value.velocity.height)
        })
  }

  private func settle(velocity: CGFloat) {
    if SwipeDismiss.shouldDismiss(offset: offset, velocity: velocity, extent: extent) {
      if isMotionReduced {
        onDismiss()
        return
      }
      // Out the way it was going.
      let direction: CGFloat = offset >= 0 ? 1 : -1
      withAnimation(.spring(duration: 0.3, bounce: 0)) {
        offset = direction * extent
      } completion: {
        onDismiss()
      }
    } else if isMotionReduced {
      offset = 0
    } else {
      // Bounce only when a fling carried momentum into the spring.
      let bounce: Double = abs(velocity) > 500 ? 0.2 : 0
      withAnimation(.spring(duration: 0.35, bounce: bounce)) { offset = 0 }
    }
  }
}
