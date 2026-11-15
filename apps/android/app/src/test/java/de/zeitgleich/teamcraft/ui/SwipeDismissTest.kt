package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** When a swipe closes the picture and video viewer (#1999). */
class SwipeDismissTest {
  private val extent = 2000f

  @Test
  fun aLongDragClosesInEitherDirection() {
    assertTrue(shouldDismiss(700f, 0f, extent))
    assertTrue(shouldDismiss(-700f, 0f, extent))
  }

  @Test
  fun aShortSlowDragSpringsBack() {
    assertFalse(shouldDismiss(200f, 100f, extent))
    assertFalse(shouldDismiss(0f, 0f, extent))
  }

  @Test
  fun aFastFlingClosesFromAShortDrag() {
    assertTrue(shouldDismiss(150f, 2500f, extent))
    assertTrue(shouldDismiss(-150f, -2500f, extent))
  }

  @Test
  fun aFlingBackTowardsTheStartKeepsTheViewer() {
    assertFalse(shouldDismiss(700f, -2500f, extent))
  }

  @Test
  fun noHeightNeverCloses() {
    assertFalse(shouldDismiss(500f, 5000f, 0f))
  }

  @Test
  fun theBackdropFadesWithTheDistance() {
    assertEquals(0f, dismissProgress(0f, extent), 0.0001f)
    assertEquals(0.5f, dismissProgress(500f, extent), 0.0001f)
    assertEquals(0.5f, dismissProgress(-500f, extent), 0.0001f)
    assertEquals(1f, dismissProgress(5000f, extent), 0.0001f)
    assertEquals(0f, dismissProgress(500f, 0f), 0.0001f)
  }
}
