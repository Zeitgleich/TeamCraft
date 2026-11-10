import SwiftUI

/// The directory across all the user's spaces, a sheet of its own (Home's "New" menu, #1337).
struct DirectoryView: View {
  @Environment(\.dismiss) private var dismiss
  let onOpen: (String) -> Void

  var body: some View {
    NavigationStack {
      DirectoryContent(maybeSpaceId: nil) { conversationId in
        dismiss()
        onOpen(conversationId)
      }
      .navigationTitle("directory_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
      }
    }
  }
}

/// People, channels, external people and invitations, each paged from the server (#1337). With
/// `maybeSpaceId` every list shows that space only (a space's People view); without, all the
/// user's spaces.
struct DirectoryContent: View {
  @Environment(AppModel.self) private var model
  let maybeSpaceId: String?
  let onOpen: (String) -> Void

  @State private var maybeSnapshot: BrowseSnapshot?
  @State private var chosen = DirectoryTab.people
  @State private var isInviting = false
  /// An invitation sent from the sheet shows in the list once the sheet closed.
  @State private var invitationsGeneration = 0

  var body: some View {
    let isGuest = maybeSnapshot?.me.isGuest == true
    let inviting = invitingSpaces
    let tabs = DirectoryTab.offered(isGuest: isGuest, canInvite: !inviting.isEmpty)
    let tab = tabs.contains(chosen) ? chosen : .people
    let onInvite: (() -> Void)? = inviting.isEmpty ? nil : { isInviting = true }
    let spaceId = maybeSpaceId ?? ""
    VStack(spacing: 0) {
      if tabs.count > 1 {
        Picker("directory_title", selection: $chosen) {
          ForEach(tabs) { entry in Text(entry.title).tag(entry) }
        }
        .pickerStyle(.segmented)
        .padding(.horizontal)
        .padding(.bottom, 8)
      }
      switch tab {
      case .people:
        if isGuest && maybeSpaceId != nil {
          // A guest sees no roster of a space, only the people they share a conversation with.
          DirectoryHint(text: "space_people_hidden")
        } else {
          DirectoryPeopleList(
            spaceId: spaceId, kind: isGuest ? .everybody : .members, maybeSnapshot: maybeSnapshot, onOpen: onOpen,
            maybeOnInvite: onInvite)
        }
      case .external:
        DirectoryPeopleList(spaceId: spaceId, kind: .guests, maybeSnapshot: maybeSnapshot, onOpen: onOpen, maybeOnInvite: onInvite)
      case .channels:
        DirectoryChannelList(spaceId: spaceId, maybeSnapshot: maybeSnapshot, onOpen: onOpen)
      case .invitations:
        DirectoryInvitationList(spaceId: spaceId, generation: invitationsGeneration, maybeOnInvite: onInvite)
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    .task(id: model.changeCount) {
      if let snapshot = await model.browseSnapshot() { maybeSnapshot = snapshot }
    }
    .sheet(isPresented: $isInviting) {
      InvitePeopleView(spaces: inviting) { invitationsGeneration += 1 }
    }
  }

  /// The spaces the user may invite people to, within the space on screen.
  private var invitingSpaces: [BrowseSnapshot.Space] {
    (maybeSnapshot?.spaces ?? []).filter { space in
      (maybeSpaceId == nil || space.id == maybeSpaceId) && space.canInvite
    }
  }
}

// MARK: People

private struct DirectoryPeopleList: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let kind: DirectoryPeopleKind
  let maybeSnapshot: BrowseSnapshot?
  let onOpen: (String) -> Void
  let maybeOnInvite: (() -> Void)?

  @State private var text = ""
  @State private var query = ""
  @State private var organization = ""
  @State private var department = ""
  @State private var title = ""
  @State private var choices = DirectoryFilterChoices(organizations: [], departments: [], titles: [])
  @State private var maybePager: DirectoryPager<DirectoryPersonItem>?
  @State private var isOpening = false

  var body: some View {
    VStack(spacing: 0) {
      DirectoryTools(text: $text, prompt: "directory_people_filter", maybeOnInvite: maybeOnInvite)
      if !choices.organizations.isEmpty || !choices.departments.isEmpty || !choices.titles.isEmpty {
        ScrollView(.horizontal, showsIndicators: false) {
          HStack(spacing: 8) {
            DirectoryFilterMenu(label: "directory_filter_organization", values: choices.organizations, chosen: $organization)
            DirectoryFilterMenu(label: "directory_filter_department", values: choices.departments, chosen: $department)
            DirectoryFilterMenu(label: "directory_filter_title", values: choices.titles, chosen: $title)
          }
          .padding(.horizontal)
        }
        .padding(.bottom, 8)
      }
      PagedList(maybePager: maybePager, empty: kind == .guests ? "directory_external_empty" : "directory_people_empty") { person in
        PersonRow(person: person, isMe: person.userId == maybeSnapshot?.me.id) { Task { await message(person) } }
      }
    }
    .debounced(text, into: $query)
    .task(id: "\(kind)/\(spaceId)") {
      guard kind != .everybody else { return }
      choices = await model.directoryFilters(kind: kind, spaceId: spaceId)
    }
    .task(id: [kind == .guests ? "g" : "m", spaceId, query, organization, department, title].joined(separator: "\u{1F}")) {
      let request = DirectoryPeopleRequest(
        query: query, kind: kind, organization: organization, department: department, title: title, spaceId: spaceId, after: "")
      let pager = DirectoryPager<DirectoryPersonItem> { [model] after in
        var paged = request
        paged.after = after
        return await model.directoryPeople(paged)
      }
      maybePager = pager
      pager.reload()
    }
  }

  private func message(_ person: DirectoryPersonItem) async {
    guard !isOpening else { return }
    if let existing = maybeSnapshot?.directConversation(with: [person.userId]) {
      onOpen(existing)
      return
    }
    isOpening = true
    if case .created(let id) = await model.openDirect([person.userId]) { onOpen(id) }
    isOpening = false
  }
}

/// A profile filter: a menu of its values; "Any" clears it. Hidden while it offers nothing.
private struct DirectoryFilterMenu: View {
  let label: LocalizedStringKey
  let values: [String]
  @Binding var chosen: String

