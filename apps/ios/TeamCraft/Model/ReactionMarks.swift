/// Which messages' unseen reactions the feed already reported as seen (issue #1009): each one once
/// while it waits, and again when a new reaction makes it wait again.
struct ReactionMarks: Equatable {
  private var reported: Set<String> = []

  /// The messages among `onScreen` whose reactions wait in `unseen` and weren't reported yet; they
  /// count as reported from now on.
  mutating func seen(onScreen: Set<String>, unseen: UnseenReactions) -> [String] {
    reported.formIntersection(unseen.messages)
    let fresh = unseen.messages.filter { onScreen.contains($0) && !reported.contains($0) }
    reported.formUnion(fresh)
    return fresh
  }

  /// The user jumped to `message`: seen now, whether or not its row is on screen yet.
  mutating func jumped(to message: String) -> Bool {
    reported.insert(message).inserted
  }
}
