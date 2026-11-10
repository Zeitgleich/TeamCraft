package de.zeitgleich.teamcraft.crash

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

private const val CREATED_AT_MS = 1_700_000_000_000L

class StartVisibilityTest {
  @Test
  fun aFirstFrameSoonAfterCreationWasSeen() {
    assertTrue(StartVisibility.isSeen(CREATED_AT_MS, CREATED_AT_MS + 400))
    assertTrue(StartVisibility.isSeen(CREATED_AT_MS, CREATED_AT_MS + StartVisibility.FIRST_FRAME_SEEN_WITHIN_MS))
  }

  @Test
  fun aFirstFrameLongAfterCreationWasNotSeen() {
    // Created with the screen off, drawn when the user unlocked the phone minutes later.
    assertFalse(StartVisibility.isSeen(CREATED_AT_MS, CREATED_AT_MS + 746_000))
    assertFalse(StartVisibility.isSeen(CREATED_AT_MS, CREATED_AT_MS + StartVisibility.FIRST_FRAME_SEEN_WITHIN_MS + 1))
  }

  @Test
  fun anActivityRightAfterTheProcessStartIsCold() {
    assertTrue(StartVisibility.isColdStart(CREATED_AT_MS - 1_200, CREATED_AT_MS))
    assertTrue(StartVisibility.isColdStart(CREATED_AT_MS - StartVisibility.COLD_START_WITHIN_MS, CREATED_AT_MS))
  }

  @Test
  fun anActivityLongAfterTheProcessStartIsWarm() {
    // The process was started for a push; the user opened the app an hour later.
    assertFalse(StartVisibility.isColdStart(CREATED_AT_MS - 3_600_000, CREATED_AT_MS))
    assertFalse(StartVisibility.isColdStart(CREATED_AT_MS - StartVisibility.COLD_START_WITHIN_MS - 1, CREATED_AT_MS))
  }
}
