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

  private fun item(
    id: String,
    name: String,
    kind: ConversationKind,
    space: String? = null,
    isFavorite: Boolean = false,
    unread: UInt = 0u,
    mentions: UInt = 0u,
    isMuted: Boolean = false,
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
  )
}
