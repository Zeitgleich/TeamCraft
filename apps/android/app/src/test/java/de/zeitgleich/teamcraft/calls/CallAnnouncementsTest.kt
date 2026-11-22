package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class CallAnnouncementsTest {
  @Test
  fun aRosterChangeNamesWhoCameAndWhoWent() {
    val change = rosterChange(before = listOf("a", "b"), now = listOf("b", "c"))
    assertEquals(listOf("c"), change.joined)
    assertEquals(listOf("a"), change.left)
  }

  @Test
  fun noChangeIsEmpty() {
    assertTrue(rosterChange(listOf("a"), listOf("a")).isEmpty)
  }

  @Test
  fun aFlickerInsideOneWindowSaysNothing() {
    val gone = rosterChange(before = listOf("a", "b"), now = listOf("b"))
    val back = rosterChange(before = listOf("b"), now = listOf("a", "b"))
    assertTrue((gone + back).isEmpty)
  }

  @Test
  fun aBurstAddsUp() {
    val first = rosterChange(emptyList(), listOf("a"))
    val second = rosterChange(listOf("a"), listOf("a", "b", "c"))
    assertEquals(listOf("a", "b", "c"), (first + second).joined)
  }
}
