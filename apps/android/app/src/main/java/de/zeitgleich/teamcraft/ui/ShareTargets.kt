package de.zeitgleich.teamcraft.ui

import uniffi.teamcraft_ffi.ConversationItem

/** How many of the most recently active conversations the share picker offers on top. */
const val SHARE_RECENT_COUNT = 5

/**
 * The share picker's sections (issue #829), from the core's share targets (conversations the user
 * may post in): the notes (named [notesTitle]) first, then the [SHARE_RECENT_COUNT] most recently
 * active, then the DMs and the channels by space, each by name. Only targets whose list name
 * contains [filter] (any case) are kept; while filtering there is no recent section, so every
 * match shows once in its place. Empty sections are left out.
 */
fun shareSections(targets: List<ConversationItem>, notesTitle: String, filter: String = ""): List<Section> {
  val needle = filter.trim()
  val matching = targets.filter { needle.isEmpty() || it.listName(notesTitle).contains(needle, ignoreCase = true) }
  val (notes, rest) = matching.partition { it.isNotesToSelf }
  val recent = if (needle.isEmpty()) recentTargets(rest) else emptyList()
  val recentIds = recent.mapTo(HashSet()) { it.id }
  val byName = compareBy<ConversationItem, String>(String.CASE_INSENSITIVE_ORDER) { it.name }
  val others = rest.filterNot { it.id in recentIds }
  val (direct, channels) = others.partition { it.isDirect }
  val bySpace = channels
    .groupBy { it.maybeSpaceName }
    .toSortedMap(nullsLast(String.CASE_INSENSITIVE_ORDER))
    .map { (maybeSpace, items) ->
      Section(maybeSpace?.let { SectionTitle.Space(it) } ?: SectionTitle.Channels, items.sortedWith(byName))
    }
  return buildList {
    if (notes.isNotEmpty()) add(Section(SectionTitle.Notes, notes))
    if (recent.isNotEmpty()) add(Section(SectionTitle.Recent, recent))
    if (direct.isNotEmpty()) add(Section(SectionTitle.DirectMessages, direct.sortedWith(byName)))
    addAll(bySpace)
  }
}

private fun recentTargets(targets: List<ConversationItem>): List<ConversationItem> =
  targets
    .filter { it.maybeLastActivityMs != null }
    .sortedByDescending { it.maybeLastActivityMs }
    .take(SHARE_RECENT_COUNT)
