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
  @State private var maybeReplying: MessageItem?
  @State private var maybeDeleting: MessageItem?
  @State private var isAtBottom = true
  @State private var newBelowCount = 0
  @State private var isSendFailed = false
  @FocusState private var isComposerFocused: Bool

  private static let quickReactions = ["👍", "❤️", "😂", "🎉", "😮", "🙏"]
  /// The reaction a double tap toggles, as in most chat apps.
  private static let quickReaction = "👍"

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
    .confirmationDialog("delete_confirm_title", isPresented: deletingBinding, titleVisibility: .visible, presenting: maybeDeleting) {
      message in
      Button("delete", role: .destructive) {
        Task { await model.perform(.deleteMessage(conversationId: conversation.id, messageId: message.id)) }
      }
    } message: { _ in
      Text("delete_confirm_text")
    }
    .alert("send_failed", isPresented: $isSendFailed) {
      Button("done", role: .cancel) {}
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
        LazyVStack(alignment: .leading, spacing: 4) {
          ForEach(feedItems(messages)) { item in
            switch item {
            case .day(let date):
              DayChip(date: date)
            case .entry(let message, let isContinuation):
              MessageRow(
                message: message, maybeExtras: extras[message.id], isMine: message.authorId == account.userId,
                isContinuation: isContinuation, showsThreadLink: threadId == nil,
                onOpenThread: { maybeThread = ThreadTarget(threadId: message.maybeThreadId ?? message.id) }
              )
              .padding(.top, isContinuation ? 0 : 8)
              .contentShape(Rectangle())
              .onTapGesture(count: 2) { react(message, Self.quickReaction) }
              .contextMenu { menu(for: message) }
              .accessibilityAction(named: Text("react_quick \(Self.quickReaction)")) { react(message, Self.quickReaction) }
              .id(message.id)
            }
          }
          // Visible exactly while the user reads at the bottom.
          Color.clear.frame(height: 1).id(Self.bottomId)
            .onAppear {
              isAtBottom = true
              newBelowCount = 0
            }
            .onDisappear { isAtBottom = false }
        }
        .padding()
      }
      .overlay(alignment: .bottomTrailing) {
        if !isAtBottom {
          JumpDownButton(count: newBelowCount) {
            withAnimation { proxy.scrollTo(Self.bottomId, anchor: .bottom) }
          }
          .padding()
        }
      }
      .onChange(of: messages.last?.id) { _, last in
        guard last != nil else { return }
        // Own messages always come into view; others' only while reading at the bottom.
        if isAtBottom || messages.last?.authorId == account.userId {
          withAnimation { proxy.scrollTo(Self.bottomId, anchor: .bottom) }
        } else {
          newBelowCount += 1
        }
      }
    }
  }

  private static let bottomId = "bottom"

  @ViewBuilder
  private var composer: some View {
    if conversation.canPost {
      VStack(alignment: .leading, spacing: 6) {
        if let replying = maybeReplying { replyBar(replying) }
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
            .focused($isComposerFocused)
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

  /// The message being answered, with a way out.
  private func replyBar(_ message: MessageItem) -> some View {
    HStack {
      Rectangle().fill(Theme.accent).frame(width: 3)
      VStack(alignment: .leading, spacing: 2) {
        Text("replying_to \(message.authorName)").font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
        Text(oneLine(message.body)).font(.caption).foregroundStyle(Theme.textMuted).lineLimit(1)
      }
      Spacer()
      Button { maybeReplying = nil } label: { Image(systemName: "xmark.circle.fill") }
        .foregroundStyle(Theme.textMuted)
        .accessibilityLabel(Text("cancel"))
    }
    .frame(maxHeight: 40)
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

  private var deletingBinding: Binding<Bool> {
    Binding(get: { maybeDeleting != nil }, set: { if !$0 { maybeDeleting = nil } })
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
      Button("reply", systemImage: "arrowshape.turn.up.left") {
        maybeReplying = message
        isComposerFocused = true
      }
      Button("reply_in_thread", systemImage: "bubble.left.and.bubble.right") {
        maybeThread = ThreadTarget(threadId: message.maybeThreadId ?? message.id)
      }
    }
    Button("copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = message.body }
    if message.authorId == account.userId && !message.isPending {
      Button("edit", systemImage: "pencil") {
        maybeEditing = message
        draft = message.body
      }
      Button("delete", systemImage: "trash", role: .destructive) { maybeDeleting = message }
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
    } else if attachments.isEmpty && maybeReplying == nil {
      guard !body.isEmpty else { return }
      draft = ""
      if !(await model.send(conversationId: conversation.id, threadId: threadId, body: body)) {
        // Nothing was sent: the text stays for another try.
        draft = body
        isSendFailed = true
      }
    } else {
      let files = attachments
      let replying = maybeReplying
      draft = ""
      attachments = []
      maybeReplying = nil
      let request = SendRequest(
        conversationId: conversation.id, maybeThreadId: threadId, body: body, files: files,
        maybeReplyToId: replying?.id)
      if !(await model.performChecked(.sendMessage(request))) {
        draft = body
        maybeReplying = replying
        if files.isEmpty { isSendFailed = true } else { attachmentError = .sendFailed }
      }
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
  var isContinuation = false
  let showsThreadLink: Bool
  let onOpenThread: () -> Void

  var body: some View {
    HStack(alignment: .top, spacing: 10) {
      if isContinuation {
        // Keeps follow-ups aligned under the first message of the block.
        Color.clear.frame(width: 36, height: 1)
      } else {
        AvatarView(name: message.authorName)
      }
      VStack(alignment: .leading, spacing: 4) {
        if !isContinuation || message.isPending || message.isEdited {
          HStack(spacing: 6) {
            if !isContinuation {
              Text(isMine ? String(localized: "you") : message.authorName).font(.subheadline.weight(.semibold))
              Text(message.sentDate, style: .time)
                .font(.caption).foregroundStyle(Theme.textMuted)
            }
            if message.isEdited { Text("edited").font(.caption).foregroundStyle(Theme.textMuted) }
            if message.isPending { Text("pending").font(.caption).foregroundStyle(Theme.textMuted) }
          }
        }
        if !message.body.isEmpty || files.isEmpty {
          Text(linkedText(message.body))
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

/// A date above the first message of each day.
struct DayChip: View {
  let date: Date

  var body: some View {
    Text(label)
      .font(.caption.weight(.medium))
      .foregroundStyle(Theme.textMuted)
      .padding(.horizontal, 12).padding(.vertical, 4)
      .background(Theme.surface)
      .clipShape(Capsule())
      .frame(maxWidth: .infinity)
      .padding(.vertical, 6)
  }

  private var label: String {
    if Calendar.current.isDateInToday(date) { return String(localized: "day_today") }
    if Calendar.current.isDateInYesterday(date) { return String(localized: "day_yesterday") }
    return date.formatted(.dateTime.weekday(.wide).day().month(.wide))
  }
}

/// Back to the newest message, with how many arrived below.
struct JumpDownButton: View {
  let count: Int
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      Image(systemName: "chevron.down")
        .font(.body.weight(.semibold))
        .frame(width: 40, height: 40)
        .background(.regularMaterial)
        .clipShape(Circle())
        .overlay(alignment: .topTrailing) {
          if count > 0 { CountBadge(count: UInt32(min(count, 999))).offset(x: 6, y: -6) }
        }
    }
    .buttonStyle(.plain)
    .accessibilityLabel(count > 0 ? Text("jump_down_new \(count)") : Text("jump_down"))
  }
}
