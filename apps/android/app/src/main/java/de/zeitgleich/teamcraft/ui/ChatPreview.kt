package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message

/** Longest preview text kept; the row ellipsizes anyway, this only bounds the work. */
private const val MAX_PREVIEW_CHARS = 160

/** The chat list's second line: the newest message, who sent it and when. */
data class ChatPreview(val sender: PreviewSender, val content: PreviewContent, val sentAt: Long)

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
}

/**
 * The preview of a conversation's newest message. In a one-to-one chat the other person's name is
 * the conversation's name already, so only the user's own messages get a sender ("You: …").
 */
fun chatPreview(message: Message, myId: String, authorName: String, isOneToOne: Boolean): ChatPreview {
  val isEvent = message.maybeSystemEvent != null
  val sender = when {
    isEvent -> PreviewSender.None
    message.author == myId -> PreviewSender.Me
    isOneToOne -> PreviewSender.None
    else -> PreviewSender.Other(authorName)
  }
  val text = oneLine(message.body)
  val content = when {
    isEvent -> PreviewContent.Event
    text.isEmpty() -> PreviewContent.Deleted
    else -> PreviewContent.Text(text)
  }
  return ChatPreview(sender, content, message.sentAt)
}

/** Markdown-ish text folded into one short line. */
fun oneLine(body: String): String =
  body.take(MAX_PREVIEW_CHARS * 2).replace(Regex("\\s+"), " ").trim().take(MAX_PREVIEW_CHARS)
