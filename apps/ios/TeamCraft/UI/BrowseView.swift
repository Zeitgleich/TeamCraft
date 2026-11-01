import SwiftUI

/// Find a conversation to start with: the channels of the user's spaces (join or create one; a
/// workspace admin asks the owners of a private one) and the people to message. Picking one opens
/// it in the chat list.
struct BrowseView: View {
  enum Tab: Hashable, Identifiable {
    case channels, people

    var id: Self { self }
  }

  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  @State var tab: Tab
  let onOpen: (String) -> Void

  @State private var snapshot: BrowseSnapshot?
  @State private var spaceId: String?
  @State private var channels: [BrowsableChannel] = []
  /// People picked for a new DM (one) or group DM (several).
  @State private var picked: [String] = []
  @State private var maybePendingDm: [String]?
  @State private var isCreating = false

  var body: some View {
    NavigationStack {
      VStack(spacing: 0) {
        if snapshot?.canBrowseChannels != false {
          Picker("browse_title", selection: $tab) {
            Text("channels").tag(Tab.channels)
            Text("people").tag(Tab.people)
          }
          .pickerStyle(.segmented)
          .padding()
        }
        switch shownTab {
        case .channels: channelList
        case .people: peopleList
        }
      }
      .navigationTitle("browse_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) { Button("done") { dismiss() } }
      }
      // A sheet, not an alert: an alert's text field can't show an error (forms.md).
      .sheet(isPresented: $isCreating) {
        NewChannelSheet(onCreate: createChannel)
      }
      .joinNoteAlert(model)
    }
    .task(id: model.changeCount) { await reload() }
  }

  /// The people for guests, who browse no channels.
  private var shownTab: Tab { snapshot?.canBrowseChannels == false ? .people : tab }

  @ViewBuilder
  private var channelList: some View {
    let spaces = snapshot?.spaces ?? []
    if snapshot != nil && spaces.isEmpty {
      hint("browse_no_spaces")
    } else {
      List {
        if spaces.count > 1 {
          Picker("spaces", selection: $spaceId) {
            ForEach(spaces) { space in Text(verbatim: space.name).tag(Optional(space.id)) }
          }
        }
        if let spaceId, spaces.first(where: { $0.id == spaceId })?.offersChannelCreation == true {
          Button("channel_create", systemImage: "plus") { isCreating = true }
        }
        let visible = channels.filter { $0.isArchived != true }
        if snapshot != nil && visible.isEmpty {
          Text("browse_no_channels").foregroundStyle(Theme.textMuted)
        }
        ForEach(visible) { channel in channelRow(channel) }
      }
      .task(id: spaceId) { await loadChannels() }
    }
  }

  private func channelRow(_ channel: BrowsableChannel) -> some View {
    let maybeJoin = channel.maybeJoin
    let isPrivate = maybeJoin != .open
    return HStack {
      VStack(alignment: .leading) {
        HStack(spacing: 4) {
          if isPrivate {
            Image(systemName: "lock.fill").font(.footnote).foregroundStyle(Theme.textMuted)
              .accessibilityLabel(Text("browse_private"))
          }
          Text(verbatim: (isPrivate ? "" : "# ") + channel.name)
        }
        if !channel.isMember && maybeJoin == .requested {
          Text("browse_join_requested_hint").font(.footnote).foregroundStyle(Theme.textMuted)
        } else if let topic = channel.topic, !topic.isEmpty {
          Text(verbatim: topic).font(.footnote).foregroundStyle(Theme.textMuted)
        }
      }
      Spacer()
      if channel.isMember {
        Button("open") { open(channel.conversation) }
      } else {
        joinControl(channel, maybeJoin: maybeJoin)
      }
    }
    .accessibilityElement(children: .combine)
  }

  /// Join a public channel; ask the owners of a private one (admin.md §2.4.1), then "Requested"
  /// with "Withdraw". Joining as an admin without asking stays on desktop and web.
  @ViewBuilder
  private func joinControl(_ channel: BrowsableChannel, maybeJoin: BrowseJoin?) -> some View {
    let isPending = model.pendingJoinKeys.contains(channel.conversation)
    switch maybeJoin {
    case .open:
      Button("channel_join") {
        Task {
          await model.perform(.joinChannel(conversationId: channel.conversation))
          open(channel.conversation)
        }
      }
      .buttonStyle(.bordered)
    case .needsApproval:
      Button("browse_join_ask") { Task { await model.askToJoin(conversationId: channel.conversation) } }
        .buttonStyle(.bordered)
        .disabled(isPending)
    case .requested:
      VStack(alignment: .trailing, spacing: 2) {
        Text("browse_join_requested").font(.caption).foregroundStyle(Theme.textMuted)
        Button("browse_join_withdraw") {
          Task { await model.withdrawJoinRequest(conversationId: channel.conversation) }
        }
        .font(.subheadline)
        .disabled(isPending)
      }
    case .asAdmin:
      Text("browse_join_admin_elsewhere")
        .font(.caption)
        .foregroundStyle(Theme.textMuted)
        .multilineTextAlignment(.trailing)
        .frame(maxWidth: 140, alignment: .trailing)
    case nil:
      EmptyView()
    }
  }

