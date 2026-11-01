import SwiftUI

/// What every bubble of a conversation shares: how wide a bubble may get, whether bubbles name
/// their sender (not in a one-to-one chat) and who can be mentioned.
struct BubbleLook: Equatable {
  /// A bubble takes at most this share of the list's width, and never more than `maxBubbleWidth`.
  static let widthShare: CGFloat = 0.8
  static let maxBubbleWidth: CGFloat = 520

  let bubbleWidth: CGFloat
  let showsSenders: Bool
  var names = MentionNames()

  init(listWidth: CGFloat, showsSenders: Bool, names: MentionNames) {
    bubbleWidth = max(120, min(listWidth * Self.widthShare, Self.maxBubbleWidth))
    self.showsSenders = showsSenders
    self.names = names
  }

  /// The room beside a bubble that stays free, so a long message doesn't span the whole row.
  func gutter(listWidth: CGFloat) -> CGFloat {
    max(0, listWidth - bubbleWidth)
  }
}

/// Room for the sender's avatar beside the bubbles of others in channels and groups.
private let senderAvatarSize: CGFloat = 30
private let senderAvatarGap: CGFloat = 6
/// Media sit close to the bubble's edge, text has room.
private let bubbleMediaInset: CGFloat = 3
private let bubbleTextInset: CGFloat = 9

/// One message as a chat bubble (#713, the same design as the Android app): own messages on the
/// trailing side in the accent, others on the leading side in a neutral bubble with the sender's
/// name and avatar once per group (channels and groups). Reactions, "not sent" and the thread link
/// go below the bubble. System messages (joins, renames, who added whom) are a quiet line instead,
/// worded as on desktop and web (#769).
struct MessageRow: View {
  let entry: FeedEntry
  let maybeExtras: MessageExtras?
  let isMine: Bool
  let look: BubbleLook
  let listWidth: CGFloat
  let showsThreadLink: Bool
  let onOpenThread: () -> Void
  var displayNames: [String: String] = [:]
  /// Without the right to react, the reactions stay readable but don't toggle (issue #758).
  var canReact = true

  var body: some View {
    if let line = SystemLine(message: message, maybeExtras: maybeExtras, names: displayNames) {
      Text(line.text)
        .font(.footnote.italic())
        .foregroundStyle(Theme.textMuted)
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 4)
    } else {
      bubbleRow
    }
  }

  private var message: MessageItem { entry.message }
  private var showsSender: Bool { look.showsSenders && !isMine }

  private var bubbleRow: some View {
    VStack(alignment: isMine ? .trailing : .leading, spacing: 2) {
      HStack(alignment: .bottom, spacing: senderAvatarGap) {
        if isMine { Spacer(minLength: look.gutter(listWidth: listWidth)) }
        if showsSender {
          // The avatar sits by the group's last bubble; the others keep its room so bubbles line up.
          Group {
            if entry.isGroupEnd {
              SenderAvatar(name: message.authorName, userId: message.authorId)
            } else {
              Color.clear
            }
          }
          .frame(width: senderAvatarSize, height: senderAvatarSize)
        }
        MessageBubble(
          entry: entry, maybeExtras: maybeExtras, isMine: isMine, look: look,
          showsName: showsSender && !entry.isContinuation, displayNames: displayNames)
        if !isMine { Spacer(minLength: look.gutter(listWidth: listWidth)) }
      }
      BelowBubble(
        message: message, maybeExtras: maybeExtras, showsThreadLink: showsThreadLink, onOpenThread: onOpenThread,
        canReact: canReact
      )
        .padding(.leading, showsSender ? senderAvatarSize + senderAvatarGap : 0)
    }
    .frame(maxWidth: .infinity, alignment: isMine ? .trailing : .leading)
  }
}

/// The bubble itself: name, forward and reply headers, media first, the text as caption, files and
/// the link preview, and the time with the sent state at the bottom trailing corner.
struct MessageBubble: View {
  let entry: FeedEntry
  let maybeExtras: MessageExtras?
  let isMine: Bool
  let look: BubbleLook
  let showsName: Bool
  let displayNames: [String: String]

