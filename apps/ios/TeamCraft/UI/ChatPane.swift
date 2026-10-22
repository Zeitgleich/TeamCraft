import SwiftUI

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
    .task(id: model.changeCount) { await reload() }
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
              message: message, isMine: message.authorId == account.userId, showsThreadLink: threadId == nil,
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
      HStack(alignment: .bottom, spacing: 8) {
        if maybeEditing != nil {
          Button { cancelEditing() } label: { Image(systemName: "xmark.circle") }
            .accessibilityLabel(Text("cancel"))
        }
        TextField("composer_hint", text: $draft, axis: .vertical)
          .lineLimit(1...5)
          .padding(10)
          .background(Theme.surface)
          .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
        Button { Task { await submit() } } label: {
          Image(systemName: maybeEditing == nil ? "arrow.up.circle.fill" : "checkmark.circle.fill").font(.title)
        }
        .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        .accessibilityLabel(Text("send"))
      }
      .padding(10)
    } else {
      Text("read_only_channel").font(.footnote).foregroundStyle(Theme.textMuted).padding()
    }
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
      if conversation.kind != .integration {
        Button {
          model.calls.startCall(conversationId: conversation.id, displayName: conversation.name)
        } label: {
          Image(systemName: "phone")
        }
        .accessibilityLabel(Text("call_start"))
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
    guard !body.isEmpty else { return }
    draft = ""
    if let editing = maybeEditing {
      maybeEditing = nil
      await model.perform(.editMessage(conversationId: conversation.id, messageId: editing.id, body: body))
    } else {
      await model.send(conversationId: conversation.id, threadId: threadId, body: body)
    }
    await reload()
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
  }
}

struct ThreadTarget: Hashable {
  let threadId: String
}

struct MessageRow: View {
  @Environment(AppModel.self) private var model
  let message: MessageItem
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
        Text(message.body)
          .textSelection(.enabled)
          .padding(10)
          .background(isMine ? Theme.ownBubble : Theme.surface)
          .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
        if !message.reactions.isEmpty { reactions }
        if showsThreadLink && message.replyCount > 0 {
          Button(action: onOpenThread) {
            Text("replies_count \(Int(message.replyCount))").font(.footnote.weight(.semibold))
          }
        }
      }
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
