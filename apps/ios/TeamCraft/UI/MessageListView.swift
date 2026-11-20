import SwiftUI
import UIKit

/// A conversation's messages on a recycling collection view (issue #2180), the SwiftUI rows in
/// self-sizing cells. A `LazyVStack` only estimates the rows it hasn't drawn: a jump far down
/// landed on an estimate, the real heights moved the target away and the list showed nothing in
/// between ("each tap gets a bit further"). Here every jump lays out the rows it reached and
/// tries again until it lands (`MessageListScroll.maxSettlePasses`), rows that get their real
/// height above the reader move the scroll position with them, and a page of older messages keeps
/// the row the reader looks at in place.
///
/// The rows are the conversation's own views (`row`, `dayHeader`); the list only knows their
/// identities. A cell is configured again when its row changed (`MessageRowMarks.changed`) or
/// everything looks different (`look`), never on unrelated state of the conversation.
struct MessageListView: UIViewRepresentable {
  /// The open conversation or thread: another one starts over (positioned, measured).
  let placeId: String
  let feed: MessageFeed
  let top: MessageListTop
  let marks: MessageRowMarks
  let look: RowsLook
  /// The composer, the keyboard and the home indicator over the end of the list.
  let bottomInset: CGFloat
  /// The first read is in: the list positions itself and shows.
  let isLoaded: Bool
  /// Where it opens when not at the newest message: the first unread one (#2013).
  let maybeOpenAt: String?
  let reduceMotion: Bool
  let controller: MessageListController
  let row: (MessageListItem) -> AnyView
  let dayHeader: (Date) -> AnyView
  let onVisibleRows: (VisibleRows) -> Void
  let onAtBottom: (Bool) -> Void
  /// The list showed its first rows, positioned: the channel switch is done.
  let onShown: () -> Void
  /// Pull to refresh: reconnects; false when that didn't work.
  let onRefresh: () async -> Void

  /// What every row's look depends on: a change configures every cell on screen again.
  struct RowsLook: Equatable {
    var bubble: BubbleLook
    var isSelecting: Bool
    var canReact: Bool
    var canPost: Bool
    var displayNames: [String: String]
    /// The picture viewer or a video covers the list: animated pictures pause.
    var areAnimationsCovered: Bool
  }

  /// Leading and trailing room of the rows.
  static let listPadding: CGFloat = 8
  /// Room below the newest row, above the composer.
  static let bottomPadding: CGFloat = 8
  /// Rows around those on screen whose cells may exist (prefetched or just scrolled off).
  private static let liveMargin = 30

  func makeCoordinator() -> Coordinator { Coordinator(self) }

  func makeUIView(context: Context) -> MessageCollectionView {
    context.coordinator.makeView()
  }

  func updateUIView(_ view: MessageCollectionView, context: Context) {
    context.coordinator.update(self, isAnimated: context.transaction.animation != nil)
  }

  static func dismantleUIView(_ view: MessageCollectionView, coordinator: Coordinator) {
    coordinator.tearDown()
  }

  @MainActor
  final class Coordinator: NSObject, UICollectionViewDelegate {
    private var parent: MessageListView
    private var maybeView: MessageCollectionView?
    private var maybeLayout: MessageListLayout?
    private var maybeDataSource: UICollectionViewDiffableDataSource<MessageListSectionId, MessageListItem>?
    private var maybePlaceId: String?
    private var shownFeed = MessageFeed()
    private var shownTop = MessageListTop()
    private var shownMarks = MessageRowMarks()
    private var maybeShownLook: RowsLook?
    private var hasApplied = false
    private var hasPositioned = false
    private var isAtBottom = true
    private var visible = VisibleRows()
    private var maybeSettle: Settle?
    private var isPositionRetryQueued = false
    private var isSettling = false
    private let scrollBack = ScrollBackMeasure()
    private let jump = JumpMeasure()