  @ViewBuilder
  private var peopleList: some View {
    let people = snapshot?.people ?? []
    if snapshot != nil && people.isEmpty {
      hint("browse_no_people")
    } else {
      List {
        Section {
          ForEach(people) { person in
            PersonPickRow(person: person, picked: $picked)
              .disabled(maybePendingDm != nil)
          }
        } footer: {
          Text("dm_pick_hint")
        }
      }
      .safeAreaInset(edge: .bottom) {
        if !picked.isEmpty {
          Button(LocalizedStringKey(picked.count == 1 ? "dm_start" : "dm_start_group")) { message(picked) }
            .buttonStyle(.borderedProminent)
            .frame(maxWidth: .infinity)
            .padding()
            .disabled(maybePendingDm != nil)
        }
      }
    }
  }

  private func hint(_ text: LocalizedStringKey) -> some View {
    Text(text)
      .multilineTextAlignment(.center)
      .foregroundStyle(Theme.textMuted)
      .padding(32)
      .frame(maxHeight: .infinity, alignment: .top)
  }

  private func reload() async {
    let loaded = await model.browseSnapshot()
    snapshot = loaded
    if spaceId == nil { spaceId = loaded?.spaces?.first?.id }
    await loadChannels()
    // A new direct conversation opens as soon as the core created it, as on desktop.
    if let users = maybePendingDm, let created = loaded?.directConversation(with: users) {
      maybePendingDm = nil
      open(created)
    }
  }

  private func loadChannels() async {
    guard let spaceId, snapshot?.canBrowseChannels != false else { return }
    channels = await model.browseChannels(spaceId: spaceId)
  }

  private func message(_ userIds: [String]) {
    if let existing = snapshot?.directConversation(with: userIds) {
      open(existing)
      return
    }
    maybePendingDm = userIds
    let action: ChatAction = userIds.count == 1 ? .openDm(userId: userIds[0]) : .openGroupDm(userIds: userIds)
    Task { await model.perform(action) }
  }

  /// Creates the channel; nil when the core took it, else why not (shown at the name field).
  private func createChannel(_ name: String) async -> AppFailure? {
    guard let spaceId else { return .generic }
    return await model.performReporting(.createChannel(spaceId: spaceId, name: name))
  }

  private func open(_ conversationId: String) {
    onOpen(conversationId)
    dismiss()
  }
}

/// The name of a new channel. An empty name and the server's refusal (a taken name) are said at
/// the field; the sheet stays open with the input until the channel exists.
private struct NewChannelSheet: View {
  @Environment(\.dismiss) private var dismiss
  let onCreate: (String) async -> AppFailure?

  @State private var name = ""
  @State private var maybeProblem: TextProblem?
  @State private var isCreating = false
  @FocusState private var isFocused: Bool
  @AccessibilityFocusState private var isAccessibilityFocused: Bool

  var body: some View {
    NavigationStack {
      Form {
        Section {
          TextField("channel_name_placeholder", text: $name)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .submitLabel(.done)
            .focused($isFocused)
            .accessibilityFocused($isAccessibilityFocused)
            .accessibilityLabel(Text("channel_name"))
            .fieldError(maybeProblem?.message)
            .onSubmit { Task { await create() } }
            .onChange(of: name) { maybeProblem = nil }
          FieldMessage(maybeError: maybeProblem?.message)
        } header: {
          Text("channel_name")
        }
      }
      .navigationTitle("channel_create")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) { Button("cancel") { dismiss() } }
        ToolbarItem(placement: .confirmationAction) {
          Button("channel_create_action") { Task { await create() } }
            .disabled(isCreating)
        }
      }
      .onAppear { isFocused = true }
    }
    .presentationDetents([.medium])
  }

  private func create() async {
    guard !isCreating else { return }
    if let problem = TextProblem.required(name, missing: "channel_name_missing") {
      show(problem)
      return
    }
    isCreating = true
    if let failure = await onCreate(name.trimmingCharacters(in: .whitespacesAndNewlines)) {
      show(.refused(failure))
    } else {
      dismiss()
    }
    isCreating = false
  }

  private func show(_ problem: TextProblem) {
    maybeProblem = problem
    isFocused = true
    isAccessibilityFocused = true
  }
}

/// A person to pick (for a new DM or group, or to add to a call): tapping adds or removes them.
struct PersonPickRow: View {
  let person: BrowseSnapshot.Person
  @Binding var picked: [String]

  var body: some View {
    let isPicked = picked.contains(person.id)
    Button {
      if isPicked { picked.removeAll { $0 == person.id } } else { picked.append(person.id) }
    } label: {
      HStack {
        Image(systemName: isPicked ? "checkmark.circle.fill" : "circle")
          .foregroundStyle(isPicked ? Theme.accent : Theme.textMuted)
        VStack(alignment: .leading) {
          Text(verbatim: person.displayName).foregroundStyle(Theme.text)
          Text(verbatim: "@" + person.username).font(.footnote).foregroundStyle(Theme.textMuted)
        }
      }
    }
    .accessibilityAddTraits(isPicked ? .isSelected : [])
  }
}
