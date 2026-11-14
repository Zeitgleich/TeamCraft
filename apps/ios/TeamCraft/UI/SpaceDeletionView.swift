import SwiftUI

/// What a space's deletion confirmation is about.
struct SpaceDeletionChoice: Identifiable, Equatable {
  let id: String
  let name: String
  /// The space menu's count, said until the core has listed the channels.
  let channelCount: UInt32
}

/// The confirmation of a space's deletion (e2ee.md §4.6.5, D7, #1875): lists the channels that go,
/// each with who last named it, as desktop, web and Android do. The core signs exactly the channels
/// listed here, so "Delete" waits for the list; the sheet stays open with the reason when the
/// deletion is refused.
struct SpaceDeletionView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let choice: SpaceDeletionChoice

  @State private var listing: Listing = .listing
  @State private var isDeleting = false
  @State private var maybeFailure: AppFailure?

  var body: some View {
    NavigationStack {
      List {
        Section {
          Text("entry_delete_space_text \(channelCount)")
        }
        switch listing {
        case .listing:
          Section {
            HStack(spacing: 12) {
              ProgressView()
              Text("space_delete_listing")
                .foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)
          }
        case .listed(let preview) where !preview.channels.isEmpty:
          Section("space_delete_channels") {
            ForEach(preview.channels, id: \.conversationId) { channel in
              SpaceDeletionRow(channel: channel)
            }
          }
        case .listed, .unlisted:
          EmptyView()
        }
        Section {
          Button(role: .destructive) {
            Task { await delete() }
          } label: {
            HStack {
              Text("entry_delete_channel_confirm")
              if isDeleting {
                Spacer()
                ProgressView()
              }
            }
          }
          .disabled(isListing || isDeleting)
        } footer: {
          if let failure = maybeFailure {
            Text(failure.message)
              .foregroundStyle(Theme.danger)
          }
        }
      }
      .navigationTitle(Text("entry_delete_channel_title \(choice.name)"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel") { dismiss() }.disabled(isDeleting)
        }
      }
    }
    .interactiveDismissDisabled(isDeleting)
    .task(id: choice.id) {
      if let preview = await model.prepareSpaceDeletion(choice.id) {
        listing = .listed(preview)
      } else {
        listing = .unlisted
      }
    }
  }

  private var isListing: Bool {
    if case .listing = listing { return true }
    return false
  }

  private var channelCount: Int {
    if case .listed(let preview) = listing { return preview.channels.count }
    return Int(choice.channelCount)
  }

  private func delete() async {
    guard !isDeleting else { return }
    isDeleting = true
    maybeFailure = nil
    maybeFailure = await model.deleteSpace(choice.id)
    isDeleting = false
    if maybeFailure == nil { dismiss() }
  }

  /// What the sheet knows of the channels; without a list the deletion is unsigned.
  private enum Listing {
    case listing
    case listed(SpaceDeletionPreview)
    case unlisted
  }
}

/// A channel the deletion takes, with who last set its name.
private struct SpaceDeletionRow: View {
  let channel: SpaceDeletionChannel

  var body: some View {
    VStack(alignment: .leading, spacing: 2) {
      Text(verbatim: "#\(channel.name)")
      if let renamer = channel.maybeRenamedBy {
        Text(renamerText(renamer))
          .font(.app(.footnote))
          .foregroundStyle(.secondary)
      }
      if let note = historyNoteText(channel.historyNote) {
        Text(note)
          .font(.app(.footnote))
          .foregroundStyle(.secondary)
      }
    }
    .accessibilityElement(children: .combine)
  }

  private func historyNoteText(_ note: SpaceDeletionHistoryNote) -> LocalizedStringKey? {
    switch note {
    case .none: return nil
    case .memberNameKept: return "space_delete_member_name_kept"
    case .mayBeKept: return "space_delete_unconfirmed_may_be_kept"
    }
  }

  private func renamerText(_ renamer: ChannelRenamer) -> LocalizedStringKey {
    let name = renamer.displayName.isEmpty ? String(localized: "channel_roles_review_unknown") : renamer.displayName
    switch renamer.role {
    case .founder: return "space_delete_named_by_founder \(name)"
    case .channelAdmin, .workspaceAdmin: return "space_delete_renamed_by_admin \(name)"
    case .member: return "space_delete_renamed_by_member \(name)"
    }
  }
}
