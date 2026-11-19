import Foundation

/// A poll as the core counted it (teamcraft-client-api `PollView`, #205, e2ee.md §4.12). While
/// `areResultsShown` is false every option's count is 0 and nobody is named; `voterCount` and the
/// user's own `isChosen` still are.
struct Poll: Decodable, Equatable, Sendable {
  let question: String
  let options: [PollOption]
  /// `single`, `multiple`, or a mode of a newer version that takes no votes here.
  let choice: String
  /// `shown` names who voted for what; anything else names nobody.
  let voters: String
  /// `always`, `after_vote` or `after_close`: when the counts show.
  var results = "always"
  var areResultsShown = true
  /// When the poll closes by itself, Unix milliseconds; the server's time decides.
  var maybeClosesAtMs: Int64?
  var voterCount: UInt32 = 0
  var isClosed = false
  /// The user made it.
  var isMine = false
  /// The user may close it: they made it, or the channel's signed roles make them a moderator (#2120).
  var canClose = false

  var isMultipleChoice: Bool { choice == "multiple" }
  /// Whether this version knows how to vote in it.
  var isKnownChoice: Bool { choice == "single" || choice == "multiple" }
  var areVotersShown: Bool { voters == "shown" }
  /// The options the user chose, by position.
  var myChoices: [UInt32] { options.indices.filter { options[$0].isChosen }.map { UInt32($0) } }
  var closesAt: Date? { maybeClosesAtMs.map { Date(timeIntervalSince1970: Double($0) / 1000) } }

  /// The option with the most votes, when exactly one has the most and anybody voted.
  var maybeLeader: Int? {
    guard let most = options.map(\.count).max(), most > 0 else { return nil }
    let leaders = options.indices.filter { options[$0].count == most }
    return leaders.count == 1 ? leaders.first : nil
  }

  private enum CodingKeys: String, CodingKey {
    case question, options, choice, voters, results, areResultsShown, voterCount, isClosed, isMine, canClose
    case maybeClosesAtMs = "closesAt"
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    question = try container.decode(String.self, forKey: .question)
    options = try container.decode([PollOption].self, forKey: .options)
    choice = try container.decode(String.self, forKey: .choice)
    voters = try container.decode(String.self, forKey: .voters)
    results = try container.decodeIfPresent(String.self, forKey: .results) ?? "always"
    areResultsShown = try container.decodeIfPresent(Bool.self, forKey: .areResultsShown) ?? true
    maybeClosesAtMs = try container.decodeIfPresent(Int64.self, forKey: .maybeClosesAtMs)
    voterCount = try container.decodeIfPresent(UInt32.self, forKey: .voterCount) ?? 0
    isClosed = try container.decodeIfPresent(Bool.self, forKey: .isClosed) ?? false
    isMine = try container.decodeIfPresent(Bool.self, forKey: .isMine) ?? false
    canClose = try container.decodeIfPresent(Bool.self, forKey: .canClose) ?? false
  }
}

/// One option of a `Poll`.
struct PollOption: Decodable, Equatable, Sendable {
  let text: String
  var count: UInt32 = 0
  var isChosen = false
  /// Who chose it, oldest vote first; empty unless the poll names voters and shows counts.
  var voters: [String] = []

  private enum CodingKeys: String, CodingKey { case text, count, isChosen, voters }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    text = try container.decode(String.self, forKey: .text)
    count = try container.decodeIfPresent(UInt32.self, forKey: .count) ?? 0
    isChosen = try container.decodeIfPresent(Bool.self, forKey: .isChosen) ?? false
    voters = try container.decodeIfPresent([String].self, forKey: .voters) ?? []
  }
}

/// When a new poll ends, as the sheet offers it.
enum PollClosing: String, CaseIterable, Identifiable, Sendable {
  case never
  case hour
  case day
  case week
  /// At the time picked in the sheet.
  case at

  var id: String { rawValue }

  /// How long after sending it closes, for the relative choices.
  var maybeAfter: TimeInterval? {
    switch self {
    case .hour: 3_600
    case .day: 86_400
    case .week: 7 * 86_400
    case .never, .at: nil
    }
  }

  var label: LocalizedStringResource {
    switch self {
    case .never: "poll_closes_never"
    case .hour: "poll_closes_hour"
    case .day: "poll_closes_day"
    case .week: "poll_closes_week"
    case .at: "poll_closes_at"
    }
  }
}
