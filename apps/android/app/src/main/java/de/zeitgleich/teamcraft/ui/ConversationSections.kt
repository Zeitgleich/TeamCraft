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

/** The two lists of the chats screen: Home (the DMs, outside spaces) and the channels by space. */
enum class ChatTab { Home, Spaces }

/** The filter chips of Home (as in Element): one at a time, `All` is none selected. */
enum class HomeFilter {
  All,
  Unread,
  People,
  Groups,
  ;

  fun matches(item: ConversationItem): Boolean = when (this) {
    All -> true
    Unread -> item.mentionCount > 0u || (item.unreadCount > 0u && !item.isMuted)
    People -> item.kind == ConversationKind.DIRECT
    Groups -> item.kind == ConversationKind.GROUP
  }
}

/** DMs and group DMs live outside spaces and have their own tab. */
val ConversationItem.isDirect: Boolean
  get() = kind == ConversationKind.DIRECT || kind == ConversationKind.GROUP

/** Channels of the spaces: favorite channels first, then each space (spaces and channels by name). */
fun channelSections(conversations: List<ConversationItem>): List<Section> {
  val byName = compareBy<ConversationItem, String>(String.CASE_INSENSITIVE_ORDER) { it.name }
  val channels = conversations.filterNot { it.isDirect }
  val favorites = channels.filter { it.isFavorite }.sortedWith(byName)
  val bySpace = channels
    .filterNot { it.isFavorite }
    .groupBy { it.maybeSpaceName }
    .toSortedMap(nullsLast(String.CASE_INSENSITIVE_ORDER))
    .map { (maybeSpace, items) ->
      Section(maybeSpace?.let { SectionTitle.Space(it) } ?: SectionTitle.Channels, items.sortedWith(byName))
    }
  return buildList {
    if (favorites.isNotEmpty()) add(Section(SectionTitle.Favorites, favorites))
    addAll(bySpace)
  }
}

/**
 * The Home tab: favorites, then the rest, each the most recent first (`lastActivity`: newest
 * message per conversation; by name without one). Only conversations `chip` lets through whose
 * names contain `filter` (any case) are kept. Empty sections are left out.
 */
fun directSections(
  conversations: List<ConversationItem>,
  lastActivity: Map<String, Long> = emptyMap(),
  filter: String = "",
  chip: HomeFilter = HomeFilter.All,
): List<Section> {
  val needle = filter.trim()
  val byName = compareBy<ConversationItem, String>(String.CASE_INSENSITIVE_ORDER) { it.name }
  val byRecency = compareByDescending<ConversationItem> { lastActivity[it.id] ?: Long.MIN_VALUE }.then(byName)
  val direct = conversations
    .filter { it.isDirect && chip.matches(it) && (needle.isEmpty() || it.name.contains(needle, ignoreCase = true)) }
    .sortedWith(byRecency)
  val (favorites, rest) = direct.partition { it.isFavorite }
  return buildList {
    if (favorites.isNotEmpty()) add(Section(SectionTitle.Favorites, favorites))
    if (rest.isNotEmpty()) add(Section(SectionTitle.DirectMessages, rest))
  }
}

/** What a tab shows next to its name: mentions, unread messages, or nothing. */
data class TabBadge(val mentions: Int, val unread: Int)

/**
 * The badge of a tab: mentions everywhere; unread messages count in DMs (they are addressed to the
 * user), in channels only as "something new". Muted conversations count only mentions.
 */
fun tabBadge(conversations: List<ConversationItem>): TabBadge {
  val audible = conversations.filterNot { it.isMuted }
  return TabBadge(
    mentions = conversations.sumOf { it.mentionCount.toInt() },
    unread = audible.sumOf { it.unreadCount.toInt() },
  )
}
