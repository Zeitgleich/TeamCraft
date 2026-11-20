import CoreGraphics
import Foundation

/// A row of a conversation's message list (issue #2180): what the collection view's data source
/// keys its cells by. Only identities: what a row shows is read when its cell is configured.
enum MessageListItem: Hashable, Sendable {
  /// The shape of the first messages while none are read yet (#2008).
  case skeleton
  /// Older history on its way, above the oldest row.
  case olderLoading
  /// The last request for older history failed: "Try again".
  case olderFailed
  /// A hole in the history (#1438), by the newest message before it.
  case gap(after: String)
  case message(String)
}

/// A section of the list: the rows above the first day, then one per day under its date pill.
enum MessageListSectionId: Hashable, Sendable {
  case top
  /// A day, and how many sections of the same day came before (normally none).
  case day(Date, Int)
}

struct MessageListSection: Equatable {
  let id: MessageListSectionId
  var items: [MessageListItem]

  /// The day whose pill heads the section; none for the top.
  var maybeDay: Date? {
    if case .day(let day, _) = id { return day }
    return nil
  }
}

/// The rows above the oldest message (the skeleton, loading or failed older history).
struct MessageListTop: Equatable {
  var showsSkeleton = false
  var older = OlderRow.none

  enum OlderRow: Equatable {
    case none
    case loading
    case failed
  }

  var items: [MessageListItem] {
    var items: [MessageListItem] = []
    if showsSkeleton { items.append(.skeleton) }
    switch older {
    case .none: break
    case .loading: items.append(.olderLoading)
    case .failed: items.append(.olderFailed)
    }
    return items
  }
}

/// A conversation's messages laid out for the list: built once when the messages or the holes
/// change, never while a view renders (#2180). `revision` grows with every build, so the list
/// knows without comparing every row whether anything changed.
struct MessageFeed: Equatable {
  var revision = 0
  /// The days, oldest first; the holes above the first message are the first items of `aboveGaps`.
  var days: [MessageListSection] = []
  var aboveGaps: [MessageListItem] = []
  var entries: [String: FeedEntry] = [:]
  var gaps: [String: HistoryGap] = [:]
  /// The page's message IDs, oldest first, and each one's position among them.
  var ids: [String] = []
  var positions: [String: Int] = [:]

  var messageCount: Int { ids.count }

  /// The list's sections with `top`'s rows and the holes above the first message in the first.
  func sections(top: MessageListTop) -> [MessageListSection] {
    [MessageListSection(id: .top, items: top.items + aboveGaps)] + days
  }

  var maybeFirstId: String? { ids.first }
  var maybeLastId: String? { ids.last }

  /// The newest rows were added at the end and nothing else moved: a message arrived.
  func isArrival(after old: MessageFeed) -> Bool {
    old.messageCount > 0 && messageCount > old.messageCount && maybeFirstId == old.maybeFirstId
      && maybeLastId != old.maybeLastId
  }

  /// The IDs of the messages within `margin` rows of `visible` (the newest when none is on
  /// screen yet): the rows whose cells may exist.
  func ids(around visible: VisibleRows, margin: Int) -> ArraySlice<String> {
    guard !ids.isEmpty else { return [] }
    let first = visible.maybeFirst ?? ids.count - 1
    let last = visible.maybeLast ?? ids.count - 1
    let lower = min(max(first - margin, 0), ids.count - 1)
    let upper = min(max(last + margin, lower), ids.count - 1)
    return ids[lower...upper]
  }

  /// Where a row lies among the page's messages; a hole counts as the message before it.
  func position(of item: MessageListItem) -> Int? {
    switch item {
    case .message(let id): positions[id]
    case .gap(let after): gaps[after].map { Int($0.index) - 1 }.map { max($0, 0) }
    case .skeleton, .olderLoading, .olderFailed: nil
    }
  }
}

/// Builds the list's days from a page of messages (oldest first) and its holes (`feedSections`).
func messageFeed(
  _ messages: [Message], gaps: [HistoryGap], revision: Int, calendar: Calendar = .current
) -> MessageFeed {
  // The data source refuses a row or a section twice (it would crash): the first one counts.
  var seen: Set<MessageListItem> = []
  func unique(_ items: [MessageListItem]) -> [MessageListItem] { items.filter { seen.insert($0).inserted } }

  let messages = messages.filter { seen.insert(.message($0.id)).inserted }
  seen = []
  let placed = PlacedGaps(gaps, among: messages)
  var feed = MessageFeed(revision: revision)
  for gap in placed.above + placed.after.values.flatMap({ $0 }) { feed.gaps[gap.after] = gap }
  feed.aboveGaps = unique(placed.above.map { .gap(after: $0.after) })
  feed.ids = messages.map(\.id)
  for (position, id) in feed.ids.enumerated() { feed.positions[id] = position }
  var daysSeen: [Date: Int] = [:]
  for section in feedSections(messages, gaps: placed, calendar: calendar) {
    var items: [MessageListItem] = []
    for entry in section.entries {
      feed.entries[entry.id] = entry
      items += unique([.message(entry.id)] + entry.gapsAfter.map { .gap(after: $0.after) })
    }
    // A day seen again (a message sent with a wrong clock between two others) is a section of its own.
    let repeats = daysSeen[section.day, default: 0]
    daysSeen[section.day] = repeats + 1
    feed.days.append(MessageListSection(id: .day(section.day, repeats), items: items))
  }
  return feed
}

/// The message rows on screen, as positions among the page's messages (oldest first).
struct VisibleRows: Equatable {
  var ids: Set<String> = []
  var maybeFirst: Int?
  var maybeLast: Int?

  init() {}

