import SwiftUI

/// The conversation's title in the navigation bar (#713): a round avatar (with the person's
/// presence in a direct message), the name and one quiet status line (presence or topic).
struct ConversationHeader: View {
  let conversation: ConversationItem
  /// Who types in this conversation itself right now (#882); in a direct message the status gives way.
  var typingNames: [String] = []
  /// The channel's name isn't the one members' apps hold (e2ee.md §4.6.5, #1842): marked under it.
  var isNameStale = false

  var body: some View {
    HStack(spacing: 8) {
      avatar
      VStack(alignment: .leading, spacing: 0) {
        Text(conversation.listName)
          .font(.app(.headline))
          .lineLimit(1)
        if isNameStale {
          Label("channel_name_stale", systemImage: "exclamationmark.triangle.fill")
            .font(.app(.caption))
            .foregroundStyle(Theme.danger)
            .lineLimit(1)
            .accessibilityHint(Text("channel_name_stale_hint"))
        } else if conversation.kind == .direct, !typingNames.isEmpty {
          HStack(spacing: 6) {
            TypingDots()
            Text("typing_direct").italic()
          }
          .font(.app(.caption))
          .foregroundStyle(Theme.accent)
          .lineLimit(1)
        } else if let subtitle {
          subtitle
            .font(.app(.caption))
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
      case .direct where conversation.isNotesToSelf:
        symbolAvatar
      case .direct, .group:
        Text(AvatarView.initials(of: conversation.name))
          .font(.system(size: Self.size * 0.4, weight: .semibold))
          .foregroundStyle(.white)
          .frame(width: Self.size, height: Self.size)
          .background(PersonColor.avatar(conversation.id), in: Circle())
      case .public, .private, .integration:
        symbolAvatar
      }
    }
    .overlay(alignment: .bottomTrailing) {
      if let presence = conversation.maybePeerPresence {
        PresenceDot(presence: presence)
          .padding(2)
          .background(Theme.background, in: Circle())
      }
    }
    .accessibilityHidden(true)
  }

  /// Channels and the notes to self show a symbol instead of initials.
  private var symbolAvatar: some View {
    Image(systemName: symbol)
      .font(.system(size: Self.size * 0.42, weight: .semibold))
      .foregroundStyle(Theme.accentStrong)
      .frame(width: Self.size, height: Self.size)
      .background(Theme.accentSoft, in: Circle())
  }

  private var symbol: String {
    if conversation.isNotesToSelf { return "bookmark.fill" }
    switch conversation.kind {
    case .private: return "lock.fill"
    case .integration: return "bolt.fill"
    default: return "number"
    }
  }

  private static let size: CGFloat = 32
}

