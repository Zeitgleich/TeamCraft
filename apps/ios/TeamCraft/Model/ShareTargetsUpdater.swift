import Foundation
import UIKit

/// Keeps the share extension's conversation list (issue #1037, `ShareTargetsSnapshot`) up to date
/// from the store's own changes, like the home screen widget: when nothing changes, nothing runs.
/// The core is asked off the main thread (`shareTargets()` is async); only the file write is here.
/// Only in the foreground: `core()` would open a store closed for the suspension (#2179), and a
/// change seen in the background is read again with the next catch-up in the foreground.
@MainActor
final class ShareTargetsUpdater {
  /// Changes come in bursts while syncing; one update after they settle.
  private static let settleDelay: Duration = .seconds(2)

  private let store = ShareTargetsStore.shared()
  private var maybePending: Task<Void, Never>?
  private var maybeShown: ShareTargetsSnapshot?

  /// Something the list shows may have changed (a conversation, a name, the app lock).
  func storeDidChange(_ model: AppModel) {
    maybePending?.cancel()
    maybePending = Task { [weak self, weak model] in
      try? await Task.sleep(for: Self.settleDelay)
      guard !Task.isCancelled, let self, let model else { return }
      await self.refresh(model)
    }
  }

  /// Signed out: the extension forgets the account at once.
  func clear() {
    maybePending?.cancel()
    store.delete()
    maybeShown = nil
  }

  private func refresh(_ model: AppModel) async {
    // The app lock hides names outside the app; the extension then leaves the choice to the app.
    if SharedContainer().isAppLockOn {
      clear()
      return
    }
    guard UIApplication.shared.applicationState == .active, let owner = SharedContainer().activeAccount else { return }
    guard let targets = await model.loadShareTargets() else { return }
    let snapshot = ShareTargetsSnapshot.make(owner: owner, rows: targets.map(Self.row))
    // Signed out or locked meanwhile: nothing is written after `clear()`.
    guard !Task.isCancelled, !SharedContainer().isAppLockOn, SharedContainer().activeAccount == owner else { return }
    guard snapshot != maybeShown else { return }
    do {
      try store.write(snapshot)
      maybeShown = snapshot
    } catch {
      AppLog.failure("writing the share targets", error)
    }
  }

  private static func row(_ item: ConversationItem) -> ShareTargetRow {
    let kind: ShareTargetRow.Kind = item.isNotesToSelf ? .notes : (item.isDirect ? .direct : .channel)
    return ShareTargetRow(
      conversationId: item.id, name: item.listName, kind: kind,
      maybeSpaceName: kind == .channel ? item.maybeSpaceName : nil,
      maybeLastActivityMs: item.maybeLastActivityMs)
  }
}
