import SwiftUI

/// The directory's tabs (#1337), as on the web. Guests have no directory beyond the people they
/// share a conversation with (docs/admin.md); Invitations is for those who may invite.
enum DirectoryTab: String, CaseIterable, Identifiable, Sendable {
  case people, channels, external, invitations

  var id: Self { self }

  var title: LocalizedStringKey {
    switch self {
    case .people: return "directory_people"
    case .channels: return "directory_channels"
    case .external: return "directory_external"
    case .invitations: return "directory_invitations"
    }
  }

  static func offered(isGuest: Bool, canInvite: Bool) -> [DirectoryTab] {
    if isGuest { return [.people] }
    return canInvite ? allCases : [.people, .channels, .external]
  }
}

/// A page of a server list, or why it could not be read.
enum PageAnswer<Value> {
  case loaded(Value)
  case failed(AppFailure)
}

/// One page of a server list: its rows and where the next starts (empty on the last).
struct DirectoryPage<Row> {
  let rows: [Row]
  let next: String
}

/// The rows of a server list read a page at a time (#1337): the first page when it starts, the
/// next when the list scrolls to its end. Nothing loads "all of them".
@MainActor
@Observable
final class DirectoryPager<Row: Identifiable> where Row.ID == String {
  enum Phase: Equatable {
    case loading, rows, empty
    case failed(AppFailure)
  }

  private(set) var rows: [Row] = []
  private(set) var phase = Phase.loading
  private(set) var maybeMoreFailure: AppFailure?
  @ObservationIgnored private var next = ""
  @ObservationIgnored private var maybeTask: Task<Void, Never>?
  @ObservationIgnored private let fetch: (String) async -> PageAnswer<DirectoryPage<Row>>

  init(fetch: @escaping (String) async -> PageAnswer<DirectoryPage<Row>>) {
    self.fetch = fetch
  }

  var hasMore: Bool { !next.isEmpty }

  func reload() {
    maybeTask?.cancel()
    rows = []
    next = ""
    phase = .loading
    maybeMoreFailure = nil
    maybeTask = Task { await load(after: "") }
  }

  func loadMore() {
    guard maybeTask == nil, !next.isEmpty else { return }
    maybeMoreFailure = nil
    let after = next
    maybeTask = Task { await load(after: after) }
  }

  /// Drops rows that are gone on the server (a revoked invitation).
  func remove(_ id: String) {
    rows.removeAll { $0.id == id }
    if rows.isEmpty, next.isEmpty, phase == .rows { phase = .empty }
  }

  private func load(after: String) async {
    let answer = await fetch(after)
    guard !Task.isCancelled else { return }
    maybeTask = nil
    switch answer {
    case .loaded(let page):
      // Pages never overlap on the server; a row that moved between two pages shows once.
      var known = Set(rows.map(\.id))
      rows += page.rows.filter { known.insert($0.id).inserted }
      next = page.next
      phase = rows.isEmpty ? .empty : .rows
    case .failed(let failure):
      if after.isEmpty { phase = .failed(failure) } else { maybeMoreFailure = failure }
    }
  }
}

// The generated types are public, so their conformances' witnesses must be too.
extension DirectoryPersonItem: Identifiable {
  public var id: String { userId }
}

extension DirectoryChannelItem: Identifiable {
  public var id: String { conversationId }
}

extension DirectoryInvitationItem: Identifiable {
  public var id: String { invitationId }
}

/// The directory's calls through the core (#1337). The server decides what each list holds.
extension AppModel {
  func directoryPeople(_ request: DirectoryPeopleRequest) async -> PageAnswer<DirectoryPage<DirectoryPersonItem>> {
    await page { core in
      let page = try await core.directoryPeople(request: request)
      return DirectoryPage(rows: page.people, next: page.nextAfter)
    }
  }

  /// The values the people filters offer; empty when the server can't say now.
  func directoryFilters(kind: DirectoryPeopleKind, spaceId: String) async -> DirectoryFilterChoices {
    let empty = DirectoryFilterChoices(organizations: [], departments: [], titles: [])
    return (try? await core().directoryFilters(kind: kind, spaceId: spaceId)) ?? empty
  }

