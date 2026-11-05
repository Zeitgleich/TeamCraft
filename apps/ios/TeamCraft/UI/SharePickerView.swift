import SwiftUI

/// Where a share from another app goes (issue #208): the notes and the recent conversations on top,
/// then the others, searchable. The pick opens the conversation with its composer filled in; nothing
/// is sent without the user, and an offline send waits in the core's outbox.
struct SharePickerView: View {
  @Environment(AppModel.self) private var model
  let share: StagedShare

  @State private var targets: [ConversationItem] = []
  @State private var filter = ""
  @State private var isLoaded = false

  var body: some View {
    NavigationStack {
      List {
        switch model.shareImportError {
        case .tooLarge(let limit):
          Text("attachment_too_large \(FileSizeText.format(limit))").font(.footnote).foregroundStyle(.red)
        case .unreadable, .sendFailed:
          Text("attachment_unreadable").font(.footnote).foregroundStyle(.red)
        case nil:
          EmptyView()
        }
        if share.leftOut > 0 {
          Text("share_left_out \(share.leftOut)")
            .font(.footnote)
            .foregroundStyle(.red)
        }
        let sections = ShareSections(targets: targets, filter: filter)
        if !sections.top.isEmpty {
          Section("share_recent") { rows(sections.top) }
        }
        if !sections.others.isEmpty {
          Section("share_all") { rows(sections.others) }
        }
      }
      .overlay {
        if isLoaded && targets.isEmpty {
          ContentUnavailableView("share_empty_title", systemImage: "tray", description: Text("share_empty"))
        }
      }
      .searchable(text: $filter, prompt: Text("share_filter"))
      .navigationTitle("share_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("share_discard", role: .destructive) { model.discardShare(share) }
        }
      }
    }
    // Swiping the sheet away must not look like keeping or losing it: the two buttons say which.
    .interactiveDismissDisabled()
    // Again on changes: the conversations and the notes arrive with the first catch-up.
    .task(id: model.changeCount) {
      targets = await model.shareTargets()
      isLoaded = true
    }
  }

  private func rows(_ items: [ConversationItem]) -> some View {
    ForEach(items, id: \.id) { target in
      Button(target.listName) { model.sendShare(share, to: target.id) }
    }
  }
}

/// The list, from the core's order (notes first, then by last activity): notes and the most recently
/// active on top, the rest by name; while filtering, every match once by name.
struct ShareSections {
  private static let recentCount = 5
  let top: [ConversationItem]
  let others: [ConversationItem]

  init(targets: [ConversationItem], filter: String) {
    let needle = filter.trimmingCharacters(in: .whitespaces)
    let byName = targets.sorted {
      $0.listName.localizedCaseInsensitiveCompare($1.listName) == .orderedAscending
    }
    if !needle.isEmpty {
      top = []
      others = byName.filter { $0.listName.localizedCaseInsensitiveContains(needle) }
      return
    }
    let notes = targets.filter(\.isNotesToSelf)
    let recent = targets
      .filter { !$0.isNotesToSelf && $0.maybeLastActivityMs != nil }
      .prefix(Self.recentCount)
    let shown = notes + recent
    top = shown
    others = byName.filter { target in !shown.contains { $0.id == target.id } }
  }
}

/// While the sign-in comes first: what was shared is kept and comes back after it.
struct ShareWaitingNotice: View {
  @Environment(AppModel.self) private var model
  let share: StagedShare

  var body: some View {
    HStack(spacing: 12) {
      Text("share_waiting").font(.footnote)
      Button("share_discard") { model.discardShare(share) }
        .font(.footnote.weight(.semibold))
    }
    .padding(12)
    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
    .padding(.horizontal, 16)
  }
}
