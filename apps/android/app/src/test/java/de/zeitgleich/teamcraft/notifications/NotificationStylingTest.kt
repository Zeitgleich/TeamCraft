package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Test

class NotificationStylingTest {
  @Test
  fun initialsTakeTheFirstAndLastWord() {
    assertEquals("JK", NotificationStyling.initials("Jan Kahrs"))
    assertEquals("AB", NotificationStyling.initials("  ada  maria   byron "))
    assertEquals("A", NotificationStyling.initials("alice"))
    assertEquals("ÄÖ", NotificationStyling.initials("äne öhm"))
    assertEquals("?", NotificationStyling.initials("   "))
    assertEquals("B", NotificationStyling.initials("(Bob)"))
  }

  @Test
  fun theHueMatchesTheAppForUserIds() {
    // teamcraft-core: the user ID modulo 360.
    assertEquals(123_456_789L % 360, NotificationStyling.hue("123456789").toLong())
    val byName = NotificationStyling.hue("Alice")
    assertEquals(byName, NotificationStyling.hue("Alice"))
    assert(byName in 0 until 360)
  }

  @Test
  fun mentionsAreFoundButNotMailAddresses() {
    val text = "@alice see @first.last and @channel, mail bob@example.com."
    val found = NotificationStyling.mentionRanges(text).map { text.substring(it) }
    assertEquals(listOf("@alice", "@first.last", "@channel"), found)
    assertEquals(emptyList<IntRange>(), NotificationStyling.mentionRanges("no mentions @"))
  }
}
