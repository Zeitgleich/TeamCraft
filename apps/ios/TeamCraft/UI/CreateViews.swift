import SwiftUI

/// "New channel" in a space (#1534): name, who can join, description. The sheet closes once the
/// server took it, then the channel opens; until then it keeps the input, every error at its field.
struct NewChannelView: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let spaceName: String
  let onCreated: (String) -> Void

  @State private var isPrivate = false

  var body: some View {
    CreateForm(
      title: "create_channel_title \(spaceName)",
      nameLabel: "channel_name",
      missingName: "channel_name_missing",
      submit: "create_channel_submit",
      access: {
        Picker("channel_access_label", selection: $isPrivate) {
          AccessOption(title: "channel_access_public", hint: "channel_access_public_hint", symbol: "number")
            .tag(false)
          AccessOption(title: "channel_access_private", hint: "channel_access_private_hint", symbol: "lock")
            .tag(true)
        }
        .pickerStyle(.inline)
      },
      create: { name, description, key in
        await model.createChannel(
          NewChannelInput(spaceId: spaceId, name: name, isPrivate: isPrivate, description: description, requestKey: key))
      },
      onCreated: onCreated
    )
  }
}

/// "New space" (#1534), where the workspace role allows it: name, who can join, description.
struct NewSpaceView: View {
  @Environment(AppModel.self) private var model
  let onCreated: (String) -> Void

  @State private var isOpen = true

  var body: some View {
    CreateForm(
      title: "create_space",
      nameLabel: "space_name_label",
      missingName: "space_name_missing",
      submit: "create_space_submit",
      access: {
        Picker("channel_access_label", selection: $isOpen) {
          AccessOption(title: "space_access_open", hint: "space_access_open_hint", symbol: "person.3")
            .tag(true)
          AccessOption(title: "space_access_invite", hint: "space_access_invite_hint", symbol: "envelope")
            .tag(false)
        }
        .pickerStyle(.inline)
      },
      create: { name, description, key in
        await model.createSpace(NewSpaceInput(name: name, description: description, isOpen: isOpen, requestKey: key))
      },
      onCreated: onCreated
    )
  }
}

/// One choice of who can join, with a line that says what it means.
private struct AccessOption: View {
  let title: LocalizedStringKey
  let hint: LocalizedStringKey
  let symbol: String

  var body: some View {
    Label {
      VStack(alignment: .leading, spacing: 2) {
        Text(title)
        Text(hint).font(.footnote).foregroundStyle(Theme.textMuted)
      }
    } icon: {
      Image(systemName: symbol)
    }
  }
}

/// The form both sheets share: name first (the keyboard comes up with it), then who can join, then
/// an optional description; Cancel and the create button in the toolbar, as in other iOS sheets.
private struct CreateForm<Access: View>: View {
  @Environment(\.dismiss) private var dismiss
  let title: LocalizedStringKey
  let nameLabel: LocalizedStringKey
  let missingName: String
  let submit: LocalizedStringKey
  @ViewBuilder let access: () -> Access
  let create: (String, String, String) async -> CreateResult
  let onCreated: (String) -> Void

  @State private var name = ""
  @State private var description = ""
  /// One key per attempt: a retry after a lost answer creates nothing twice; a refusal takes a new
  /// one, since the input changes.
  @State private var requestKey = newRequestKey()
  @State private var hasTried = false
  @State private var isCreating = false
  @State private var isNameRefused = false
  @State private var maybeFailure: LocalizedStringKey?
  @FocusState private var focused: CreateField?
  @AccessibilityFocusState private var accessibilityFocused: CreateField?

