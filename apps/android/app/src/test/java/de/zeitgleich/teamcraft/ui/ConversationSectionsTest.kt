package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

class ConversationSectionsTest {
  @Test
  fun channelsByFavoritesThenSpaceWithoutDirectMessages() {
    val result = channelSections(
      listOf(
        item("1", "zeta", ConversationKind.PUBLIC, space = "Project B"),
        item("2", "alpha", ConversationKind.PRIVATE, space = "Project A"),
        item("3", "Bob", ConversationKind.DIRECT),
        item("4", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true),
        item("5", "Beta", ConversationKind.PUBLIC, space = "Project A"),
      ),
    )
    assertEquals(
      listOf(SectionTitle.Favorites, SectionTitle.Space("Project A"), SectionTitle.Space("Project B")),
      result.map { it.title },
    )
    assertEquals(listOf("alpha", "Beta"), result[1].conversations.map { it.name })
  }

  @Test
  fun aSearchHitInTheNotesIsNamedByTheNotesTitle() {
    assertEquals("Notes", conversationListName("", isNotesToSelf = true, notesTitle = "Notes"))
    assertEquals("general", conversationListName("general", isNotesToSelf = false, notesTitle = "Notes"))
    assertEquals("Notes", hitLocation(null, "", true, "Notes"))
    assertEquals("Project A › general", hitLocation("Project A", "general", false, "Notes"))
  }

  @Test
  fun archivedChannelsLeaveTheLists() {
    val archived = item("1", "old", ConversationKind.PUBLIC, space = "Project A", isFavorite = true).copy(isArchived = true)
    assertEquals(emptyList<Section>(), channelSections(listOf(archived)))
    assertEquals(listOf(SectionTitle.Archived), channelSections(listOf(archived), listsArchived = true).map { it.title })
    assertEquals(TabBadge(mentions = 0, unread = 0), tabBadge(listOf(archived.copy(mentionCount = 2u, unreadCount = 3u))))
  }

  @Test
  fun activityHoldsMentionsFirstThenUnreadNewestFirstWithoutMutedOrRead() {
    val result = activityConversations(
      listOf(
        item("1", "read", ConversationKind.PUBLIC, space = "Project A"),
        item("2", "Amy", ConversationKind.DIRECT, unread = 2u),
        item("3", "muted", ConversationKind.PUBLIC, unread = 5u, isMuted = true),
        item("4", "release", ConversationKind.PUBLIC, unread = 1u, mentions = 1u),
        item("5", "Bob", ConversationKind.DIRECT, unread = 1u),
        item("6", "quiet", ConversationKind.PRIVATE, unread = 3u, mentions = 2u, isMuted = true),
      ),
      lastActivity = mapOf("2" to 10L, "5" to 20L),
    )
    assertEquals(listOf("quiet", "release", "Bob", "Amy"), result.map { it.name })
  }

  @Test
  fun directMessagesNewestFirstWithFavoritesOnTop() {
    val result = directSections(
      listOf(
        item("1", "Amy", ConversationKind.DIRECT),
        item("2", "zed", ConversationKind.DIRECT),
        item("3", "Amy, Bob", ConversationKind.GROUP, isFavorite = true),
        item("4", "general", ConversationKind.PUBLIC, space = "Project A"),
        item("5", "bob", ConversationKind.DIRECT),
      ),
      lastActivity = mapOf("1" to 10L, "2" to 30L, "4" to 99L),
    )
    assertEquals(listOf(SectionTitle.Favorites, SectionTitle.DirectMessages), result.map { it.title })
    assertEquals(listOf("zed", "Amy", "bob"), result[1].conversations.map { it.name })
  }

  @Test
  fun homeHasUnreadDirectMessagesThenFavoritesThenSpaces() {
    val all = listOf(
      item("1", "Amy", ConversationKind.DIRECT, unread = 2u),
      item("2", "Bob", ConversationKind.DIRECT),
      item("3", "muted", ConversationKind.GROUP, unread = 4u, isMuted = true),
      item("4", "Cid", ConversationKind.DIRECT, unread = 1u),
      item("5", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true),
      item("6", "Dan", ConversationKind.DIRECT, unread = 1u, isFavorite = true),
      item("7", "general", ConversationKind.PUBLIC, space = "Project A"),
      item("8", "design", ConversationKind.PUBLIC, space = "Project B"),
    )
    val home = homeSections(all + all.take(1), lastActivity = mapOf("1" to 10L, "4" to 20L))
    assertEquals(
      listOf(SectionTitle.UnreadDirect, SectionTitle.Favorites, SectionTitle.Space("Project A"), SectionTitle.Space("Project B")),
      home.map { it.title },
    )
    assertEquals(listOf("Cid", "Amy"), home[0].conversations.map { it.name })
    assertEquals(listOf("Dan", "release"), home[1].conversations.map { it.name })
    assertEquals(listOf("general"), home[2].conversations.map { it.name })
  }

