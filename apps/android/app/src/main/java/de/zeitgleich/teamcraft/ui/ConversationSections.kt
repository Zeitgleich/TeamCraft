package de.zeitgleich.teamcraft.ui

import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

/** A sidebar section, as the desktop shows them. */
sealed interface SectionTitle {
  data object Favorites : SectionTitle
  data object Archived : SectionTitle
  /**
   * A space's channels under its name. [maybeId] and [maybeImageVersion] are set where the section
   * comes from the space list rather than its channels: a space without joined channels (#1526).
   */
  data class Space(val name: String, val maybeId: String? = null, val maybeImageVersion: String? = null) : SectionTitle

  /** The selected space's channels on Home of the spaces layout (#1526); the header names the space. */
  data class SpaceChannels(val spaceId: String) : SectionTitle
  data object Channels : SectionTitle
  data object DirectMessages : SectionTitle

  /** The list layout's conversations that are not favorites (#1151). */
  data object Everything : SectionTitle

  /** The spaces under the list layout's list. */
  data object Spaces : SectionTitle

  /** The user's notes to themself, pinned above everything (the share picker, issue #829). */
  data object Notes : SectionTitle

  /** The most recently active conversations (the share picker). */
  data object Recent : SectionTitle

  /** The Activity tab's one list (shown without a header). */
  data object Activity : SectionTitle

  /** Home's DMs with something new, on top of its list (#1488). */
  data object UnreadDirect : SectionTitle
}

data class Section(val title: SectionTitle, val conversations: List<ConversationItem>)

/**
 * The bottom navigation of the main screen (the desktop's One Home, #237; the layout of #1488):
 * Home (the quick-access cards, the DMs with something new and the channels by space), the DMs,
 * and Activity (what waits for the user).
 */
enum class ChatTab { Home, Direct, Activity }

/**
 * The Activity tab: every conversation with a mention, or unread and not muted (the Unread filter
 * across DMs and channels), the most recent first.
 */
fun activityConversations(conversations: List<ConversationItem>, lastActivity: Map<String, Long> = emptyMap()): List<ConversationItem> =
  conversations
    .filter { HomeFilter.Unread.matches(it) }
    .sortedWith(
      compareByDescending<ConversationItem> { it.mentionCount }
        .thenByDescending { lastActivity[it.id] ?: Long.MIN_VALUE }
        .thenBy(String.CASE_INSENSITIVE_ORDER) { it.name },
    )

/** The filter chips of Home (as in Element): one at a time, `All` is none selected. */
enum class HomeFilter {
  All,
  Unread,
  People,
  Groups,

  /** Channels of spaces; a chip of the list layout only. */
  Channels,
  ;

  fun matches(item: ConversationItem): Boolean = when (this) {
    All -> true
    Unread -> item.mentionCount > 0u || (item.unreadCount > 0u && !item.isMuted)
    People -> item.kind == ConversationKind.DIRECT
    Groups -> item.kind == ConversationKind.GROUP
    Channels -> !item.isDirect
  }
}

/**
 * The name lists show: the notes have none of their own (they are named after the other people,
 * and there are none), so they are called [notesTitle] in the user's language.
 */
fun ConversationItem.listName(notesTitle: String): String = conversationListName(name, isNotesToSelf, notesTitle)

/** The same rule for a plain conversation name, such as the one of a search hit (#1041). */
fun conversationListName(name: String, isNotesToSelf: Boolean, notesTitle: String): String =
  if (isNotesToSelf) notesTitle else name

/** What a search hit row shows above the message: the space and the conversation, the notes by their title. */
fun hitLocation(maybeSpaceName: String?, name: String, isNotesToSelf: Boolean, notesTitle: String): String =
  listOfNotNull(maybeSpaceName, conversationListName(name, isNotesToSelf, notesTitle)).joinToString(" › ")

/** DMs and group DMs live outside spaces and have their own tab. */
val ConversationItem.isDirect: Boolean
  get() = kind == ConversationKind.DIRECT || kind == ConversationKind.GROUP

