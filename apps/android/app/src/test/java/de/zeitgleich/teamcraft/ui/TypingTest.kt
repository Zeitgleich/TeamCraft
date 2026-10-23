package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TypingTest {
  @Test
  fun lineByNumberOfPeople() {
    assertNull(typingLine(emptyList()))
    assertEquals(TypingLine.One("Alice"), typingLine(listOf("Alice")))
    assertEquals(TypingLine.Two("Alice", "Bob"), typingLine(listOf("Alice", "Bob")))
    assertEquals(TypingLine.Several, typingLine(listOf("Alice", "Bob", "Carol")))
  }

  @Test
  fun typingIsAnnouncedAtMostEveryFewSeconds() {
    assertTrue(shouldNotifyTyping("h", nowMs = 1_000, lastNotifiedMs = null))
    assertFalse(shouldNotifyTyping("he", nowMs = 1_000 + TYPING_NOTIFY_INTERVAL_MS - 1, lastNotifiedMs = 1_000))
    assertTrue(shouldNotifyTyping("hel", nowMs = 1_000 + TYPING_NOTIFY_INTERVAL_MS, lastNotifiedMs = 1_000))
  }

  @Test
  fun clearingTheTextAnnouncesNothing() {
    assertFalse(shouldNotifyTyping("  ", nowMs = 10_000, lastNotifiedMs = null))
  }
}
