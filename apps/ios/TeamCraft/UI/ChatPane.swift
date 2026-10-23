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
  @State private var mentionNames: (usernames: Set<String>, me: String) = ([], "")
  @State private var maybeHighlighted: String?
  @State private var isPermalinkMissing = false
  @State private var typingNames: [String] = []
  @State private var displayNames: [String: String] = [:]
  @State private var maybeForwarding: MessageTarget?
  @State private var maybeShowingEdits: MessageTarget?
  @State private var maybeLastTypingNotified: Date?
  @AppStorage("share_typing") private var isSharingTyping = true
  @FocusState private var isComposerFocused: Bool

  private static let quickReactions = ["👍", "❤️", "😂", "🎉", "😮", "🙏"]
  /// The reaction a double tap toggles, as in most chat apps.
  private static let quickReaction = "👍"

  var body: some View {
    screenWithDialogs
    .task(id: model.changeCount) {
      await reload()
      await followTransfers()
    }
    .task {
      mentionNames = await model.mentionNames()
      displayNames = await model.displayNames()
    }
    .sheet(item: $maybeForwarding) { target in
      ForwardSheet(fromConversationId: conversation.id, message: target.message)
    }
    .sheet(item: $maybeShowingEdits) { target in
      EditHistorySheet(edits: extras[target.message.id]?.edits ?? [], current: target.message.body)
    }
    .task(id: model.changeCount) { await followTyping() }
    .onAppear { model.visibleConversationId = conversation.id }
    .onDisappear {
      if model.visibleConversationId == conversation.id { model.visibleConversationId = nil }
    }
  }

  /// The messages and the composer, with navigation and pickers.
  private var screen: some View {
    VStack(spacing: 0) {
      OfflineBanner()
      messageList
      if let line = typingText(typingNames) {
        Text(line)
          .font(.caption.italic())
          .foregroundStyle(Theme.textMuted)
          .lineLimit(1)
          .frame(maxWidth: .infinity, alignment: .leading)
          .padding(.horizontal)
          .padding(.vertical, 2)
      }
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
  }

  /// `screen` with its confirmations, alerts and the in-app handling of message links.
  private var screenWithDialogs: some View {
    screen
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
    .alert("permalink_unavailable", isPresented: $isPermalinkMissing) {
      Button("done", role: .cancel) {}
    }
    // Message links open in the app; web links go to the browser as usual.
    .environment(\.openURL, OpenURLAction { url in
      guard url.scheme == "teamcraft" else { return .systemAction }
      Task { await openPermalink(url.absoluteString) }
      return .handled
    })
    .alert("attachment_error_title", isPresented: attachmentErrorBinding, presenting: attachmentError) { _ in
      Button("done", role: .cancel) {}
    } message: { error in
      switch error {
      case .tooLarge(let limit): Text("attachment_too_large \(FileSizeText.format(limit))")
      case .unreadable: Text("attachment_unreadable")
      case .sendFailed: Text("attachment_send_failed")
      }
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
              entryRow(message, isContinuation: isContinuation)
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
      // A message link opened this conversation, or points into it: show the message.
      .onChange(of: messages.map(\.id)) { _, _ in focusLinkedMessage(proxy) }
      .onChange(of: model.focusMessageId) { _, _ in focusLinkedMessage(proxy) }
      .onChange(of: messages.last?.id) { _, last in
        guard last != nil, model.focusMessageId == nil, maybeHighlighted == nil else { return }
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

  /// One message with its gestures: double tap reacts, long press opens the menu.
  private func entryRow(_ message: MessageItem, isContinuation: Bool) -> some View {
    let row = MessageRow(
      message: message, maybeExtras: extras[message.id], isMine: message.authorId == account.userId,
      isContinuation: isContinuation, showsThreadLink: threadId == nil,
      onOpenThread: { maybeThread = ThreadTarget(threadId: message.maybeThreadId ?? message.id) },
      mentionNames: mentionNames,
      displayNames: displayNames
    )
    let highlight: Color = maybeHighlighted == message.id ? Theme.accentSoft : .clear
    return
      row
      .padding(.top, isContinuation ? 0 : 8)
      .background(highlight)
      .contentShape(Rectangle())
      .onTapGesture(count: 2) { react(message, Self.quickReaction) }
      .contextMenu { menu(for: message) }
      .accessibilityAction(named: Text("react_quick \(Self.quickReaction)")) { react(message, Self.quickReaction) }
      .id(message.id)
  }

  @ViewBuilder
  private var composer: some View {
    if conversation.canPost {
      VStack(alignment: .leading, spacing: 6) {
        if let replying = maybeReplying { replyBar(replying) }
        EmojiSuggestionBar(text: draft) { emoji in draft = completeEmoji(in: draft, with: emoji) }
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
            .onChange(of: draft) { _, text in announceTyping(text) }
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
      Button("reply", systemImage: "arrowshape.turn.up.left") {
        maybeReplying = message
        isComposerFocused = true
      }
      Button("reply_in_thread", systemImage: "bubble.left.and.bubble.right") {
        maybeThread = ThreadTarget(threadId: message.maybeThreadId ?? message.id)
      }
    }
    Button("copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = message.body }
    if !message.isPending { sharedMessageActions(for: message) }
    if message.authorId == account.userId && !message.isPending {
      Button("edit", systemImage: "pencil") {
        maybeEditing = message
        draft = message.body
      }
      Button("delete", systemImage: "trash", role: .destructive) { maybeDeleting = message }
    }
  }

  /// Re-reads who types on every change (a typing frame is one) and every couple of seconds while
  /// someone types, since indicators run out without an event.
  private func followTyping() async {
    typingNames = await model.typingNames(conversationId: conversation.id)
    while !typingNames.isEmpty {
      try? await Task.sleep(for: .seconds(2))
      if Task.isCancelled { return }
      typingNames = await model.typingNames(conversationId: conversation.id)
    }
  }

  /// Tells the others that the user types, at most every few seconds and only if they share it.
  private func announceTyping(_ text: String) {
    guard isSharingTyping, maybeEditing == nil, shouldNotifyTyping(text, now: .now, lastNotified: maybeLastTypingNotified)
    else { return }
    maybeLastTypingNotified = .now
    Task { await model.perform(.typing(conversationId: conversation.id)) }
  }

  /// Opens a message link: its thread here, or its conversation (the list switches to it).
  private func openPermalink(_ link: String) async {
    guard let target = await model.resolvePermalink(link) else {
      isPermalinkMissing = true
      return
    }
    model.focusMessageId = target.message
    if target.conversation != conversation.id {
      model.openConversationId = target.conversation
    } else if let thread = target.maybeThread, thread != threadId {
      maybeThread = ThreadTarget(threadId: thread)
    }
  }

  /// Scrolls to the message a link pointed at, once it is loaded, and highlights it for a moment.
  private func focusLinkedMessage(_ proxy: ScrollViewProxy) {
    guard let focus = model.focusMessageId, messages.contains(where: { $0.id == focus }) else { return }
    model.focusMessageId = nil
    maybeHighlighted = focus
    withAnimation { proxy.scrollTo(focus, anchor: .center) }
    Task {
      try? await Task.sleep(for: .seconds(2.5))
      withAnimation { maybeHighlighted = nil }
    }
  }

  /// Forward, pin, save, link, unread from here and the edit history: what Android offers too.
  @ViewBuilder
  private func sharedMessageActions(for message: MessageItem) -> some View {
    let maybeExtras = extras[message.id]
    let isPinned = maybeExtras?.isPinned == true
    let isSaved = maybeExtras?.isSaved == true
    Button("forward", systemImage: "arrowshape.turn.up.right") { maybeForwarding = MessageTarget(message: message) }
    Button(LocalizedStringKey(isPinned ? "unpin" : "pin"), systemImage: isPinned ? "pin.slash" : "pin") {
      Task {
        await model.perform(.setPinned(conversationId: conversation.id, messageId: message.id, isPinned: !isPinned))
      }
    }
    Button(LocalizedStringKey(isSaved ? "unsave" : "save_for_later"), systemImage: isSaved ? "bookmark.slash" : "bookmark") {
      Task { await model.perform(.setSaved(conversationId: conversation.id, messageId: message.id, isSaved: !isSaved)) }
    }
    Button("copy_link", systemImage: "link") {
      Task {
        if let link = await model.permalink(conversationId: conversation.id, messageId: message.id) {
          UIPasteboard.general.string = link
        }
      }
    }
    Button("mark_unread_from_here", systemImage: "envelope.badge") {
      Task { await model.perform(.markUnread(conversationId: conversation.id, fromMessageId: message.id)) }
    }
    if !(maybeExtras?.edits ?? []).isEmpty {
      Button("edit_history", systemImage: "clock.arrow.circlepath") { maybeShowingEdits = MessageTarget(message: message) }
    }
  }

  private func react(_ message: MessageItem, _ emoji: String) {
    Task {
      await model.perform(.toggleReaction(conversationId: conversation.id, messageId: message.id, emoji: emoji))
    }
  }

  private func submit() async {
    // `:)` and `:tada:` become emoji, as on desktop and Android.
    let body = emojiConvert(text: draft.trimmingCharacters(in: .whitespacesAndNewlines))
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
  var mentionNames: (usernames: Set<String>, me: String) = ([], "")
  var displayNames: [String: String] = [:]

  var body: some View {
    HStack(alignment: .top, spacing: 10) {
      if isContinuation {
        // Keeps follow-ups aligned under the first message of the block.
        Color.clear.frame(width: 36, height: 1)
      } else {
        AvatarView(name: message.authorName)
      }
      VStack(alignment: .leading, spacing: 4) {
        if !isContinuation || (message.isPending && !isSendFailed) || message.isEdited {
          HStack(spacing: 6) {
            if !isContinuation {
              Text(isMine ? String(localized: "you") : message.authorName).font(.subheadline.weight(.semibold))
              Text(message.sentDate, style: .time)
                .font(.caption).foregroundStyle(Theme.textMuted)
            }
            if message.isEdited { Text("edited").font(.caption).foregroundStyle(Theme.textMuted) }
            if maybeExtras?.isPinned == true {
              Image(systemName: "pin.fill").font(.caption2).foregroundStyle(Theme.textMuted)
                .accessibilityLabel(Text("pinned"))
            }
            if message.isPending && !isSendFailed {
              Text("pending").font(.caption).foregroundStyle(Theme.textMuted)
            }
          }
        }
        if let reply = maybeExtras?.maybeReply {
          ReplyQuote(author: displayNames[reply.author] ?? "", excerpt: reply.excerpt)
        }
        if !message.body.isEmpty || files.isEmpty {
          Text(messageText(message.body, usernames: mentionNames.usernames, myUsername: mentionNames.me))
            .textSelection(.enabled)
            .padding(10)
            .background(isMine ? Theme.ownBubble : Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
        }
        if isSendFailed { notSentLine }
        if gridImages.count >= 2 { ImageGrid(images: gridImages) }
        ForEach(files.filter { file in !gridImages.contains(file) }) { file in AttachmentView(file: file) }
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

  /// Several pictures in one message show as a grid.
  private var gridImages: [FileInfo] {
    let images = files.filter { $0.isImage && $0.maybeThumbnailData != nil }
    return images.count >= 2 ? images : []
  }

  private var isSendFailed: Bool { maybeExtras?.isSendFailed == true }

  /// An own message the server rejected: say so, and offer to send it again or drop it.
  private var notSentLine: some View {
    HStack(spacing: 12) {
      Label("not_sent", systemImage: "exclamationmark.circle").font(.caption).foregroundStyle(Theme.danger)
      Button("not_sent_retry") {
        Task { await model.perform(.retrySend(conversationId: message.conversationId, messageId: message.id)) }
      }
      .font(.caption.weight(.semibold))
      Button("not_sent_delete", role: .destructive) {
        Task { await model.perform(.discardUnsent(conversationId: message.conversationId, messageId: message.id)) }
      }
      .font(.caption.weight(.semibold))
    }
  }

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

/// A message as the item of a sheet.
struct MessageTarget: Identifiable {
  let message: MessageItem

  var id: String { message.id }
}

/// Pick where to forward a message; it is sent as a copy, re-encrypted for the target.
struct ForwardSheet: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let fromConversationId: String
  let message: MessageItem

  @State private var targets: [ConversationItem] = []

  var body: some View {
    NavigationStack {
      List(targets, id: \.id) { target in
        Button(target.name) {
          Task {
            await model.perform(
              .forward(fromConversationId: fromConversationId, messageId: message.id, toConversationId: target.id))
            dismiss()
          }
        }
      }
      .navigationTitle("forward")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { ToolbarItem(placement: .cancellationAction) { Button("cancel") { dismiss() } } }
    }
    .task { targets = await model.conversations().filter(\.canPost) }
  }
}

/// Earlier versions of an edited message, oldest first, then the current text.
struct EditHistorySheet: View {
  @Environment(\.dismiss) private var dismiss
  let edits: [MessageEdit]
  let current: String

  var body: some View {
    NavigationStack {
      List {
        ForEach(Array(edits.enumerated()), id: \.offset) { _, edit in
          VStack(alignment: .leading, spacing: 4) {
            Text(edit.body)
            Text(Date(timeIntervalSince1970: TimeInterval(edit.replacedAt) / 1000), style: .date)
              .font(.caption).foregroundStyle(Theme.textMuted)
          }
        }
        Text(current).fontWeight(.semibold)
      }
      .navigationTitle("edit_history")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } } }
    }
  }
}

/// The quoted message above a reply.
struct ReplyQuote: View {
  let author: String
  let excerpt: String

  var body: some View {
    HStack(spacing: 8) {
      Rectangle().fill(Theme.accent).frame(width: 3)
      VStack(alignment: .leading, spacing: 2) {
        Text(author).font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
        Text(excerpt).font(.caption).foregroundStyle(Theme.textMuted).lineLimit(2)
      }
    }
    .fixedSize(horizontal: false, vertical: true)
  }
}

/// Square tiles, two per row; a tap opens the picture like a single one.
struct ImageGrid: View {
  let images: [FileInfo]

  var body: some View {
    LazyVGrid(columns: [GridItem(.flexible(), spacing: 4), GridItem(.flexible(), spacing: 4)], spacing: 4) {
      ForEach(images) { image in AttachmentView(file: image, isTile: true) }
    }
    .frame(maxWidth: 260)
  }
}

/// Emoji for the `:name` being typed, from the core's catalog; a tap puts the emoji in.
struct EmojiSuggestionBar: View {
  let text: String
  let onPick: (String) -> Void

  var body: some View {
    if let query = activeEmojiQuery(text) {
      let suggestions = emojiSuggestions(query: query, limit: 8)
      if !suggestions.isEmpty {
        ScrollView(.horizontal, showsIndicators: false) {
          HStack(spacing: 6) {
            ForEach(suggestions, id: \.emoji) { suggestion in
              Button { onPick(suggestion.emoji) } label: {
                Text("\(suggestion.emoji) :\(suggestion.shortcode):")
                  .font(.caption)
                  .padding(.horizontal, 10).padding(.vertical, 6)
                  .background(Theme.surface)
                  .clipShape(Capsule())
              }
              .buttonStyle(.plain)
            }
          }
        }
      }
    }
  }
}
