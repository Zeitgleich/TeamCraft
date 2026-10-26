package de.zeitgleich.teamcraft.ui

import uniffi.teamcraft_ffi.ConversationItem

/**
 * What the user may do in a conversation, as the core decided it from the server's
 * `my_permissions` (issue #758). The screens offer an action only when it is allowed, as on
 * desktop; the server checks every request again. Until the conversation is known, everything is
 * offered and the server decides.
 */
data class ConversationRights(
  /** Write, reply, edit and delete the own messages (an edit or a deletion is a message too). */
  val canPost: Boolean = true,
  val canReact: Boolean = true,
  /** Pin and unpin messages. */
  val canManageBookmarks: Boolean = true,
  /** Change the channel itself, such as archiving it. */
  val canManageChannel: Boolean = true,
) {
  companion object {
    fun of(maybeItem: ConversationItem?): ConversationRights = maybeItem?.let { item ->
      ConversationRights(
        canPost = item.canPost,
        canReact = item.canReact,
        canManageBookmarks = item.canManageBookmarks,
        canManageChannel = item.canManageChannel,
      )
    } ?: ConversationRights()
  }
}

/** An entry of a message's action sheet, below the quick reactions. */
enum class MessageAction { Reply, ReplyInThread, Edit, Delete, Forward, Pin, Save, Acknowledge, CopyLink, CopyText, MarkUnread }

/** What a message's action sheet offers, in order: only what the user's rights allow. */
fun messageActions(
  rights: ConversationRights,
  isMine: Boolean,
  isDeleted: Boolean,
  hasThreads: Boolean,
  wantsAcknowledgement: Boolean,
): List<MessageAction> = buildList {
  if (rights.canPost) add(MessageAction.Reply)
  if (rights.canPost && hasThreads) add(MessageAction.ReplyInThread)
  if (rights.canPost && isMine && !isDeleted) {
    add(MessageAction.Edit)
    add(MessageAction.Delete)
  }
  add(MessageAction.Forward)
  if (rights.canManageBookmarks) add(MessageAction.Pin)
  add(MessageAction.Save)
  if (wantsAcknowledgement && !isMine) add(MessageAction.Acknowledge)
  add(MessageAction.CopyLink)
  add(MessageAction.CopyText)
  add(MessageAction.MarkUnread)
}