    /// What an animated scroll lands on exactly once it ends.
    private enum Settle: Equatable {
      case bottom
      case item(MessageListItem, MessageListScroll.Place)
    }

    init(_ parent: MessageListView) {
      self.parent = parent
    }

    func makeView() -> MessageCollectionView {
      let layout = MessageListLayout { [weak self] index, _ in self?.section(at: index) }
      layout.onCorrection = { [weak self] in self?.scrollBack.corrected() }
      let view = MessageCollectionView(frame: .zero, collectionViewLayout: layout)
      view.backgroundColor = .clear
      view.alwaysBounceVertical = true
      view.keyboardDismissMode = .interactive
      // The insets come from SwiftUI (the composer floats over the list): set by `update`.
      view.contentInsetAdjustmentBehavior = .never
      view.delegate = self
      view.isAtBottom = { [weak self] in self?.isAtBottom ?? false }
      let refresh = UIRefreshControl()
      refresh.addTarget(self, action: #selector(refresh(_:)), for: .valueChanged)
      view.refreshControl = refresh
      maybeDataSource = makeDataSource(view)
      maybeView = view
      maybeLayout = layout
      parent.controller.coordinator = self
      return view
    }

    func update(_ next: MessageListView, isAnimated: Bool) {
      parent = next
      next.controller.coordinator = self
      guard let view = maybeView else { return }
      if maybePlaceId != next.placeId { startOver(next.placeId) }
      updateInsets(view, isAnimated: isAnimated)
      let sectionsChanged = !hasApplied || next.feed.revision != shownFeed.revision || next.top != shownTop
      let isLookChanged = maybeShownLook != nil && maybeShownLook != next.look
      let changed = changedRows(next, sectionsChanged: sectionsChanged)
      if sectionsChanged {
        apply(next, reconfiguring: changed)
      } else if isLookChanged {
        reconfigureOnScreen()
      } else if !changed.isEmpty {
        reconfigure(changed)
      }
      shownFeed = next.feed
      shownTop = next.top
      shownMarks = next.marks
      maybeShownLook = next.look
      positionIfDue()
    }

    var isScrolling: Bool { maybeView.map { $0.isTracking || $0.isDecelerating } ?? false }

    func tearDown() {
      scrollBack.cancel()
      jump.cancel()
      parent.controller.coordinator = nil
    }

    // MARK: Commands

    /// To the newest message. `isMeasured`: the reader's tap (feed.jump_bottom).
    func scrollToBottom(animated: Bool, isMeasured: Bool) {
      guard let view = maybeView, hasPositioned else { return }
      if isMeasured { jump.begin(key: parent.placeId) }
      view.layoutIfNeeded()
      let target = bottomOffset(view)
      if !animated || parent.reduceMotion || abs(target - view.contentOffset.y) < 1 {
        jump.end(passes: settleAtBottom(view), screens: 0)
        return
      }
      let screens = (target - view.contentOffset.y) / max(view.bounds.height, 1)
      jump.screens = screens
      maybeSettle = .bottom
      approach(view, screens: screens) { [weak self] in self.map { $0.bottomOffset(view) } ?? target }
    }

    /// To a message: under its day's pill (`top`) or in the middle (`center`).
    func scrollTo(_ messageId: String, place: MessageListScroll.Place, animated: Bool) {
      guard let view = maybeView else { return }
      let item = MessageListItem.message(messageId)
      view.layoutIfNeeded()
      guard let target = offset(showing: item, at: place, in: view) else { return }
      if !hasPositioned || !animated || parent.reduceMotion || abs(target - view.contentOffset.y) < 1 {
        settle(on: item, at: place, in: view)
        return
      }
      maybeSettle = .item(item, place)
      let screens = (target - view.contentOffset.y) / max(view.bounds.height, 1)
      approach(view, screens: screens) { [weak self] in
        self?.offset(showing: item, at: place, in: view) ?? target
      }
    }

    // MARK: UIScrollViewDelegate

    func scrollViewDidScroll(_ scrollView: UIScrollView) {
      // While a jump lays out what it passes, the rows on the way aren't what the reader sees.
      guard let view = maybeView, hasPositioned, !isSettling else { return }
      scrollBack.scrolled(offset: view.contentOffset.y, isDragging: view.isTracking, key: parent.placeId)
      report(view)
    }

    func scrollViewWillBeginDragging(_ scrollView: UIScrollView) {
      // The reader took over: an animated jump doesn't fight them.
      maybeSettle = nil
      jump.cancel()
    }

    func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate decelerate: Bool) {
      if !decelerate { rested(scrollView) }
    }

