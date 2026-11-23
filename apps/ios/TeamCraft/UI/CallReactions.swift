import SwiftUI

/// One reaction rising over the call's stage (#2324).
struct FloatingReaction: Identifiable, Equatable {
  let id: Int
  let emoji: String
  /// Who reacted; empty for the user's own.
  let name: String
  /// Where across the stage it rises (0.1 … 0.9).
  let xFraction: CGFloat
}

/// The reactions rising over the stage (#2324): transform and opacity only. With Reduce Motion
/// they fade in and out in place. Hidden from VoiceOver: the call screen announces them.
struct FloatingReactionsLayer: View {
  let reactions: [FloatingReaction]
  let reduceMotion: Bool

  var body: some View {
    GeometryReader { proxy in
      ForEach(reactions) { reaction in
        FloatingReactionView(reaction: reaction, reduceMotion: reduceMotion)
          .position(x: proxy.size.width * reaction.xFraction, y: proxy.size.height - 40)
      }
    }
    .allowsHitTesting(false)
    .accessibilityHidden(true)
  }
}

private struct FloatingReactionView: View {
  let reaction: FloatingReaction
  let reduceMotion: Bool
  @State private var isShown = false
  @State private var isRisen = false

  var body: some View {
    VStack(spacing: 2) {
      Text(verbatim: reaction.emoji).font(.system(size: 36))
      if !reaction.name.isEmpty {
        Text(verbatim: reaction.name)
          .font(.app(.caption2).weight(.medium))
          .foregroundStyle(.white)
          .lineLimit(1)
          .frame(maxWidth: 140)
          .padding(.horizontal, 8)
          .padding(.vertical, 1)
          .background(.black.opacity(0.55), in: Capsule())
      }
    }
    .opacity(isShown ? 1 : 0)
    .offset(y: isRisen && !reduceMotion ? -FloatingReactions.rise : 0)
    .task {
      withAnimation(.easeOut(duration: 0.3)) { isShown = true }
      withAnimation(.easeOut(duration: FloatingReactions.seconds)) { isRisen = true }
      try? await Task.sleep(for: .seconds(FloatingReactions.seconds - 0.8))
      withAnimation(.easeIn(duration: 0.8)) { isShown = false }
    }
  }
}

/// What the call screen keeps of the floating reactions, and the reactions' names.
enum FloatingReactions {
  /// How long a reaction shows over the stage.
  static let seconds: Double = 3.2
  /// How far it rises.
  static let rise: CGFloat = 220
  /// The most floating at once; older ones make room.
  static let maxFloating = 12

  /// Spreads reactions across the stage, so a burst doesn't stack in one column.
  static func spread(_ key: Int) -> CGFloat {
    CGFloat(10 + abs((key &* 61_803) % 80)) / 100
  }

  /// The reaction's spoken name, by its stable name from the core.
  static func name(of reaction: String) -> String {
    switch reaction {
    case "thumbs_up": String(localized: "call_reaction_thumbs_up")
    case "applause": String(localized: "call_reaction_applause")
    case "laugh": String(localized: "call_reaction_laugh")
    case "heart": String(localized: "call_reaction_heart")
    case "celebrate": String(localized: "call_reaction_celebrate")
    case "surprise": String(localized: "call_reaction_surprise")
    default: String(localized: "call_reaction_other")
    }
  }
}
