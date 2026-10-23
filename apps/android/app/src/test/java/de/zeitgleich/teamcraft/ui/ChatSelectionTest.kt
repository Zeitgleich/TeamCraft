package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

class ChatSelectionTest {
  @Test
  fun anyUnreadMarksAllRead() {
    val actions = selectionActions(listOf(item("1", unread = 2u), item("2")))
    assertTrue(actions.isMarkingRead)
    assertFalse(selectionActions(listOf(item("1"), item("2"))).isMarkingRead)
  }

  @Test
  fun favoritesAreAddedUnlessAllAreFavorites() {
    assertTrue(selectionActions(listOf(item("1", favorite = true), item("2"))).isAddingFavorite)
    assertFalse(selectionActions(listOf(item("1", favorite = true), item("2", favorite = true))).isAddingFavorite)
  }

  @Test
  fun notificationsOnlyForOne() {
    assertEquals("1", selectionActions(listOf(item("1"))).maybeSingle)
    assertNull(selectionActions(listOf(item("1"), item("2"))).maybeSingle)
  }

  @Test
  fun tapsToggleMembership() {
    assertEquals(listOf("1", "2"), toggled(listOf("1"), "2"))
    assertEquals(listOf("2"), toggled(listOf("1", "2"), "1"))
  }

  private fun item(id: String, unread: UInt = 0u, favorite: Boolean = false) = ConversationItem(
    id = id,
    maybeSpaceId = null,
    maybeSpaceName = null,
    kind = ConversationKind.PUBLIC,
    name = "c$id",
    topic = "",
    unreadCount = unread,
    mentionCount = 0u,
    isFavorite = favorite,
    isMuted = false,
    canPost = true,
  )
}