  var body: some View {
    BubbleStack {
      if showsName {
        Text(message.authorName)
          .font(.subheadline.weight(.semibold))
          .foregroundStyle(PersonColor.name(message.authorId))
          .lineLimit(1)
          .padding(.horizontal, bubbleTextInset)
          .padding(.top, 4)
      }
      if let forwarded = maybeExtras?.maybeForwarded {
        Text("forwarded_from \(forwarded.authorName)")
          .font(.caption.italic())
          .foregroundStyle(.secondary)
          .padding(.horizontal, bubbleTextInset)
          .padding(.top, 4)
      }
      if let reply = maybeExtras?.maybeReply {
        BubbleReplyQuote(author: displayNames[reply.author] ?? "", excerpt: reply.excerpt, accent: quoteAccent)
          .padding(.top, showsName ? 4 : 0)
          .bubbleStretch()
      }
      if !images.isEmpty && !isDeleted {
        media.padding(.top, hasHeader ? 4 : 0)
      }
      if let placeholder = message.maybePlaceholder {
        PlaceholderLine(placeholder: placeholder)
          .padding(.horizontal, bubbleTextInset)
          .padding(.top, 3)
      } else if isDeleted {
        Text("message_deleted")
          .font(.body.italic())
          .foregroundStyle(.secondary)
          .padding(.horizontal, bubbleTextInset)
          .padding(.top, 3)
      } else if hasText {
        MessageBodyView(text: message.body, colors: markupColors, names: look.names)
          .padding(.horizontal, bubbleTextInset)
          .padding(.top, 3)
      }
      if !isDeleted {
        ForEach(otherFiles) { file in
          AttachmentView(file: file, isInBubble: true).padding(.top, 4)
        }
        if let preview = maybeExtras?.maybeLinkPreview {
          LinkPreviewCard(preview: preview, isInBubble: true).padding(.top, 4).bubbleStretch()
        }
      }
      BubbleFooter(message: message, maybeExtras: maybeExtras, isMine: isMine)
        .padding(.horizontal, bubbleTextInset - 2)
        .padding(.top, 2)
    }
    .padding(bubbleMediaInset)
    // Pictures set the bubble's width and the caption wraps to it.
    .frame(width: images.isEmpty || isDeleted ? nil : shownMediaWidth + bubbleMediaInset * 2)
    .foregroundStyle(isMine ? Theme.onOwnBubble : Theme.text)
    .tint(isMine ? Theme.onOwnBubble : Theme.accent)
    .background(isMine ? Theme.ownBubble : Theme.otherBubble, in: shape)
    .contentShape(shape)
  }

  private var message: MessageItem { entry.message }
  private var files: [FileInfo] { maybeExtras?.files ?? [] }
  private var images: [FileInfo] { files.filter { $0.isImage && $0.maybeThumbnailData != nil } }
  private var otherFiles: [FileInfo] { files.filter { file in !images.contains(file) } }
  private var isDeleted: Bool { maybeExtras?.isDeleted == true }
  private var hasHeader: Bool { showsName || maybeExtras?.maybeForwarded != nil || maybeExtras?.maybeReply != nil }

  private var hasText: Bool {
    !message.body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !isOnlyFileLabel(message.body, files: files)
  }

  /// The pictures' width: the bubble's room inside its edge (and beside the avatar).
  private var mediaWidth: CGFloat {
    let avatarRoom = look.showsSenders && !isMine ? senderAvatarSize + senderAvatarGap : 0
    return look.bubbleWidth - avatarRoom - bubbleMediaInset * 2
  }

  /// How wide the pictures show: a grid takes the room, one picture its own shape's width.
  private var shownMediaWidth: CGFloat {
    guard images.count == 1, let image = images.first.flatMap(ThumbnailCache.image(for:)) else { return mediaWidth }
    return bubblePictureSize(image, room: mediaWidth).width
  }

  @ViewBuilder
  private var media: some View {
    if images.count >= 2 {
      ImageGrid(images: images, width: mediaWidth)
    } else if let image = images.first {
      AttachmentView(file: image, maybeMediaWidth: mediaWidth)
    }
  }

  private var shape: UnevenRoundedRectangle {
    let corners = BubbleCorners(isMine: isMine, isContinuation: entry.isContinuation, isGroupEnd: entry.isGroupEnd)
    return UnevenRoundedRectangle(
      cornerRadii: RectangleCornerRadii(
        topLeading: corners.topLeading, bottomLeading: corners.bottomLeading,
        bottomTrailing: corners.bottomTrailing, topTrailing: corners.topTrailing),
      style: .continuous)
  }

  private var quoteAccent: Color { isMine ? Theme.onOwnBubble : Theme.accent }

