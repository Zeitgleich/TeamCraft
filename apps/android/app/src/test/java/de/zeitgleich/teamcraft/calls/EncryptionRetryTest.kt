package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class EncryptionRetryTest {
  @Test
  fun retryWaitsWholeMinutesRoundedUp() {
    assertEquals(5, encryptionRetryMinutesLeft(300, shownAtMs = 0, nowMs = 0))
    assertEquals(5, encryptionRetryMinutesLeft(300, shownAtMs = 0, nowMs = 1))
    assertEquals(1, encryptionRetryMinutesLeft(300, shownAtMs = 0, nowMs = 240_001))
    assertEquals(0, encryptionRetryMinutesLeft(300, shownAtMs = 0, nowMs = 300_000))
    assertEquals(0, encryptionRetryMinutesLeft(0, shownAtMs = 0, nowMs = 0))
  }

  @Test
  fun tryAgainIsDisabledUntilTheWaitHasPassed() {
    assertFalse(canRetryEncryption(minutesLeft = 5))
    assertFalse(canRetryEncryption(minutesLeft = 1))
    assertTrue(canRetryEncryption(minutesLeft = 0))
  }
}
