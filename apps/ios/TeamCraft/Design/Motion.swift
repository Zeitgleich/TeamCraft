import SwiftUI

/// The motion tokens of the web and desktop UI (crates/teamcraft-ui/style/main/000-tokens.css,
/// docs/motion-audit.md, issue #302), the same values as the Android app's `MotionTokens`: three
/// durations, a short exit and four curves. Views pass their `accessibilityReduceMotion`: with
/// Reduce Motion, fades stay (short cross-fades help reading) and everything that moves or scales
/// becomes a fade, as the web's reduced-motion block does.
enum Motion {
  static let fastDuration: Double = 0.12
  static let baseDuration: Double = 0.18
  static let slowDuration: Double = 0.26
  static let exitDuration: Double = 0.1
  /// The web's reduced-motion block shortens `--dur-base` and `--dur-slow` to this.
  static let reducedDuration: Double = 0.15

  /// `motion-pop` starts at this scale; never from zero.
  static let popFromScale: CGFloat = 0.7
  /// Popups and floating buttons grow from this scale while they fade in.
  static let enterScale: CGFloat = 0.95

  /// `--ease-pop`: its own overshoot is the pop of reactions and badges.
  static let popCurve = UnitCurve.bezier(
    startControlPoint: UnitPoint(x: 0.34, y: 1.56), endControlPoint: UnitPoint(x: 0.64, y: 1))

  /// Small state changes: a fade, a colour, a pressed state.
  static let fast = Animation.timingCurve(0.2, 0, 0, 1, duration: fastDuration)

  /// Leaving: a strong ease-out, so the exit starts at once (motion-audit.md finding 1).
  static let exit = Animation.timingCurve(0.23, 1, 0.32, 1, duration: exitDuration)

  /// Movement on screen (`--ease-standard`, `--dur-base`): a row making room, a section opening.
  static func base(_ reduceMotion: Bool) -> Animation {
    .timingCurve(0.2, 0, 0, 1, duration: reduceMotion ? reducedDuration : baseDuration)
  }

  /// Larger entrances (`--ease-emphasized`, `--dur-slow`): something arriving that should land softly.
  static func slow(_ reduceMotion: Bool) -> Animation {
    reduceMotion
      ? .timingCurve(0.2, 0, 0, 1, duration: reducedDuration)
      : .timingCurve(0.05, 0.7, 0.1, 1, duration: slowDuration)
  }

  /// Chrome and panels that slide (the call bar, call controls): a critically damped spring, so a
  /// second change mid-way starts from where they are; a fade with Reduce Motion.
  static func spring(_ reduceMotion: Bool) -> Animation {
    reduceMotion ? .easeOut(duration: reducedDuration) : .spring(response: 0.35, dampingFraction: 1)
  }

  /// Scrolling to a place in a list or form: smooth, or a jump with Reduce Motion (forms.md).
  static func scroll(_ reduceMotion: Bool) -> Animation? {
    reduceMotion ? nil : .timingCurve(0.2, 0, 0, 1, duration: slowDuration)
  }

  /// Something small appearing in place (a jump button, a toast): fades in and grows a little.
  static func enter(_ reduceMotion: Bool) -> AnyTransition {
    reduceMotion ? .opacity : .scale(scale: enterScale).combined(with: .opacity)
  }

  /// Something sliding in from an edge and back out the same way; a fade with Reduce Motion.
  static func slide(from edge: Edge, _ reduceMotion: Bool) -> AnyTransition {
    reduceMotion ? .opacity : .move(edge: edge).combined(with: .opacity)
  }
}

extension View {
  /// Pops the view (scales from `Motion.popFromScale` back to full size) whenever `key` changes,
  /// e.g. a reaction's count or a badge's number. It doesn't play when the view appears, and not at
  /// all with Reduce Motion. Only a scale effect: no layout pass, nothing runs while it rests.
  func popOnChange<Key: Equatable>(of key: Key, reduceMotion: Bool) -> some View {
    keyframeAnimator(initialValue: CGFloat(1), trigger: reduceMotion ? nil : key) { content, scale in
      content.scaleEffect(scale)
    } keyframes: { _ in
      KeyframeTrack {
        MoveKeyframe(Motion.popFromScale)
        LinearKeyframe(1, duration: Motion.slowDuration, timingCurve: Motion.popCurve)
      }
    }
  }
}
