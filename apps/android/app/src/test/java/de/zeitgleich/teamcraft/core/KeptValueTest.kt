package de.zeitgleich.teamcraft.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class KeptValueTest {
  @Test
  fun aValueIsSharedUntilTheNextChange() {
    val kept = KeptValue<String>()
    kept.keep(generation = 1, changes = 5, "first")
    assertEquals("first", kept.at(1, 5))
    assertNull(kept.at(1, 6))
    // Until it is read again, the screens show the one from before the change.
    assertEquals("first", kept.latest(1))
  }

  @Test
  fun anotherAccountNeverSeesIt() {
    // Signing out and in to another account opens a new generation, with or without a change.
    val kept = KeptValue<String>()
    kept.keep(generation = 1, changes = 5, "account A")
    assertNull(kept.at(2, 5))
    assertNull(kept.latest(2))
  }

  @Test
  fun clearedItIsGone() {
    val kept = KeptValue<String>()
    kept.keep(1, 1, "value")
    kept.clear()
    assertNull(kept.latest(1))
  }
}