    func scrollViewDidEndDecelerating(_ scrollView: UIScrollView) { rested(scrollView) }

    func scrollViewDidEndScrollingAnimation(_ scrollView: UIScrollView) {
      guard let view = maybeView, let settle = maybeSettle else { return }
      maybeSettle = nil
      switch settle {
      case .bottom:
        jump.end(passes: settleAtBottom(view), screens: jump.screens)
      case .item(let item, let place):
        self.settle(on: item, at: place, in: view)
      }
      report(view)
    }

    func scrollViewDidScrollToTop(_ scrollView: UIScrollView) { rested(scrollView) }

    @objc private func refresh(_ control: UIRefreshControl) {
      Task { @MainActor [weak self] in
        await self?.parent.onRefresh()
        control.endRefreshing()
      }
    }

    // MARK: Building

    private func makeDataSource(_ view: UICollectionView)
      -> UICollectionViewDiffableDataSource<MessageListSectionId, MessageListItem>
    {
      let cell = UICollectionView.CellRegistration<MessageListCell, MessageListItem> { [weak self] cell, _, item in
        guard let self else { return }
        // Every row starts with fresh view state: a reused cell must not carry another message's.
        let content = parent.row(item).id(item)
        cell.contentConfiguration = UIHostingConfiguration { content }.margins(.all, 0)
        cell.backgroundConfiguration = .clear()
      }
      let header = UICollectionView.SupplementaryRegistration<MessageListCell>(
        elementKind: UICollectionView.elementKindSectionHeader
      ) { [weak self] header, _, indexPath in
        guard let self, let day = maybeDataSource?.sectionIdentifier(for: indexPath.section).flatMap(Self.day)
        else { return }
        let content = parent.dayHeader(day)
        header.contentConfiguration = UIHostingConfiguration { content }.margins(.all, 0)
        header.backgroundConfiguration = .clear()
      }
      let dataSource = UICollectionViewDiffableDataSource<MessageListSectionId, MessageListItem>(collectionView: view) {
        view, indexPath, item in
        view.dequeueConfiguredReusableCell(using: cell, for: indexPath, item: item)
      }
      dataSource.supplementaryViewProvider = { view, _, indexPath in
        view.dequeueConfiguredReusableSupplementary(using: header, for: indexPath)
      }
      return dataSource
    }

    /// One row per line, as tall as it is; a day's section is headed by its pinned pill.
    private func section(at index: Int) -> NSCollectionLayoutSection {
      let size = NSCollectionLayoutSize(widthDimension: .fractionalWidth(1), heightDimension: .estimated(72))
      let group = NSCollectionLayoutGroup.vertical(layoutSize: size, subitems: [NSCollectionLayoutItem(layoutSize: size)])
      let section = NSCollectionLayoutSection(group: group)
      section.contentInsets = NSDirectionalEdgeInsets(
        top: 0, leading: MessageListView.listPadding, bottom: 0, trailing: MessageListView.listPadding)
      if maybeDataSource?.sectionIdentifier(for: index).flatMap(Self.day) != nil {
        let header = NSCollectionLayoutBoundarySupplementaryItem(
          layoutSize: NSCollectionLayoutSize(widthDimension: .fractionalWidth(1), heightDimension: .estimated(36)),
          elementKind: UICollectionView.elementKindSectionHeader, alignment: .top)
        header.pinToVisibleBounds = true
        header.zIndex = 2
        section.boundarySupplementaryItems = [header]
      }
      return section
    }

