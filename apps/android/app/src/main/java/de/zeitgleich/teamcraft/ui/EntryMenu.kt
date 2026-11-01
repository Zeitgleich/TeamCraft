package de.zeitgleich.teamcraft.ui

import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.LeaveChoice

/** What a long-press menu entry does. */
enum class EntryAction {
  /** Starts the multi-selection of the chat list. */
  Select,
  MarkRead,
  /** The conversation's level (all, mentions, nothing) and mute time. */
  Notifications,
  CopyLink,
  LeaveChannel,
  MuteSpace,
  MarkSpaceRead,
  LeaveSpace,
}

/** One entry of the menu: [isEnabled] is false where the user is told why instead (last admin). */
data class EntryItem(val action: EntryAction, val isEnabled: Boolean = true)

/**
 * The menu of a channel or chat, only with what the user may do (#691). The leave rule is the
 * core's ([LeaveChoice]); this only turns it into entries.
 */
fun conversationMenu(conversation: ConversationItem, leave: LeaveChoice, hasLink: Boolean): List<EntryItem> = buildList {
  add(EntryItem(EntryAction.Select))
  if (conversation.unreadCount > 0u || conversation.mentionCount > 0u) add(EntryItem(EntryAction.MarkRead))
  add(EntryItem(EntryAction.Notifications))
  if (hasLink) add(EntryItem(EntryAction.CopyLink))
  leaveEntry(EntryAction.LeaveChannel, leave)?.let(::add)
}

/** The menu of a space's header. */
fun spaceMenu(hasUnread: Boolean, leave: LeaveChoice): List<EntryItem> = buildList {
  add(EntryItem(EntryAction.MuteSpace))
  if (hasUnread) add(EntryItem(EntryAction.MarkSpaceRead))
  leaveEntry(EntryAction.LeaveSpace, leave)?.let(::add)
}

private fun leaveEntry(action: EntryAction, leave: LeaveChoice): EntryItem? = when (leave) {
  LeaveChoice.HIDDEN -> null
  LeaveChoice.ALLOWED -> EntryItem(action)
  LeaveChoice.LAST_ADMIN -> EntryItem(action, isEnabled = false)
}
