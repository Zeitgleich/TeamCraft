package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import de.zeitgleich.teamcraft.core.User
import uniffi.teamcraft_ffi.MentionableGroupItem
import uniffi.teamcraft_ffi.MentionableGroupKindItem

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

  @Test
  fun groupsAreSuggestedByHandleOrNameAndCompleteToTheirHandle() {
    val groups = listOf(
      MentionableGroupItem("accounting", "Accounting Team", MentionableGroupKindItem.CUSTOM, 3u, 0u),
      MentionableGroupItem("admins", "Admins", MentionableGroupKindItem.ADMINS, 2u, 1u),
    )
    assertEquals(listOf("accounting"), Mentions.groupSuggestions("acc", groups, "Administratoren").map { it.handle })
    assertEquals(listOf("accounting"), Mentions.groupSuggestions("team", groups, "Administratoren").map { it.handle })
    // The built-in group is found by the app's own name for it.
    assertEquals(listOf("admins"), Mentions.groupSuggestions("administ", groups, "Administratoren").map { it.handle })
    assertEquals("Administratoren", Mentions.shownName(groups[1], "Administratoren"))
    assertEquals("ping @accounting ", Mentions.complete("ping @acc", "accounting"))
  }
}
