package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import de.zeitgleich.teamcraft.core.User

class MentionsTest {
  private val people = listOf(
    User(id = "1", username = "alice", displayName = "Alice Example"),
    User(id = "2", username = "bob", displayName = "Bob Builder"),
    User(id = "3", username = "builder-bot", displayName = "Builder", isBot = true),
  )

  @Test
  fun findsTheTypedMentionAndCompletesIt() {
    assertEquals("bo", Mentions.activeQuery("hello @bo"))
    assertNull(Mentions.activeQuery("hello bob"))
    assertNull(Mentions.activeQuery("mail@example.com"))
    assertEquals(listOf("bob"), Mentions.suggestions("bu", people).map { it.username })
    assertEquals("hello @bob ", Mentions.complete("hello @bo", people[1]))
  }

  @Test
  fun deactivatedAndDeletedMembersAreNeverSuggested() {
    val members = people + listOf(
      User(id = "4", username = "bea", displayName = "Bea Former", isDeactivated = true),
      User(id = "5", username = "", displayName = "Deleted user", isDeactivated = true, isDeleted = true),
    )
    assertEquals(listOf("bob"), Mentions.suggestions("b", members).map { it.username })
    assertEquals(emptyList<User>(), Mentions.suggestions("deleted", members))
  }
}
