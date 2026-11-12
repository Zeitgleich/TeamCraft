import SwiftUI

/// The chats screen's one search field (#1786), under the header on every tab: names first, then
/// messages. The search filters open from the button inside it, filled while some are set.
struct ChatSearchField: View {
  @Binding var text: String
  let hasFilters: Bool
  let onFilters: () -> Void

  @FocusState private var isFocused: Bool

  var body: some View {
    HStack(spacing: 8) {
      HStack(spacing: 6) {
        Image(systemName: "magnifyingglass")
          .foregroundStyle(Theme.textMuted)
          .accessibilityHidden(true)
        TextField("search_chats_hint", text: $text)
          .textFieldStyle(.plain)
          .focused($isFocused)
          .autocorrectionDisabled()
          .textInputAutocapitalization(.never)
          .submitLabel(.search)
        if !text.isEmpty {
          Button { text = "" } label: {
            Image(systemName: "xmark.circle.fill")
              .foregroundStyle(Theme.textMuted)
              .frame(minWidth: 32, minHeight: 44)
              .contentShape(Rectangle())
          }
          .buttonStyle(.plain)
          .accessibilityLabel(Text("search_clear"))
        }
        Button(action: onFilters) {
          Image(systemName: hasFilters ? "line.3.horizontal.decrease.circle.fill" : "line.3.horizontal.decrease.circle")
            .font(.app(.title3))
            .foregroundStyle(hasFilters ? Theme.accent : Theme.textMuted)
            .frame(minWidth: 44, minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("search_filters_title"))
        .accessibilityAddTraits(hasFilters ? .isSelected : [])
      }
      .padding(.leading, 12)
      .frame(minHeight: 44)
      .background(Color(.tertiarySystemFill), in: .capsule)
      if isFocused || !text.isEmpty {
        Button("cancel") {
          text = ""
          isFocused = false
        }
        .frame(minHeight: 44)
        .transition(.move(edge: .trailing).combined(with: .opacity))
      }
    }
    .animation(.snappy(duration: 0.2), value: isFocused)
  }
}

/// The filter chips under the search field (#1786): flat, in one row that scrolls sideways, one
/// selected; "All" is a chip of its own. Unread carries its count.
struct FilterChipRow: View {
  let chips: [HomeFilter]
  @Binding var selection: HomeFilter
  let unreadCount: Int

  var body: some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 8) {
        ForEach(chips, id: \.self) { chip in
          let isSelected = selection == chip
          Button { selection = chip } label: {
            HStack(spacing: 4) {
              Text(chip.titleKey)
              if chip == .unread && unreadCount > 0 {
                Text(verbatim: "\(unreadCount)").monospacedDigit()
              }
            }
            .font(.app(.subheadline).weight(.semibold))
            .lineLimit(1)
            .foregroundStyle(isSelected ? Theme.onAccent : Theme.text)
            .padding(.horizontal, 14)
            .frame(minHeight: 32)
            .background(isSelected ? Theme.accent : Color(.tertiarySystemFill), in: .capsule)
            // The visible chip is smaller; the hit area is 44 pt high.
            .padding(.vertical, 6)
            .contentShape(Rectangle())
          }
          .buttonStyle(.plain)
          .accessibilityLabel(Text(chip.titleKey))
          .accessibilityValue(chip == .unread && unreadCount > 0 ? Text(verbatim: "\(unreadCount)") : Text(verbatim: ""))
          .accessibilityAddTraits(isSelected ? .isSelected : [])
        }
      }
      .padding(.horizontal, 16)
    }
    .sensoryFeedback(.selection, trigger: selection)
  }
}

extension HomeFilter {
  var titleKey: LocalizedStringKey {
    switch self {
    case .all: return "home_filter_all"
    case .unread: return "home_filter_unread"
    case .people: return "home_filter_people"
    case .groups: return "home_filter_groups"
    case .channels: return "channels"
    }
  }
}

extension ChatTab {
  var titleKey: LocalizedStringKey {
    switch self {
    case .chats: return "nav_chats"
    case .channels: return "nav_channels"
    case .meetings: return "nav_meetings"
    case .activity: return "nav_activity"
    }
  }

  var symbol: String {
    switch self {
    case .chats: return "bubble.left.and.bubble.right"
    case .channels: return "number"
    case .meetings: return "calendar"
    case .activity: return "bell"
    }
  }
}