    private static func day(_ section: MessageListSectionId) -> Date? {
      if case .day(let day, _) = section { return day }
      return nil
    }

    // MARK: Updating

    private func startOver(_ placeId: String) {
      scrollBack.cancel()
      scrollBack.reset()
      jump.cancel()
      maybePlaceId = placeId
      hasApplied = false
      hasPositioned = false
      isAtBottom = true
      maybeSettle = nil
      visible = VisibleRows()
      shownFeed = MessageFeed()
      shownTop = MessageListTop()
      shownMarks = MessageRowMarks()
      maybeShownLook = nil
    }

    private func updateInsets(_ view: MessageCollectionView, isAnimated: Bool) {
      let bottom = parent.bottomInset + MessageListView.bottomPadding
      guard view.contentInset.bottom != bottom else { return }
      let keepsBottom = hasPositioned && isAtBottom
      let change = {
        view.contentInset.bottom = bottom
        view.verticalScrollIndicatorInsets.bottom = self.parent.bottomInset
        if keepsBottom { view.contentOffset.y = self.bottomOffset(view) }
      }
      // The keyboard or the composer moves with an animation: the newest message moves with it.
      if isAnimated && hasPositioned && !parent.reduceMotion {
        UIView.animate(withDuration: 0.25, delay: 0, options: [.beginFromCurrentState, .curveEaseOut], animations: change)
      } else {
        change()
      }
    }

    /// The rows to configure again: on a new feed every message that changed, else those around
    /// the screen whose marks changed.
    private func changedRows(_ next: MessageListView, sectionsChanged: Bool) -> [MessageListItem] {
      let old = (feed: shownFeed, marks: shownMarks)
      let new = (feed: next.feed, marks: next.marks)
      let candidates: [String] =
        sectionsChanged
        ? next.feed.ids.filter { shownFeed.positions[$0] != nil }
        : Array(next.feed.ids(around: visible, margin: MessageListView.liveMargin))
      var items = MessageRowMarks.changed(candidates, from: old, to: new).map { MessageListItem.message($0) }
      if sectionsChanged {
        // A hole whose request started, failed or ended shows that.
        for (after, gap) in next.feed.gaps where shownFeed.gaps[after].map({ $0 != gap }) ?? false {
          items.append(.gap(after: after))
        }
      }
      return items
    }

    /// New rows: the row the reader looks at stays where it is, the newest stays at the bottom.
    private func apply(_ next: MessageListView, reconfiguring changed: [MessageListItem]) {
      guard let view = maybeView, let dataSource = maybeDataSource else { return }
      var snapshot = NSDiffableDataSourceSnapshot<MessageListSectionId, MessageListItem>()
      for section in next.feed.sections(top: next.top) {
        snapshot.appendSections([section.id])
        snapshot.appendItems(section.items, toSection: section.id)
      }
      let kept = changed.filter { snapshot.indexOfItem($0) != nil }
      // The same rows in the same order (a message edited, a reaction): only those rows are
      // configured again; the scroll position is left alone, also in the middle of a fling.
      let shown = dataSource.snapshot()
      if hasApplied && hasPositioned && shown.sectionIdentifiers == snapshot.sectionIdentifiers
        && shown.itemIdentifiers == snapshot.itemIdentifiers
      {
        reconfigure(kept)
        return
      }
      if !kept.isEmpty { snapshot.reconfigureItems(kept) }
      let keepsBottom = !hasPositioned || isAtBottom
      let maybeAnchor = keepsBottom ? nil : anchor(view)
      let isArrival = hasPositioned && keepsBottom && !next.reduceMotion && next.feed.isArrival(after: shownFeed)
      if let anchor = maybeAnchor {
        // The layout moves the scroll position with the update itself: the rows that came above
        // are never laid out at the old position, and a fling goes on.
        maybeLayout?.maybeTargetOffset = { [weak self, weak view] in
          guard let self, let view, let indexPath = self.maybeDataSource?.indexPath(for: anchor.item),
            let frame = view.collectionViewLayout.layoutAttributesForItem(at: indexPath)?.frame
          else { return nil }
          return max(frame.minY - anchor.minY, -view.adjustedContentInset.top)
        }
      }
      isSettling = true
      dataSource.apply(snapshot, animatingDifferences: isArrival)
      isSettling = false
      maybeLayout?.maybeTargetOffset = nil
      hasApplied = true
      guard hasPositioned else { return }
      view.layoutIfNeeded()
      if keepsBottom {
        if isArrival {
          maybeSettle = .bottom
          animate(view, to: bottomOffset(view))
        } else {
          settleAtBottom(view)
        }
      } else if let anchor = maybeAnchor {
        keep(anchor, in: view)
      }
      report(view)
    }

