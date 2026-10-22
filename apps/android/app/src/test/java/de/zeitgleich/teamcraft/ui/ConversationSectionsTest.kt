package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

class ConversationSectionsTest {
  @Test
  fun favoritesSpacesThenDirectMessages() {
    val result = sections(
      listOf(
        item("1", "zeta", ConversationKind.PUBLIC, space = "Project B"),
        item("2", "alpha", ConversationKind.PRIVATE, space = "Project A"),
        item("3", "Bob", ConversationKind.DIRECT),
        item("4", "release", ConversationKind.PUBLIC, space = "Project A", isFavorite = true),
        item("5", "Beta", ConversationKind.PUBLIC, space = "Project A"),
      ),
    )
    assertEquals(
      listOf(
        SectionTitle.Favorites,
        SectionTitle.Space("Project A"),
        SectionTitle.Space("Project B"),
        SectionTitle.DirectMessages,
      ),
      result.map { it.title },
    )
    assertEquals(listOf("alpha", "Beta"), result[1].conversations.map { it.name })
  }

  @Test
  fun emptySectionsAreLeftOut() {
    assertEquals(emptyList<Section>(), sections(emptyList()))
    assertEquals(
      listOf(SectionTitle.DirectMessages),
      sections(listOf(item("1", "Carol", ConversationKind.GROUP))).map { it.title },
    )
  }

  private fun item(
    id: String,
    name: String,
    kind: ConversationKind,
    space: String? = null,
    isFavorite: Boolean = false,
  ) = ConversationItem(
    id = id,
    maybeSpaceId = space?.let { "s-$it" },
    maybeSpaceName = space,
    kind = kind,
    name = name,
    topic = "",
    unreadCount = 0u,
    mentionCount = 0u,
    isFavorite = isFavorite,
    isMuted = false,
    canPost = true,
  )
}