  var body: some View {
    let errors = shownErrors
    NavigationStack {
      Form {
        Section {
          TextField(nameLabel, text: $name)
            .submitLabel(.done)
            .focused($focused, equals: .name)
            .accessibilityFocused($accessibilityFocused, equals: .name)
            .fieldError(errors[.name]?.message)
            .onSubmit { Task { await send() } }
            .onChange(of: name) { isNameRefused = false }
          FieldMessage(maybeError: errors[.name]?.message)
        } header: {
          Text(nameLabel)
        }
        Section { access() }
        Section {
          TextField("create_description_label", text: $description, axis: .vertical)
            .lineLimit(2...5)
            .focused($focused, equals: .description)
            .accessibilityFocused($accessibilityFocused, equals: .description)
            .fieldError(errors[.description]?.message)
          FieldMessage(maybeError: errors[.description]?.message)
        } footer: {
          // Failures that belong to no field, next to where the user acts (forms.md).
          if let maybeFailure {
            Label { Text(maybeFailure) } icon: { Image(systemName: "exclamationmark.triangle.fill") }
              .foregroundStyle(Theme.danger)
          }
        }
      }
      .navigationTitle(title)
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel") { dismiss() }.disabled(isCreating)
        }
        ToolbarItem(placement: .confirmationAction) {
          if isCreating {
            ProgressView()
          } else {
            Button(submit) { Task { await send() } }.fontWeight(.semibold)
          }
        }
      }
      .onAppear { focused = .name }
    }
    // While the server is asked the sheet stays, so its answer has somewhere to show.
    .interactiveDismissDisabled(isCreating)
    .presentationDetents([.large])
  }

  private var checkedErrors: FieldErrors<CreateField, CreateProblem> {
    CreateRules.errors(name: name, description: description, missing: missingName)
  }

  private var shownErrors: FieldErrors<CreateField, CreateProblem> {
    (hasTried ? checkedErrors : FieldErrors()).with(isNameRefused ? .nameRefused : nil, at: .name)
  }

  private func send() async {
    guard !isCreating else { return }
    hasTried = true
    if let first = checkedErrors.maybeFirst {
      focus(first)
      return
    }
    isCreating = true
    maybeFailure = nil
    let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
    let trimmedDescription = description.trimmingCharacters(in: .whitespacesAndNewlines)
    switch await create(trimmedName, trimmedDescription, requestKey) {
    case .created(let id):
      dismiss()
      onCreated(id)
    case .nameRefused:
      isNameRefused = true
      requestKey = newRequestKey()
      focus(.name)
    case .notAllowed:
      maybeFailure = "create_not_allowed"
      requestKey = newRequestKey()
    case .failed(let failure):
      maybeFailure = failure.message
    }
    isCreating = false
  }

  private func focus(_ field: CreateField) {
    focused = field
    accessibilityFocused = field
  }
}

/// "Add people" to a channel (#1534), where the server says the user may: the directory through
/// the core, without the members, several at once. As on desktop, people join the channel's space
/// first where the user may add them there. The sheet closes once everybody is in; on a failure it
/// stays with the picks and says why.
struct AddChannelMembersView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String
  let memberIds: [String]

  @State private var query = ""
  @State private var maybeFound: [BrowseSnapshot.Person]?
  @State private var picked: [String] = []
  @State private var pickedPeople: [String: BrowseSnapshot.Person] = [:]
  @State private var isAdding = false
  @State private var maybeFailure: LocalizedStringKey?

  var body: some View {
    NavigationStack {
      List {
        if let failure = maybeFailure {
          Section {
            Label(failure, systemImage: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
          }
        }
        if maybeFound != nil && shown.isEmpty {
          Text("members_add_none").foregroundStyle(Theme.textMuted)
        }
        ForEach(shown) { person in
          PersonPickRow(person: person, picked: $picked).disabled(isAdding)
        }
      }
      .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: Text("browse_filter_people"))
      .navigationTitle("members_add")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) { Button("cancel") { dismiss() }.disabled(isAdding) }
        ToolbarItem(placement: .confirmationAction) {
          if isAdding {
            ProgressView()
          } else {
            Button("members_add_submit") { Task { await add() } }
              .fontWeight(.semibold)
              .disabled(picked.isEmpty)
          }
        }
      }
      .onChange(of: picked) {
        for id in picked where pickedPeople[id] == nil {
          pickedPeople[id] = maybeFound?.first { $0.id == id }
        }
      }
      .task(id: AddSearch(query: query, changes: model.changeCount)) {
        if !query.isEmpty { try? await Task.sleep(for: .milliseconds(250)) }
        guard !Task.isCancelled else { return }
        if let found = await model.findPeople(query.trimmingCharacters(in: .whitespacesAndNewlines)) {
          maybeFound = found
        }
      }
    }
    .interactiveDismissDisabled(isAdding)
  }

  /// The picked people first, then everybody found who isn't a member yet.
  private var shown: [BrowseSnapshot.Person] {
    let members = Set(memberIds)
    let found = (maybeFound ?? []).filter { !members.contains($0.id) }
    return picked.compactMap { id in found.first { $0.id == id } ?? pickedPeople[id] }
      + found.filter { !picked.contains($0.id) }
  }

  private func add() async {
    guard !isAdding else { return }
    isAdding = true
    maybeFailure = nil
    maybeFailure = await model.addPeople(picked, to: conversationId)
    isAdding = false
    if maybeFailure == nil { dismiss() }
  }
}

/// What the member search waits for: the typed text, and a change that may have brought the
/// directory's answer.
private struct AddSearch: Equatable {
  let query: String
  let changes: Int
}