    private func reconfigureOnScreen() {
      guard let view = maybeView, let dataSource = maybeDataSource else { return }
      let items = view.indexPathsForVisibleItems.compactMap { dataSource.itemIdentifier(for: $0) }
      reconfigure(items)
      // The pills too: a new text size changes their height.
      for indexPath in view.indexPathsForVisibleSupplementaryElements(ofKind: UICollectionView.elementKindSectionHeader) {
        guard let header = view.supplementaryView(forElementKind: UICollectionView.elementKindSectionHeader, at: indexPath)
            as? UICollectionViewCell,
          let day = dataSource.sectionIdentifier(for: indexPath.section).flatMap(Self.day)
        else { continue }
        let content = parent.dayHeader(day)
        header.contentConfiguration = UIHostingConfiguration { content }.margins(.all, 0)
      }
    }

    private func reconfigure(_ items: [MessageListItem]) {
      guard let dataSource = maybeDataSource, !items.isEmpty else { return }
      var snapshot = dataSource.snapshot()
      let kept = items.filter { snapshot.indexOfItem($0) != nil }
      guard !kept.isEmpty else { return }
      snapshot.reconfigureItems(kept)
      dataSource.apply(snapshot, animatingDifferences: false)
    }

    /// The first time the list has its rows and a size: at the first unread message, or at the
    /// newest; hidden until then, so the reader never sees it move there.
    private func positionIfDue() {
      guard !hasPositioned, let view = maybeView else { return }
      guard parent.isLoaded else {
        view.alpha = 1
        return
      }
      guard view.bounds.height > 0 else {
        view.alpha = 0
        // No size yet (the first update comes before the first layout): once it has one.
        guard !isPositionRetryQueued else { return }
        isPositionRetryQueued = true
        DispatchQueue.main.async { [weak self] in
          self?.isPositionRetryQueued = false
          self?.positionIfDue()
        }
        return
      }
      view.layoutIfNeeded()
      if let id = parent.maybeOpenAt, maybeDataSource?.indexPath(for: .message(id)) != nil {
        settle(on: .message(id), at: .top, in: view)
      } else {
        settleAtBottom(view)
      }
      hasPositioned = true
      view.alpha = 1
      report(view)
      DispatchQueue.main.async { [weak self] in self?.parent.onShown() }
    }

    // MARK: Scrolling

    private func bottomOffset(_ view: UIScrollView) -> CGFloat {
      MessageListScroll.bottomOffset(
        contentHeight: view.contentSize.height, viewHeight: view.bounds.height,
        insetTop: view.adjustedContentInset.top, insetBottom: view.adjustedContentInset.bottom)
    }

    /// Lands on the newest row exactly: each pass lays out the rows the last one reached.
    @discardableResult
    private func settleAtBottom(_ view: UICollectionView) -> Int {
      settle(view) { [weak self] in self?.bottomOffset(view) }
    }

