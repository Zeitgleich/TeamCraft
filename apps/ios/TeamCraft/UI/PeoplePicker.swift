import SwiftUI

/// The one people picker of the app (#1534): a search over the directory through the core
/// (`AppModel.searchPeople`), the same everywhere people are picked — a new message, a channel's
/// members, a call, do not disturb. People in `excluded` aren't offered; picked people stay on top
/// while the search changes. With `onPickOne` a tap is the choice; otherwise rows toggle `picked`.
struct PeoplePicker: View {
  @Environment(AppModel.self) private var model
  @Binding var picked: [String]
  var excluded: Set<String> = []
  var isEnabled = true
  /// Under the list: what picking does here.
  var maybeFooter: LocalizedStringKey?
  /// Why the last attempt failed, on top of the list (forms.md: next to where the user acts).
  var maybeFailure: LocalizedStringKey?
  var onPickOne: ((BrowseSnapshot.Person) -> Void)?

  @State private var query = ""
  @State private var maybeFound: [BrowseSnapshot.Person]?
  /// Who was picked, so a pick stays named when the search no longer finds them.
  @State private var known: [String: BrowseSnapshot.Person] = [:]

  var body: some View {
    let people = shown
    List {
      if let maybeFailure {
        Section {
          Label { Text(maybeFailure) } icon: { Image(systemName: "exclamationmark.triangle.fill") }
            .foregroundStyle(Theme.danger)
        }
      }
      if maybeFound != nil && people.isEmpty {
        Text("browse_no_people").foregroundStyle(Theme.textMuted)
      }
      Section {
        ForEach(people) { person in row(person) }
      } footer: {
        if let maybeFooter { Text(maybeFooter) }
      }
    }
    .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: Text("browse_filter_people"))
    .onChange(of: picked) {
      for id in picked where known[id] == nil {
        known[id] = maybeFound?.first { $0.id == id }
      }
    }
    .task(id: Search(query: query, changes: model.changeCount)) {
      // A keystroke cancels this before the directory is asked; a change reads again at once.
      if !query.isEmpty { try? await Task.sleep(for: .milliseconds(250)) }
      guard !Task.isCancelled else { return }
      if let found = await model.searchPeople(query.trimmingCharacters(in: .whitespacesAndNewlines)) {
        maybeFound = found
      }
    }
  }

  private var shown: [BrowseSnapshot.Person] {
    let found = (maybeFound ?? []).filter { !excluded.contains($0.id) }
    return picked.compactMap { id in found.first { $0.id == id } ?? known[id] }
      + found.filter { !picked.contains($0.id) }
  }

  @ViewBuilder
  private func row(_ person: BrowseSnapshot.Person) -> some View {
    if let onPickOne {
      Button {
        onPickOne(person)
      } label: {
        PersonLabel(person: person)
      }
      .disabled(!isEnabled)
    } else {
      PersonPickRow(person: person, picked: $picked).disabled(!isEnabled)
    }
  }
}

/// A person's name and username, as every people list shows them.
struct PersonLabel: View {
  let person: BrowseSnapshot.Person

  var body: some View {
    VStack(alignment: .leading) {
      Text(verbatim: person.displayName).foregroundStyle(Theme.text)
      Text(verbatim: "@" + person.username).font(.footnote).foregroundStyle(Theme.textMuted)
    }
  }
}

/// What the people search waits for: the typed text, and a change that may have brought the
/// directory's answer.
private struct Search: Equatable {
  let query: String
  let changes: Int
}