  var body: some View {
    if !values.isEmpty || !chosen.isEmpty {
      Menu {
        Button("directory_filter_any") { chosen = "" }
        ForEach(values, id: \.self) { value in
          Button(value) { chosen = value }
        }
      } label: {
        HStack(spacing: 4) {
          if chosen.isEmpty {
            Text(label)
          } else {
            Text(label) + Text(verbatim: ": \(chosen)")
          }
          Image(systemName: "chevron.down").font(.app(.caption))
        }
        .lineLimit(1)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(chosen.isEmpty ? Color.clear : Theme.accentSoft, in: Capsule())
        .overlay(Capsule().stroke(Theme.border))
      }
    }
  }
}

private struct PersonRow: View {
  let person: DirectoryPersonItem
  let isMe: Bool
  let onMessage: () -> Void

  var body: some View {
    Button(action: onMessage) {
      HStack(spacing: 12) {
        AvatarView(name: person.displayName)
          .overlay(alignment: .bottomTrailing) {
            PresenceDot(presence: Presence(directoryWire: person.presence))
              .padding(2)
              .background(Theme.surface, in: Circle())
          }
        VStack(alignment: .leading, spacing: 2) {
          Text(person.displayName).foregroundStyle(Theme.text).lineLimit(1)
          let detail = [person.title, person.department, person.organization].filter { !$0.isEmpty }.joined(separator: " · ")
          if !detail.isEmpty {
            Text(detail).font(.app(.caption)).foregroundStyle(Theme.textMuted).lineLimit(1)
          }
        }
        Spacer()
        if person.isGuest { GuestBadge() }
      }
    }
    .disabled(isMe)
    .accessibilityHint(isMe ? Text(verbatim: "") : Text("directory_message_hint"))
  }
}

private struct GuestBadge: View {
  var body: some View {
    Text("directory_guest")
      .font(.app(.caption))
      .padding(.horizontal, 8)
      .padding(.vertical, 2)
      .background(Theme.accentSoft, in: Capsule())
  }
}

// MARK: Channels

private struct DirectoryChannelList: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let maybeSnapshot: BrowseSnapshot?
  let onOpen: (String) -> Void