  @Test
  fun aCollapsedSectionStillShowsWhatIsNewAndTheOpenConversation() {
    val section = Section(
      SectionTitle.Space("Project A"),
      listOf(
        item("1", "general", ConversationKind.PUBLIC, space = "Project A"),
        item("2", "release", ConversationKind.PUBLIC, space = "Project A", unread = 1u),
        item("3", "muted", ConversationKind.PUBLIC, space = "Project A", unread = 3u, isMuted = true),
        item("4", "open", ConversationKind.PUBLIC, space = "Project A"),
      ),
    )
    assertEquals(4, section.visibleRows(isCollapsed = false, maybeSelectedId = null).size)
    assertEquals(listOf("release", "open"), section.visibleRows(isCollapsed = true, maybeSelectedId = "4").map { it.name })
    assertEquals("space:s-Project A", section.collapseKey())
    assertEquals("unread-direct", Section(SectionTitle.UnreadDirect, emptyList()).collapseKey())
  }

  @Test
  fun theDirectMessagesTabListsNoChannelsNotEvenStarredOnes() {
    val starred = item("2", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true)
    assertEquals(emptyList<Section>(), directSections(listOf(starred, item("3", "general", ConversationKind.PUBLIC, space = "Project A"))))
  }

  @Test
  fun theHomeTabCountsOnlyDirectMessagesSoAStarredChannelIsNotCountedTwice() {
    val all = listOf(
      item("1", "Amy", ConversationKind.DIRECT, unread = 1u),
      item("2", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true, unread = 4u),
    )
    val (direct, channels) = all.partition { it.isDirect }
    assertEquals(TabBadge(mentions = 0, unread = 1), tabBadge(direct))
    assertEquals(TabBadge(mentions = 0, unread = 4), tabBadge(channels))
  }

  @Test
  fun directMessagesFilterIgnoresCase() {
    val list = listOf(item("1", "Amy", ConversationKind.DIRECT), item("2", "Bob", ConversationKind.DIRECT))
    assertEquals(listOf("Amy"), directSections(list, filter = " aM").flatMap { it.conversations }.map { it.name })
    assertEquals(emptyList<Section>(), directSections(list, filter = "nobody"))
  }

  @Test
  fun homeChipsPickUnreadPeopleOrGroups() {
    val list = listOf(
      item("1", "Amy", ConversationKind.DIRECT, unread = 1u),
      item("2", "Bob", ConversationKind.DIRECT, unread = 4u, isMuted = true),
      item("3", "Amy, Bob", ConversationKind.GROUP),
    )
    fun names(chip: HomeFilter) = directSections(list, chip = chip).flatMap { it.conversations }.map { it.name }
    assertEquals(listOf("Amy"), names(HomeFilter.Unread))
    assertEquals(listOf("Amy", "Bob"), names(HomeFilter.People))
    assertEquals(listOf("Amy, Bob"), names(HomeFilter.Groups))
  }

  @Test
  fun emptySectionsAreLeftOut() {
    assertEquals(emptyList<Section>(), channelSections(emptyList()))
    assertEquals(emptyList<Section>(), channelSections(listOf(item("1", "Carol", ConversationKind.GROUP))))
  }

  @Test
  fun badgesSkipMutedUnreadButCountMentions() {
    val badge = tabBadge(
      listOf(
        item("1", "Amy", ConversationKind.DIRECT, unread = 2u),
        item("2", "Bob", ConversationKind.DIRECT, unread = 5u, mentions = 1u, isMuted = true),
      ),
    )
    assertEquals(TabBadge(mentions = 1, unread = 2), badge)
  }

