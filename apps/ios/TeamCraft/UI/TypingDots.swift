import SwiftUI

/// Three dots that pulse one after the other while someone types, in the chat list's row and the
/// header (#882). They stay still with "Reduce Motion". Decorative: the text beside them says what
/// is going on.
struct TypingDots: View {
  var color: Color = Theme.accent
  var dotSize: CGFloat = 5
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    Group {
      if reduceMotion {
        dots { _ in TypingDots.restAlpha }
      } else {
        TimelineView(.animation) { context in
          let time = context.date.timeIntervalSinceReferenceDate
          dots { TypingDots.alpha(at: time, dot: $0) }
        }
      }
    }
    .accessibilityHidden(true)
  }

  private func dots(alpha: (Int) -> Double) -> some View {
    HStack(spacing: dotSize / 2) {
      ForEach(0..<TypingDots.count, id: \.self) { dot in
        Circle().fill(color.opacity(alpha(dot))).frame(width: dotSize, height: dotSize)
      }
    }
  }

  static let count = 3
  /// Alpha of a dot at rest, and of all three when motion is reduced.
  static let restAlpha = 0.6
  private static let cycle = 1.2
  private static let stagger = 0.16

  /// One pulse per cycle, each dot a little after the one before.
  static func alpha(at time: Double, dot: Int) -> Double {
    let phase = (time - Double(dot) * stagger).truncatingRemainder(dividingBy: cycle) / cycle
    let wrapped = phase < 0 ? phase + 1 : phase
    // Up and down in the first half of the cycle, resting low in the second.
    let pulse = wrapped < 0.5 ? sin(wrapped * 2 * .pi) : 0
    return restAlpha / 2 + (1 - restAlpha / 2) * max(0, pulse)
  }
}
