package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.TypingPlace
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

  private val places = listOf(
    TypingPlace("7", null, listOf("a")),
    TypingPlace("7", "9", listOf("b", "c")),
    TypingPlace("8", null, listOf("c")),
    TypingPlace("8", "4", emptyList()),
  )

  @Test
  fun aPlaceIsTheConversationOrOneOfItsThreads() {
    assertEquals(listOf("a"), typingIn(places, "7"))
    assertEquals(listOf("b", "c"), typingIn(places, "7", "9"))
    assertEquals(emptyList<String>(), typingIn(places, "7", "10"))
    assertEquals(emptyList<String>(), typingIn(places, "9"))
  }

  @Test
  fun theListNamesThoseTypingInConversationsOnly() {
    val names = typingNamesByConversation(places) { "N$it" }
    assertEquals(mapOf("7" to listOf("Na"), "8" to listOf("Nc")), names)
  }

  @Test
  fun typingNowAnswerParses() {
    val answer = de.zeitgleich.teamcraft.core.decodeAnswer(
      """{"answer":"typing","typing":[{"conversation":"7","users":["1"]},{"conversation":"7","maybe_thread":"9","users":["2"]}]}""",
    )
    assertEquals(listOf(TypingPlace("7", null, listOf("1")), TypingPlace("7", "9", listOf("2"))), answer.typing)
  }

  @Test
  fun typingActionCarriesTheThread() {
    assertEquals("""{"action":"typing","conversation":"7","maybe_thread":null}""", ChatActions.typing("7"))
    assertEquals("""{"action":"typing","conversation":"7","maybe_thread":"9"}""", ChatActions.typing("7", "9"))
  }
}
