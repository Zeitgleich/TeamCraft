package de.zeitgleich.teamcraft.ui

import java.time.LocalDate
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PinnedDayTest {
  @Test
  fun theDayAtTheTopStaysInPlace() {
    val dividers = listOf(DividerOnScreen(SECOND, 500))
    assertEquals(PinnedDay(FIRST, 0), pinnedDay(dividers, FIRST, PIN_TOP, PILL))
  }

  @Test
  fun theNextDividerPushesItUp() {
    // 34 px from the top: 4 px into the pinned pill's place and its gap.
    val dividers = listOf(DividerOnScreen(SECOND, 34))
    assertEquals(PinnedDay(FIRST, -4), pinnedDay(dividers, FIRST, PIN_TOP, PILL))
  }

  @Test
  fun theNextDayTakesOverOnceItsPillReachesThePlace() {
    val justBefore = pinnedDay(listOf(DividerOnScreen(SECOND, PIN_TOP + 1)), FIRST, PIN_TOP, PILL)!!
    assertEquals(FIRST, justBefore.date)
    assertEquals(1 - PIN_TOP - PILL, justBefore.push)
    val reached = listOf(DividerOnScreen(SECOND, PIN_TOP), DividerOnScreen(THIRD, 900))
    assertEquals(PinnedDay(SECOND, 0), pinnedDay(reached, FIRST, PIN_TOP, PILL))
  }

  @Test
  fun nothingIsPinnedAboveTheFirstDay() {
    assertNull(pinnedDay(listOf(DividerOnScreen(FIRST, 40)), null, PIN_TOP, PILL))
  }

  private companion object {
    const val PIN_TOP = 8
    const val PILL = 22
    val FIRST: LocalDate = LocalDate.of(2024, 7, 24)
    val SECOND: LocalDate = LocalDate.of(2024, 7, 25)
    val THIRD: LocalDate = LocalDate.of(2026, 1, 2)
  }
}
