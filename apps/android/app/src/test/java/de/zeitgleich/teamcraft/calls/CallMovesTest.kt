package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** "Add notetaker" moved the call (e2ee.md §8.2, "In every call"): the banner says where the audio goes. */
class CallMovesTest {
  @Test
  fun aMoveThatAddsOnlyTheNotetakerSaysSo() {
    assertTrue(isNotetakerMove(listOf("9"), maybeBot = "9"))
  }

  @Test
  fun aMoveThatAddsPeopleTooIsAnOrdinaryOne() {
    assertFalse(isNotetakerMove(listOf("9", "3"), maybeBot = "9"))
    assertFalse(isNotetakerMove(listOf("3"), maybeBot = "9"))
  }

  @Test
  fun withoutANotetakerInThePolicyNoMoveIsOne() {
    assertFalse(isNotetakerMove(listOf("9"), maybeBot = null))
  }
}
