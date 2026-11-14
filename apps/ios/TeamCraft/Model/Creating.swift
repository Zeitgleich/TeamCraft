import SwiftUI

/// Starting things from the phone (#1534): the directory search behind the people picker, and the
/// create calls of the "New channel" and "New space" forms. The core awaits the server, so a form
/// closes only once the server took it and the new conversation or space opens right away.
extension AppModel {
  /// People whose name or username matches `query` (the server's directory included), without
  /// the user and bots, at most `limit`. `nil` when the core can't answer now; the directory's own
  /// answer arrives later as a change.
  func searchPeople(_ query: String, limit: UInt32 = 100) async -> [BrowseSnapshot.Person]? {
    guard let json = try? await core().searchPeople(query: query, limit: limit) else { return nil }
    return Wire.decode(PeopleAnswer.self, from: json)?.people
  }

  /// Whether "New space" shows: the workspace role allows creating spaces. False while the server
  /// can't tell (offline).
  func canCreateSpaces() async -> Bool {
    (try? await core().canCreateSpaces()) ?? false
  }

  /// Opens, or finds, the DM (one person) or group DM (several) with exactly these people.
  func openDirect(_ userIds: [String]) async -> CreateResult {
    await create { try await $0.openDirect(userIds: userIds) }
  }

  func createChannel(_ input: NewChannelInput) async -> CreateResult {
    await create { try await $0.createChannel(input: input) }
  }

  func createSpace(_ input: NewSpaceInput) async -> CreateResult {
    await create { try await $0.createSpace(input: input) }
  }

  /// Adds people to a channel, to its space first where the user may (#1534); nil once everybody
  /// is in, else why not.
  func addPeople(_ userIds: [String], to conversationId: String) async -> LocalizedStringKey? {
    do {
      switch try await core().addPeopleToChannel(conversationId: conversationId, userIds: userIds) {
      case .added: return nil
      case .notInSpace: return "members_add_not_in_space"
      case .notAllowed: return "create_not_allowed"
      // Their app is from before the channel anchor (e2ee.md §4.6.5): named, nobody was added.
      case .mustUpdate(let name): return "error_must_update \(name)"
      }
    } catch {
      return AppFailure(CoreFailure(error)).message
    }
  }

  private func create(_ call: (TeamCraft) async throws -> CreateOutcome) async -> CreateResult {
    do {
      return CreateResult(try await call(core()))
    } catch {
      return .failed(AppFailure(CoreFailure(error)))
    }
  }
}

/// The `people` answer of `search_people`.
private struct PeopleAnswer: Decodable {
  let people: [BrowseSnapshot.Person]
}

/// How a create form's request ended.
enum CreateResult: Equatable {
  /// Created or found: the new conversation's or space's ID.
  case created(String)
  /// The server refused the name: shown at the name field.
  case nameRefused
  /// The channel name breaks the name rules (#1842): the problem this device found, or `nil` when
  /// the server refused it.
  case channelNameRefused(ChannelNameProblem?)
  /// The role no longer allows it: shown next to the button.
  case notAllowed
  case failed(AppFailure)

  init(_ outcome: CreateOutcome) {
    switch outcome {
    case .created(let id): self = .created(id)
    case .nameRefused: self = .nameRefused
    case .channelNameRefused(let maybeProblem): self = .channelNameRefused(maybeProblem)
    case .notAllowed: self = .notAllowed
    }
  }
}

// MARK: Form rules

/// The fields of "New channel" and "New space", top to bottom.
enum CreateField: CaseIterable, Hashable {
  case name, description
}

enum CreateProblem: FieldProblem {
  /// The key of what to enter.
  case nameMissing(String)
  case nameTooLong, nameInvalid, descriptionTooLong, nameRefused
  /// A channel name breaking the name rules (#1842): the key of its text.
  case channelName(String)

  var message: LocalizedStringKey {
    switch self {
    case .nameMissing(let key), .channelName(let key): return LocalizedStringKey(key)
    case .nameTooLong: return "create_name_too_long"
    case .nameInvalid: return "create_name_invalid"
    case .descriptionTooLong: return "create_description_too_long"
    case .nameRefused: return "create_name_refused"
    }
  }
}

enum CreateRules {
  /// Longest name the server takes (`max_name_length`, default 100).
  static let maxNameCharacters = 100
  /// Longest description the server takes (`max_text_length`, default 1000).
  static let maxDescriptionCharacters = 1000

  /// The checks of a new channel's or space's name and description, the same as the server's. A
  /// channel's name also keeps the channel name rules (e2ee.md §4.6.5): `maybeChannelProblem` is
  /// what the core's check found (`ChannelAnchor.problem(of:)`).
  static func errors(
    name: String, description: String, missing key: String, maybeChannelProblem: ChannelNameProblem? = nil
  ) -> FieldErrors<CreateField, CreateProblem> {
    let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
    var nameProblem: CreateProblem?
    if trimmed.isEmpty {
      nameProblem = .nameMissing(key)
    } else if trimmed.count > maxNameCharacters {
      nameProblem = .nameTooLong
    } else if let problem = maybeChannelProblem, let ruleKey = ChannelAnchor.problemKey(problem) {
      nameProblem = .channelName(ruleKey)
    } else if name.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) {
      nameProblem = .nameInvalid
    }
    let descriptionTooLong = description.trimmingCharacters(in: .whitespacesAndNewlines).count > maxDescriptionCharacters
    return FieldErrors([.name: nameProblem, .description: descriptionTooLong ? .descriptionTooLong : nil])
  }
}