  /// Inside an own (accent) bubble everything is in the bubble's text colour; in others' the
  /// accent and the mention colours of the app.
  private var markupColors: MarkupColors {
    if isMine {
      let content = Theme.onOwnBubble
      return MarkupColors(
        link: content, mention: content, mentionMe: content, mentionMeBackground: content.opacity(0.22),
        codeBackground: content.opacity(0.18))
    }
    return MarkupColors(
      link: Theme.accent, mention: Theme.accent, mentionMe: Theme.mentionText,
      mentionMeBackground: Theme.mentionBackground, codeBackground: Theme.text.opacity(0.08))
  }
}

/// What a placeholder bubble says (issue #888): waiting with a clock, not available with a lock,
/// failed with a warning. The clock is still: a row may wait for days, and nothing animates while
/// the app idles.
struct PlaceholderLine: View {
  let placeholder: PlaceholderItem

  var body: some View {
    Label {
      Text(text).font(.body.italic())
    } icon: {
      Image(systemName: symbol).font(.footnote)
    }
    .foregroundStyle(.secondary)
  }

  private var text: String { Self.text(placeholder) }

  static func text(_ placeholder: PlaceholderItem) -> String {
    String(localized: resource(placeholder))
  }

  static func resource(_ placeholder: PlaceholderItem) -> String.LocalizationValue {
    switch placeholder {
    case .waiting: "message_placeholder_waiting"
    case .notAvailable: "message_placeholder_not_available"
    case .failed: "message_placeholder_failed"
    }
  }

  private var symbol: String {
    switch placeholder {
    case .waiting: "clock"
    case .notAvailable: "lock"
    case .failed: "exclamationmark.triangle"
    }
  }
}

/// A message body in the shared Markdown subset, rendered like desktop and web: paragraphs,
/// headings, quotes, lists and code blocks. The core parses (`messageMarkup`), so every platform
/// reads a body the same way.
struct MessageBodyView: View {
  let text: String
  let colors: MarkupColors
  let names: MentionNames

  var body: some View {
    VStack(alignment: .leading, spacing: 6) {
      ForEach(Array(MarkupCache.blocks(text).enumerated()), id: \.offset) { _, block in
        blockView(block)
      }
    }
    .textSelection(.enabled)
  }

  @ViewBuilder
  private func blockView(_ block: MarkupBlock) -> some View {
    switch block {
    case .paragraph(let lines):
      Text(styled(lines)).font(.body)
    case .heading(let level, let line):
      Text(styled([line])).font(headingFont(level))
        .accessibilityAddTraits(.isHeader)
    case .quote(let lines):
      Text(styled(lines))
        .font(.body)
        .opacity(0.85)
        .padding(.leading, 11)
        .overlay(alignment: .leading) {
          RoundedRectangle(cornerRadius: 1.5).fill(HierarchicalShapeStyle.primary.opacity(0.45)).frame(width: 3)
        }
    case .items(let kind, let items):
      VStack(alignment: .leading, spacing: 2) {
        ForEach(Array(items.enumerated()), id: \.offset) { index, item in
          HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(marker(kind, index)).monospacedDigit()
            Text(styled([item]))
          }
          .font(.body)
        }
      }
    case .code(_, let code):
      // Monospace on its own surface, scrolled sideways rather than wrapped.
      ScrollView(.horizontal, showsIndicators: false) {
        Text(code).font(.callout.monospaced()).fixedSize().padding(.horizontal, 10).padding(.vertical, 8)
      }
      .background(colors.codeBackground, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }
  }

  private func styled(_ lines: [MarkupLine]) -> AttributedString {
    markupText(lines, colors: colors, names: names, permalinkLabel: String(localized: "message_link_label"))
  }

  private func headingFont(_ level: UInt8) -> Font {
    switch level {
    case 1: return .title3.weight(.semibold)
    case 2: return .headline
    default: return .subheadline.weight(.semibold)
    }
  }

  private func marker(_ kind: MarkupListKind, _ index: Int) -> String {
    switch kind {
    case .bulleted: return "•"
    case .numbered(let start): return "\(Int(start) + index)."
    }
  }
}

/// Parsed bodies by text: a list re-renders often, the markup only changes with the text.
enum MarkupCache {
  private final class Blocks {
    let blocks: [MarkupBlock]

    init(_ blocks: [MarkupBlock]) {
      self.blocks = blocks
    }
  }