  @State private var text = ""
  @State private var query = ""
  @State private var maybePager: DirectoryPager<DirectoryChannelItem>?
  @State private var joining: Set<String> = []

  var body: some View {
    // The answer is from when the page loaded; a join here shows as soon as the channel arrives.
    let joined = Set((maybeSnapshot?.conversations ?? []).map(\.id))
    VStack(spacing: 0) {
      DirectoryTools(text: $text, prompt: "directory_channel_filter", maybeOnInvite: nil)
      PagedList(maybePager: maybePager, empty: "directory_channels_empty") { channel in
        let isMember = channel.isMember || joined.contains(channel.conversationId)
        ChannelRow(
          channel: channel, isMember: isMember, isJoining: joining.contains(channel.conversationId) && !isMember,
          onOpen: { onOpen(channel.conversationId) },
          onJoin: {
            joining.insert(channel.conversationId)
            Task { await model.perform(.joinChannel(conversationId: channel.conversationId)) }
          })
      }
    }
    .debounced(text, into: $query)
    .task(id: "\(spaceId)/\(query)") {
      let pager = DirectoryPager<DirectoryChannelItem> { [model, spaceId, query] after in
        await model.directoryChannels(query: query, spaceId: spaceId, after: after)
      }
      maybePager = pager
      pager.reload()
    }
  }
}

private struct ChannelRow: View {
  let channel: DirectoryChannelItem
  let isMember: Bool
  let isJoining: Bool
  let onOpen: () -> Void
  let onJoin: () -> Void

  var body: some View {
    HStack(spacing: 12) {
      Image(systemName: channel.isPrivate ? "lock" : "number")
        .foregroundStyle(Theme.textMuted)
        .accessibilityLabel(channel.isPrivate ? Text("directory_private") : Text(verbatim: ""))
      VStack(alignment: .leading, spacing: 2) {
        Text(channel.name).lineLimit(1)
        Text(verbatim: "\(channel.spaceName) · ") + Text("directory_channel_members \(Int(channel.memberCount))")
        if !channel.topic.isEmpty {
          Text(channel.topic).font(.app(.caption)).foregroundStyle(Theme.textMuted).lineLimit(2)
        }
      }
      .font(.app(.body))
      Spacer()
      if isMember {
        Button("directory_open", action: onOpen).buttonStyle(.borderless)
      } else if isJoining {
        ProgressView().accessibilityLabel(Text("directory_joining"))
      } else {
        Button("channel_join", action: onJoin).buttonStyle(.bordered)
      }
    }
  }
}

// MARK: Invitations

private struct DirectoryInvitationList: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let generation: Int
  let maybeOnInvite: (() -> Void)?

  @State private var text = ""
  @State private var query = ""
  @State private var maybePager: DirectoryPager<DirectoryInvitationItem>?

  var body: some View {
    VStack(spacing: 0) {
      DirectoryTools(text: $text, prompt: "directory_invitation_filter", maybeOnInvite: maybeOnInvite)
      PagedList(maybePager: maybePager, empty: "directory_invitations_empty") { invitation in
        InvitationRow(invitation: invitation) { maybePager?.remove(invitation.invitationId) }
      }
    }
    .debounced(text, into: $query)
    .task(id: "\(spaceId)/\(query)/\(generation)") {
      let pager = DirectoryPager<DirectoryInvitationItem> { [model, spaceId, query] after in
        await model.directoryInvitations(query: query, spaceId: spaceId, after: after)
      }
      maybePager = pager
      pager.reload()
    }
  }
}

private struct InvitationRow: View {
  /// What the row says after the user resent its invitation.
  private enum Resend { case none, sent, tooSoon }

  @Environment(AppModel.self) private var model
  let invitation: DirectoryInvitationItem
  let onGone: () -> Void

  @State private var isBusy = false
  @State private var resend = Resend.none
  @State private var isConfirming = false
  @State private var maybeFailure: AppFailure?