  @Test
  fun theListHoldsDmsAndChannelsOfAllSpacesByRecencyWithFavoritesFirst() {
    val list = listOf(
      item("1", "Amy", ConversationKind.DIRECT),
      item("2", "general", ConversationKind.PUBLIC, space = "Project A"),
      item("3", "general", ConversationKind.PUBLIC, space = "Project B"),
      item("4", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true),
    )
    val result = listSections(list + list[0], lastActivity = mapOf("1" to 20L, "2" to 30L, "3" to 10L))
    assertEquals(listOf(SectionTitle.Favorites, SectionTitle.Everything), result.map { it.title })
    assertEquals(listOf("4"), result[0].conversations.map { it.id })
    assertEquals(listOf("2", "1", "3"), result[1].conversations.map { it.id })
  }

  @Test
  fun theListNarrowsByChipAndName() {
    val list = listOf(
      item("1", "Amy", ConversationKind.DIRECT),
      item("2", "Launch", ConversationKind.PUBLIC, space = "Project A"),
    )
    assertEquals(listOf("2"), listSections(list, chip = HomeFilter.Channels).flatMap { it.conversations }.map { it.id })
    assertEquals(listOf("1"), listSections(list, chip = HomeFilter.People).flatMap { it.conversations }.map { it.id })
    assertEquals(listOf("2"), listSections(list, filter = " laun").flatMap { it.conversations }.map { it.id })
  }

  @Test
  fun groupedBySpaceTheListHoldsFavoritesThenDmsThenEachSpaceByNameEachByRecency() {
    val list = listOf(
      item("1", "Amy", ConversationKind.DIRECT),
      item("2", "general", ConversationKind.PUBLIC, space = "Project B"),
      item("3", "random", ConversationKind.PUBLIC, space = "Project A"),
      item("4", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true),
      item("5", "launch", ConversationKind.PUBLIC, space = "Project A"),
      item("6", "only", ConversationKind.PUBLIC, space = "Project C", isFavorite = true),
    )
    val result = listSections(list, lastActivity = mapOf("3" to 10L, "5" to 30L), bySpace = true)
    assertEquals(
      listOf(
        SectionTitle.Favorites,
        SectionTitle.DirectMessages,
        SectionTitle.Space("Project A"),
        SectionTitle.Space("Project B"),
      ),
      result.map { it.title },
    )
    assertEquals(listOf("5", "3"), result[2].conversations.map { it.id })
  }

  @Test
  fun groupedBySpaceTheChipsStillNarrowIt() {
    val list = listOf(
      item("1", "Amy", ConversationKind.DIRECT),
      item("2", "general", ConversationKind.PUBLIC, space = "Project A"),
    )
    assertEquals(
      listOf(SectionTitle.Space("Project A")),
      listSections(list, chip = HomeFilter.Channels, bySpace = true).map { it.title },
    )
    assertEquals(listOf(SectionTitle.DirectMessages), listSections(list, chip = HomeFilter.People, bySpace = true).map { it.title })
  }

  @Test
  fun aSpaceShowsItsDirectMessagesNewestFirst() {
    val conversations = listOf(
      item("1", "general", ConversationKind.PUBLIC, space = "Project A"),
      item("2", "Amy", ConversationKind.DIRECT),
      item("3", "Bob", ConversationKind.DIRECT, unread = 2u),
      item("4", "Amy, Bob", ConversationKind.GROUP),
      item("5", "Carl", ConversationKind.DIRECT),
    )
    val result = spaceDirectMessages(conversations, ids = listOf("1", "2", "3", "4"), lastActivity = mapOf("3" to 30L, "4" to 40L))
    // The channel is no DM; Carl is not with the space's people.
    assertEquals(listOf("Amy, Bob", "Bob", "Amy"), result.map { it.name })
    assertEquals(emptyList<ConversationItem>(), spaceDirectMessages(conversations, ids = emptyList()))
  }

  @Test
  fun spaceHomeShowsOnlyTheSelectedSpacesChannels() {
    val conversations = listOf(
      item("1", "general", ConversationKind.PUBLIC, space = "Project A"),
      item("2", "random", ConversationKind.PUBLIC, space = "Project B"),
      item("3", "release", ConversationKind.PUBLIC, space = "Project B", isFavorite = true),
      item("4", "Amy", ConversationKind.DIRECT, unread = 1u),
      item("5", "Bob", ConversationKind.DIRECT),
    )
    val result = spaceHomeSections(conversations, spaceId = "s-Project A")
    assertEquals(
      listOf(SectionTitle.UnreadDirect, SectionTitle.Favorites, SectionTitle.SpaceChannels("s-Project A")),
      result.map { it.title },
    )
    assertEquals(listOf("general"), result.last().conversations.map { it.name })
    assertEquals("space:s-Project A", result.last().collapseKey())
  }

