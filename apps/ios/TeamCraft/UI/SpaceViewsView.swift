import SwiftUI

/// People, calls, meetings, saved messages and files of one space (#954). The app's other entries
/// stay global; this sheet is what "inside a space" means here, where the channels of all spaces
/// share one list and none is selected. Everything content is filtered by the core on this device.
struct SpaceViewsView: View {
  /// How long a space's member list may load before the user is offered another try.
  private static let rosterDeadline = Duration.seconds(15)

  @Environment(\.dismiss) private var dismiss
  let account: Account
  let spaces: [SpaceChoice]
  let onOpen: (String) -> Void

  @State private var spaceId: String
  @State private var tab = SpaceViewTab.people

  init(account: Account, spaces: [SpaceChoice], initialSpaceId: String, onOpen: @escaping (String) -> Void) {
    self.account = account
    self.spaces = spaces
    self.onOpen = onOpen
    _spaceId = State(initialValue: initialSpaceId)
  }

  var body: some View {
    NavigationStack {
      VStack(spacing: 0) {
        Picker("space_views_open", selection: $tab) {
          ForEach(SpaceViewTab.allCases) { entry in
            Text(entry.title).tag(entry)
          }
        }
        .pickerStyle(.segmented)
        .padding()
        content
          .frame(maxWidth: .infinity, maxHeight: .infinity)
      }
      .navigationTitle(spaces.first { $0.id == spaceId }?.name ?? "")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        if let space = spaces.first(where: { $0.id == spaceId }), space.maybeImageVersion != nil {
          ToolbarItem(placement: .principal) {
            HStack(spacing: 8) {
              SpacePictureView(spaceId: space.id, maybeVersion: space.maybeImageVersion, size: 28)
              Text(space.name).font(.headline).lineLimit(1)
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
          }
        }
        ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
        if spaces.count > 1 {
          ToolbarItem(placement: .topBarLeading) {
            // Another space: the open view stays and shows the new space at once.
            Menu {
              ForEach(spaces) { space in
                Button(space.name) { spaceId = space.id }
              }
            } label: {
              Image(systemName: "chevron.down.circle")
            }
            .accessibilityLabel(Text("space_views_switch"))
          }
        }
      }
    }
  }

  @ViewBuilder
  private var content: some View {
    switch tab {
    case .people: SpacePeopleList(spaceId: spaceId, rosterDeadline: Self.rosterDeadline)
    case .calls: SpaceCallsList(spaceId: spaceId) { dismiss() }
    case .meetings: SpaceMeetingsList(account: account, spaceId: spaceId) { dismiss() }
    case .saved:
      SpaceSavedList(spaceId: spaceId) { conversationId in
        dismiss()
        onOpen(conversationId)
      }
    case .files: FilesGalleryView(spaceId: spaceId)
    }
  }
}

private struct SpaceLoadKey: Hashable {
  let changeCount: Int
  let spaceId: String
}

private struct SpacePeopleList: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let rosterDeadline: Duration

  @State private var maybeRoster: SpaceRoster?
  @State private var attempt = 0
  @State private var isOverdue = false

  var body: some View {
    Group {
      switch maybeRoster?.state {
      case "hidden":
        Text("space_people_hidden").foregroundStyle(Theme.textMuted)
      case "loaded":
        if let people = maybeRoster?.people, !people.isEmpty {
          List(people) { member in
            HStack {
              AvatarView(name: member.displayName)
              VStack(alignment: .leading) {
                Text(member.displayName)
                if member.isGuest == true {
                  Text("space_person_guest").font(.caption).foregroundStyle(Theme.textMuted)
                }
              }
            }
          }
        } else {
          Text("space_people_empty").foregroundStyle(Theme.textMuted)
        }
      default:
        if isOverdue {
          VStack(spacing: 8) {
            Text("space_people_failed").foregroundStyle(Theme.textMuted)
            Button("retry") { attempt += 1 }
          }
        } else {
          ProgressView("loading")
        }
      }
    }
    .task(id: SpaceLoadKey(changeCount: model.changeCount, spaceId: spaceId)) {
      maybeRoster = await model.roster(spaceId: spaceId)
    }
    // What this device has shows at once; the fresh list follows as a store change.
    .task(id: "\(spaceId)/\(attempt)") {
      isOverdue = false
      await model.perform(.refreshSpaceRoster(spaceId: spaceId))
      guard (try? await Task.sleep(for: rosterDeadline)) != nil else { return }
      isOverdue = true
    }
  }
}

