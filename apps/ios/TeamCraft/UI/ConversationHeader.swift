import SwiftUI

/// The conversation's title in the navigation bar (#713): a round avatar (with the person's
/// presence in a direct message), the name and one quiet status line (presence or topic).
struct ConversationHeader: View {
  let conversation: ConversationItem

  var body: some View {
    HStack(spacing: 8) {
      avatar
      VStack(alignment: .leading, spacing: 0) {
        Text(conversation.name)
          .font(.headline)
          .lineLimit(1)
        if let subtitle {
          subtitle
            .font(.caption)
            .foregroundStyle(Theme.textMuted)
            .lineLimit(1)
        }
      }
    }
    .accessibilityElement(children: .combine)
    .accessibilityAddTraits(.isHeader)
  }

  private var subtitle: Text? {
    if let presence = conversation.maybePeerPresence { return Text(presence.label) }
    let topic = oneLine(conversation.topic)
    // The topic is the members' text, never a translation key.
    return topic.isEmpty ? nil : Text(verbatim: topic)
  }

  private var avatar: some View {
    Group {
      switch conversation.kind {
      case .direct, .group:
        Text(AvatarView.initials(of: conversation.name))
          .font(.system(size: Self.size * 0.4, weight: .semibold))
          .foregroundStyle(.white)
          .frame(width: Self.size, height: Self.size)
          .background(PersonColor.avatar(conversation.id), in: Circle())
      case .public, .private, .integration:
        Image(systemName: symbol)
          .font(.system(size: Self.size * 0.42, weight: .semibold))
          .foregroundStyle(Theme.accentStrong)
          .frame(width: Self.size, height: Self.size)
          .background(Theme.accentSoft, in: Circle())
      }
    }
    .overlay(alignment: .bottomTrailing) {
      if let presence = conversation.maybePeerPresence {
        Circle()
          .fill(presence.color)
          .frame(width: 10, height: 10)
          .overlay(Circle().stroke(Theme.background, lineWidth: 2))
      }
    }
    .accessibilityHidden(true)
  }

  private var symbol: String {
    switch conversation.kind {
    case .private: return "lock.fill"
    case .integration: return "bolt.fill"
    default: return "number"
    }
  }

  private static let size: CGFloat = 32
}

extension Presence {
  /// The presence dot's colour, the same in the chat list and the header.
  var color: Color {
    switch self {
    case .online: return .green
    case .away: return .orange
    case .doNotDisturb: return .red
    case .offline: return .gray
    }
  }

  var label: LocalizedStringKey {
    switch self {
    case .online: return "presence_online"
    case .away: return "presence_away"
    case .doNotDisturb: return "presence_dnd"
    case .offline: return "presence_peer_offline"
    }
  }
}