  var body: some View {
    VStack(alignment: .leading, spacing: 6) {
      HStack(spacing: 12) {
        Image(systemName: "envelope").foregroundStyle(Theme.textMuted).accessibilityHidden(true)
        VStack(alignment: .leading, spacing: 2) {
          Text(verbatim: invitation.email).lineLimit(1)
          (Text(verbatim: "\(invitation.spaceName) · ") + Text("directory_invited_by \(invitation.sponsorName)"))
            .font(.app(.caption)).foregroundStyle(Theme.textMuted).lineLimit(1)
          if invitation.isExpired {
            Text("directory_invitation_expired").font(.app(.caption)).foregroundStyle(Theme.danger)
          } else if let expiresAt = invitation.maybeExpiresAt {
            let date = Date(timeIntervalSince1970: TimeInterval(expiresAt) / 1000)
            Text("directory_invitation_expires \(date.formatted(date: .abbreviated, time: .omitted))")
              .font(.app(.caption)).foregroundStyle(Theme.textMuted)
          }
        }
        Spacer()
        if invitation.isGuest { GuestBadge() }
      }
      HStack(spacing: 16) {
        switch resend {
        case .sent: Text("directory_resent").font(.app(.caption)).foregroundStyle(Theme.textMuted)
        case .tooSoon: Text("directory_resend_later").font(.app(.caption)).foregroundStyle(Theme.textMuted)
        case .none:
          if invitation.canResend {
            Button("directory_resend") { Task { await resendNow() } }.disabled(isBusy)
          }
        }
        Button("directory_revoke", role: .destructive) { isConfirming = true }.disabled(isBusy)
        if isBusy { ProgressView() }
      }
      .buttonStyle(.borderless)
      .font(.app(.subheadline))
      .padding(.leading, 32)
      if let maybeFailure {
        Text(maybeFailure.message).font(.app(.caption)).foregroundStyle(Theme.danger)
      }
    }
    .confirmationDialog(
      "directory_revoke_title", isPresented: $isConfirming, titleVisibility: .visible
    ) {
      Button("directory_revoke", role: .destructive) { Task { await revokeNow() } }
    } message: {
      Text("directory_revoke_text \(invitation.email) \(invitation.spaceName)")
    }
  }

  private func resendNow() async {
    await run(await model.resendInvitation(invitation)) { resend = .sent }
  }

  private func revokeNow() async {
    await run(await model.revokeInvitation(invitation), onDone: onGone)
  }

  private func run(_ call: @autoclosure () async -> Result<InvitationOutcome, AppFailure>, onDone: () -> Void) async {
    guard !isBusy else { return }
    isBusy = true
    maybeFailure = nil
    switch await call() {
    case .success(.done): onDone()
    case .success(.tooSoon): resend = .tooSoon
    case .success(.gone): onGone()
    case .success(.notAllowed): maybeFailure = .generic
    case .failure(let failure): maybeFailure = failure
    }
    isBusy = false
  }
}

// MARK: Shared

/// The search field of a list, and "Invite people" where the user may invite.
private struct DirectoryTools: View {
  @Binding var text: String
  let prompt: LocalizedStringKey
  let maybeOnInvite: (() -> Void)?

  var body: some View {
    HStack(spacing: 8) {
      HStack {
        Image(systemName: "magnifyingglass").foregroundStyle(Theme.textMuted).accessibilityHidden(true)
        TextField(prompt, text: $text)
          .textInputAutocapitalization(.never)
          .autocorrectionDisabled()
          .submitLabel(.search)
      }
      .padding(.horizontal, 10)
      .padding(.vertical, 8)
      .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radius))
      .fieldEdge(isError: false)
      if let onInvite = maybeOnInvite {
        Button(action: onInvite) {
          Label("invite_title", systemImage: "person.badge.plus")
        }
        .buttonStyle(.bordered)
      }
    }
    .padding(.horizontal)
    .padding(.vertical, 8)
  }
}

/// A pager's rows, then its placeholder, failure or next page's loader.
private struct PagedList<Row: Identifiable, RowView: View>: View where Row.ID == String {
  let maybePager: DirectoryPager<Row>?
  let empty: LocalizedStringKey
  @ViewBuilder let row: (Row) -> RowView