/// The floating bottom bar (#1786): a capsule inset from the screen's edges with the tabs, each with
/// what is new in it; the selected one on the accent's soft tint. Big text sizes stop at a size the
/// bar can hold; a long press shows the item larger (Large Content Viewer), as the system tab bar.
struct FloatingTabBar: View {
  let items: [ChatTab]
  let selected: ChatTab
  let mark: (ChatTab) -> (count: Int, hasNew: Bool)
  let onSelect: (ChatTab) -> Void

  var body: some View {
    HStack(spacing: 0) {
      ForEach(items, id: \.self) { item in
        let badge = mark(item)
        let isSelected = item == selected
        Button { onSelect(item) } label: {
          VStack(spacing: 2) {
            Image(systemName: item.symbol)
              .symbolVariant(isSelected ? .fill : .none)
              .font(.system(size: 19, weight: .medium))
              .frame(height: 24)
              .overlay(alignment: .topTrailing) { TabBadgeMark(count: badge.count, hasNew: badge.hasNew) }
            Text(item.titleKey)
              .font(.app(.caption2).weight(isSelected ? .semibold : .regular))
              .lineLimit(1)
              .minimumScaleFactor(0.7)
          }
          .foregroundStyle(isSelected ? Theme.accentStrong : Theme.textMuted)
          .frame(maxWidth: .infinity, minHeight: 48)
          .background {
            if isSelected { Capsule().fill(Theme.accentSoft) }
          }
          .contentShape(.capsule)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(item.titleKey))
        .accessibilityValue(badge.count > 0 ? Text("chat_unread \(badge.count)") : Text(verbatim: ""))
        .accessibilityAddTraits(isSelected ? [.isSelected, .isButton] : .isButton)
        .accessibilityShowsLargeContentViewer {
          Label { Text(item.titleKey) } icon: { Image(systemName: item.symbol) }
        }
      }
    }
    .padding(4)
    .floatingSurface(in: .capsule)
    .dynamicTypeSize(...DynamicTypeSize.xLarge)
    .sensoryFeedback(.selection, trigger: selected)
  }
}

/// The round compose button next to the bar (#1786), in the accent colour.
struct ComposeMark: View {
  let symbol: String

  var body: some View {
    Image(systemName: symbol)
      .font(.system(size: 22, weight: .semibold))
      .foregroundStyle(Theme.onAccent)
      .frame(width: 56, height: 56)
      .floatingSurface(in: .circle, tint: Theme.accent)
      .contentShape(.circle)
  }
}

/// The user's own avatar with their presence, at the header's trailing edge (#1786): it opens
/// Settings.
struct OwnAvatarButton: View {
  let name: String
  let presence: Presence
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      AvatarView(name: name, size: 32)
        .overlay(alignment: .bottomTrailing) {
          PresenceDot(presence: presence, size: 11, maybeRing: Color(.systemBackground))
            .offset(x: 2, y: 2)
        }
        .frame(minWidth: 44, minHeight: 44)
        .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text("settings_title"))
    .accessibilityValue(Text(presence.label))
  }
}

/// A tab's mark: the number of what is addressed to the user, or a dot for something new.
struct TabBadgeMark: View {
  let count: Int
  let hasNew: Bool

  var body: some View {
    if count > 0 {
      Text(count > 99 ? "99+" : "\(count)")
        .font(.app(.caption2).weight(.bold))
        .foregroundStyle(.white)
        .padding(.horizontal, 5)
        .frame(minWidth: 18, minHeight: 18)
        .background(.red, in: .capsule)
        .offset(x: 12, y: -6)
        .accessibilityHidden(true)
    } else if hasNew {
      Circle().fill(Theme.textMuted).frame(width: 8, height: 8).offset(x: 6, y: -2).accessibilityHidden(true)
    }
  }
}

extension View {
  /// The floating bar's surface (#1786, #1661): the system glass from iOS 26, else the surface colour
  /// with an edge and a shadow; solid while Reduce Transparency is on.
  func floatingSurface<S: Shape>(in shape: S, tint: Color? = nil) -> some View {
    modifier(FloatingSurface(shape: shape, tint: tint))
  }
}

private struct FloatingSurface<S: Shape>: ViewModifier {
  let shape: S
  let tint: Color?
  @Environment(\.accessibilityReduceTransparency) private var reducesTransparency

  func body(content: Content) -> some View {
    if #available(iOS 26, *), !reducesTransparency {
      content.glassEffect(tint.map { Glass.regular.tint($0).interactive() } ?? Glass.regular, in: shape)
    } else {
      content
        .background(tint ?? Theme.surface, in: shape)
        .overlay { shape.stroke(tint == nil ? Theme.border : .clear, lineWidth: 1) }
        .shadow(color: .black.opacity(0.14), radius: 10, y: 4)
    }
  }
}
