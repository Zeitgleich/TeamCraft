import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// A conversation, or one thread of it (`threadId`): messages, reactions and the composer.
struct ChatPane: View {
  @Environment(AppModel.self) private var model
  let conversation: ConversationItem
  let threadId: String?
  let account: Account

  @State private var messages: [MessageItem] = []
  @State private var draft = ""
  @State private var maybeEditing: MessageItem?
  @State private var maybeThread: ThreadTarget?
  @State private var extras: [String: MessageExtras] = [:]
  @State private var attachments: [PickedFile] = []
  @State private var attachmentError: AttachmentError?
  @State private var isPickingPhoto = false
  @State private var isPickingDocument = false
  @State private var photoSelection: [PhotosPickerItem] = []
  @State private var isShowingFiles = false

  private static let quickReactions = ["👍", "❤️", "😂", "🎉", "😮", "🙏"]

  var body: some View {
    VStack(spacing: 0) {
      messageList
      Divider()
      composer
    }
    .background(Theme.background)
    .navigationTitle(threadId == nil ? conversation.name : String(localized: "thread_title"))
    .navigationBarTitleDisplayMode(.inline)
    .toolbar { if threadId == nil { toolbarItems } }
    .navigationDestination(item: $maybeThread) { target in
      ChatPane(conversation: conversation, threadId: target.threadId, account: account)
    }
    .navigationDestination(isPresented: $isShowingFiles) { FilesGalleryView(conversation: conversation) }
    .photosPicker(
      isPresented: $isPickingPhoto, selection: $photoSelection, maxSelectionCount: 10,
      matching: .any(of: [.images, .videos])
    )
    .onChange(of: photoSelection) { _, items in
      if !items.isEmpty { Task { await attach(photos: items) } }
    }
    .fileImporter(isPresented: $isPickingDocument, allowedContentTypes: [.item], allowsMultipleSelection: true) {
      result in
      Task { await attach(documents: result) }
    }
    .alert("attachment_error_title", isPresented: attachmentErrorBinding, presenting: attachmentError) { _ in
      Button("done", role: .cancel) {}
    } message: { error in
      switch error {
      case .tooLarge(let limit): Text("attachment_too_large \(FileSizeText.format(limit))")
      case .unreadable: Text("attachment_unreadable")
      case .sendFailed: Text("attachment_send_failed")
      }
    }
    .task(id: model.changeCount) {
      await reload()
      await followTransfers()
    }
    .onAppear { model.visibleConversationId = conversation.id }
    .onDisappear {
      if model.visibleConversationId == conversation.id { model.visibleConversationId = nil }
    }
  }

  private var messageList: some View {
    ScrollViewReader { proxy in
      ScrollView {
        LazyVStack(alignment: .leading, spacing: 10) {
          ForEach(messages, id: \.id) { message in
            MessageRow(
              message: message, maybeExtras: extras[message.id], isMine: message.authorId == account.userId,
              showsThreadLink: threadId == nil,
              onOpenThread: { maybeThread = ThreadTarget(threadId: message.maybeThreadId ?? message.id) }
            )
            .contextMenu { menu(for: message) }
            .id(message.id)
          }
        }
        .padding()
      }
      .onChange(of: messages.last?.id) { _, last in
        if let last { withAnimation { proxy.scrollTo(last, anchor: .bottom) } }
      }
    }
  }

  @ViewBuilder
  private var composer: some View {
    if conversation.canPost {
      VStack(alignment: .leading, spacing: 6) {
        if !attachments.isEmpty { attachmentChips }
        HStack(alignment: .bottom, spacing: 8) {
          if maybeEditing != nil {
            Button { cancelEditing() } label: { Image(systemName: "xmark.circle") }
              .accessibilityLabel(Text("cancel"))
          } else {
            Menu {
              Button("attach_photo", systemImage: "photo") { isPickingPhoto = true }
              Button("attach_file", systemImage: "doc") { isPickingDocument = true }
            } label: {
              Image(systemName: "plus.circle").font(.title)
            }
            .accessibilityLabel(Text("attach"))
          }
          TextField("composer_hint", text: $draft, axis: .vertical)
            .lineLimit(1...5)
            .padding(10)
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
          Button { Task { await submit() } } label: {
            Image(systemName: maybeEditing == nil ? "arrow.up.circle.fill" : "checkmark.circle.fill").font(.title)
          }
          .disabled(!canSubmit)
          .accessibilityLabel(Text("send"))
        }
      }
      .padding(10)
    } else {
      Text("read_only_channel").font(.footnote).foregroundStyle(Theme.textMuted).padding()
    }
  }

