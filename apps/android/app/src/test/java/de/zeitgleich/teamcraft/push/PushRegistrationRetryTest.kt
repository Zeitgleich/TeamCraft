package de.zeitgleich.teamcraft.push

import org.junit.Assert.assertEquals
import org.junit.Test

class PushRegistrationRetryTest {
  @Test
  fun theWaitDoublesFromThirtySecondsUpToAnHour() {
    assertEquals(30_000L, PushRegistrationRetry.delayMs(1))
    assertEquals(60_000L, PushRegistrationRetry.delayMs(2))
    assertEquals(60 * 60 * 1000L, PushRegistrationRetry.delayMs(8))
    assertEquals(60 * 60 * 1000L, PushRegistrationRetry.delayMs(100))
  }

  @Test
  fun outOfRangeAttemptsStayBounded() {
    assertEquals(30_000L, PushRegistrationRetry.delayMs(0))
    assertEquals(60 * 60 * 1000L, PushRegistrationRetry.delayMs(Int.MAX_VALUE))
  }
}