/** Channels of the spaces: favorite channels first, then each space (spaces and channels by name). */
fun channelSections(conversations: List<ConversationItem>, listsArchived: Boolean = false): List<Section> {
  val byName = compareBy<ConversationItem, String>(String.CASE_INSENSITIVE_ORDER) { it.name }
  // Archived channels leave the lists; Browse finds them (issue #212).
  val channels = conversations.filterNot { it.isDirect || it.isArchived }
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
    // Guests have no channel browser to find them in.
    val archived = conversations.filter { !it.isDirect && it.isArchived }
    if (listsArchived && archived.isNotEmpty()) add(Section(SectionTitle.Archived, archived.sortedWith(byName)))
  }
}

/**
 * Home of the spaces layout (#1488): the DMs with something new (not favorites, the most recent
 * first), then the favorites (DMs and channels, by name), then each space with its channels, as
 * [channelSections] has them. Read DMs are on the DMs tab. Empty sections are left out.
 */
fun homeSections(
  conversations: List<ConversationItem>,
  lastActivity: Map<String, Long> = emptyMap(),
  listsArchived: Boolean = false,
): List<Section> {
  val byName = compareBy<ConversationItem, String>(String.CASE_INSENSITIVE_ORDER) { it.name }
  val byRecency = compareByDescending<ConversationItem> { lastActivity[it.id] ?: Long.MIN_VALUE }.then(byName)
  val unique = conversations.distinctBy { it.id }
  val unreadDirect = unique.filter { it.isDirect && !it.isFavorite && HomeFilter.Unread.matches(it) }.sortedWith(byRecency)
  val favorites = unique.filter { it.isFavorite && !it.isArchived }.sortedWith(byName)
  return buildList {
    if (unreadDirect.isNotEmpty()) add(Section(SectionTitle.UnreadDirect, unreadDirect))
    if (favorites.isNotEmpty()) add(Section(SectionTitle.Favorites, favorites))
    addAll(channelSections(unique, listsArchived).filterNot { it.title == SectionTitle.Favorites })
  }
}

/**
 * Home of the spaces layout with a space selected (#1526), as desktop's sidebar shows one space at
 * a time: the DMs with something new, the favorites (of every space, as on desktop), then the
 * selected space's channels under [SectionTitle.SpaceChannels], also when it has none yet (the
 * header has the space's name, Home offers browsing). Channels outside any space stay listed.
 */
fun spaceHomeSections(
  conversations: List<ConversationItem>,
  spaceId: String,
  lastActivity: Map<String, Long> = emptyMap(),
  listsArchived: Boolean = false,
): List<Section> {
  val shown = conversations.filter { it.isDirect || it.isFavorite || it.maybeSpaceId == null || it.maybeSpaceId == spaceId }
  val sections = homeSections(shown, lastActivity, listsArchived)
  val channels = sections.filter { it.title is SectionTitle.Space }.flatMap { it.conversations }
  val head = sections.filter { it.title == SectionTitle.UnreadDirect || it.title == SectionTitle.Favorites }
  val tail = sections.filter { it.title == SectionTitle.Channels || it.title == SectionTitle.Archived }
  return head + Section(SectionTitle.SpaceChannels(spaceId), channels) + tail
}

/** A space the user belongs to, as the space switcher and the lists name it (#1526). */
data class HomeSpace(
  val id: String,
  val name: String,
  val maybeImageVersion: String? = null,
  /** Mentions in its channels. */
  val mentionCount: Int = 0,
  /** Something new in a channel that isn't muted. */
  val hasUnread: Boolean = false,
)

/**
 * Every space of the snapshot by name ([spaces]: id, name, picture version), with what is new in
 * its channels counted from the live `conversations`, so the badges follow each change without a
 * new snapshot read (#1466); so are its name and picture, where it has a channel. Spaces without
 * joined channels are listed too.
 */