  /// The picked files waiting to be sent, each removable.
  private var attachmentChips: some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 6) {
        ForEach(attachments) { file in
          Button { attachments.removeAll { $0.id == file.id } } label: {
            Label(file.fileName, systemImage: "xmark.circle.fill")
              .labelStyle(.titleAndIcon).font(.caption).lineLimit(1)
              .padding(.horizontal, 10).padding(.vertical, 6)
              .background(Theme.accentSoft).clipShape(Capsule())
          }
          .buttonStyle(.plain)
          .accessibilityLabel(Text(file.fileName))
        }
      }
    }
  }

  private var canSubmit: Bool {
    !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || (maybeEditing == nil && !attachments.isEmpty)
  }

  private var attachmentErrorBinding: Binding<Bool> {
    Binding(get: { attachmentError != nil }, set: { if !$0 { attachmentError = nil } })
  }

  @ToolbarContentBuilder
  private var toolbarItems: some ToolbarContent {
    ToolbarItemGroup(placement: .topBarTrailing) {
      Button {
        Task { await model.setFavorite(conversationId: conversation.id, isFavorite: !conversation.isFavorite) }
      } label: {
        Image(systemName: conversation.isFavorite ? "star.fill" : "star")
      }
      .accessibilityLabel(Text("favorite"))
      ConversationMuteMenu(conversationId: conversation.id)
      Button { isShowingFiles = true } label: { Image(systemName: "folder") }
        .accessibilityLabel(Text("files_title"))
      if conversation.kind != .integration {
        Button {
          model.calls.startCall(conversationId: conversation.id, displayName: conversation.name)
        } label: {
          Image(systemName: "phone")
        }
        .accessibilityLabel(Text("call_start"))
        Button {
          model.calls.startCall(conversationId: conversation.id, displayName: conversation.name, withVideo: true)
        } label: {
          Image(systemName: "video")
        }
        .accessibilityLabel(Text("call_start_video"))
      }
    }
  }

  @ViewBuilder
  private func menu(for message: MessageItem) -> some View {
    Menu("react") {
      ForEach(Self.quickReactions, id: \.self) { emoji in
        Button(emoji) { react(message, emoji) }
      }
    }
    if threadId == nil {
      Button("reply_in_thread", systemImage: "arrowshape.turn.up.left") {
        maybeThread = ThreadTarget(threadId: message.maybeThreadId ?? message.id)
      }
    }
    Button("copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = message.body }
    if message.authorId == account.userId && !message.isPending {
      Button("edit", systemImage: "pencil") {
        maybeEditing = message
        draft = message.body
      }
      Button("delete", systemImage: "trash", role: .destructive) {
        Task {
          await model.perform(.deleteMessage(conversationId: conversation.id, messageId: message.id))
        }
      }
    }
  }

  private func react(_ message: MessageItem, _ emoji: String) {
    Task {
      await model.perform(.toggleReaction(conversationId: conversation.id, messageId: message.id, emoji: emoji))
    }
  }

  private func submit() async {
    let body = draft.trimmingCharacters(in: .whitespacesAndNewlines)
    if let editing = maybeEditing {
      guard !body.isEmpty else { return }
      draft = ""
      maybeEditing = nil
      await model.perform(.editMessage(conversationId: conversation.id, messageId: editing.id, body: body))
    } else if attachments.isEmpty {
      guard !body.isEmpty else { return }
      draft = ""
      await model.send(conversationId: conversation.id, threadId: threadId, body: body)
    } else {
      let files = attachments
      draft = ""
      attachments = []
      let request = SendRequest(conversationId: conversation.id, maybeThreadId: threadId, body: body, files: files)
      if !(await model.performChecked(.sendMessage(request))) { attachmentError = .sendFailed }
    }
    await reload()
  }

  private func attach(photos items: [PhotosPickerItem]) async {
    let importer = await model.attachmentImporter()
    for item in items {
      do {
        guard let media = try await item.loadTransferable(type: PickedMedia.self) else { continue }
        attachments.append(try importer.importPickedMedia(media))
      } catch let error as AttachmentError {
        attachmentError = error
      } catch {
        attachmentError = .unreadable
      }
    }
    photoSelection = []
  }

  private func attach(documents result: Result<[URL], Error>) async {
    guard case .success(let urls) = result else { return }
    let importer = await model.attachmentImporter()
    for url in urls {
      do {
        attachments.append(try importer.importDocument(at: url))
      } catch let error as AttachmentError {
        attachmentError = error
      } catch {
        attachmentError = .unreadable
      }
    }
  }

  private func cancelEditing() {
    maybeEditing = nil
    draft = ""
  }

  private func reload() async {
    if let threadId {
      messages = await model.thread(conversationId: conversation.id, threadId: threadId)
    } else {
      messages = await model.messages(conversationId: conversation.id)
    }
    if conversation.unreadCount > 0 { await model.markRead(conversationId: conversation.id) }
    await reloadExtras()
  }

  private func reloadExtras() async {
    extras = await model.extras(conversationId: conversation.id, messageIds: messages.map(\.id))
  }

  /// While a file moves, its progress is re-read every second (the core reports changes in the
  /// store, not every chunk).
  private func followTransfers() async {
    while extras.values.contains(where: { $0.files?.contains { $0.transfer.isInProgress } == true }) {
      try? await Task.sleep(for: .seconds(1))
      if Task.isCancelled { return }
      await reloadExtras()
    }
  }
}

