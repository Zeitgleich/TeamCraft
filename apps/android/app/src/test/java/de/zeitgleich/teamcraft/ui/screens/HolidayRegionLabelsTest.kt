package de.zeitgleich.teamcraft.ui.screens

import de.zeitgleich.teamcraft.core.HOLIDAY_REGIONS
import org.junit.Assert.assertEquals
import org.junit.Test

class HolidayRegionLabelsTest {
  @Test
  fun everyRegionHasANameAndNoneOtherDoes() {
    assertEquals(setOf("") + HOLIDAY_REGIONS, HOLIDAY_REGION_LABELS.keys)
    assertEquals(HOLIDAY_REGION_LABELS.size, HOLIDAY_REGION_LABELS.values.toSet().size)
  }
}