  private static let cache: NSCache<NSString, Blocks> = {
    let cache = NSCache<NSString, Blocks>()
    cache.countLimit = 500
    return cache
  }()

  static func blocks(_ body: String) -> [MarkupBlock] {
    let key = body as NSString
    if let cached = cache.object(forKey: key) { return cached.blocks }
    let blocks = messageMarkup(body: body)
    cache.setObject(Blocks(blocks), forKey: key)
    return blocks
  }
}

/// Bottom trailing inside the bubble: pinned, "edited", the time and, for own messages, the state
/// (sending, sent, not sent), time first and the state at the far edge.
struct BubbleFooter: View {
  let message: MessageItem
  let maybeExtras: MessageExtras?
  let isMine: Bool

  var body: some View {
    HStack(spacing: 3) {
      if maybeExtras?.isPinned == true {
        Image(systemName: "pin.fill").accessibilityLabel(Text("pinned"))
      }
      if message.isEdited { Text("edited") }
      Text(message.sentDate, style: .time)
      if isMine { stateIcon }
    }
    .font(.caption2)
    .foregroundStyle(.secondary)
    .lineLimit(1)
    .accessibilityElement(children: .combine)
  }

  @ViewBuilder
  private var stateIcon: some View {
    if maybeExtras?.isSendFailed == true {
      Image(systemName: "exclamationmark.circle").accessibilityLabel(Text("not_sent"))
    } else if message.isPending {
      // Not confirmed by the server yet; the core keeps retrying in the background.
      Image(systemName: "clock").accessibilityLabel(Text("pending"))
    } else if maybeExtras?.isRead == true {
      // The other member of the DM read it: two ticks at the bubble's full text colour, where the
      // single "sent" tick is muted (desktop and web: two accent ticks, #769).
      DoubleCheckmark().foregroundStyle(Theme.onOwnBubble).accessibilityLabel(Text("message_read"))
    } else {
      Image(systemName: "checkmark").accessibilityLabel(Text("message_sent"))
    }
  }
}

/// Two overlapping ticks, the read state of an own message. SF Symbols has no double tick, so two
/// `checkmark`s share some room; the overlap scales with the text like the symbols do.
struct DoubleCheckmark: View {
  @ScaledMetric(relativeTo: .caption2) private var overlap: CGFloat = 5

  var body: some View {
    HStack(spacing: -overlap) {
      Image(systemName: "checkmark")
      Image(systemName: "checkmark")
    }
    .accessibilityElement(children: .ignore)
  }
}

/// Reactions, "not sent" and the thread link, under the bubble.
struct BelowBubble: View {
  @Environment(AppModel.self) private var model
  let message: MessageItem
  let maybeExtras: MessageExtras?
  let showsThreadLink: Bool
  let onOpenThread: () -> Void
  var canReact = true

  var body: some View {
    if maybeExtras?.isSendFailed == true { notSentLine }
    if !message.reactions.isEmpty {
      FlowStack(spacing: 4) {
        ForEach(message.reactions, id: \.emoji) { reaction in
          ReactionPill(reaction: reaction) {
            Task {
              await model.perform(
                .toggleReaction(conversationId: message.conversationId, messageId: message.id, emoji: reaction.emoji))
            }
          }
          .disabled(!canReact)
        }
      }
    }
    if showsThreadLink && message.replyCount > 0 {
      Button(action: onOpenThread) {
        Text("replies_count \(Int(message.replyCount))").font(.footnote.weight(.semibold))
      }
      .padding(.horizontal, 4)
    }
  }

  /// An own message the server rejected: say so, and offer to send it again or drop it.
  private var notSentLine: some View {
    HStack(spacing: 12) {
      Label("not_sent", systemImage: "exclamationmark.circle").font(.caption).foregroundStyle(Theme.danger)
      Button("not_sent_retry") {
        Task { await model.perform(.retrySend(conversationId: message.conversationId, messageId: message.id)) }
      }
      .font(.caption.weight(.semibold))
      Button("not_sent_delete", role: .destructive) {
        Task { await model.perform(.discardUnsent(conversationId: message.conversationId, messageId: message.id)) }
      }
      .font(.caption.weight(.semibold))
    }
  }
}