private struct SpaceCallsList: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let onCall: () -> Void

  @State private var maybeCalls: [CallHistoryItem]?
  @State private var names: [String: String] = [:]

  var body: some View {
    Group {
      if let calls = maybeCalls {
        if calls.isEmpty {
          Text("space_calls_empty").foregroundStyle(Theme.textMuted)
        } else {
          List(calls) { call in row(call) }
        }
      } else {
        ProgressView("loading")
      }
    }
    .task { await model.perform(.refreshCallHistory) }
    .task(id: SpaceLoadKey(changeCount: model.changeCount, spaceId: spaceId)) {
      let conversations = await model.conversations()
      names = Dictionary(conversations.map { ($0.id, $0.name) }, uniquingKeysWith: { first, _ in first })
      maybeCalls = await model.calls(spaceId: spaceId)
    }
  }

  private func row(_ call: CallHistoryItem) -> some View {
    let name = names[call.conversation] ?? ""
    return HStack {
      VStack(alignment: .leading) {
        Text(name).font(.headline)
        HStack(spacing: 4) {
          Text(call.directionTitle)
          Text(call.startDate.formatted(date: .abbreviated, time: .shortened))
        }
        .font(.subheadline)
        .foregroundStyle(Theme.textMuted)
      }
      Spacer()
      Button("space_call_back") {
        onCall()
        model.calls.startCall(conversationId: call.conversation, displayName: name)
      }
      .buttonStyle(.bordered)
    }
  }
}

private struct SpaceMeetingsList: View {
  @Environment(AppModel.self) private var model
  let account: Account
  let spaceId: String
  let onJoin: () -> Void

  @State private var maybeMeetings: [Meeting]?
  @State private var isRsvpFailed = false

  var body: some View {
    Group {
      if let all = maybeMeetings {
        let meetings = meetingsOfSpace(all, spaceId: spaceId)
        if meetings.isEmpty {
          Text("space_meetings_empty").foregroundStyle(Theme.textMuted)
        } else {
          // A repeating meeting is listed once per occurrence, all with the series' ID.
          List(meetings, id: \.occurrenceKey) { meeting in
            MeetingRow(account: account, meeting: meeting, onJoin: onJoin) { answer in
              if await model.setRsvp(meetingId: meeting.id, answer: answer) {
                maybeMeetings = await model.meetings()
              } else {
                isRsvpFailed = true
              }
            }
          }
        }
      } else {
        ProgressView("loading")
      }
    }
    .task(id: model.changeCount) { maybeMeetings = await model.meetings() }
    .alert("meetings_rsvp_failed", isPresented: $isRsvpFailed) {
      Button("done", role: .cancel) {}
    }
  }
}

private struct SpaceSavedList: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let onOpen: (String) -> Void

  @State private var maybeItems: [SavedItem]?

  var body: some View {
    Group {
      if let items = maybeItems {
        if items.isEmpty {
          Text("space_saved_empty").foregroundStyle(Theme.textMuted)
        } else {
          List(items) { item in
            Button {
              onOpen(item.message.conversation)
            } label: {
              VStack(alignment: .leading, spacing: 4) {
                Text(item.listName).font(.caption).foregroundStyle(Theme.accent)
                Text(item.message.body).lineLimit(3)
              }
            }
            .buttonStyle(.plain)
          }
        }
      } else {
        ProgressView("loading")
      }
    }
    .task(id: SpaceLoadKey(changeCount: model.changeCount, spaceId: spaceId)) {
      maybeItems = await model.savedItems(spaceId: spaceId)
    }
  }
}