    @discardableResult
    private func settle(on item: MessageListItem, at place: MessageListScroll.Place, in view: UICollectionView) -> Int {
      settle(view) { [weak self] in self?.offset(showing: item, at: place, in: view) }
    }

    @discardableResult
    private func settle(_ view: UICollectionView, target: () -> CGFloat?) -> Int {
      var passes = 0
      while passes < MessageListScroll.maxSettlePasses {
        view.layoutIfNeeded()
        guard let offset = target(), abs(view.contentOffset.y - offset) >= 0.5 else { break }
        // Set as a property: `setContentOffset(_:animated:)` would end a fling in flight.
        isSettling = true
        view.contentOffset = CGPoint(x: view.contentOffset.x, y: offset)
        isSettling = false
        passes += 1
      }
      return passes
    }

    /// An animated scroll: a far one first skips to a screen before the target without animation
    /// (rows laid out there), then moves the rest, which lands where it was aimed.
    private func approach(_ view: UICollectionView, screens: CGFloat, target: @escaping () -> CGFloat) {
      if abs(screens) > MessageListScroll.farScreens {
        let lead = view.bounds.height * 0.75 * (screens > 0 ? 1 : -1)
        settle(view) { target() - lead }
      }
      animate(view, to: target())
    }

    private func animate(_ view: UIScrollView, to offset: CGFloat) {
      if abs(view.contentOffset.y - offset) < 1 {
        // Nothing to animate, so no end of an animation either: land now.
        scrollViewDidEndScrollingAnimation(view)
        return
      }
      view.setContentOffset(CGPoint(x: view.contentOffset.x, y: offset), animated: true)
    }

    private func offset(showing item: MessageListItem, at place: MessageListScroll.Place, in view: UICollectionView)
      -> CGFloat?
    {
      guard let indexPath = maybeDataSource?.indexPath(for: item),
        let frame = view.collectionViewLayout.layoutAttributesForItem(at: indexPath)?.frame
      else { return nil }
      return MessageListScroll.offset(
        showing: frame.minY, frame.maxY, at: place, viewHeight: view.bounds.height,
        insetTop: view.adjustedContentInset.top, insetBottom: view.adjustedContentInset.bottom,
        bottomOffset: bottomOffset(view))
    }

    /// The topmost row on screen and where it is: what stays in place when rows come above it.
    private func anchor(_ view: UICollectionView) -> (item: MessageListItem, minY: CGFloat)? {
      guard let dataSource = maybeDataSource else { return nil }
      let visibleTop = view.contentOffset.y + view.adjustedContentInset.top
      return view.indexPathsForVisibleItems.sorted()
        .compactMap { indexPath -> (MessageListItem, CGRect)? in
          guard let item = dataSource.itemIdentifier(for: indexPath),
            let frame = view.collectionViewLayout.layoutAttributesForItem(at: indexPath)?.frame
          else { return nil }
          return (item, frame)
        }
        .first { $0.1.maxY > visibleTop }
        .map { ($0.0, $0.1.minY - view.contentOffset.y) }
    }

    private func keep(_ anchor: (item: MessageListItem, minY: CGFloat), in view: UICollectionView) {
      settle(view) { [weak self] in
        guard let indexPath = self?.maybeDataSource?.indexPath(for: anchor.item),
          let frame = view.collectionViewLayout.layoutAttributesForItem(at: indexPath)?.frame
        else { return nil }
        let offset = MessageListScroll.keepingAnchor(
          offset: view.contentOffset.y, anchorBefore: view.contentOffset.y + anchor.minY, anchorAfter: frame.minY)
        return min(max(offset, -view.adjustedContentInset.top), self?.bottomOffset(view) ?? offset)
      }
    }