fun homeSpaces(spaces: List<HomeSpace>, conversations: List<ConversationItem>): List<HomeSpace> {
  val bySpace = conversations
    .filter { !it.isDirect && !it.isArchived && it.maybeSpaceId != null }
    .distinctBy { it.id }
    .groupBy { it.maybeSpaceId }
  return spaces
    .distinctBy { it.id }
    .map { space ->
      val channels = bySpace[space.id].orEmpty()
      // A rename or a new picture reaches the channels before the next snapshot read.
      val maybeLive = channels.firstOrNull()
      space.copy(
        name = maybeLive?.maybeSpaceName ?: space.name,
        maybeImageVersion = maybeLive?.maybeSpaceImageVersion ?: space.maybeImageVersion,
        mentionCount = channels.sumOf { it.mentionCount.toInt() },
        hasUnread = channels.any { it.unreadCount > 0u && !it.isMuted },
      )
    }
    .sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name })
}

/**
 * The space Home of the spaces layout shows: the remembered one while the user still belongs to
 * it, otherwise the first by name; `null` without spaces.
 */
fun selectedSpace(spaces: List<HomeSpace>, maybeRemembered: String?): HomeSpace? =
  spaces.firstOrNull { it.id == maybeRemembered } ?: spaces.firstOrNull()

/** Whether a space other than [selectedId] has something new: the switcher shows a dot then. */
fun hasNewElsewhere(spaces: List<HomeSpace>, selectedId: String?): Boolean =
  spaces.any { it.id != selectedId && (it.mentionCount > 0 || it.hasUnread) }

/**
 * The list layout grouped by space (#1526): [sections] as [listSections] made them, plus a section
 * for each space in [spaces] that has none (no joined channels, or all of them favorites), so every
 * space can be found, its menu opened and its channels browsed. Spaces stay ordered by name.
 */
fun withEmptySpaces(sections: List<Section>, spaces: List<HomeSpace>): List<Section> {
  val listed = sections.mapNotNull { section ->
    (section.title as? SectionTitle.Space)?.let { title -> title.maybeId ?: section.conversations.firstNotNullOfOrNull { it.maybeSpaceId } }
  }.toSet()
  val missing = spaces.filter { it.id !in listed }.map { Section(SectionTitle.Space(it.name, it.id, it.maybeImageVersion), emptyList()) }
  if (missing.isEmpty()) return sections
  val head = sections.filter { it.title !is SectionTitle.Space && it.title != SectionTitle.Channels }
  val tail = sections.filter { it.title == SectionTitle.Channels }
  val merged = (sections.filter { it.title is SectionTitle.Space } + missing)
    .sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { (it.title as SectionTitle.Space).name })
  return head + merged + tail
}

/**
 * The key a section's collapsed state is remembered under (#1488): a space by its id, so renaming
 * it keeps the choice; `null` for the sections that can't be collapsed.
 */
fun Section.collapseKey(): String? = when (val title = title) {
  SectionTitle.Favorites -> "favorites"
  SectionTitle.UnreadDirect -> "unread-direct"
  SectionTitle.Channels -> "channels"
  SectionTitle.Archived -> "archived"
  SectionTitle.DirectMessages -> "direct"
  is SectionTitle.Space ->
    (title.maybeId ?: conversations.firstNotNullOfOrNull { it.maybeSpaceId })?.let { "space:$it" } ?: "space-name:${title.name}"
  is SectionTitle.SpaceChannels -> "space:${title.spaceId}"
  else -> null
}

/** The space a section is of, from its title or its channels; `null` for other sections. */
fun Section.spaceId(): String? = when (val title = title) {
  is SectionTitle.Space -> title.maybeId ?: conversations.firstNotNullOfOrNull { it.maybeSpaceId }
  is SectionTitle.SpaceChannels -> title.spaceId
  else -> null
}

/**
 * The rows a section shows: all of them while it is open; while collapsed, only those with
 * something new and the open conversation, as the desktop sidebar does, so nothing new hides.
 */
fun Section.visibleRows(isCollapsed: Boolean, maybeSelectedId: String?): List<ConversationItem> =
  if (!isCollapsed) conversations else conversations.filter { HomeFilter.Unread.matches(it) || it.id == maybeSelectedId }

