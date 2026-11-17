import Foundation
import WidgetKit

/// Keeps the home screen widget (issue #2076) up to date from the store's own changes: the live sync
/// while the app is open, a background sync push, reading a conversation, a changed setting. No
/// timer: when nothing changes, nothing runs. Without a widget on the home screen it does nothing.
///
/// A message notification the extension shows doesn't move it: the extension only reads a copy in
/// memory (docs/e2ee.md §7.2), so the store learns of the message when the app syncs next.
@MainActor
final class HomeWidgetUpdater {
  private static let previewKey = "homeWidgetPreviews"
  /// Changes come in bursts while syncing; one update after they settle.
  private static let settleDelay: Duration = .seconds(1)

  private let store = WidgetSnapshotStore()
  private var maybePending: Task<Void, Never>?
  private var maybeShown: WidgetSnapshot?

  /// The widget's own opt-in to message previews; off by default.
  var isPreviewWanted: Bool {
    UserDefaults.standard.bool(forKey: Self.previewKey)
  }

  func setPreviewWanted(_ isWanted: Bool, model: AppModel) {
    UserDefaults.standard.set(isWanted, forKey: Self.previewKey)
    storeDidChange(model)
  }

  /// Something the widget shows may have changed: reads again once the changes settle.
  func storeDidChange(_ model: AppModel) {
    maybePending?.cancel()
    maybePending = Task { [weak self, weak model] in
      try? await Task.sleep(for: Self.settleDelay)
      guard !Task.isCancelled, let self, let model else { return }
      await self.refresh(model)
    }
  }

  /// Signed out: the widget forgets the account at once.
  func clear() {
    maybePending?.cancel()
    store.delete()
    maybeShown = nil
    WidgetCenter.shared.reloadTimelines(ofKind: HomeWidget.kind)
  }

  private func refresh(_ model: AppModel) async {
    guard await isPlaced(), let myId = model.maybeUserId else { return }
    guard let privacy = await privacy(model) else { return }
    let rows = privacy == .locked ? [] : await rows(model, myId: myId, isReadingNewest: privacy != .countOnly)
    let snapshot = HomeWidget.snapshot(privacy: privacy, rows: rows)
    guard snapshot != maybeShown else { return }
    do {
      try store.write(snapshot)
      maybeShown = snapshot
      WidgetCenter.shared.reloadTimelines(ofKind: HomeWidget.kind)
    } catch {
      AppLog.failure("writing the widget's snapshot", error)
    }
  }

  /// The app lock first (it reads nothing then); otherwise the stricter of this device's
  /// notification setting and the workspace policy, as the notifications themselves.
  private func privacy(_ model: AppModel) async -> WidgetPrivacy? {
    if SharedContainer().isAppLockOn { return .locked }
    guard let settings = try? await model.core().notificationContent() else { return nil }
    return HomeWidget.privacy(
      showsNames: settings.device != .nothing && settings.workspace != .nothing,
      showsContent: settings.device == .content && settings.workspace == .content,
      isAppLockOn: false, isPreviewWanted: isPreviewWanted)
  }

  /// The unread conversations in the Activity order (mentions, then the newest message), named as
  /// the list names them. With `isReadingNewest` the newest messages are read for the order and the
  /// preview line; `HomeWidget.snapshot` keeps the line only where previews may show.
  private func rows(_ model: AppModel, myId: String, isReadingNewest: Bool) async -> [WidgetRow] {
    let unread = activityConversations(await model.conversations())
    guard isReadingNewest, !unread.isEmpty else { return unread.map { row($0, maybePreview: nil) } }
    let wanted = Array(unread.prefix(HomeWidget.maxRows))
    let previews = await model.chatPreviews(wanted, myId: myId)
    let ordered = activityConversations(wanted, lastActivity: previews.mapValues(\.sentDate))
    return ordered.map { row($0, maybePreview: previews[$0.id].flatMap(Self.previewLine)) }
      + unread.dropFirst(HomeWidget.maxRows).map { row($0, maybePreview: nil) }
  }

  private func row(_ conversation: ConversationItem, maybePreview: String?) -> WidgetRow {
    let badge = rowBadge(
      unreadCount: conversation.unreadCount, mentionCount: conversation.mentionCount, isMuted: conversation.isMuted)
    return WidgetRow(
      conversationId: conversation.id, name: conversation.listName, countLabel: badge?.label ?? "",
      maybePreview: maybePreview)
  }

  /// The chat list's second line; events, deleted and unreadable messages show none here.
  private static func previewLine(_ preview: ChatPreview) -> String? {
    guard case .text(let text) = preview.content else { return nil }
    switch preview.sender {
    case .none: return text
    case .me: return String(localized: "chat_preview_sender \(String(localized: "you")) \(text)")
    case .other(let name): return String(localized: "chat_preview_sender \(name) \(text)")
    }
  }

  private func isPlaced() async -> Bool {
    let configurations = (try? await WidgetCenter.shared.currentConfigurations()) ?? []
    return configurations.contains { $0.kind == HomeWidget.kind }
  }
}
