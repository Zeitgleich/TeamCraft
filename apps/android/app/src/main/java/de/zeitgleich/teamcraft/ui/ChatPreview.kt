package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.MessageItem
import uniffi.teamcraft_ffi.PlaceholderItem

/** Longest preview text kept; the row ellipsizes anyway, this only bounds the work. */
private const val MAX_PREVIEW_CHARS = 160

/** The chat list's second line: the newest message, who sent it and when. */
data class ChatPreview(val sender: PreviewSender, val content: PreviewContent, val sentAt: Long)

/** What the chat list shows: `null` conversations until the first read. */
data class ChatListState(
  val maybeConversations: List<ConversationItem>? = null,
  val previews: Map<String, ChatListEntry> = emptyMap(),
  /** Conversations with a call running now, and how many people are in it (issue #1352). */
  val runningCalls: Map<String, Int> = emptyMap(),
)

/** A conversation's row data: its preview and the newest message's ID (for "mark unread"). */
data class ChatListEntry(val preview: ChatPreview, val lastMessageId: String)

sealed interface PreviewSender {
  /** Nobody named: the other person of a direct message, or an event. */
  data object None : PreviewSender
  data object Me : PreviewSender
  data class Other(val name: String) : PreviewSender
}

sealed interface PreviewContent {
  data class Text(val text: String) : PreviewContent

  /** No text left: the message was deleted (files carry their name as text). */
  data object Deleted : PreviewContent
  data object Event : PreviewContent

  /** Not readable on this device yet (issue #888): [Message.maybePlaceholder]'s state. */
  data class Placeholder(val state: String) : PreviewContent
}

/**
 * The preview of a conversation's newest message. In a one-to-one chat the other person's name is
 * the conversation's name already, so only the user's own messages get a sender ("You: …").
 */
fun chatPreview(message: Message, myId: String, authorName: String, isOneToOne: Boolean): ChatPreview = chatPreview(
  isEvent = message.maybeSystemEvent != null,
  authorId = message.author,
  body = message.body,
  maybePlaceholder = message.maybePlaceholder,
  sentAt = message.sentAt,
  myId = myId,
  authorName = authorName,
  isOneToOne = isOneToOne,
)

/** The same from the core's typed item ([uniffi.teamcraft_ffi.TeamCraft.chatPreviews]). */
fun chatPreview(item: MessageItem, myId: String, isOneToOne: Boolean): ChatPreview = chatPreview(
  isEvent = item.maybeSystemEvent != null,
  authorId = item.authorId,
  body = item.body,
  maybePlaceholder = item.maybePlaceholder?.let(::placeholderState),
  sentAt = item.sentAtMs,
  myId = myId,
  authorName = item.authorName,
  isOneToOne = isOneToOne,
)

private fun chatPreview(
  isEvent: Boolean,
  authorId: String,
  body: String,
  maybePlaceholder: String?,
  sentAt: Long,
  myId: String,
  authorName: String,
  isOneToOne: Boolean,
): ChatPreview {
  val sender = when {
    isEvent -> PreviewSender.None
    authorId == myId -> PreviewSender.Me
    isOneToOne -> PreviewSender.None
    else -> PreviewSender.Other(authorName)
  }
  val text = oneLine(body)
  val content = when {
    isEvent -> PreviewContent.Event
    maybePlaceholder != null -> PreviewContent.Placeholder(maybePlaceholder)
    text.isEmpty() -> PreviewContent.Deleted
    else -> PreviewContent.Text(text)
  }
  return ChatPreview(sender, content, sentAt)
}

/** The bridge's name for a placeholder state ([Message.maybePlaceholder]). */
private fun placeholderState(placeholder: PlaceholderItem): String = when (placeholder) {
  PlaceholderItem.WAITING -> "waiting"
  PlaceholderItem.NOT_AVAILABLE -> "not_available"
  PlaceholderItem.FAILED -> "failed"
}

/** Markdown-ish text folded into one short line. */
fun oneLine(body: String): String =
  body.take(MAX_PREVIEW_CHARS * 2).replace(Regex("\\s+"), " ").trim().take(MAX_PREVIEW_CHARS)
