import SwiftUI

/// Find a conversation to start with: the channels of the user's spaces (join or create one) and
/// the people to message. Picking one opens it in the chat list.
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
  @State private var newChannelName = ""

  var body: some View {
    NavigationStack {
      VStack(spacing: 0) {
        Picker("browse_title", selection: $tab) {
          Text("channels").tag(Tab.channels)
          Text("people").tag(Tab.people)
        }
        .pickerStyle(.segmented)
        .padding()
        switch tab {
        case .channels: channelList
        case .people: peopleList
        }
      }
      .navigationTitle("browse_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) { Button("done") { dismiss() } }
      }
      .alert("channel_create", isPresented: $isCreating) {
        TextField("channel_name", text: $newChannelName)
        Button("cancel", role: .cancel) {}
        Button("channel_create") { createChannel() }
      }
    }
    .task(id: model.changeCount) { await reload() }
  }

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
        if spaceId != nil {
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
    HStack {
      VStack(alignment: .leading) {
        Text(verbatim: "# " + channel.name)
        if let topic = channel.topic, !topic.isEmpty {
          Text(verbatim: topic).font(.footnote).foregroundStyle(Theme.textMuted)
        }
      }
      Spacer()
      if channel.isMember {
        Button("open") { open(channel.conversation) }
      } else {
        Button("channel_join") {
          Task {
            await model.perform(.joinChannel(conversationId: channel.conversation))
            open(channel.conversation)
          }
        }
        .buttonStyle(.bordered)
      }
    }
    .accessibilityElement(children: .combine)
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
    guard let spaceId else { return }
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

  private func createChannel() {
    let name = newChannelName.trimmingCharacters(in: .whitespacesAndNewlines)
    newChannelName = ""
    guard let spaceId, !name.isEmpty else { return }
    Task { await model.perform(.createChannel(spaceId: spaceId, name: name)) }
  }

  private func open(_ conversationId: String) {
    onOpen(conversationId)
    dismiss()
  }
}