  func directoryChannels(query: String, spaceId: String, after: String) async -> PageAnswer<DirectoryPage<DirectoryChannelItem>> {
    await page { core in
      let page = try await core.directoryChannels(query: query, spaceId: spaceId, after: after)
      return DirectoryPage(rows: page.channels, next: page.nextAfter)
    }
  }

  func directoryInvitations(query: String, spaceId: String, after: String) async -> PageAnswer<DirectoryPage<DirectoryInvitationItem>> {
    await page { core in
      let page = try await core.directoryInvitations(query: query, spaceId: spaceId, after: after)
      return DirectoryPage(rows: page.invitations, next: page.nextAfter)
    }
  }

  /// Invites somebody by e-mail; the outcome, or why the server didn't answer.
  func invitePeople(_ input: InvitePeopleInput) async -> Result<InviteOutcome, AppFailure> {
    do {
      return .success(try await core().invitePeople(input: input))
    } catch {
      return .failure(AppFailure(CoreFailure(error)))
    }
  }

  func resendInvitation(_ invitation: DirectoryInvitationItem) async -> Result<InvitationOutcome, AppFailure> {
    await resendInvitation(spaceId: invitation.spaceId, invitationId: invitation.invitationId)
  }

  func resendInvitation(spaceId: String, invitationId: String) async -> Result<InvitationOutcome, AppFailure> {
    do {
      return .success(try await core().resendInvitation(spaceId: spaceId, invitationId: invitationId))
    } catch {
      return .failure(AppFailure(CoreFailure(error)))
    }
  }

  func revokeInvitation(_ invitation: DirectoryInvitationItem) async -> Result<InvitationOutcome, AppFailure> {
    do {
      return .success(try await core().revokeInvitation(spaceId: invitation.spaceId, invitationId: invitation.invitationId))
    } catch {
      return .failure(AppFailure(CoreFailure(error)))
    }
  }

  private func page<Row>(_ call: (TeamCraft) async throws -> DirectoryPage<Row>) async -> PageAnswer<DirectoryPage<Row>> {
    do {
      return .loaded(try await call(core()))
    } catch {
      return .failed(AppFailure(CoreFailure(error)))
    }
  }
}

// MARK: Invite people

/// The fields of "Invite people", top to bottom.
enum InviteField: CaseIterable, Hashable {
  case email, note
}

enum InviteProblem: FieldProblem {
  case emailMissing, emailInvalid, emailRefused, alreadyInvited, noteTooLong

  var message: LocalizedStringKey {
    switch self {
    case .emailMissing: return "invite_email_missing"
    case .emailInvalid: return "invite_email_invalid"
    case .emailRefused: return "invite_email_refused"
    case .alreadyInvited: return "invite_already_invited"
    case .noteTooLong: return "create_description_too_long"
    }
  }
}

enum InviteRules {
  /// Longest personal note the server takes (`max_text_length`, default 1000).
  static let maxNoteCharacters = 1000

  /// The checks of an invitation before the server has its say (it also checks the address and
  /// which domains guests may come from).
  static func errors(email: String, note: String) -> FieldErrors<InviteField, InviteProblem> {
    let trimmed = email.trimmingCharacters(in: .whitespacesAndNewlines)
    var emailProblem: InviteProblem?
    if trimmed.isEmpty {
      emailProblem = .emailMissing
    } else if !isAddress(trimmed) {
      emailProblem = .emailInvalid
    }
    let noteProblem: InviteProblem? =
      note.trimmingCharacters(in: .whitespacesAndNewlines).count > maxNoteCharacters ? .noteTooLong : nil
    return FieldErrors([.email: emailProblem, .note: noteProblem])
  }

  /// Something on both sides of one `@`, a dot in the domain, no spaces.
  private static func isAddress(_ text: String) -> Bool {
    let parts = text.split(separator: "@", omittingEmptySubsequences: false)
    return parts.count == 2 && parts.allSatisfy { !$0.isEmpty } && parts[1].contains(".")
      && !text.contains(where: \.isWhitespace)
  }
}
