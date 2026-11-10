package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Test

class MeetingNoticeReportTest {
  @Test
  fun theReportNamesTheMeetingAndTheOccurrence() {
    assertEquals("/meetings/77/notices/dismiss", MeetingNoticeReport.path("77"))
    assertEquals("""{"starts_at":1800000000000}""", MeetingNoticeReport.body(1_800_000_000_000L))
  }
}