struct ThreadTarget: Hashable {
  let threadId: String
}

struct MessageRow: View {
  @Environment(AppModel.self) private var model
  let message: MessageItem
  let maybeExtras: MessageExtras?
  let isMine: Bool
  let showsThreadLink: Bool
  let onOpenThread: () -> Void

  var body: some View {
    HStack(alignment: .top, spacing: 10) {
      AvatarView(name: message.authorName)
      VStack(alignment: .leading, spacing: 4) {
        HStack(spacing: 6) {
          Text(isMine ? String(localized: "you") : message.authorName).font(.subheadline.weight(.semibold))
          Text(Date(timeIntervalSince1970: TimeInterval(message.sentAtMs) / 1000), style: .time)
            .font(.caption).foregroundStyle(Theme.textMuted)
          if message.isEdited { Text("edited").font(.caption).foregroundStyle(Theme.textMuted) }
          if message.isPending { Text("pending").font(.caption).foregroundStyle(Theme.textMuted) }
        }
        if !message.body.isEmpty || files.isEmpty {
          Text(message.body)
            .textSelection(.enabled)
            .padding(10)
            .background(isMine ? Theme.ownBubble : Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
        }
        ForEach(files) { file in AttachmentView(file: file) }
        if let preview = maybeExtras?.maybeLinkPreview { LinkPreviewCard(preview: preview) }
        if !message.reactions.isEmpty { reactions }
        if showsThreadLink && message.replyCount > 0 {
          Button(action: onOpenThread) {
            Text("replies_count \(Int(message.replyCount))").font(.footnote.weight(.semibold))
          }
        }
      }
    }
  }

  private var files: [FileInfo] { maybeExtras?.files ?? [] }

  private var reactions: some View {
    HStack(spacing: 6) {
      ForEach(message.reactions, id: \.emoji) { reaction in
        Button {
          Task {
            await model.perform(
              .toggleReaction(
                conversationId: message.conversationId, messageId: message.id, emoji: reaction.emoji))
          }
        } label: {
          Text("\(reaction.emoji) \(reaction.count)")
            .font(.caption)
            .padding(.horizontal, 8).padding(.vertical, 4)
            .background(reaction.reactedByMe ? Theme.accentSoft : Theme.surface)
            .overlay(Capsule().stroke(reaction.reactedByMe ? Theme.accent : Theme.border))
            .clipShape(Capsule())
        }
        .buttonStyle(.plain)
      }
    }
  }
}
