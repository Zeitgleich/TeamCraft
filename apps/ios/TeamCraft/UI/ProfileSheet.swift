import SwiftUI

/// Opens the profile of a person: by ID (the sender's avatar) or by username (an @mention), #2002.
struct OpenProfileAction {
  let run: (String) -> Void

  func callAsFunction(_ userId: String) { run(userId) }
}

private struct OpenProfileKey: EnvironmentKey {
  static let defaultValue: OpenProfileAction? = nil
}

extension EnvironmentValues {
  var openProfile: OpenProfileAction? {
    get { self[OpenProfileKey.self] }
    set { self[OpenProfileKey.self] = newValue }
  }
}

/// The URL an @mention of a person carries, so the conversation's link handler can open the
/// profile; the username is percent-encoded.
let profilePrefix = "teamcraft:/u/"

extension AppModel {
  /// The person with this ID, or this username (case-insensitive), from the core's people.
  func person(userId: String? = nil, username: String? = nil) async -> PeopleSnapshot.Person? {
    guard let json = try? await core().snapshot(), let people = Wire.decode(PeopleSnapshot.self, from: json) else {
      return nil
    }
    let all = [people.me] + (people.users ?? []) + (people.formerUsers ?? [])
    return all.first { person in
      if let userId { return person.id == userId }
      if let username { return person.username.caseInsensitiveCompare(username) == .orderedSame }
      return false
    }
  }
}

/// A person's profile as a sheet: name, @username, presence, custom status, and what one does
/// next: message them and call them.
struct ProfileSheet: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let person: PeopleSnapshot.Person
  let onOpenConversation: (String) -> Void

  @State private var isOpening = false
  @State private var isMe = false

  var body: some View {
    NavigationStack {
      VStack(spacing: 8) {
        AvatarView(name: person.shownName, size: 88)
        Text(verbatim: person.shownName).font(.app(.title2).weight(.semibold)).accessibilityAddTraits(.isHeader)
        if !person.username.isEmpty {
          Text(verbatim: "@\(person.username)").font(.app(.subheadline)).foregroundStyle(Theme.textMuted)
        }
        let presence = Presence(snapshotName: person.presence)
        HStack(spacing: 6) {
          PresenceDot(presence: presence)
          Text(presence.label).font(.app(.subheadline))
        }
        if let status = person.maybeCustomStatus, !status.text.isEmpty {
          // The emoji is the person's own content, not an icon of the app.
          Text(verbatim: "\(status.emoji) \(status.text)").font(.app(.subheadline)).multilineTextAlignment(.center)
        }
        if canContact {
          HStack(spacing: 12) {
            Button { Task { await open(thenCall: false) } } label: {
              Label("profile_message", systemImage: "bubble.left").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            Button { Task { await open(thenCall: true) } } label: {
              Label("profile_call", systemImage: "phone").frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
          }
          .disabled(isOpening)
          .padding(.top, 12)
        }
        Spacer(minLength: 0)
      }
      .padding()
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
      }
    }
    .presentationDetents([.medium])
    .task { isMe = await model.browseSnapshot()?.me.id == person.id }
  }

  /// Not for yourself, bots or removed accounts.
  private var canContact: Bool {
    !isMe && person.isBot != true && person.isDeleted != true
  }

  /// The direct conversation with the person, found or made; then the conversation opens, and
  /// the call starts in it when asked.
  private func open(thenCall: Bool) async {
    guard !isOpening else { return }
    isOpening = true
    defer { isOpening = false }
    let snapshot = await model.browseSnapshot()
    let conversationId: String
    if let existing = snapshot?.directConversation(with: [person.id]) {
      conversationId = existing
    } else if case .created(let id) = await model.openDirect([person.id]) {
      conversationId = id
    } else {
      return
    }
    dismiss()
    onOpenConversation(conversationId)
    if thenCall { model.calls.startCall(conversationId: conversationId, displayName: person.shownName) }
  }
}

extension Presence {
  /// The core's name of a presence in a snapshot (`Online`, `DoNotDisturb`, …); offline when unknown.
  init(snapshotName name: String?) {
    switch name {
    case "Online": self = .online
    case "Busy": self = .busy
    case "DoNotDisturb": self = .doNotDisturb
    case "BeRightBack": self = .beRightBack
    case "Away": self = .away
    default: self = .offline
    }
  }
}
