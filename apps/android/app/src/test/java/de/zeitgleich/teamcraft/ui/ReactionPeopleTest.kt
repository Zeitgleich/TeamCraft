package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Reaction
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ReactionPeopleTest {
  private val names = mapOf("1" to "Me", "2" to "Alex Example", "3" to "Sam Example")

  @Test
  fun theViewerIsNotNamedAmongTheOthers() {
    val people = reactionPeople(Reaction("+1", 3, reactedByMe = true, reactors = listOf("1", "2", "3")), names::get)
    assertTrue(people.hasMe)
    assertEquals(listOf("Alex Example", "Sam Example"), people.others)
    assertEquals(0, people.unnamed)
  }

  @Test
  fun unknownPeopleOnlyCountAndNeverShowTheirId() {
    val people = reactionPeople(Reaction("+1", 4, reactors = listOf("2", "99", "98")), names::get)
    assertFalse(people.hasMe)
    assertEquals(listOf("Alex Example"), people.others)
    assertEquals(3, people.unnamed)
  }

  @Test
  fun aReactionWithoutTheListStillCounts() {
    val people = reactionPeople(Reaction("+1", 2), names::get)
    assertEquals(2, people.unnamed)
  }
}
