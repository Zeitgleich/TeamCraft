package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CallProximityTest {
  @Test
  fun screenOffAtTheEarOnlyInACallOnTheEarpiece() {
    assertTrue(isScreenOffAtEar(isInCall = true, isOnEarpiece = true))
    assertFalse(isScreenOffAtEar(isInCall = true, isOnEarpiece = false))
    assertFalse(isScreenOffAtEar(isInCall = false, isOnEarpiece = true))
    assertFalse(isScreenOffAtEar(isInCall = false, isOnEarpiece = false))
  }
}
