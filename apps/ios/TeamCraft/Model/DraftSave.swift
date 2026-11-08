import Foundation

/// Whether the composer's text waits for the typing pause before it is stored as the draft (#1391).
/// Each edit restarts the pause. Leaving (another conversation, the thread closes, the app goes to
/// the background) stores a waiting text at once, or the last words typed are lost. Nothing is
/// stored when nothing waits, also not the draft that was just read on opening.
struct DraftSave: Equatable {
  /// A text that waits to be stored, with the conversation and thread it was typed in.
  struct Pending: Equatable {
    let conversationId: String
    let maybeThreadId: String?
    let text: String
  }

  private var maybePending: Pending?
  /// The text the core is known to hold for the place; nil once the composer changed it.
  private var maybeKnown: String?

  /// The draft was read from the core: it is no edit.
  mutating func loaded(_ text: String) {
    maybePending = nil
    maybeKnown = text
  }

  /// The composer text changed to `text` in that conversation and thread.
  mutating func edit(_ text: String, conversationId: String, threadId: String?) {
    if text == maybeKnown {
      maybePending = nil
    } else {
      maybeKnown = nil
      maybePending = Pending(conversationId: conversationId, maybeThreadId: threadId, text: text)
    }
  }

  /// The pause after typing `text` is over: what to store now, unless a later edit replaced it.
  mutating func takeDue(for text: String) -> Pending? {
    guard maybePending?.text == text else { return nil }
    return takePending()
  }

  /// The composer or the app goes: what to store now, if something waits.
  mutating func takePending() -> Pending? {
    guard let pending = maybePending else { return nil }
    maybePending = nil
    maybeKnown = pending.text
    return pending
  }
}
