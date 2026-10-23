package de.zeitgleich.teamcraft.push

import org.junit.Assert.assertEquals
import org.junit.Test

class UnifiedPushRecoveryTest {
  @Test
  fun theWaitDoublesFromThirtySecondsUpToSixHours() {
    assertEquals(30_000L, UnifiedPushRecovery.delayMs(1))
    assertEquals(60_000L, UnifiedPushRecovery.delayMs(2))
    assertEquals(120_000L, UnifiedPushRecovery.delayMs(3))
    assertEquals(6 * 60 * 60 * 1000L, UnifiedPushRecovery.delayMs(UnifiedPushRecovery.MAX_ATTEMPTS))
  }

  @Test
  fun outOfRangeAttemptsStayBounded() {
    assertEquals(30_000L, UnifiedPushRecovery.delayMs(0))
    assertEquals(6 * 60 * 60 * 1000L, UnifiedPushRecovery.delayMs(Int.MAX_VALUE))
  }
}
