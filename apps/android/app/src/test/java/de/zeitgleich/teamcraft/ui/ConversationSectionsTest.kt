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
  fun aStarredChannelListsUnderHomeFavoritesAndStaysUnderFavoritesInItsSpace() {
    val all = listOf(
      item("1", "Amy", ConversationKind.DIRECT, isFavorite = true),
      item("2", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true, unread = 3u),
      item("3", "general", ConversationKind.PUBLIC, space = "Project A"),
    )
    val home = directSections(all, lastActivity = mapOf("1" to 10L, "2" to 20L))
    assertEquals(listOf(SectionTitle.Favorites), home.map { it.title })
    assertEquals(listOf("release", "Amy"), home[0].conversations.map { it.name })
    assertEquals(listOf("release"), channelSections(all).first { it.title == SectionTitle.Favorites }.conversations.map { it.name })
  }

  @Test
  fun aChannelHandedOverTwiceIsOneRowAndAnUnstarredOneIsNone() {
    val starred = item("2", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true)
    val home = directSections(listOf(starred, starred, item("3", "general", ConversationKind.PUBLIC, space = "Project A")))
    assertEquals(listOf("release"), home.flatMap { it.conversations }.map { it.name })
  }

  @Test
  fun removingTheStarTakesTheChannelOutOfHome() {
    val channel = item("2", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = false)
    assertEquals(emptyList<Section>(), directSections(listOf(channel)))
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
  fun listedSpacesAreOneEachByName() {
    val spaces = listedSpaces(
      listOf(
        item("1", "a", ConversationKind.PUBLIC, space = "Project B"),
        item("2", "b", ConversationKind.PUBLIC, space = "Project A"),
        item("3", "c", ConversationKind.PUBLIC, space = "Project B"),
        item("4", "Amy", ConversationKind.DIRECT),
      ),
    )
    assertEquals(listOf("Project A", "Project B"), spaces.map { it.name })
  }

  @Test
  fun listedSpacesCarryThePictureVersion() {
    val spaces = listedSpaces(
      listOf(
        item("1", "a", ConversationKind.PUBLIC, space = "Project A", imageVersion = "v2"),
        item("2", "b", ConversationKind.PUBLIC, space = "Project B"),
      ),
    )
    assertEquals(listOf("v2", null), spaces.map { it.maybeImageVersion })
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
