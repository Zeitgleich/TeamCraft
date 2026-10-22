package de.zeitgleich.teamcraft.ui

import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

/** A sidebar section, as the desktop shows them. */
sealed interface SectionTitle {
  data object Favorites : SectionTitle
  data class Space(val name: String) : SectionTitle
  data object Channels : SectionTitle
  data object DirectMessages : SectionTitle
}

data class Section(val title: SectionTitle, val conversations: List<ConversationItem>)

/**
 * Favorites first, then the channels of each space (spaces and channels by name), then direct
 * messages. Empty sections are left out.
 */
fun sections(conversations: List<ConversationItem>): List<Section> {
  val byName = compareBy<ConversationItem, String>(String.CASE_INSENSITIVE_ORDER) { it.name }
  val favorites = conversations.filter { it.isFavorite }.sortedWith(byName)
  val rest = conversations.filterNot { it.isFavorite }
  val direct = rest.filter { it.kind == ConversationKind.DIRECT || it.kind == ConversationKind.GROUP }
  val channels = rest - direct.toSet()
  val bySpace = channels
    .groupBy { it.maybeSpaceName }
    .toSortedMap(nullsLast(String.CASE_INSENSITIVE_ORDER))
    .map { (maybeSpace, items) ->
      Section(maybeSpace?.let { SectionTitle.Space(it) } ?: SectionTitle.Channels, items.sortedWith(byName))
    }
  return buildList {
    if (favorites.isNotEmpty()) add(Section(SectionTitle.Favorites, favorites))
    addAll(bySpace)
    if (direct.isNotEmpty()) add(Section(SectionTitle.DirectMessages, direct.sortedWith(byName)))
  }
}
