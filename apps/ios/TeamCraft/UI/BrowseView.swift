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
  /// The space whose channels show first: Home's selected space (#1526).
  var maybeInitialSpaceId: String? = nil
  let onOpen: (String) -> Void

  @State private var snapshot: BrowseSnapshot?
  @State private var spaceId: String?
  @State private var channels: [BrowsableChannel] = []
  /// People picked for a new DM (one) or group DM (several).
  @State private var picked: [String] = []
  @State private var isOpening = false
  @State private var maybeDmFailure: AppFailure?
  @State private var isCreating = false
  @State private var showsArchived = false

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
        if let spaceId, let space = snapshot?.spaces?.first(where: { $0.id == spaceId }) {
          NewChannelView(spaceId: spaceId, spaceName: space.name) { created in open(created) }
        }
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
        // Archived channels are hidden from the lists, so this is the way back to the ones the user
        // is in; nobody can join an archived channel.
        if channels.contains(where: { $0.isArchived == true && $0.isMember }) {
          Toggle("browse_show_archived", isOn: $showsArchived)
        }
        let visible = channels.filter { $0.isArchived != true || (showsArchived && $0.isMember) }
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

  private var peopleList: some View {
    PeoplePicker(
      picked: $picked, isEnabled: !isOpening, maybeFooter: "dm_pick_hint", maybeFailure: maybeDmFailure?.message
    )
    .safeAreaInset(edge: .bottom) {
      if !picked.isEmpty {
        Button {
          message(picked)
        } label: {
          if isOpening {
            ProgressView().frame(maxWidth: .infinity)
          } else {
            Text(LocalizedStringKey(picked.count == 1 ? "dm_start" : "dm_start_group")).frame(maxWidth: .infinity)
          }
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .disabled(isOpening)
        .padding()
        .background(.bar)
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
    if spaceId == nil {
      let spaces = loaded?.spaces ?? []
      spaceId = spaces.first { $0.id == maybeInitialSpaceId }?.id ?? spaces.first?.id
    }
    await loadChannels()
  }

  private func loadChannels() async {
    guard let spaceId, snapshot?.canBrowseChannels != false else { return }
    channels = await model.browseChannels(spaceId: spaceId)
  }

  /// Opens the DM or group: one the device knows at once, a new one once the server made it.
  private func message(_ userIds: [String]) {
    if let existing = snapshot?.directConversation(with: userIds) {
      open(existing)
      return
    }
    guard !isOpening else { return }
    isOpening = true
    maybeDmFailure = nil
    Task {
      switch await model.openDirect(userIds) {
      case .created(let id): open(id)
      case .nameRefused, .notAllowed: maybeDmFailure = .generic
      case .failed(let failure): maybeDmFailure = failure
      }
      isOpening = false
    }
  }

  private func open(_ conversationId: String) {
    onOpen(conversationId)
    dismiss()
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
        PersonLabel(person: person)
      }
    }
    .accessibilityAddTraits(isPicked ? .isSelected : [])
  }
}
