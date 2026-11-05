package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AnimatedPicturesTest {
  @Test
  fun `the setting decides until the user chooses`() {
    assertTrue(wantsToMove(AnimatedImagesMode.AUTOPLAY, PlayChoice.UNSET))
    assertFalse(wantsToMove(AnimatedImagesMode.ON_TAP, PlayChoice.UNSET))
    assertFalse(wantsToMove(AnimatedImagesMode.AUTOPLAY, PlayChoice.PAUSED))
    assertTrue(wantsToMove(AnimatedImagesMode.ON_TAP, PlayChoice.PLAYING))
  }

  @Test
  fun `removed animations stop what would start by itself`() {
    assertEquals(AnimatedImagesMode.ON_TAP, AnimatedImagesMode.AUTOPLAY.effective(isMotionReduced = true))
    assertEquals(AnimatedImagesMode.AUTOPLAY, AnimatedImagesMode.AUTOPLAY.effective(isMotionReduced = false))
    assertEquals(AnimatedImagesMode.ON_TAP, AnimatedImagesMode.ON_TAP.effective(isMotionReduced = false))
  }

  @Test
  fun `only an explicit choice leaves automatic play`() {
    assertEquals(AnimatedImagesMode.AUTOPLAY, animatedImagesOf(null))
    assertEquals(AnimatedImagesMode.AUTOPLAY, animatedImagesOf("SOMETHING_NEW"))
    assertEquals(AnimatedImagesMode.ON_TAP, animatedImagesOf("ON_TAP"))
  }

  @Test
  fun `a stop is no end of the file`() {
    assertFalse("stopped on purpose", isRealEnd(wantsPlaying = false, isRunning = false))
    assertFalse("stopped and started again before the report", isRealEnd(wantsPlaying = true, isRunning = true))
    assertTrue("the file's loops are over", isRealEnd(wantsPlaying = true, isRunning = false))
  }

  @Test
  fun `only frames within the limit are animated, without overflowing`() {
    assertTrue(isWithinAnimationLimit(4000, 4000))
    assertFalse(isWithinAnimationLimit(4001, 4000))
    assertFalse(isWithinAnimationLimit(Int.MAX_VALUE, Int.MAX_VALUE))
  }

  @Test
  fun `the fetch budget stops at its limit and takes nothing then`() {
    val budget = AutoFetchBudget(maxBytes = 10)
    assertTrue(budget.take(6))
    assertFalse(budget.take(5))
    assertTrue(budget.take(4))
    assertFalse(budget.take(1))
  }

  @Test
  fun `media types are compared by their essence`() {
    assertTrue(isAnimationType(" Image/GIF; foo=bar"))
    assertTrue(isAnimationType("image/apng"))
    assertFalse(isAnimationType("image/webp"))
    assertTrue(mayBeAnimated("image/webp"))
    assertTrue(mayBeAnimated("image/avif"))
    assertFalse(mayBeAnimated("image/jpeg"))
    assertEquals("GIF", animationLabel("image/gif;x=y"))
  }
}