  @Test
  fun aSpaceWithoutChannelsStillHasItsSection() {
    val result = spaceHomeSections(listOf(item("1", "general", ConversationKind.PUBLIC, space = "Project A")), spaceId = "s-empty")
    assertEquals(listOf(SectionTitle.SpaceChannels("s-empty")), result.map { it.title })
    assertEquals(emptyList<ConversationItem>(), result.single().conversations)
  }

  @Test
  fun spacesCountWhatIsNewInTheirChannelsAndListEmptySpaces() {
    val spaces = listOf(HomeSpace("s-b", "beta"), HomeSpace("s-Project A", "Old name"), HomeSpace("s-empty", "Empty"))
    val conversations = listOf(
      item("1", "general", ConversationKind.PUBLIC, space = "Project A", unread = 2u, mentions = 1u),
      item("2", "quiet", ConversationKind.PUBLIC, space = "Project A", unread = 4u, mentions = 2u, isMuted = true),
      item("3", "Amy", ConversationKind.DIRECT, unread = 1u),
    )
    val result = homeSpaces(spaces, conversations)
    // The name follows the channels (a rename before the next snapshot read).
    assertEquals(listOf("beta", "Empty", "Project A"), result.map { it.name })
    assertEquals(HomeSpace("s-Project A", "Project A", mentionCount = 3, hasUnread = true), result[2])
    assertEquals(HomeSpace("s-empty", "Empty"), result[1])
    assertEquals(true, hasNewElsewhere(result, "s-b"))
    assertEquals(false, hasNewElsewhere(result, "s-Project A"))
  }

  /** #1600: a DM, also one tied to a space, never marks a space; Chats counts it. */
  @Test
  fun dmsNeverMarkASpace() {
    val spaces = listOf(HomeSpace("s-Project A", "Project A"), HomeSpace("s-Project B", "Project B"))
    val conversations = listOf(
      item("1", "Amy", ConversationKind.DIRECT, space = "Project A", unread = 3u, mentions = 1u),
      item("2", "Amy, Bob", ConversationKind.GROUP, unread = 2u),
      item("3", "general", ConversationKind.PUBLIC, space = "Project B", unread = 1u, mentions = 1u),
    )
    val result = homeSpaces(spaces, conversations)
    assertEquals(HomeSpace("s-Project A", "Project A"), result[0])
    assertEquals(HomeSpace("s-Project B", "Project B", mentionCount = 1, hasUnread = true), result[1])
  }

  @Test
  fun theRememberedSpaceIsKeptWhileTheUserIsInIt() {
    val spaces = listOf(HomeSpace("a", "Alpha"), HomeSpace("b", "Beta"))
    assertEquals("b", selectedSpace(spaces, "b")?.id)
    assertEquals("a", selectedSpace(spaces, "gone")?.id)
    assertEquals("a", selectedSpace(spaces, null)?.id)
    assertEquals(null, selectedSpace(emptyList(), "b"))
  }

  @Test
  fun theListBySpaceListsSpacesWithoutChannelsInNameOrder() {
    val sections = listSections(
      listOf(
        item("1", "general", ConversationKind.PUBLIC, space = "Project B"),
        item("2", "Amy", ConversationKind.DIRECT),
      ),
      bySpace = true,
    )
    val spaces = listOf(HomeSpace("s-Project B", "Project B"), HomeSpace("s-a", "Project A", maybeImageVersion = "v1"))
    val result = withEmptySpaces(sections, spaces)
    assertEquals(
      listOf(SectionTitle.DirectMessages, SectionTitle.Space("Project A", "s-a", "v1"), SectionTitle.Space("Project B")),
      result.map { it.title },
    )
    assertEquals("space:s-a", result[1].collapseKey())
    assertEquals(sections, withEmptySpaces(sections, spaces.take(1)))
  }

  private fun item(
    id: String,
    name: String,
    kind: ConversationKind,
    space: String? = null,
    isFavorite: Boolean = false,
    unread: UInt = 0u,
    mentions: UInt = 0u,
    isMuted: Boolean = false,
    imageVersion: String? = null,
  ) = ConversationItem(
    id = id,
    maybeSpaceId = space?.let { "s-$it" },
    maybeSpaceName = space,
    kind = kind,
    name = name,
    topic = "",
    unreadCount = unread,
    mentionCount = mentions,
    isFavorite = isFavorite,
    isMuted = isMuted,
    canPost = true,
    maybeSpaceImageVersion = imageVersion,
  )
}
