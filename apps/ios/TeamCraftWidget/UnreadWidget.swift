import SwiftUI
import WidgetKit

/// The widget extension (issue #2076). It links no core and reads nothing but the snapshot the app
/// writes into the App Group (`WidgetSnapshotStore`), already cut to the privacy settings.
@main
struct TeamCraftWidgets: WidgetBundle {
  var body: some Widget {
    UnreadWidget()
  }
}

/// "Unread" on the home screen: the conversations waiting for the user and a button for a new
/// message. A tap opens a conversation the way its notification does.
struct UnreadWidget: Widget {
  var body: some WidgetConfiguration {
    StaticConfiguration(kind: HomeWidget.kind, provider: UnreadProvider()) { entry in
      UnreadWidgetView(entry: entry)
        .containerBackground(.background, for: .widget)
    }
    .configurationDisplayName("widget_name")
    .description("widget_description")
    .supportedFamilies([.systemSmall, .systemMedium, .systemLarge])
  }
}

struct UnreadEntry: TimelineEntry {
  let date: Date
  /// `nil` while signed out.
  let maybeSnapshot: WidgetSnapshot?
}

/// One entry and no schedule: the app reloads the timeline when something changed.
struct UnreadProvider: TimelineProvider {
  func placeholder(in context: Context) -> UnreadEntry {
    UnreadEntry(date: .now, maybeSnapshot: WidgetSnapshot(privacy: .countOnly, unreadConversations: 0, rows: []))
  }

  func getSnapshot(in context: Context, completion: @escaping (UnreadEntry) -> Void) {
    completion(entry())
  }

  func getTimeline(in context: Context, completion: @escaping (Timeline<UnreadEntry>) -> Void) {
    completion(Timeline(entries: [entry()], policy: .never))
  }

  private func entry() -> UnreadEntry {
    UnreadEntry(date: .now, maybeSnapshot: WidgetSnapshotStore().read())
  }
}

struct UnreadWidgetView: View {
  /// The brand's badge red (Theme.badge): white numbers on it reach 4.5:1, light and dark.
  private static let badge = Color(red: 0xC8 / 255, green: 0x44 / 255, blue: 0x1F / 255)

  let entry: UnreadEntry
  @Environment(\.widgetFamily) private var family

  var body: some View {
    VStack(alignment: .leading, spacing: 4) {
      header
      content
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }
    // A small widget is one tap target: the app (its rows can't be told apart).
    .widgetURL(family == .systemSmall ? HomeWidget.appURL : nil)
  }

  private var header: some View {
    HStack(alignment: .center) {
      Text("widget_title")
        .font(.headline)
        .foregroundStyle(.primary)
      Spacer(minLength: 4)
      // Signed out there is nobody to write to; a small widget has no second target.
      if entry.maybeSnapshot != nil && family != .systemSmall {
        Link(destination: HomeWidget.newMessageURL) {
          Image(systemName: "square.and.pencil")
            .font(.system(size: 15, weight: .semibold))
            .frame(width: 32, height: 32)
            .background(.tint.opacity(0.15), in: Circle())
            .foregroundStyle(.tint)
        }
        .accessibilityLabel(Text("widget_new_message"))
      }
    }
  }

  @ViewBuilder
  private var content: some View {
    if let snapshot = entry.maybeSnapshot {
      switch snapshot.privacy {
      case .locked:
        message("widget_locked", symbol: "lock.fill")
      case _ where snapshot.unreadConversations == 0:
        message("widget_caught_up", symbol: nil)
      case .countOnly:
        message("widget_unread_conversations \(snapshot.unreadConversations)", symbol: nil)
      case .names, .previews:
        rows(snapshot)
      }
    } else {
      message("widget_signed_out", symbol: nil)
    }
  }

  private func rows(_ snapshot: WidgetSnapshot) -> some View {
    VStack(alignment: .leading, spacing: 2) {
      ForEach(snapshot.rows.prefix(rowLimit)) { row in
        Link(destination: HomeWidget.conversationURL(row.conversationId)) {
          rowView(row)
        }
      }
    }
  }

  private func rowView(_ row: WidgetRow) -> some View {
    HStack(spacing: 8) {
      VStack(alignment: .leading, spacing: 1) {
        Text(row.name)
          .font(.subheadline.weight(.semibold))
          .foregroundStyle(.primary)
          .lineLimit(1)
        if let preview = row.maybePreview, family != .systemSmall {
          Text(preview)
            .font(.caption)
            .foregroundStyle(.secondary)
            .lineLimit(1)
        }
      }
      .privacySensitive()
      Spacer(minLength: 4)
      if !row.countLabel.isEmpty {
        Text(row.countLabel)
          .font(.caption2.weight(.bold))
          .monospacedDigit()
          .foregroundStyle(.white)
          .padding(.horizontal, 6)
          .frame(minWidth: 20, minHeight: 20)
          .background(Self.badge, in: Capsule())
          .accessibilityLabel(Text("widget_unread_count \(row.countLabel)"))
      }
    }
    .frame(minHeight: row.maybePreview == nil ? 32 : 40)
    .contentShape(Rectangle())
  }

  private func message(_ text: LocalizedStringKey, symbol: String?) -> some View {
    VStack(spacing: 6) {
      if let symbol {
        Image(systemName: symbol)
          .font(.body)
          .foregroundStyle(.secondary)
      }
      Text(text)
        .font(.subheadline)
        .foregroundStyle(.secondary)
        .multilineTextAlignment(.center)
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
  }

  /// What fits each size without scrolling (widgets don't scroll).
  private var rowLimit: Int {
    switch family {
    case .systemSmall: 3
    case .systemMedium: 3
    default: 8
    }
  }
}
