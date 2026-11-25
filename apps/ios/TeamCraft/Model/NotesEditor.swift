import Foundation
import Observation

/// Editing a page of the notes on the phone (issue #2361, e2ee.md §4.11): the core's editor of the
/// page (the one desktop and web run) and the block the user types in. Every edit goes to the core in
/// order and comes back as the page with it; the core says when the edits of a burst of typing go out
/// (`flushAfterMs`), and they go out at once when the page is left or the app goes to the background.
@MainActor
@Observable
final class NotesEditor {
  /// `nil` for a page ID beyond the rules: the page then shows read-only.
  let maybeEditor: CanvasPageEditor?
  /// The block the user types in, and where the caret goes when it opens; `nil` while nobody types.
  private(set) var maybeFocus: CanvasFocusItem?
  private(set) var canUndo = false
  private(set) var canRedo = false
  /// Edits handed to the core whose answer hasn't come: a page read meanwhile may not have them yet.
  private(set) var inFlight = 0
  /// Why the last edit failed, shown under the page until the next one goes through.
  private(set) var maybeFailure: String?
  /// The page after each edit.
  var onPage: (CanvasPageItem) -> Void = { _ in }

  @ObservationIgnored private let model: AppModel
  /// The last queued piece of work: each waits for the one before, so edits reach the core in order.
  @ObservationIgnored private var maybeTail: Task<Void, Never>?
  @ObservationIgnored private var maybeFlush: Task<Void, Never>?

  init(model: AppModel, conversationId: String, pageId: String?) {
    self.model = model
    maybeEditor = try? CanvasPageEditor(conversationId: conversationId, maybePageId: pageId)
  }

  func edit(_ edit: CanvasEdit) {
    guard let editor = maybeEditor else { return }
    inFlight += 1
    enqueue { [self] in
      do {
        let outcome = try await model.canvasEdit(editor, edit)
        inFlight -= 1
        maybeFailure = nil
        onPage(outcome.page)
        canUndo = outcome.canUndo
        canRedo = outcome.canRedo
        if let focus = outcome.maybeFocus { maybeFocus = focus }
        scheduleFlush(outcome.flushAfterMs)
      } catch {
        inFlight -= 1
        maybeFailure = AppFailure(CoreFailure(error)).message
      }
    }
  }

  /// The user taps a block to type in it, the caret at its end.
  func startEditing(_ blockId: String, caret: Int) {
    maybeFocus = CanvasFocusItem(blockId: blockId, caret: UInt32(max(caret, 0)))
  }

  /// Done typing: the waiting edits go out now.
  func stopEditing() {
    maybeFocus = nil
    flushNow()
  }

  func flushNow() {
    guard let editor = maybeEditor else { return }
    maybeFlush?.cancel()
    enqueue { [self] in await flush(editor) }
  }

  /// The page is left: the waiting edits go out and the core lets go of the editor, after the
  /// edits still on their way.
  func close() {
    guard let editor = maybeEditor else { return }
    maybeFlush?.cancel()
    let model = model
    enqueue {
      // Nothing to tell: the sheet is gone; an edit that didn't go out is in the core's outbox or lost
      // with the editor, as on desktop when its view closes.
      try? await model.closeCanvasEditor(editor)
    }
  }

  private func scheduleFlush(_ maybeAfterMs: UInt32?) {
    guard let editor = maybeEditor else { return }
    maybeFlush?.cancel()
    guard let afterMs = maybeAfterMs else { return }
    maybeFlush = Task { [weak self] in
      try? await Task.sleep(for: .milliseconds(Int(afterMs)))
      guard !Task.isCancelled, let self else { return }
      enqueue { [self] in await flush(editor) }
    }
  }

  private func flush(_ editor: CanvasPageEditor) async {
    do {
      try await model.flushCanvasEdits(editor)
    } catch {
      maybeFailure = AppFailure(CoreFailure(error)).message
    }
  }

  private func enqueue(_ work: @escaping @MainActor () async -> Void) {
    let previous = maybeTail
    maybeTail = Task { @MainActor in
      await previous?.value
      await work()
    }
  }
}

extension AppModel {
  /// The page an editor shows, with its own edits not sent yet (#2361); `nil` when the core can't answer.
  func canvasEditorPage(_ editor: CanvasPageEditor) async -> CanvasPageItem? {
    try? await core().canvasEditorPage(editor: editor)
  }

  /// Makes an edit on the editor's page: the page with it, and when to send the waiting edits.
  func canvasEdit(_ editor: CanvasPageEditor, _ edit: CanvasEdit) async throws -> CanvasEditOutcome {
    try await core().canvasEdit(editor: editor, edit: edit)
  }

  /// Sends the edits waiting in the editor: typing paused, or the page is left.
  func flushCanvasEdits(_ editor: CanvasPageEditor) async throws {
    try await core().flushCanvasEdits(editor: editor)
  }

  /// The page is left: the editor's waiting edits go out and the core lets go of it.
  func closeCanvasEditor(_ editor: CanvasPageEditor) async throws {
    try await core().closeCanvasEditor(editor: editor)
  }

  /// A page's versions as this device holds them, newest first; `nil` when the core can't answer.
  func canvasVersions(_ conversationId: String, pageId: String?) async -> [CanvasVersionItem]? {
    try? await core().canvasVersions(conversationId: conversationId, maybePageId: pageId)
  }

  /// The page as it was at `version`; `nil` when this device no longer has that version or the core
  /// can't answer.
  func canvasVersionPage(_ conversationId: String, pageId: String?, version: CanvasVersionKey) async
    -> [CanvasBlockItem]?
  {
    // `try?` flattens the core's optional: a failure reads as "not on this device".
    try? await core().canvasVersionPage(conversationId: conversationId, maybePageId: pageId, version: version)
  }
}
