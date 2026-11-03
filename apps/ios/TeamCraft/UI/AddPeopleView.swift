import SwiftUI

/// "Add people" in a DM or group DM call (docs/meetings.md, "A call becomes a meeting"): a DM never
/// takes more people, so the call moves into a new meeting room with everybody in it and the picked
/// people. One request key per opened sheet: a retry after a failure gets the same room and adds
/// nobody twice.
struct AddPeopleView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String

  @State private var requestKey = newRequestKey()
  @State private var snapshot: BrowseSnapshot?
  @State private var query = ""
  @State private var picked: [String] = []
  @State private var isAdding = false
  @State private var maybeFailure: AppFailure?

  var body: some View {
    NavigationStack {
      List {
        if let failure = maybeFailure {
          Section {
            Label(failure.message, systemImage: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
          }
        }
        Section {
          ForEach(candidates) { person in
            PersonPickRow(person: person, picked: $picked).disabled(isAdding)
          }
        } footer: {
          Text("call_add_people_hint")
        }
      }
      .overlay {
        if snapshot != nil && candidates.isEmpty {
          Text("call_add_people_none").multilineTextAlignment(.center).foregroundStyle(Theme.textMuted).padding(32)
        }
      }
      .searchable(text: $query)
      .safeAreaInset(edge: .bottom) { addButton }
      .navigationTitle("call_add_people")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel") { dismiss() }.disabled(isAdding)
        }
      }
    }
    .interactiveDismissDisabled(isAdding)
    .task(id: model.changeCount) { snapshot = await model.browseSnapshot() }
  }

  private var candidates: [BrowseSnapshot.Person] {
    snapshot?.addablePeople(to: conversationId, matching: query) ?? []
  }

  @ViewBuilder
  private var addButton: some View {
    if !picked.isEmpty {
      Button(action: add) {
        if isAdding {
          ProgressView().frame(maxWidth: .infinity)
        } else {
          Text("call_add_people_confirm").frame(maxWidth: .infinity)
        }
      }
      .buttonStyle(.borderedProminent)
      .padding()
      .disabled(isAdding)
    }
  }

  private func add() {
    guard let snapshot, !isAdding else { return }
    let inCallUsers = model.calls.activeCall?.participantUserIds ?? []
    let addition = snapshot.callAddition(inCallUsers: inCallUsers, picked: picked)
    guard !addition.added.isEmpty else { return }
    isAdding = true
    maybeFailure = nil
    Task {
      defer { isAdding = false }
      do {
        try await model.calls.addPeople(
          conversationId: conversationId, inCall: addition.inCall, added: addition.added, requestKey: requestKey)
        dismiss()
      } catch {
        // The DM call goes on; the sheet stays for a retry with the same key.
        maybeFailure = AppFailure(CoreFailure(error))
      }
    }
  }
}
