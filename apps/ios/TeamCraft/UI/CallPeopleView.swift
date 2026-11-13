import SwiftUI

/// The people in a meeting's call with their "Host" and "Co-host" badges (#1720). The organizer gets
/// a menu per person: make or remove a co-host, or hand the host role over after a confirmation.
/// Above the list, how the last change of the hosts went.
struct CallPeopleView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String
  let participants: [String]
  let names: [String: String]

  @State private var maybeHosts: MeetingHosts?
  @State private var maybeStatus: HostChangeStatus?
  @State private var canHostIds: Set<String> = []
  @State private var maybeHandover: CallPersonRow?
  /// Moves after a change was answered here, so the hosts and the outcome are read again.
  @State private var answers = 0

  var body: some View {
    NavigationStack {
      List {
        if let line = statusLine {
          Text(line.text)
            .font(.app(.footnote))
            .foregroundStyle(line.isFailure ? Theme.danger : .secondary)
        }
        ForEach(rows) { row in
          HStack {
            VStack(alignment: .leading, spacing: 2) {
              Text(row.name)
              if let badge = row.badge {
                Text(badge).font(.app(.caption)).foregroundStyle(.secondary)
              }
            }
            Spacer(minLength: 8)
            if !row.actions.isEmpty { menu(for: row) }
          }
        }
      }
      .navigationTitle(Text("call_people_title"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
      }
    }
    .task(id: ReadKey(changes: model.changeCount, answers: answers, participants: participants)) {
      maybeHosts = await model.meetingHosts(conversationId: conversationId)
      maybeStatus = await model.meetingHostsChange(conversationId: conversationId)
      canHostIds = Set(await model.hostCandidates(participants: participants).map(\.id))
    }
    .alert(
      handoverTitle, isPresented: handoverBinding, presenting: maybeHandover
    ) { row in
      Button("host_hand_over_confirm") { change(row, .handOver) }
      Button("cancel", role: .cancel) {}
    } message: { row in
      Text("host_hand_over_body \(row.name)")
    }
  }

  private func menu(for row: CallPersonRow) -> some View {
    Menu {
      ForEach(row.actions, id: \.self) { action in
        Button(Self.label(action)) {
          if action == .handOver { maybeHandover = row } else { change(row, action) }
        }
      }
    } label: {
      Image(systemName: "ellipsis.circle")
        .font(.app(.title3))
        .frame(minWidth: 44, minHeight: 44)
        .contentShape(Rectangle())
    }
    .accessibilityLabel(Text("host_menu_for \(row.name)"))
  }

  private func change(_ row: CallPersonRow, _ action: MeetingHostAction) {
    maybeStatus = .saving
    Task {
      await model.changeMeetingHost(conversationId: conversationId, userId: row.id, action: action)
      answers += 1
    }
  }

  private var rows: [CallPersonRow] {
    participants.map { id in
      let part = maybeHosts.map { CallPersonRow.part(of: id, in: $0) } ?? .participant
      let canHost = canHostIds.contains(id)
      return CallPersonRow(
        id: id, name: names[id] ?? "", badge: Self.badge(part),
        actions: maybeHosts.map { CallPersonRow.actions(for: part, canHost: canHost, hosts: $0) }
          ?? [])
    }
  }

  private var statusLine: (text: LocalizedStringKey, isFailure: Bool)? {
    switch maybeStatus {
    case .saving: ("host_change_saving", false)
    case .failed(let reason): ("host_change_failed \(reason)", true)
    case .saved, nil: nil
    }
  }

  private var handoverTitle: LocalizedStringKey {
    "host_hand_over_title \(maybeHandover?.name ?? "")"
  }

  private var handoverBinding: Binding<Bool> {
    Binding(get: { maybeHandover != nil }, set: { if !$0 { maybeHandover = nil } })
  }

  private static func badge(_ part: MeetingPart) -> LocalizedStringKey? {
    switch part {
    case .organizer: "host_role_organizer"
    case .coHost: "host_role_co_host"
    case .participant: nil
    }
  }

  private static func label(_ action: MeetingHostAction) -> LocalizedStringKey {
    switch action {
    case .makeCoHost: "host_make_co_host"
    case .removeCoHost: "host_remove_co_host"
    case .handOver: "host_hand_over"
    }
  }

  private struct ReadKey: Hashable {
    let changes: Int
    let answers: Int
    let participants: [String]
  }
}

/// One person in the call as the list shows them.
struct CallPersonRow: Identifiable {
  let id: String
  let name: String
  let badge: LocalizedStringKey?
  let actions: [MeetingHostAction]

  static func part(of userId: String, in hosts: MeetingHosts) -> MeetingPart {
    if userId == hosts.organizerUserId { return .organizer }
    return hosts.coHostUserIds.contains(userId) ? .coHost : .participant
  }

  /// The organizer's menu for somebody: the core's three changes, as fit their part.
  static func actions(for part: MeetingPart, canHost: Bool, hosts: MeetingHosts)
    -> [MeetingHostAction]
  {
    guard hosts.ownPart == .organizer, canHost else { return [] }
    switch part {
    case .organizer: return []
    case .coHost: return [.removeCoHost, .handOver]
    case .participant: return [.makeCoHost, .handOver]
    }
  }
}
