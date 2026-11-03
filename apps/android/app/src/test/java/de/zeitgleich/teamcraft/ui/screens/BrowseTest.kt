package de.zeitgleich.teamcraft.ui.screens

import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Conversation
import de.zeitgleich.teamcraft.core.User
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

private const val ME = "1"
private const val ALICE = "2"
private const val BOB = "3"

class BrowseTest {
  @Test
  fun findsTheDirectConversationWithAPerson() {
    val snapshot = snapshot(
      Conversation(id = "10", kind = "Group", name = "Trio", memberIds = listOf(ME, ALICE, BOB)),
      Conversation(id = "11", kind = "Direct", name = "Alice", memberIds = listOf(ME, ALICE)),
    )
    assertEquals("11", directWith(snapshot, listOf(ALICE)))
  }

  @Test
  fun aGroupWithThePersonIsNoDirectConversation() {
    val snapshot = snapshot(Conversation(id = "10", kind = "Group", name = "Trio", memberIds = listOf(ME, ALICE, BOB)))
    assertNull(directWith(snapshot, listOf(BOB)))
  }

  @Test
  fun findsTheGroupWithExactlyThesePeople() {
    val snapshot = snapshot(
      Conversation(id = "10", kind = "Group", name = "Trio", memberIds = listOf(ME, ALICE, BOB)),
      Conversation(id = "11", kind = "Direct", name = "Alice", memberIds = listOf(ME, ALICE)),
    )
    assertEquals("10", directWith(snapshot, listOf(BOB, ALICE, ME)))
    assertNull(directWith(snapshot, listOf(BOB, ALICE, "4")))
  }

  private fun snapshot(vararg conversations: Conversation) = ChatSnapshot(
    me = User(id = ME, username = "me", displayName = "Me"),
    conversations = conversations.toList(),
  )
}
