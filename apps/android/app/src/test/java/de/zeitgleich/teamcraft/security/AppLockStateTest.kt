package de.zeitgleich.teamcraft.security

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppLockStateTest {
  private var now = 1_000L
  private val state = AppLockState { now }

  @Test
  fun locksOnlyAfterTheChosenTimeInTheBackground() {
    state.onBackground()
    now += LockAfter.OneMinute.millis - 1
    assertFalse(state.shouldLockOnForeground(isEnabled = true, lockAfter = LockAfter.OneMinute))
    state.onBackground()
    now += LockAfter.OneMinute.millis
    assertTrue(state.shouldLockOnForeground(isEnabled = true, lockAfter = LockAfter.OneMinute))
  }

  @Test
  fun immediatelyLocksOnEveryReturn() {
    state.onBackground()
    assertTrue(state.shouldLockOnForeground(isEnabled = true, lockAfter = LockAfter.Immediately))
  }

  @Test
  fun neverLocksWhenDisabledOrWithoutABackgroundPhase() {
    assertFalse(state.shouldLockOnForeground(isEnabled = true, lockAfter = LockAfter.Immediately))
    state.onBackground()
    now += LockAfter.FifteenMinutes.millis
    assertFalse(state.shouldLockOnForeground(isEnabled = false, lockAfter = LockAfter.Immediately))
  }

  @Test
  fun aForegroundUsesUpTheBackgroundPhase() {
    state.onBackground()
    now += LockAfter.FiveMinutes.millis
    assertTrue(state.shouldLockOnForeground(isEnabled = true, lockAfter = LockAfter.FiveMinutes))
    assertFalse(state.shouldLockOnForeground(isEnabled = true, lockAfter = LockAfter.FiveMinutes))
  }
}