/// One reaction: the emoji and how many, outlined in the accent when the user is among them.
struct ReactionPill: View {
  let reaction: ReactionItem
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      Text("\(reaction.emoji) \(reaction.count)")
        .font(.caption)
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(reaction.reactedByMe ? Theme.accentSoft : Theme.surface, in: Capsule())
        .overlay(Capsule().stroke(reaction.reactedByMe ? Theme.accent : Theme.border))
    }
    .buttonStyle(.plain)
    .accessibilityAddTraits(reaction.reactedByMe ? .isSelected : [])
  }
}

/// The quoted message at the top of a reply's bubble: an accent bar, the author in the accent and
/// one line of the text, on a tint of the bubble's text colour.
struct BubbleReplyQuote: View {
  let author: String
  let excerpt: String
  let accent: Color

  var body: some View {
    VStack(alignment: .leading, spacing: 1) {
      if !author.isEmpty {
        Text(author).font(.footnote.weight(.semibold)).foregroundStyle(accent).lineLimit(1)
      }
      Text(excerpt).font(.footnote).foregroundStyle(.secondary).lineLimit(2)
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding(.leading, 11).padding(.trailing, 8).padding(.vertical, 6)
    .background(HierarchicalShapeStyle.primary.opacity(0.08))
    .overlay(alignment: .leading) { Rectangle().fill(accent).frame(width: 3) }
    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
    .accessibilityElement(children: .combine)
  }
}

/// A message's pictures as an album across the bubble, laid out as on every platform (the core's
/// `albumLayout`): two side by side, three with the first spanning both rows, four in two rows, and
/// "+N" on the last tile for the rest. A tap opens the viewer on that picture; it pages through all of
/// them.
struct ImageGrid: View {
  let images: [FileInfo]
  let width: CGFloat

  private let spacing: CGFloat = 3

  var body: some View {
    let layout =
      albumLayout(pictures: UInt32(clamping: images.count))
      ?? AlbumLayout(tiles: UInt32(clamping: images.count), hidden: 0, isFirstTall: false)
    let shown = Array(images.prefix(Int(layout.tiles)))
    let side = max(0, (width - spacing) / 2)
    Group {
      if layout.isFirstTall, shown.count == 3 {
        HStack(spacing: spacing) {
          tile(shown, 0, layout).frame(width: side, height: side * 2 + spacing)
          VStack(spacing: spacing) {
            tile(shown, 1, layout).frame(width: side, height: side)
            tile(shown, 2, layout).frame(width: side, height: side)
          }
        }
      } else {
        VStack(spacing: spacing) {
          ForEach(Array(stride(from: 0, to: shown.count, by: 2)), id: \.self) { row in
            HStack(spacing: spacing) {
              ForEach(row..<min(row + 2, shown.count), id: \.self) { index in
                tile(shown, index, layout).frame(width: side, height: side)
              }
            }
            .frame(width: width, alignment: .leading)
          }
        }
      }
    }
    .frame(width: width, alignment: .leading)
  }

  private func tile(_ shown: [FileInfo], _ index: Int, _ layout: AlbumLayout) -> some View {
    let isLast = index + 1 == Int(layout.tiles)
    return AttachmentView(
      file: shown[index], isTile: true, maybeHiddenCount: isLast && layout.hidden > 0 ? Int(layout.hidden) : nil)
  }
}

/// "Today", "Yesterday" or the date, as a small centred pill; pinned while its day scrolls by.
struct DayPill: View {
  let date: Date

  var body: some View {
    Text(label)
      .font(.footnote.weight(.medium))
      .foregroundStyle(Theme.textMuted)
      .padding(.horizontal, 12).padding(.vertical, 4)
      .background(.regularMaterial, in: Capsule())
      .frame(maxWidth: .infinity)
      .padding(.vertical, 6)
      .accessibilityAddTraits(.isHeader)
  }

  private var label: String {
    if Calendar.current.isDateInToday(date) { return String(localized: "day_today") }
    if Calendar.current.isDateInYesterday(date) { return String(localized: "day_yesterday") }
    return Self.showsYear(date, now: .now)
      ? date.formatted(.dateTime.weekday(.wide).day().month(.wide).year())
      : date.formatted(.dateTime.weekday(.wide).day().month(.wide))
  }

  /// Whether a day's label names its year: only outside the current year (#1086). `now` is read
  /// on every render, so the year appears after New Year's Eve without a timer.
  static func showsYear(_ date: Date, now: Date, calendar: Calendar = .current) -> Bool {
    !calendar.isDate(date, equalTo: now, toGranularity: .year)
  }
}

/// A round avatar with the sender's initials on their own colour.
struct SenderAvatar: View {
  let name: String
  let userId: String