  var body: some View {
    if let pager = maybePager {
      switch pager.phase {
      case .loading: ProgressView("loading").frame(maxHeight: .infinity)
      case .empty: DirectoryHint(text: empty)
      case .failed(let failure):
        VStack(spacing: 8) {
          Text(failure.message).foregroundStyle(Theme.textMuted).multilineTextAlignment(.center)
          Button("retry") { pager.reload() }
        }
        .padding()
        .frame(maxHeight: .infinity)
      case .rows:
        List {
          ForEach(pager.rows) { item in row(item) }
          if pager.hasMore {
            if pager.maybeMoreFailure != nil {
              VStack(spacing: 4) {
                Text("directory_more_failed").font(.app(.caption)).foregroundStyle(Theme.textMuted)
                Button("retry") { pager.loadMore() }
              }
              .frame(maxWidth: .infinity)
            } else {
              // Shown when the list scrolls near its end: that asks for the next page.
              ProgressView().frame(maxWidth: .infinity).onAppear { pager.loadMore() }
            }
          }
        }
        .listStyle(.plain)
      }
    } else {
      ProgressView("loading").frame(maxHeight: .infinity)
    }
  }
}

private struct DirectoryHint: View {
  let text: LocalizedStringKey

  var body: some View {
    Text(text)
      .foregroundStyle(Theme.textMuted)
      .multilineTextAlignment(.center)
      .padding(32)
      .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
  }
}

extension View {
  /// Copies `text` into `settled` once typing paused, so the server is asked once per pause.
  fileprivate func debounced(_ text: String, into settled: Binding<String>) -> some View {
    task(id: text) {
      let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
      if trimmed != settled.wrappedValue {
        guard (try? await Task.sleep(for: .milliseconds(250))) != nil else { return }
      }
      settled.wrappedValue = trimmed
    }
  }
}

// MARK: Invite people

