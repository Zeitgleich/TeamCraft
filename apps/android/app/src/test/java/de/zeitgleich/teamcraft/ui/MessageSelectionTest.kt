package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MessageSelectionTest {
  @Test
  fun togglingTwiceLeavesTheSelectionAsItWas() {
    val selection = setOf("1")
    assertEquals(setOf("1", "2"), selection.toggled("2"))
    assertEquals(selection, selection.toggled("2").toggled("2"))
    assertTrue(selection.toggled("1").isEmpty())
  }

  @Test
  fun selectedMessagesComeOldestFirstWithoutPlaceholders() {
    val feed = listOf(message("3", 300), message("1", 100), message("2", 200, placeholder = "waiting"))
    assertEquals(listOf("1", "3"), selectedInOrder(feed, setOf("3", "1", "2", "9")).map { it.id })
  }

  @Test
  fun messagesGoneFromTheFeedDropOut() {
    val feed = listOf(message("1", 100), message("2", 200))
    assertEquals(setOf("1"), setOf("1", "5").surviving(feed, emptyMap()))
  }

  @Test
  fun deleteNeedsEveryMessageToBeTheUsersOwn() {
    val own = message("1", 100, author = "7")
    val other = message("2", 200, author = "8")
    assertTrue(canDeleteAll(listOf(own), "7", canPost = true))
    assertFalse(canDeleteAll(listOf(own, other), "7", canPost = true))
    assertFalse(canDeleteAll(listOf(own), "7", canPost = false))
    assertFalse(canDeleteAll(emptyList(), "7", canPost = true))
    assertFalse(canDeleteAll(listOf(own), null, canPost = true))
  }

  @Test
  fun copiedTextNamesSenderAndTimePerMessage() {
    val text = copiedText(
      listOf(message("1", 100, author = "7"), message("2", 200, author = "8")),
      nameOf = { if (it == "7") "Ada" else "Bo" },
      timeOf = { "t$it" },
    )
    assertEquals("Ada, t100\nbody 1\n\nBo, t200\nbody 2", text)
  }

  private fun message(id: String, sentAt: Long, author: String = "7", placeholder: String? = null) =
    Message(id = id, conversation = "c", author = author, sentAt = sentAt, body = "body $id", maybePlaceholder = placeholder)
}
