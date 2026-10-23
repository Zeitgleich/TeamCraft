package de.zeitgleich.teamcraft.ui

import uniffi.teamcraft_ffi.ConversationItem

/** What the selection bar of the chat list offers for the selected conversations. */
data class SelectionActions(
  /** Read when any of them is unread, otherwise unread again (from the newest message). */
  val isMarkingRead: Boolean,
  /** Favorite when any of them isn't one yet, otherwise remove them all from the favorites. */
  val isAddingFavorite: Boolean,
  /** Notifications are set per conversation: only with exactly one selected. */
  val maybeSingle: String?,
)

fun selectionActions(selected: List<ConversationItem>): SelectionActions = SelectionActions(
  isMarkingRead = selected.any { it.unreadCount > 0u },
  isAddingFavorite = selected.any { !it.isFavorite },
  maybeSingle = selected.singleOrNull()?.id,
)

/** The selection after a tap on `id`: in or out. */
fun toggled(selection: List<String>, id: String): List<String> = if (id in selection) selection - id else selection + id