/// "Invite people" (#1337): an e-mail address into one of `spaces` (those the user may invite to),
/// as a member or a guest where the user may invite that way. The sheet stays until the server
/// took the invitation; a refused address shows at its field (forms.md).
struct InvitePeopleView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let spaces: [BrowseSnapshot.Space]
  let onSent: () -> Void

  @State private var spaceId = ""
  @State private var email = ""
  @State private var note = ""
  @State private var wantsGuest = false
  @State private var hasTried = false
  @State private var isSending = false
  @State private var maybeEmailRefusal: InviteProblem?
  @State private var maybeFailure: LocalizedStringKey?
  /// The invitation exists, but its e-mail did not go out: the sheet says so, with a resend where one can help.
  @State private var maybeUnsent: UnsentInvitation?
  @FocusState private var focused: InviteField?
  @AccessibilityFocusState private var accessibilityFocused: InviteField?

  var body: some View {
    let errors = shownErrors
    let space = chosenSpace
    NavigationStack {
      Form {
        if let unsent = maybeUnsent {
          Section {
            Label { Text("invite_mail_not_sent \(email.trimmingCharacters(in: .whitespacesAndNewlines))") }
              icon: { Image(systemName: "exclamationmark.triangle.fill") }
              .foregroundStyle(Theme.danger)
            if unsent.canResend {
              Button("directory_resend") { Task { await resend(unsent) } }.disabled(isSending)
            }
          } footer: {
            if let maybeFailure { Text(maybeFailure).foregroundStyle(Theme.danger) }
          }
        } else {
          if spaces.count > 1 {
            Section {
              Picker("invite_space_label", selection: $spaceId) {
                ForEach(spaces) { candidate in Text(candidate.name).tag(candidate.id) }
              }
            }
          }
          Section {
            TextField("invite_email_label", text: $email)
              .keyboardType(.emailAddress)
              .textContentType(.emailAddress)
              .textInputAutocapitalization(.never)
              .autocorrectionDisabled()
              .focused($focused, equals: .email)
              .accessibilityFocused($accessibilityFocused, equals: .email)
              .fieldError(errors[.email]?.message)
              .onChange(of: email) { maybeEmailRefusal = nil }
            FieldMessage(maybeError: errors[.email]?.message)
          } header: {
            Text("invite_email_label")
          }
          if let space, space.canAddMembers == true, space.canInviteGuests == true {
            Section {
              Picker("invite_role_label", selection: $wantsGuest) {
                RoleOption(title: "invite_role_member", hint: "invite_role_member_hint").tag(false)
                RoleOption(title: "invite_role_guest", hint: "invite_role_guest_hint").tag(true)
              }
              .pickerStyle(.inline)
            }
          }
          Section {
            TextField("invite_message_label", text: $note, axis: .vertical)
              .lineLimit(2...5)
              .focused($focused, equals: .note)
              .accessibilityFocused($accessibilityFocused, equals: .note)
              .fieldError(errors[.note]?.message)
            FieldMessage(maybeError: errors[.note]?.message)
          } footer: {
            if let maybeFailure {
              Label { Text(maybeFailure) } icon: { Image(systemName: "exclamationmark.triangle.fill") }
                .foregroundStyle(Theme.danger)
            }
          }
        }
      }
      .navigationTitle("invite_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) { Button("cancel") { dismiss() }.disabled(isSending) }
        ToolbarItem(placement: .confirmationAction) {
          if isSending {
            ProgressView()
          } else if maybeUnsent != nil {
            Button("invite_mail_done") {
              dismiss()
              onSent()
            }.fontWeight(.semibold)
          } else {
            Button("invite_submit") { Task { await send() } }.fontWeight(.semibold)
          }
        }
      }
      .onAppear {
        if spaceId.isEmpty { spaceId = spaces.first?.id ?? "" }
        focused = .email
      }
    }
    .interactiveDismissDisabled(isSending)
    .presentationDetents([.large])
  }

  private var chosenSpace: BrowseSnapshot.Space? {
    spaces.first { $0.id == spaceId } ?? spaces.first
  }

  /// Only the roles the user may give in this space; a member invitation where both are allowed.
  private var isGuest: Bool {
    guard let space = chosenSpace else { return false }
    if space.canAddMembers != true { return true }
    if space.canInviteGuests != true { return false }
    return wantsGuest
  }

  private var checkedErrors: FieldErrors<InviteField, InviteProblem> {
    InviteRules.errors(email: email, note: note)
  }

  private var shownErrors: FieldErrors<InviteField, InviteProblem> {
    (hasTried ? checkedErrors : FieldErrors()).with(maybeEmailRefusal, at: .email)
  }

  private func send() async {
    guard !isSending, let space = chosenSpace else { return }
    hasTried = true
    if let first = checkedErrors.maybeFirst {
      focus(first)
      return
    }
    isSending = true
    maybeFailure = nil
    let input = InvitePeopleInput(
      spaceId: space.id, email: email.trimmingCharacters(in: .whitespacesAndNewlines), isGuest: isGuest,
      message: note.trimmingCharacters(in: .whitespacesAndNewlines))
    switch await model.invitePeople(input) {
    case .success(.sent):
      dismiss()
      onSent()
    case .success(.mailNotSent(let spaceId, let invitationId, let canResend)):
      maybeUnsent = UnsentInvitation(spaceId: spaceId, invitationId: invitationId, canResend: canResend)
    case .success(.alreadyInvited):
      maybeEmailRefusal = .alreadyInvited
      focus(.email)
    case .success(.emailRefused):
      maybeEmailRefusal = .emailRefused
      focus(.email)
    case .success(.notAllowed):
      maybeFailure = "create_not_allowed"
    case .failure(let failure):
      maybeFailure = failure.message
    }
    isSending = false
  }

  private func resend(_ unsent: UnsentInvitation) async {
    guard !isSending else { return }
    isSending = true
    maybeFailure = nil
    switch await model.resendInvitation(spaceId: unsent.spaceId, invitationId: unsent.invitationId) {
    case .success(.done):
      dismiss()
      onSent()
    case .success(.tooSoon): maybeFailure = "directory_resend_later"
    case .success(.gone), .success(.notAllowed): maybeFailure = "create_not_allowed"
    case .failure(let failure): maybeFailure = failure.message
    }
    isSending = false
  }

  private func focus(_ field: InviteField) {
    focused = field
    accessibilityFocused = field
  }
}

/// An invitation the server created without being able to send its e-mail.
private struct UnsentInvitation {
  let spaceId: String
  let invitationId: String
  let canResend: Bool
}

private struct RoleOption: View {
  let title: LocalizedStringKey
  let hint: LocalizedStringKey

  var body: some View {
    VStack(alignment: .leading, spacing: 2) {
      Text(title)
      Text(hint).font(.app(.footnote)).foregroundStyle(Theme.textMuted)
    }
  }
}
