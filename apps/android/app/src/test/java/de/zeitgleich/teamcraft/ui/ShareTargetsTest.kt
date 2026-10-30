package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

class ShareTargetsTest {
  @Test
  fun notesFirstThenRecentThenDirectThenChannelsBySpace() {
    val result = shareSections(
      listOf(
        item("1", "zeta", ConversationKind.PUBLIC, space = "Project B"),
        item("2", "Amy", ConversationKind.DIRECT, lastActivity = 20),
        item("3", "", ConversationKind.DIRECT, isNotes = true),
        item("4", "alpha", ConversationKind.PRIVATE, space = "Project A"),
        item("5", "bob", ConversationKind.DIRECT),
        item("6", "release", ConversationKind.PUBLIC, space = "Project A", lastActivity = 30),
      ),
      notesTitle = NOTES,
    )
    assertEquals(
      listOf(
        SectionTitle.Notes,
        SectionTitle.Recent,
        SectionTitle.DirectMessages,
        SectionTitle.Space("Project A"),
        SectionTitle.Space("Project B"),
      ),
      result.map { it.title },
    )
    assertEquals(listOf("6", "2"), result[1].conversations.map { it.id })
    assertEquals(listOf("5"), result[2].conversations.map { it.id })
    assertEquals(listOf("4"), result[3].conversations.map { it.id })
  }

  @Test
  fun recentHoldsAtMostTheNewestFew() {
    val targets = (1..SHARE_RECENT_COUNT + 2).map { item("$it", "c$it", ConversationKind.PUBLIC, lastActivity = it.toLong()) }
    val recent = shareSections(targets, NOTES).first { it.title == SectionTitle.Recent }
    assertEquals(SHARE_RECENT_COUNT, recent.conversations.size)
    assertEquals("${SHARE_RECENT_COUNT + 2}", recent.conversations.first().id)
  }

  @Test
  fun filteringKeepsTheNotesByTheirLocalizedName() {
    val targets = listOf(
      item("3", "", ConversationKind.DIRECT, isNotes = true),
      item("2", "Amy", ConversationKind.DIRECT, lastActivity = 20),
    )
    val result = shareSections(targets, NOTES, filter = " speicher ")
    assertEquals(listOf(SectionTitle.Notes), result.map { it.title })
    assertEquals(listOf("3"), result.single().conversations.map { it.id })
  }

  @Test
  fun filteringShowsEachMatchOnceWithoutRecent() {
    val targets = listOf(
      item("2", "Amy", ConversationKind.DIRECT, lastActivity = 20),
      item("5", "amanda", ConversationKind.DIRECT),
      item("6", "release", ConversationKind.PUBLIC, lastActivity = 30),
    )
    val result = shareSections(targets, NOTES, filter = "AM")
    assertEquals(listOf(SectionTitle.DirectMessages), result.map { it.title })
    assertEquals(listOf("5", "2"), result.single().conversations.map { it.id })
  }

  @Test
  fun noMatchLeavesNoSections() {
    assertEquals(emptyList<Section>(), shareSections(listOf(item("1", "general", ConversationKind.PUBLIC)), NOTES, filter = "xyz"))
  }

  private fun item(
    id: String,
    name: String,
    kind: ConversationKind,
    space: String? = null,
    lastActivity: Long? = null,
    isNotes: Boolean = false,
  ) = ConversationItem(
    id = id,
    maybeSpaceId = space?.let { "s-$it" },
    maybeSpaceName = space,
    kind = kind,
    name = name,
    topic = "",
    unreadCount = 0u,
    mentionCount = 0u,
    isFavorite = false,
    isMuted = false,
    canPost = true,
    isNotesToSelf = isNotes,
    maybeLastActivityMs = lastActivity,
  )

  private companion object {
    const val NOTES = "Mein Speicher"
  }
}
