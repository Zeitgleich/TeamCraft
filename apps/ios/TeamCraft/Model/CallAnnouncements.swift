import Foundation

/// Who came and who went between two looks at the call's people, by user ID (#2281).
struct RosterChange: Equatable {
  var joined: [String] = []
  var left: [String] = []

  var isEmpty: Bool { joined.isEmpty && left.isEmpty }

  init() {}

  init(before: [String], now: [String]) {
    joined = now.filter { !before.contains($0) }.sorted()
    left = before.filter { !now.contains($0) }.sorted()
  }

  /// Both changes, a join and a leave of the same person inside one window cancelling out.
  func merged(with other: RosterChange) -> RosterChange {
    let cancelled = Set(joined).intersection(other.left).union(Set(left).intersection(other.joined))
    var result = RosterChange()
    result.joined = Self.unique((joined + other.joined).filter { !cancelled.contains($0) })
    result.left = Self.unique((left + other.left).filter { !cancelled.contains($0) })
    return result
  }

  private static func unique(_ ids: [String]) -> [String] {
    var seen = Set<String>()
    return ids.filter { seen.insert($0).inserted }
  }
}

/// What VoiceOver hears about the roster: up to `maxNamed` names, then a count.
enum RosterSentence {
  static let maxNamed = 3

  static func spoken(_ change: RosterChange, names: [String: String], in bundle: Bundle = .main) -> String {
    [
      sentence(change.joined, names: names, bundle: bundle, joined: true),
      sentence(change.left, names: names, bundle: bundle, joined: false),
    ].compactMap { $0 }.joined(separator: ". ")
  }

  private static func sentence(_ ids: [String], names: [String: String], bundle: Bundle, joined: Bool) -> String? {
    guard !ids.isEmpty else { return nil }
    if ids.count > maxNamed {
      return joined
        ? String(localized: "call_announce_joined_many \(ids.count)", bundle: bundle)
        : String(localized: "call_announce_left_many \(ids.count)", bundle: bundle)
    }
    let someone = String(localized: "call_recording_someone", bundle: bundle)
    let list = ids.map { names[$0].flatMap { $0.isEmpty ? nil : $0 } ?? someone }.joined(separator: ", ")
    return joined
      ? String(localized: "call_announce_joined \(list)", bundle: bundle)
      : String(localized: "call_announce_left \(list)", bundle: bundle)
  }
}

/// Collects roster changes for a short window and says them once: a meeting that fills up says
/// "5 people joined the call", not five sentences (#2281).
@MainActor
final class RosterAnnouncer {
  static let window: Duration = .milliseconds(1500)

  private var pending = RosterChange()
  private var flush: Task<Void, Never>?

  func note(_ change: RosterChange, names: [String: String], say: @escaping (String) -> Void) {
    guard !change.isEmpty else { return }
    pending = pending.merged(with: change)
    flush?.cancel()
    flush = Task { [weak self] in
      try? await Task.sleep(for: Self.window)
      guard !Task.isCancelled, let self else { return }
      let said = self.pending
      self.pending = RosterChange()
      let text = RosterSentence.spoken(said, names: names)
      if !text.isEmpty { say(text) }
    }
  }
}
