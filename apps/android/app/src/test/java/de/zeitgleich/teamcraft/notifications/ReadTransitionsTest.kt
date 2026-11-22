package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ReadTransitionsTest {
  @Test
  fun aShownConversationThatWasUnreadAndIsReadNowGoes() {
    assertEquals(setOf("1"), ReadTransitions.becameRead(mapOf("1" to 2), mapOf("1" to 0), shown = setOf("1")))
  }

  @Test
  fun aConversationNeverSeenUnreadStays() {
    // The store may not have the pushed message yet.
    assertEquals(emptySet<String>(), ReadTransitions.becameRead(emptyMap(), mapOf("1" to 0), shown = setOf("1")))
  }

  @Test
  fun stillUnreadOrNotShownStays() {
    val previous = mapOf("1" to 2, "2" to 1)
    val current = mapOf("1" to 1, "2" to 0)
    assertEquals(emptySet<String>(), ReadTransitions.becameRead(previous, current, shown = setOf("1")))
  }

  @Test
  fun everythingReadEndsTheUnopenedNote() {
    assertTrue(ReadTransitions.isAllRead(mapOf("1" to 2, "2" to 0), mapOf("1" to 0, "2" to 0)))
    // Nothing was unread before (a fresh start), or something still is.
    assertFalse(ReadTransitions.isAllRead(emptyMap(), mapOf("1" to 0)))
    assertFalse(ReadTransitions.isAllRead(mapOf("1" to 2), mapOf("1" to 0, "2" to 1)))
  }
}