    /// Tells the conversation which rows are on screen and whether the reader is at the bottom,
    /// each only when it changed, after the current update.
    private func report(_ view: UICollectionView) {
      guard let dataSource = maybeDataSource else { return }
      let atBottom = MessageListScroll.isAtBottom(offset: view.contentOffset.y, bottomOffset: bottomOffset(view))
      if atBottom != isAtBottom {
        isAtBottom = atBottom
        DispatchQueue.main.async { [weak self] in self?.parent.onAtBottom(atBottom) }
      }
      let items = view.indexPathsForVisibleItems.compactMap { dataSource.itemIdentifier(for: $0) }
      let rows = VisibleRows(items, in: parent.feed)
      if rows != visible {
        visible = rows
        DispatchQueue.main.async { [weak self] in self?.parent.onVisibleRows(rows) }
      }
    }

    private func rested(_ scrollView: UIScrollView) {
      guard hasPositioned else { return }
      scrollBack.rested(screenHeight: scrollView.bounds.height)
      if let view = maybeView { report(view) }
    }
  }
}

/// A cell of the list. A hosted SwiftUI row adds the safe area it overlaps to the height it
/// reports: a row reaching under the home indicator grew by a pixel on every layout pass, until
/// UIKit stopped the app with a recursive layout loop (#2180). The list sets its own insets, so its
/// rows have no safe area.
final class MessageListCell: UICollectionViewCell {
  override var safeAreaInsets: UIEdgeInsets { .zero }
}

/// Jumps to the list from the conversation's buttons (to the newest, to a message), after the
/// update that brought its rows.
@MainActor
final class MessageListController {
  fileprivate weak var coordinator: MessageListView.Coordinator?

  /// The reader's finger moves the list, or it still rolls on.
  var isScrolling: Bool { coordinator?.isScrolling ?? false }

  func scrollToBottom(animated: Bool, isMeasured: Bool = false) {
    DispatchQueue.main.async { [weak self] in
      self?.coordinator?.scrollToBottom(animated: animated, isMeasured: isMeasured)
    }
  }

  func scrollTo(_ messageId: String, place: MessageListScroll.Place, animated: Bool) {
    DispatchQueue.main.async { [weak self] in
      self?.coordinator?.scrollTo(messageId, place: place, animated: animated)
    }
  }
}

/// Keeps the newest message at the bottom while the reader reads there and rows below change
/// height (a picture loaded, a reaction added); never while the reader's finger moves the list.
final class MessageCollectionView: UICollectionView {
  var isAtBottom: () -> Bool = { false }

  override var safeAreaInsets: UIEdgeInsets { .zero }

  override func layoutSubviews() {
    let keepsBottom = isAtBottom() && !isTracking && !isDecelerating && window != nil
    let height = contentSize.height
    super.layoutSubviews()
    guard keepsBottom, contentSize.height != height else { return }
    let bottom = MessageListScroll.bottomOffset(
      contentHeight: contentSize.height, viewHeight: bounds.height, insetTop: adjustedContentInset.top,
      insetBottom: adjustedContentInset.bottom)
    if abs(contentOffset.y - bottom) >= 0.5 { contentOffset.y = bottom }
  }
}

/// The layout of the list: a row that gets its real height above the top of the screen moves the
/// scroll position by as much, so what the reader sees doesn't jump while they scroll back.
final class MessageListLayout: UICollectionViewCompositionalLayout {
  /// Each such correction, for feed.scroll_back's `scroll_corrections`.
  var onCorrection: () -> Void = {}
  /// While rows are added above the reader: where the scroll position goes with the update.
  var maybeTargetOffset: (() -> CGFloat?)?

  override func targetContentOffset(forProposedContentOffset proposed: CGPoint) -> CGPoint {
    guard let y = maybeTargetOffset?() else { return super.targetContentOffset(forProposedContentOffset: proposed) }
    return CGPoint(x: proposed.x, y: y)
  }

