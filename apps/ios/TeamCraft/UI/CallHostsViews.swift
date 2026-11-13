import SwiftUI

/// The organizer's host menu in a meeting's call (#1720), as desktop's host controls have it:
/// everybody else in the call with their part ("Host", "Co-host"), "Make co-host" or "Remove as
/// co-host", and "Hand over host role" after a confirmation (the organizer stays co-host).
struct CallHostsSheet: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String
  let participantIds: [String]
  let hosts: MeetingHosts
  /// A change went through: the call screen reads the hosts again.
  let onChanged: () async -> Void

  @State private var people: [CallPerson] = []
  @State private var change = HostsChange.idle
  @State private var maybeHandingTo: CallPerson?

  var body: some View {
    NavigationStack {
      List {
        Section {
          ForEach(people) { person in row(person) }
        } footer: {
          Text("call_hosts_hint")
        }
        switch change {
        case .saving:
          HStack(spacing: 8) {
            ProgressView()
            Text("call_hosts_saving")
          }
          .accessibilityElement(children: .combine)
        case .failed(let maybeReason):
          Text("call_hosts_failed \(maybeReason ?? String(localized: "error_generic"))")
            .foregroundStyle(Theme.danger)
        case .idle:
          EmptyView()
        }
      }
      .navigationTitle(Text("call_hosts_title"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) {
          Button("done") { dismiss() }
        }
      }
      .task(id: participantIds) { people = await model.callPeople(participantIds) }
      .alert(
        "call_hosts_hand_over", isPresented: handingBinding, presenting: maybeHandingTo
      ) { person in
        Button("call_hosts_hand_over_do") { act(.handOver, on: person) }
        Button("cancel", role: .cancel) {}
      } message: { person in
        Text("call_hosts_hand_over_confirm \(person.displayName)")
      }
    }
    .presentationDetents([.medium, .large])
  }

  private func row(_ person: CallPerson) -> some View {
    VStack(alignment: .leading, spacing: 8) {
      HStack(spacing: 8) {
        Text(verbatim: person.displayName).lineLimit(1)
        if let badge = HostBadge(userId: person.id, hosts: hosts) {
          Text(badge.label)
            .font(.app(.caption).weight(.semibold))
            .padding(.horizontal, 8)
            .padding(.vertical, 2)
            .background(Theme.accent.opacity(0.18), in: Capsule())
        }
      }
      let actions = hostActions(for: person, hosts: hosts)
      if !actions.isEmpty {
        HStack(spacing: 8) {
          ForEach(actions, id: \.self) { action in
            Button(LocalizedStringKey(Self.titleKey(action))) {
              if action == .handOver { maybeHandingTo = person } else { act(action, on: person) }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(change == .saving)
          }
        }
      }
    }
    .padding(.vertical, 4)
  }

  private func act(_ action: MeetingHostAction, on person: CallPerson) {
    change = .saving
    Task {
      change = await model.changeMeetingHost(conversationId: conversationId, userId: person.id, action: action)
      await onChanged()
    }
  }

  private var handingBinding: Binding<Bool> {
    Binding(get: { maybeHandingTo != nil }, set: { if !$0 { maybeHandingTo = nil } })
  }

  static func titleKey(_ action: MeetingHostAction) -> String {
    switch action {
    case .makeCoHost: return "call_hosts_make_co_host"
    case .removeCoHost: return "call_hosts_remove_co_host"
    case .handOver: return "call_hosts_hand_over"
    }
  }
}
