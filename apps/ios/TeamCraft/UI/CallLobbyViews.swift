import SwiftUI

/// The lobby of the call's meeting (#1859), as desktop and web show it: each waiting person with
/// "Admit" and "Deny". A guest's row shows the code of their key to compare with the guest's
/// screen; "Admit" waits until the guest's page set up its device.
struct CallLobbySheet: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String
  let lobby: MeetingLobby
  /// A decision went through or failed: the call screen reads the lobby again.
  let onDecided: () async -> Void

  @State private var deciding = LobbyDeciding.idle

  var body: some View {
    NavigationStack {
      List {
        if lobby.people.isEmpty {
          Text("call_lobby_empty").foregroundStyle(.secondary)
        }
        ForEach(lobby.people, id: \.userId) { person in row(person) }
        if case .failed(let name, let reason) = deciding {
          Text("call_lobby_failed \(name) \(String(localized: String.LocalizationValue(reason)))")
            .foregroundStyle(Theme.danger)
            .accessibilityAddTraits(.updatesFrequently)
        }
      }
      .navigationTitle(Text("call_lobby_title"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) {
          Button("done") { dismiss() }
        }
      }
    }
    .presentationDetents([.medium, .large])
  }

  private func row(_ person: LobbyPerson) -> some View {
    VStack(alignment: .leading, spacing: 6) {
      Text(verbatim: person.displayName).lineLimit(1)
      switch person.status {
      case .guest(let maybeKeyCode):
        if let code = maybeKeyCode {
          HStack(spacing: 4) {
            Text("call_lobby_guest_code")
            Text(verbatim: code).monospaced()
          }
          .font(.app(.caption))
          .foregroundStyle(.secondary)
          .accessibilityElement(children: .combine)
        } else {
          Text("call_lobby_guest_verifying").font(.app(.caption)).foregroundStyle(.secondary)
        }
      case .guestDevicePending:
        Text("call_lobby_guest_pending").font(.app(.caption)).foregroundStyle(.secondary)
      case .member:
        EmptyView()
      }
      HStack(spacing: 8) {
        Button("call_lobby_admit") { decide(person, .admit) }
          .buttonStyle(.borderedProminent)
          .disabled(isBusy || !canAdmit(person))
        Button("call_lobby_deny") { decide(person, .deny) }
          .buttonStyle(.bordered)
          .disabled(isBusy)
        if case .deciding(let userId) = deciding, userId == person.userId {
          ProgressView()
        }
      }
      .controlSize(.small)
    }
    .padding(.vertical, 4)
  }

  private var isBusy: Bool {
    if case .deciding = deciding { return true }
    return false
  }

  private func decide(_ person: LobbyPerson, _ decision: LobbyDecision) {
    deciding = .deciding(userId: person.userId)
    Task {
      deciding = await model.decideLobbyEntry(conversationId: conversationId, person: person, decision: decision)
      await onDecided()
    }
  }
}

/// "N people are waiting in the lobby" over the call's controls, for its hosts (#1859); a tap opens
/// the lobby.
struct LobbyBanner: View {
  let waiting: Int
  let onOpen: () -> Void

  var body: some View {
    HStack(spacing: 12) {
      Label {
        Text("call_lobby_banner \(waiting)")
      } icon: {
        Image(systemName: "person.badge.clock")
      }
      Spacer(minLength: 0)
      Button("call_lobby_review", action: onOpen)
        .buttonStyle(.bordered)
        .controlSize(.small)
    }
    .font(.app(.subheadline))
    .foregroundStyle(.white)
    .padding(12)
    .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    .accessibilityElement(children: .combine)
  }
}

/// "Hand over" on the reminder after the organizer ended a meeting's call in CallKit with people
/// still in the lobby (#1859): whom to hand the meeting to, read afresh (the members still in the
/// call, no guests), as the in-call leave question offers them.
struct LobbyHandoverSheet: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let request: LobbyHandoverRequest

  private enum Step: Equatable {
    case loading, nothing, unreadable
    case choose(waiting: Int, candidates: [CallPerson])
    case saving(CallPerson)
    case failed(CallPerson, String?)
  }

  @State private var step = Step.loading

  var body: some View {
    NavigationStack {
      List {
        switch step {
        case .loading:
          HStack(spacing: 8) {
            ProgressView()
            Text("lobby_left_checking")
          }
          .accessibilityElement(children: .combine)
        case .nothing:
          Text("lobby_left_nothing")
        case .unreadable:
          Text("error_network").foregroundStyle(Theme.danger)
        case .choose(let waiting, let candidates):
          Section {
            ForEach(candidates) { person in
              Button("lobby_left_hand_over_to \(person.displayName)") { handOver(to: person) }
            }
          } header: {
            Text("call_hosts_leave_body \(waiting)").textCase(nil)
          }
        case .saving:
          HStack(spacing: 8) {
            ProgressView()
            Text("call_hosts_saving")
          }
          .accessibilityElement(children: .combine)
        case .failed(let person, let maybeReason):
          Text("call_hosts_failed \(maybeReason ?? String(localized: "error_generic"))")
            .foregroundStyle(Theme.danger)
          Button("lobby_left_hand_over_to \(person.displayName)") { handOver(to: person) }
        }
      }
      .navigationTitle(Text("call_hosts_leave_title"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("done") { dismiss() }
        }
      }
      .task(id: request.conversationId) { await load() }
    }
    .presentationDetents([.medium, .large])
  }

  private func load() async {
    do {
      if let choice = try await model.lobbyHandoverChoice(conversationId: request.conversationId) {
        step = .choose(waiting: choice.waiting, candidates: choice.candidates)
      } else {
        step = .nothing
        model.removeLobbyReminder(conversationId: request.conversationId)
      }
    } catch {
      step = .unreadable
    }
  }

  private func handOver(to person: CallPerson) {
    step = .saving(person)
    Task {
      switch await model.changeMeetingHost(
        conversationId: request.conversationId, userId: person.id, action: .handOver)
      {
      case .idle:
        model.removeLobbyReminder(conversationId: request.conversationId)
        dismiss()
      case .failed(let maybeReason):
        step = .failed(person, maybeReason)
      case .saving:
        step = .saving(person)
      }
    }
  }
}