  override func invalidationContext(
    forPreferredLayoutAttributes preferredAttributes: UICollectionViewLayoutAttributes,
    withOriginalAttributes originalAttributes: UICollectionViewLayoutAttributes
  ) -> UICollectionViewLayoutInvalidationContext {
    let context = super.invalidationContext(
      forPreferredLayoutAttributes: preferredAttributes, withOriginalAttributes: originalAttributes)
    guard let view = collectionView, originalAttributes.representedElementCategory == .cell else { return context }
    let adjustment = MessageListScroll.adjustment(
      rowMinY: originalAttributes.frame.minY, oldHeight: originalAttributes.frame.height,
      newHeight: preferredAttributes.frame.height, visibleTop: view.contentOffset.y + view.adjustedContentInset.top)
    if adjustment != 0 {
      context.contentOffsetAdjustment.y += adjustment
      onCorrection()
    }
    return context
  }
}

/// feed.scroll_back on iOS (docs/crash-reports.md): the reader's first scroll up in a conversation
/// until it rests, at least a screen back; its frames and how often rows above moved the scroll
/// position. A shorter scroll back is dropped and the next one is measured.
@MainActor
private final class ScrollBackMeasure {
  private let frames = FrameWatch()
  private var maybeKey: String?
  private var startOffset: CGFloat = 0
  private var lowestOffset: CGFloat = 0
  private var lastOffset: CGFloat = 0
  private var corrections = 0
  private var isDone = false

  func scrolled(offset: CGFloat, isDragging: Bool, key: String) {
    defer { lastOffset = offset }
    if maybeKey != nil {
      lowestOffset = min(lowestOffset, offset)
      return
    }
    guard !isDone, isDragging, offset < lastOffset else { return }
    Perf.begin(.scrollBack, key: key)
    // Left out by the trace rate, or crash reports off: nothing counts frames, and this
    // conversation's scroll back isn't tried again (else every frame of the drag would start and
    // drop a transaction).
    guard Perf.isRunning(.scrollBack, key: key) else {
      isDone = true
      return
    }
    maybeKey = key
    startOffset = lastOffset
    lowestOffset = offset
    corrections = 0
    frames.start()
  }

  func corrected() { if maybeKey != nil { corrections += 1 } }

  func rested(screenHeight: CGFloat) {
    guard let key = maybeKey else { return }
    maybeKey = nil
    let stats = frames.stop()
    guard startOffset - lowestOffset >= screenHeight else {
      Perf.cancel(.scrollBack, key: key)
      return
    }
    Perf.frames(.scrollBack, key: key, stats)
    Perf.measurement(.scrollBack, key: key, name: "scroll_corrections", value: Double(corrections))
    Perf.end(.scrollBack, key: key)
    isDone = true
  }

  func cancel() {
    guard let key = maybeKey else { return }
    maybeKey = nil
    frames.stop()
    Perf.cancel(.scrollBack, key: key)
  }

  /// Another conversation: its first scroll back is measured again.
  func reset() { isDone = false }
}

/// feed.jump_bottom (docs/crash-reports.md): a tap on "jump to the newest message" until the list
/// rests there; how far it was, how many passes it took to land and how smooth it was.
@MainActor
private final class JumpMeasure {
  private let frames = FrameWatch()
  private var maybeKey: String?
  var screens: CGFloat = 0

  func begin(key: String) {
    cancel()
    Perf.begin(.jumpToBottom, key: key)
    guard Perf.isRunning(.jumpToBottom, key: key) else { return }
    maybeKey = key
    screens = 0
    frames.start()
  }

  func end(passes: Int, screens: CGFloat) {
    guard let key = maybeKey else { return }
    maybeKey = nil
    Perf.frames(.jumpToBottom, key: key, frames.stop())
    Perf.measurement(.jumpToBottom, key: key, name: "settle_passes", value: Double(passes))
    Perf.measurement(.jumpToBottom, key: key, name: "screens", value: Double(max(screens, 0).rounded()))
    Perf.end(.jumpToBottom, key: key)
  }

  func cancel() {
    guard let key = maybeKey else { return }
    maybeKey = nil
    frames.stop()
    Perf.cancel(.jumpToBottom, key: key)
  }
}
