package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertEquals
import org.junit.Test

class CarTitlesTest {
  @Test
  fun anEmptyOrBlankNameGetsThePlaceholder() {
    assertEquals("Unnamed", carRowTitle("", "Unnamed"))
    assertEquals("Unnamed", carRowTitle("  ", "Unnamed"))
  }

  @Test
  fun aNameIsKept() {
    assertEquals("Bob", carRowTitle("Bob", "Unnamed"))
  }
}