/**
 * The DMs tab: favorites, then the rest, each the most recent first (`lastActivity`: newest
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
    .distinctBy { it.id }
    .filter { it.isDirect && chip.matches(it) && (needle.isEmpty() || it.name.contains(needle, ignoreCase = true)) }
    .sortedWith(byRecency)
  val (favorites, rest) = direct.partition { it.isFavorite }
  return buildList {
    if (favorites.isNotEmpty()) add(Section(SectionTitle.Favorites, favorites))
    if (rest.isNotEmpty()) add(Section(SectionTitle.DirectMessages, rest))
  }
}

/**
 * The list layout's one list (#1151): favorites, then every other DM and channel, each the most
 * recent first. Only conversations `chip` lets through whose names contain `filter` (any case) are
 * kept. Empty sections are left out.
 *
 * [bySpace] (#1345): after the favorites, the DMs, then the other channels of each space under its
 * name (spaces by name, as in the Spaces tab), each still the most recent first. Favorites stay on
 * top, across spaces; a space whose channels are all favorites has no section.
 */
fun listSections(
  conversations: List<ConversationItem>,
  lastActivity: Map<String, Long> = emptyMap(),
  filter: String = "",
  chip: HomeFilter = HomeFilter.All,
  bySpace: Boolean = false,
): List<Section> {
  val needle = filter.trim()
  val byName = compareBy<ConversationItem, String>(String.CASE_INSENSITIVE_ORDER) { it.name }
  val byRecency = compareByDescending<ConversationItem> { lastActivity[it.id] ?: Long.MIN_VALUE }.then(byName)
  val (favorites, rest) = conversations
    .distinctBy { it.id }
    .filter { chip.matches(it) && (needle.isEmpty() || it.name.contains(needle, ignoreCase = true)) }
    .sortedWith(byRecency)
    .partition { it.isFavorite }
  return buildList {
    if (favorites.isNotEmpty()) add(Section(SectionTitle.Favorites, favorites))
    if (!bySpace) {
      if (rest.isNotEmpty()) add(Section(SectionTitle.Everything, rest))
      return@buildList
    }
    val (direct, channels) = rest.partition { it.isDirect }
    if (direct.isNotEmpty()) add(Section(SectionTitle.DirectMessages, direct))
    // groupBy keeps the recency order inside each space.
    channels
      .groupBy { it.maybeSpaceName }
      .toSortedMap(nullsLast(String.CASE_INSENSITIVE_ORDER))
      .forEach { (maybeSpace, items) -> add(Section(maybeSpace?.let { SectionTitle.Space(it) } ?: SectionTitle.Channels, items)) }
  }
}

/**
 * The DMs and group DMs a space shows (#701): those of `conversations` named in `ids` (the space's
 * [de.zeitgleich.teamcraft.core.Space.directConversations]), the most recent first (`lastActivity`:
 * newest message per conversation; by name without one). Each is still the one conversation Home
 * lists, so no total adds it twice.
 */
fun spaceDirectMessages(
  conversations: List<ConversationItem>,
  ids: List<String>,
  lastActivity: Map<String, Long> = emptyMap(),
): List<ConversationItem> {
  val wanted = ids.toSet()
  val byName = compareBy<ConversationItem, String>(String.CASE_INSENSITIVE_ORDER) { it.name }
  return conversations
    .distinctBy { it.id }
    .filter { it.isDirect && it.id in wanted }
    .sortedWith(compareByDescending<ConversationItem> { lastActivity[it.id] ?: Long.MIN_VALUE }.then(byName))
}

/** What a tab shows next to its name: mentions, unread messages, or nothing. */
data class TabBadge(val mentions: Int, val unread: Int)

/**
 * The badge of a tab: mentions everywhere; unread messages count in DMs (they are addressed to the
 * user), in channels only as "something new". Muted conversations count only mentions.
 */
fun tabBadge(conversations: List<ConversationItem>): TabBadge {
  // An archived channel has no list row to clear it from.
  val listed = conversations.filterNot { it.isArchived }
  val audible = listed.filterNot { it.isMuted }
  return TabBadge(
    mentions = listed.sumOf { it.mentionCount.toInt() },
    unread = audible.sumOf { it.unreadCount.toInt() },
  )
}