  var body: some View {
    Text(AvatarView.initials(of: name))
      .font(.system(size: senderAvatarSize * 0.4, weight: .semibold))
      .foregroundStyle(.white)
      .frame(width: senderAvatarSize, height: senderAvatarSize)
      .background(PersonColor.avatar(userId), in: Circle())
      .accessibilityHidden(true)
  }
}

/// A person's own colour (from their ID), readable in light and dark.
enum PersonColor {
  static func name(_ userId: String) -> Color {
    let hue = personHue(userId)
    return Color(
      uiColor: UIColor { traits in
        let isDark = traits.userInterfaceStyle == .dark
        return UIColor(hue: hue, saturation: isDark ? 0.45 : 0.75, brightness: isDark ? 0.95 : 0.55, alpha: 1)
      })
  }

  static func avatar(_ userId: String) -> Color {
    Color(hue: personHue(userId), saturation: 0.5, brightness: 0.6)
  }
}

/// Marks a bubble child that takes the bubble's full width (a reply quote, a link preview) without
/// making the bubble wider than its other content needs.
private struct BubbleStretchKey: LayoutValueKey {
  static let defaultValue = false
}

extension View {
  fileprivate func bubbleStretch() -> some View {
    layoutValue(key: BubbleStretchKey.self, value: true)
  }
}

/// The bubble's column: as wide as its widest child needs (at most the room offered), children
/// leading, except the last one (the time) at the trailing edge, and stretching children across.
struct BubbleStack: Layout {
  func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
    let width = contentWidth(proposal: proposal, subviews: subviews)
    let height = subviews.reduce(0) { total, subview in
      total + subview.sizeThatFits(ProposedViewSize(width: width, height: nil)).height
    }
    return CGSize(width: width, height: height)
  }

  func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
    var y = bounds.minY
    for (index, subview) in subviews.enumerated() {
      let isStretching = subview[BubbleStretchKey.self]
      let size = subview.sizeThatFits(ProposedViewSize(width: bounds.width, height: nil))
      let width = isStretching ? bounds.width : min(size.width, bounds.width)
      let isLast = index == subviews.count - 1
      let x = isLast ? bounds.maxX - width : bounds.minX
      subview.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(width: width, height: size.height))
      y += size.height
    }
  }

  private func contentWidth(proposal: ProposedViewSize, subviews: Subviews) -> CGFloat {
    let room = proposal.width ?? .infinity
    let widths = subviews.map { subview -> CGFloat in
      // A stretching child counts with what it needs, not with all it could take.
      let offer = subview[BubbleStretchKey.self] ? ProposedViewSize.unspecified : ProposedViewSize(width: room, height: nil)
      return subview.sizeThatFits(offer).width
    }
    return min(widths.max() ?? 0, room)
  }
}

/// Children in rows, wrapping to the next row when one is full (reactions).
struct FlowStack: Layout {
  var spacing: CGFloat = 4

  func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
    let rows = arrange(width: proposal.width ?? .infinity, subviews: subviews)
    let width = rows.map(\.width).max() ?? 0
    let height = rows.map(\.height).reduce(0, +) + spacing * CGFloat(max(rows.count - 1, 0))
    return CGSize(width: width, height: height)
  }

  func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
    var y = bounds.minY
    for row in arrange(width: bounds.width, subviews: subviews) {
      var x = bounds.minX
      for index in row.indices {
        let size = subviews[index].sizeThatFits(.unspecified)
        subviews[index].place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
        x += size.width + spacing
      }
      y += row.height + spacing
    }
  }

  private struct Row {
    var indices: [Int] = []
    var width: CGFloat = 0
    var height: CGFloat = 0
  }

  private func arrange(width: CGFloat, subviews: Subviews) -> [Row] {
    var rows: [Row] = []
    var current = Row()
    for index in subviews.indices {
      let size = subviews[index].sizeThatFits(.unspecified)
      let needed = current.indices.isEmpty ? size.width : current.width + spacing + size.width
      if needed > width && !current.indices.isEmpty {
        rows.append(current)
        current = Row()
      }
      current.width = current.indices.isEmpty ? size.width : current.width + spacing + size.width
      current.height = max(current.height, size.height)
      current.indices.append(index)
    }
    if !current.indices.isEmpty { rows.append(current) }
    return rows
  }
}
