import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// A conversation, or one thread of it (`threadId`): messages, reactions and the composer.
struct ChatPane: View {
  @Environment(AppModel.self) private var model
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
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
  @State private var isTakingPhoto = false
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
      composer
    }
    .background(Theme.background)
    .navigationTitle(threadId == nil ? conversation.name : String(localized: "thread_title"))
    .navigationBarTitleDisplayMode(.inline)
    .toolbar { if threadId == nil { toolbarItems } }
    .fullScreenCover(isPresented: $isTakingPhoto) {
      CameraPicker { image in Task { await attach(photo: image) } }
        .ignoresSafeArea()
    }
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
    GeometryReader { geometry in
      let listWidth = max(geometry.size.width - Self.listPadding * 2, 0)
      let look = BubbleLook(listWidth: listWidth, showsSenders: showsSenders(conversation.kind), names: names)
      ScrollViewReader { proxy in
        ScrollView {
          // One section per day: its date pill stays at the top while the day scrolls by.
          LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
            ForEach(feedSections(messages)) { section in
              Section {
                ForEach(section.entries) { entry in
                  entryRow(entry, look: look, listWidth: listWidth)
                }
              } header: {
                DayPill(date: section.day)
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
          .padding(.horizontal, Self.listPadding)
          .padding(.bottom, 8)
        }
        // Opens at the newest message, offline too (the messages come from the local store).
        .defaultScrollAnchor(.bottom)
        .scrollDismissesKeyboard(.interactively)
        .overlay(alignment: .bottomTrailing) {
          if !isAtBottom {
            JumpDownButton(count: newBelowCount) {
              withAnimation(scrollAnimation) { proxy.scrollTo(Self.bottomId, anchor: .bottom) }
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
            withAnimation(scrollAnimation) { proxy.scrollTo(Self.bottomId, anchor: .bottom) }
          } else {
            newBelowCount += 1
          }
        }
      }
    }
  }

  private static let listPadding: CGFloat = 8

  private var names: MentionNames {
    MentionNames(usernames: mentionNames.usernames, myUsername: mentionNames.me)
  }

  private static let bottomId = "bottom"

  /// One message with its gestures: double tap reacts, long press opens the menu.
  private func entryRow(_ entry: FeedEntry, look: BubbleLook, listWidth: CGFloat) -> some View {
    let message = entry.message
    let row = MessageRow(
      entry: entry, maybeExtras: extras[message.id], isMine: message.authorId == account.userId, look: look,
      listWidth: listWidth, showsThreadLink: threadId == nil,
      onOpenThread: { maybeThread = ThreadTarget(threadId: message.maybeThreadId ?? message.id) },
      displayNames: displayNames
    )
    let highlight: Color = maybeHighlighted == message.id ? Theme.accentSoft : .clear
    return SwipeToReply(isEnabled: threadId == nil && !message.isPending) {
      maybeReplying = message
      isComposerFocused = true
    } content: {
      row
    }
      .padding(.top, entry.isContinuation ? 2 : 8)
      .background(highlight)
      .contentShape(Rectangle())
      .onTapGesture(count: 2) { react(message, Self.quickReaction) }
      .contextMenu { menu(for: message) }
      .accessibilityAction(named: Text("reply")) {
        maybeReplying = message
        isComposerFocused = true
      }
      .accessibilityAction(named: Text("react_quick \(Self.quickReaction)")) { react(message, Self.quickReaction) }
      .id(message.id)
  }

  /// The message field as in the common messengers (#713): a rounded field with attach on the
  /// leading side and the camera on the trailing side inside it, and a round send button beside
  /// it, filled while there is something to send.
  @ViewBuilder
  private var composer: some View {
    if conversation.canPost {
      VStack(alignment: .leading, spacing: 6) {
        if let replying = maybeReplying { replyBar(replying) }
        EmojiSuggestionBar(text: draft) { emoji in draft = completeEmoji(in: draft, with: emoji) }
        if !attachments.isEmpty { attachmentChips }
        HStack(alignment: .bottom, spacing: 8) {
          HStack(alignment: .bottom, spacing: 0) {
            if maybeEditing != nil {
              Button { cancelEditing() } label: { composerIcon("xmark") }
                .accessibilityLabel(Text("cancel"))
            } else {
              Menu {
                Button("attach_photo", systemImage: "photo.on.rectangle") { isPickingPhoto = true }
                if CameraPicker.isAvailable {
                  Button("attach_camera", systemImage: "camera") { isTakingPhoto = true }
                }
                Button("attach_file", systemImage: "doc") { isPickingDocument = true }
              } label: {
                composerIcon("paperclip")
              }
              .accessibilityLabel(Text("attach"))
            }
            TextField("composer_hint", text: $draft, axis: .vertical)
              .focused($isComposerFocused)
              .onChange(of: draft) { _, text in announceTyping(text) }
              .lineLimit(1...6)
              .padding(.vertical, 10)
            // While typing the field needs the room; the camera comes back when it is empty.
            if draft.isEmpty && maybeEditing == nil && CameraPicker.isAvailable {
              Button { isTakingPhoto = true } label: { composerIcon("camera") }
                .accessibilityLabel(Text("attach_camera"))
            }
          }
          .padding(.horizontal, 4)
          .background(Theme.surface, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
          .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).stroke(Theme.border))
          Button { Task { await submit() } } label: {
            Image(systemName: maybeEditing == nil ? "arrow.up" : "checkmark")
              .font(.body.weight(.semibold))
              .foregroundStyle(canSubmit ? Theme.onAccent : Theme.textMuted)
              .frame(width: 44, height: 44)
              .background(canSubmit ? Theme.accent : Theme.surface, in: Circle())
              .overlay(Circle().stroke(canSubmit ? Color.clear : Theme.border))
          }
          .buttonStyle(.plain)
          .disabled(!canSubmit)
          .accessibilityLabel(Text("send"))
        }
      }
      .padding(.horizontal, 10)
      .padding(.vertical, 8)
      .background(.bar)
    } else {
      Text("read_only_channel").font(.footnote).foregroundStyle(Theme.textMuted).padding()
    }
  }

  /// An icon button inside the composer field: 44 pt to tap, in the muted colour.
  private func composerIcon(_ systemName: String) -> some View {
    Image(systemName: systemName)
      .font(.body)
      .foregroundStyle(Theme.textMuted)
      .frame(width: 40, height: 44)
      .contentShape(Rectangle())
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

  /// A calm header: the conversation's avatar (with the person's presence in a direct message),
  /// the name and one status line; calls and a menu with the rest.
  @ToolbarContentBuilder
  private var toolbarItems: some ToolbarContent {
    ToolbarItem(placement: .principal) {
      ConversationHeader(conversation: conversation)
    }
    ToolbarItemGroup(placement: .topBarTrailing) {
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
      Menu {
        Button(
          LocalizedStringKey(conversation.isFavorite ? "favorite_remove" : "favorite_add"),
          systemImage: conversation.isFavorite ? "star.slash" : "star"
        ) {
          Task { await model.setFavorite(conversationId: conversation.id, isFavorite: !conversation.isFavorite) }
        }
        ConversationMuteMenu(conversationId: conversation.id)
        Button("files_title", systemImage: "folder") { isShowingFiles = true }
      } label: {
        Image(systemName: "ellipsis.circle")
      }
      .accessibilityLabel(Text("more"))
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

  /// Scrolling to a message eases out and quickly; with Reduce Motion it jumps.
  private var scrollAnimation: Animation? { reduceMotion ? nil : .easeOut(duration: 0.2) }

  /// Scrolls to the message a link pointed at, once it is loaded, and highlights it for a moment.
  private func focusLinkedMessage(_ proxy: ScrollViewProxy) {
    guard let focus = model.focusMessageId, messages.contains(where: { $0.id == focus }) else { return }
    model.focusMessageId = nil
    maybeHighlighted = focus
    withAnimation(scrollAnimation) { proxy.scrollTo(focus, anchor: .center) }
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

  /// A photo just taken: saved as a JPEG and attached like a picked one.
  private func attach(photo image: UIImage) async {
    guard let data = image.jpegData(compressionQuality: 0.85) else {
      attachmentError = .unreadable
      return
    }
    let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    let file = folder.appendingPathComponent("Photo-\(Int(Date().timeIntervalSince1970)).jpg")
    do {
      try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
      try data.write(to: file)
      let importer = await model.attachmentImporter()
      attachments.append(try importer.importPickedMedia(PickedMedia(url: file)))
    } catch let error as AttachmentError {
      attachmentError = error
    } catch {
      attachmentError = .unreadable
    }
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

/// Back to the newest message, with how many arrived below.
struct JumpDownButton: View {
  let count: Int
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      Image(systemName: "chevron.down")
        .font(.body.weight(.semibold))
        // 44 pt: the smallest touch target the Human Interface Guidelines allow.
        .frame(width: 44, height: 44)
        .background(.regularMaterial)
        .clipShape(Circle())
        .overlay(alignment: .topTrailing) {
          if count > 0 { CountBadge(count: UInt32(min(count, 999)), tone: .unread).offset(x: 6, y: -6) }
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

/// Drag a message to the right to reply to it, as in most messengers. Vertical drags keep
/// scrolling: only a mostly horizontal drag moves the message.
struct SwipeToReply<Content: View>: View {
  let isEnabled: Bool
  let onReply: () -> Void
  @ViewBuilder let content: () -> Content

  @State private var offset: CGFloat = 0
  @State private var isPastThreshold = false
  private static var threshold: CGFloat { 64 }

  var body: some View {
    content()
      .offset(x: offset)
      .background(alignment: .leading) {
        Image(systemName: "arrowshape.turn.up.left.fill")
          .foregroundStyle(Theme.accent)
          .opacity(Double(min(offset / Self.threshold, 1)))
          .padding(.leading, 4)
          .accessibilityHidden(true)
      }
      .simultaneousGesture(drag, including: isEnabled ? .all : .subviews)
  }

  private var drag: some Gesture {
    DragGesture(minimumDistance: 20)
      .onChanged { value in
        guard value.translation.width > 0, abs(value.translation.width) > abs(value.translation.height) * 2 else { return }
        offset = min(value.translation.width, Self.threshold * 1.5)
        let isPast = offset >= Self.threshold
        if isPast && !isPastThreshold { UIImpactFeedbackGenerator(style: .light).impactOccurred() }
        isPastThreshold = isPast
      }
      .onEnded { _ in
        if isPastThreshold { onReply() }
        isPastThreshold = false
        withAnimation(.spring(duration: 0.25)) { offset = 0 }
      }
  }
}