  init(_ items: [MessageListItem], in feed: MessageFeed) {
    for item in items {
      if case .message(let id) = item { ids.insert(id) }
      guard let position = feed.position(of: item) else { continue }
      maybeFirst = min(maybeFirst ?? position, position)
      maybeLast = max(maybeLast ?? position, position)
    }
  }

  /// The positions of the message rows on screen in `feed`, which may be newer than the one they
  /// were seen in (a page added above moves every position).
  func range(in feed: MessageFeed) -> ClosedRange<Int>? {
    let positions = ids.compactMap { feed.positions[$0] }
    guard let first = positions.min(), let last = positions.max() else { return nil }
    return first...last
  }

  /// The reader is within `ahead` rows of the oldest loaded message: the next older page is due
  /// (issue #1058), a few screens before the reader meets the end of what is loaded.
  func isNearOldest(in feed: MessageFeed, ahead: Int) -> Bool {
    guard let range = range(in: feed) else { return false }
    return range.lowerBound <= ahead
  }
}

/// The message rows whose cells show something else now: the message, its extras, whether it is
/// highlighted, picked or the first unread. Rows whose cell is on screen are configured again;
/// the rest are configured when they scroll in.
struct MessageRowMarks: Equatable {
  var extras: [String: MessageExtras] = [:]
  var maybeHighlighted: String?
  var picked: Set<String> = []
  var maybeFirstUnread: String?

  /// Of `ids`, those whose row differs between `old` and `new`.
  static func changed(
    _ ids: some Sequence<String>, from old: (feed: MessageFeed, marks: MessageRowMarks),
    to new: (feed: MessageFeed, marks: MessageRowMarks)
  ) -> [String] {
    ids.filter { id in
      old.feed.entries[id] != new.feed.entries[id] || old.marks.extras[id] != new.marks.extras[id]
        || (old.marks.maybeHighlighted == id) != (new.marks.maybeHighlighted == id)
        || old.marks.picked.contains(id) != new.marks.picked.contains(id)
        || (old.marks.maybeFirstUnread == id) != (new.marks.maybeFirstUnread == id)
    }
  }
}

/// The list's scroll arithmetic (issue #2180), apart from UIKit so it can be tested.
enum MessageListScroll {
  /// Within this distance of the end the reader reads at the bottom: new messages follow, the
  /// jump button hides.
  static let bottomTolerance: CGFloat = 44
  /// Room left above a row scrolled to the top: its day's pill floats there.
  static let topMargin: CGFloat = 44
  /// A jump further than this many screens skips most of the way without animation, then moves
  /// the last screen (an animated scroll over rows never measured lands short).
  static let farScreens: CGFloat = 1.5
  /// Tries to land exactly: each lays out the rows the last one reached, whose real heights move
  /// the target. Two or three are usual.
  static let maxSettlePasses = 6

  /// The offset at which the last row rests at the bottom of the visible area.
  static func bottomOffset(contentHeight: CGFloat, viewHeight: CGFloat, insetTop: CGFloat, insetBottom: CGFloat)
    -> CGFloat
  {
    max(-insetTop, contentHeight + insetBottom - viewHeight)
  }

  static func isAtBottom(offset: CGFloat, bottomOffset: CGFloat) -> Bool {
    bottomOffset - offset <= bottomTolerance
  }

  /// The offset that shows a row whose frame is `minY...maxY` at `place`, within the content.
  static func offset(
    showing minY: CGFloat, _ maxY: CGFloat, at place: Place, viewHeight: CGFloat, insetTop: CGFloat,
    insetBottom: CGFloat, bottomOffset: CGFloat
  ) -> CGFloat {
    let visibleHeight = viewHeight - insetTop - insetBottom
    let wanted: CGFloat =
      switch place {
      case .top: minY - insetTop - topMargin
      case .center: (minY + maxY) / 2 - insetTop - visibleHeight / 2
      }
    return min(max(wanted, -insetTop), max(bottomOffset, -insetTop))
  }

  /// The offset that keeps the row the reader looked at in place after rows above it came or
  /// went: it moved from `anchorBefore` to `anchorAfter` in the content.
  static func keepingAnchor(offset: CGFloat, anchorBefore: CGFloat, anchorAfter: CGFloat) -> CGFloat {
    offset + anchorAfter - anchorBefore
  }

  /// How far the scroll position follows a row that got its real height: only a row that starts
  /// above the top of the visible area pushes what the reader sees.
  static func adjustment(rowMinY: CGFloat, oldHeight: CGFloat, newHeight: CGFloat, visibleTop: CGFloat) -> CGFloat {
    rowMinY < visibleTop ? newHeight - oldHeight : 0
  }

  enum Place: Equatable {
    /// Under the day pill: the first unread message.
    case top
    /// A linked, quoted or reacted-to message.
    case center
  }
}

/// How smooth a stretch of frames was (feed.scroll_back, feed.jump_bottom): the longest frame
/// and how many took longer than a reader notices.
struct FrameStats: Equatable {
  /// A frame longer than this is a stall the reader sees (as on web and desktop).
  static let slowFrameMs: Double = 50

  var frames = 0
  var slowFrames = 0
  var longestFrameMs: Double = 0

  mutating func add(frameMs: Double) {
    frames += 1
    if frameMs > Self.slowFrameMs { slowFrames += 1 }
    longestFrameMs = max(longestFrameMs, frameMs)
  }
}

/// What scrolling the message list changes and no view shows (the rows on screen, the reactions
/// reported seen): a reference the conversation keeps outside its state, so scrolling re-renders
/// nothing (#2180).
@MainActor
final class ListScrollState {
  var rows = VisibleRows()
  var reactionMarks = ReactionMarks()
}
