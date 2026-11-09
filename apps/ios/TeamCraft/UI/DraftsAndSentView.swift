import SwiftUI

/// Every draft of the user and their recent messages (#1220). Both lists come from this device: the
/// drafts from the encrypted account data, the messages from the local store.
struct DraftsAndSentView: View {
  private enum Tab: Hashable {
    case drafts, sent
  }

  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let onOpen: (OpenedPlace) -> Void

  @State private var tab = Tab.drafts
  @State private var maybeDrafts: [DraftItem]?
  @State private var maybeSent: [SentItem]?
  @State private var maybeDeleting: DraftItem?

  var body: some View {
    NavigationStack {
      VStack(spacing: 0) {
        Picker("drafts_title", selection: $tab) {
          Text("drafts_tab_drafts").tag(Tab.drafts)
          Text("drafts_tab_sent").tag(Tab.sent)
        }
        .pickerStyle(.segmented)
        .padding()
        Group {
          switch tab {
          case .drafts: draftList
          case .sent: sentList
          }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
      }
      .navigationTitle("drafts_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
      }
      .confirmationDialog(
        "drafts_delete_question",
        isPresented: Binding(get: { maybeDeleting != nil }, set: { if !$0 { maybeDeleting = nil } }),
        titleVisibility: .visible
      ) {
        Button("drafts_delete", role: .destructive) {
          if let draft = maybeDeleting { Task { await delete(draft) } }
        }
      }
    }
  }

  @ViewBuilder
  private var draftList: some View {
    Group {
      if let drafts = maybeDrafts {
        if drafts.isEmpty {
          Text("drafts_empty").foregroundStyle(Theme.textMuted).padding()
        } else {
          List(drafts) { draft in
            Button {
              open(OpenedPlace(conversationId: draft.conversation, maybeThreadId: draft.maybeThread, isWriting: true))
            } label: {
              DraftRow(draft: draft)
            }
            .buttonStyle(.plain)
            .swipeActions {
              Button("drafts_delete", systemImage: "trash", role: .destructive) { maybeDeleting = draft }
            }
            .contextMenu {
              Button("drafts_delete", systemImage: "trash", role: .destructive) { maybeDeleting = draft }
            }
          }
        }
      } else {
        ProgressView("loading")
      }
    }
    .task(id: model.changeCount) { maybeDrafts = await model.drafts() }
  }

  @ViewBuilder
  private var sentList: some View {
    Group {
      if let sent = maybeSent {
        if sent.isEmpty {
          Text("sent_empty").foregroundStyle(Theme.textMuted).padding()
        } else {
          List {
            ForEach(sent) { item in
              Button {
                model.focusMessageId = item.message
                open(OpenedPlace(conversationId: item.conversation, maybeThreadId: item.maybeThread))
              } label: {
                SentRow(item: item)
              }
              .buttonStyle(.plain)
            }
            Text("sent_note").font(.app(.caption)).foregroundStyle(Theme.textMuted)
          }
        }
      } else {
        ProgressView("loading")
      }
    }
    // Only read once it is looked at.
    .task(id: model.changeCount) { maybeSent = await model.sentMessages() }
  }

  private func open(_ place: OpenedPlace) {
    dismiss()
    onOpen(place)
  }

  private func delete(_ draft: DraftItem) async {
    // The text and the files, on every device of the user.
    await model.perform(.discardDraft(conversationId: draft.conversation, maybeThreadId: draft.maybeThread))
    maybeDeleting = nil
    maybeDrafts = await model.drafts()
  }
}

private struct DraftRow: View {
  let draft: DraftItem

  var body: some View {
    VStack(alignment: .leading, spacing: 4) {
      HStack {
        Text(verbatim: draft.listName).font(.app(.caption)).foregroundStyle(Theme.accent).lineLimit(1)
        Spacer()
        Text(draft.editedDate, style: .relative).font(.app(.caption)).foregroundStyle(Theme.textMuted)
      }
      if draft.maybeThread != nil {
        Group {
          if let root = draft.maybeThreadRoot {
            Text("drafts_in_thread \(root)")
          } else {
            Text("sent_in_thread")
          }
        }
        .font(.app(.caption))
        .foregroundStyle(Theme.textMuted)
        .lineLimit(1)
      }
      if !draft.body.isEmpty {
        Text(verbatim: draft.body).lineLimit(3)
      }
      if let files = draft.fileCount, files > 0 { FileCountLabel(count: files) }
    }
  }
}

private struct SentRow: View {
  let item: SentItem

  var body: some View {
    VStack(alignment: .leading, spacing: 4) {
      HStack {
        Text(verbatim: item.listName).font(.app(.caption)).foregroundStyle(Theme.accent).lineLimit(1)
        Spacer()
        Text(item.sentDate, style: .relative).font(.app(.caption)).foregroundStyle(Theme.textMuted)
      }
      if item.maybeThread != nil {
        Text("sent_in_thread").font(.app(.caption)).foregroundStyle(Theme.textMuted)
      }
      if !item.excerpt.isEmpty {
        Text(verbatim: item.excerpt).lineLimit(3)
      }
      if let files = item.fileCount, files > 0 { FileCountLabel(count: files) }
    }
  }
}

/// How many files a draft holds or a message carried.
private struct FileCountLabel: View {
  let count: Int

  var body: some View {
    Label {
      Text("sent_files \(count)")
    } icon: {
      Image(systemName: "paperclip")
    }
    .font(.app(.caption))
    .foregroundStyle(Theme.textMuted)
  }
}
