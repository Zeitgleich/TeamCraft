import SwiftUI

/// A space's picture, or the first letter of its name on a tile when it has none, so every space in
/// the switcher has a mark of its own (#1526).
struct SpaceMark: View {
  let spaceId: String
  let name: String
  let maybeImageVersion: String?
  var size: CGFloat = 28

  var body: some View {
    if maybeImageVersion != nil {
      SpacePictureView(spaceId: spaceId, maybeVersion: maybeImageVersion, size: size)
    } else {
      Text(String(name.trimmingCharacters(in: .whitespaces).prefix(1)).uppercased())
        .font(size >= 28 ? .subheadline.weight(.semibold) : .caption.weight(.semibold))
        .foregroundStyle(Theme.accent)
        .frame(width: size, height: size)
        .background(Theme.accent.opacity(0.15), in: .rect(cornerRadius: size / 4))
        .accessibilityHidden(true)
    }
  }
}

/// Home's title in the spaces layout (#1526): the selected space with a chevron; a tap opens the
/// switcher. A dot on the chevron says another space has something new, so switching hides nothing.
struct SpaceSwitcherTitle: View {
  let space: HomeSpace
  let hasNewElsewhere: Bool
  let onTap: () -> Void

  var body: some View {
    Button(action: onTap) {
      HStack(spacing: 6) {
        SpaceMark(spaceId: space.id, name: space.name, maybeImageVersion: space.maybeImageVersion, size: 24)
        Text(space.name)
          .font(.app(.headline))
          .lineLimit(1)
        Image(systemName: "chevron.down")
          .font(.app(.caption).weight(.bold))
          .foregroundStyle(.secondary)
          .overlay(alignment: .topTrailing) {
            if hasNewElsewhere {
              Circle().fill(Theme.accent).frame(width: 7, height: 7).offset(x: 4, y: -3)
            }
          }
      }
      .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text("space_switcher_open \(space.name)"))
    .accessibilityValue(hasNewElsewhere ? Text("space_switcher_new_elsewhere") : Text(verbatim: ""))
    .accessibilityAddTraits(.isHeader)
  }
}

/// Every space the user belongs to (#1526): its mark, its name and what is new in it; a tap selects
/// it. "Browse spaces" follows where the user may join spaces (not guests). Creating a space stays
/// on desktop and web, where the workspace role is known.
struct SpaceSwitcherView: View {
  @Environment(\.dismiss) private var dismiss
  let spaces: [HomeSpace]
  let selectedId: String?
  let canBrowse: Bool
  let onSelect: (String) -> Void

  var body: some View {
    NavigationStack {
      List {
        if spaces.isEmpty {
          Text("spaces_none").foregroundStyle(Theme.textMuted)
        }
        ForEach(spaces) { space in
          Button {
            onSelect(space.id)
            dismiss()
          } label: {
            HStack(spacing: 12) {
              SpaceMark(spaceId: space.id, name: space.name, maybeImageVersion: space.maybeImageVersion, size: 32)
              Text(space.name).lineLimit(1)
              Spacer(minLength: 8)
              SpaceBadgeMark(space: space)
              if space.id == selectedId {
                Image(systemName: "checkmark").foregroundStyle(Theme.accent).fontWeight(.semibold)
              }
            }
            .contentShape(Rectangle())
          }
          .buttonStyle(.plain)
          .accessibilityAddTraits(space.id == selectedId ? .isSelected : [])
        }
        if canBrowse {
          Section {
            NavigationLink {
              BrowseSpacesView { spaceId in
                onSelect(spaceId)
                dismiss()
              }
            } label: {
              Label("spaces_browse", systemImage: "magnifyingglass")
            }
          }
        }
      }
      .navigationTitle("chats_tab_spaces")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("done") { dismiss() }
        }
      }
    }
    .presentationDetents(spaces.count > 6 ? [.large] : [.medium, .large])
    .presentationDragIndicator(.visible)
  }
}

/// Mentions as a number, other news as a dot, as the bottom navigation counts channels.
struct SpaceBadgeMark: View {
  let space: HomeSpace

  var body: some View {
    if space.mentionCount > 0 {
      Text(verbatim: "\(space.mentionCount)")
        .font(.app(.caption2).weight(.bold))
        .foregroundStyle(.white)
        .padding(.horizontal, 6)
        .frame(minWidth: 18, minHeight: 18)
        .background(Color.red, in: .capsule)
        .accessibilityLabel(Text("chat_mentions \(space.mentionCount)"))
    } else if space.hasUnread {
      Circle().fill(Theme.accent).frame(width: 8, height: 8)
        .accessibilityLabel(Text("space_badge_new"))
    }
  }
}

/// The open spaces the user isn't in, from the space list the device holds (#1526). Joining one
/// selects it on Home once the core has it.
struct BrowseSpacesView: View {
  @Environment(AppModel.self) private var model
  let onJoined: (String) -> Void

  @State private var maybeSpaces: [JoinableSpace]?
  @State private var maybeJoining: String?
  @State private var maybeFailure: AppFailure?

  var body: some View {
    Group {
      if let spaces = maybeSpaces {
        if spaces.isEmpty {
          ContentUnavailableView {
            Label("spaces_browse", systemImage: "square.stack.3d.up")
          } description: {
            Text("spaces_browse_empty")
          }
        } else {
          List(spaces) { space in row(space) }
        }
      } else {
        ProgressView()
      }
    }
    .navigationTitle("spaces_browse")
    .task(id: model.changeCount) { await refresh() }
    .alert("entry_action_failed", isPresented: isFailureShown) {
    } message: {
      if let failure = maybeFailure { Text(failure.message) }
    }
  }

  private func row(_ space: JoinableSpace) -> some View {
    HStack(spacing: 12) {
      SpaceMark(spaceId: space.id, name: space.name, maybeImageVersion: space.maybeImageVersion, size: 40)
      VStack(alignment: .leading, spacing: 2) {
        Text(space.name).lineLimit(1)
        if let description = space.maybeDescription, !description.isEmpty {
          Text(description).font(.app(.footnote)).foregroundStyle(Theme.textMuted).lineLimit(2)
        }
        Text("space_member_count \(space.memberCount ?? 0)").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
      }
      Spacer(minLength: 8)
      Button("space_join") { join(space) }
        .buttonStyle(.bordered)
        .disabled(maybeJoining == space.id)
        .accessibilityLabel(Text("space_join_named \(space.name)"))
    }
  }

  private func join(_ space: JoinableSpace) {
    maybeJoining = space.id
    Task {
      if let failure = await model.joinSpace(space.id) {
        maybeFailure = failure
        maybeJoining = nil
      }
    }
  }

  private func refresh() async {
    maybeSpaces = await model.joinableSpaces() ?? maybeSpaces ?? []
    // The space left the list once the core joined it: Home shows it.
    if let joining = maybeJoining, maybeSpaces?.contains(where: { $0.id == joining }) == false {
      maybeJoining = nil
      onJoined(joining)
    }
  }

  private var isFailureShown: Binding<Bool> {
    Binding(get: { maybeFailure != nil }, set: { _ in maybeFailure = nil })
  }
}
