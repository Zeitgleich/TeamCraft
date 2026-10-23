package de.zeitgleich.teamcraft.notifications

import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import de.zeitgleich.teamcraft.core.NotificationSettings
import de.zeitgleich.teamcraft.core.WireJson

class DoNotDisturbTest {
  @Test
  fun theEditorSavesTheServersJson() {
    val editor = DndEditor(
      ranges = listOf(
        DndRange(setOf(Weekday.FRIDAY, Weekday.MONDAY), startMinute = 22 * 60, endMinute = 7 * 60),
        DndRange(setOf(Weekday.SUNDAY), startMinute = 0, endMinute = 24 * 60 - 1),
      ),
      allowUserIds = listOf("42", "42", "43"),
      allowsUrgent = true,
    )
    // A winter instant: Berlin is UTC+1.
    val schedule = editor.toSchedule(BERLIN, Instant.parse("2026-01-15T12:00:00Z"))
    val json = WireJson.encodeToString(DndSchedule.serializer(), schedule)
    assertEquals(
      """{"utc_offset_minutes":60,"time_zone":"Europe/Berlin","windows":[""" +
        """{"weekdays":["mon","fri"],"start_minute":1320,"end_minute":420},""" +
        """{"weekdays":["sun"],"start_minute":0,"end_minute":1439}],""" +
        """"allow_user_ids":["42","43"],"allows_urgent":true}""",
      json,
    )
  }

  @Test
  fun theOffsetFollowsSummerTime() {
    val schedule = DndEditor().toSchedule(BERLIN, Instant.parse("2026-07-15T12:00:00Z"))
    assertEquals(120, schedule.utcOffsetMinutes)
  }

  @Test
  fun theServersJsonLoadsIntoTheEditor() {
    val json = """{"push_mode":"always","send_read_receipts":false,"auto_follow_mentioned_threads":true,""" +
      """"dnd_schedule":{"utc_offset_minutes":60,"time_zone":"Europe/Berlin",""" +
      """"windows":[{"weekdays":["sat","sun","xyz"],"start_minute":600,"end_minute":720}],"allow_user_ids":["42"]}}"""
    val settings = WireJson.decodeFromString(NotificationSettings.serializer(), json)
    val editor = DndEditor.from(settings.maybeDndSchedule)
    // A weekday a newer server knows is dropped, not fatal.
    assertEquals(listOf(DndRange(setOf(Weekday.SATURDAY, Weekday.SUNDAY), 600, 720)), editor.ranges)
    assertEquals(listOf("42"), editor.allowUserIds)
    assertFalse(editor.allowsUrgent)
  }

  @Test
  fun noScheduleIsAnEmptyEditor() {
    assertEquals(DndEditor(), DndEditor.from(null))
  }

  @Test
  fun rangesNeedADayAndALength() {
    assertFalse(DndRange(emptySet(), 60, 120).isValid)
    assertFalse(DndRange(setOf(Weekday.MONDAY), 60, 60).isValid)
    assertTrue(DndRange(setOf(Weekday.MONDAY), 1380, 60).isValid)
  }

  @Test
  fun atMostFourteenRanges() {
    val full = DndEditor(ranges = List(DndEditor.MAX_WINDOWS) { DndRange.DEFAULT })
    assertFalse(full.canAddRange)
    assertTrue(full.isValid)
    assertFalse(full.copy(ranges = full.ranges + DndRange.DEFAULT).isValid)
  }

  @Test
  fun oneHourIsSixtyMinutesEvenWhenTheClockGoesBack() {
    // 2026-10-25 02:30 CEST; at 03:00 the clock goes back to 02:00.
    val now = Instant.parse("2026-10-25T00:30:00Z")
    assertEquals(Duration.ofHours(1), Duration.between(now, QuickDnd.forOneHour(now)))
  }

  @Test
  fun untilMorningLandsOnEightOClockAcrossTheEndOfSummerTime() {
    // Saturday 2026-10-24 22:00 CEST; the night is an hour longer.
    val now = Instant.parse("2026-10-24T20:00:00Z")
    val end = QuickDnd.untilMorning(now, BERLIN)
    assertEquals(Instant.parse("2026-10-25T07:00:00Z"), end)
    assertEquals(Duration.ofHours(11), Duration.between(now, end))
  }

  @Test
  fun untilMorningLandsOnEightOClockAcrossTheStartOfSummerTime() {
    // Saturday 2026-03-28 22:00 CET; the night is an hour shorter.
    val now = Instant.parse("2026-03-28T21:00:00Z")
    val end = QuickDnd.untilMorning(now, BERLIN)
    assertEquals(Instant.parse("2026-03-29T06:00:00Z"), end)
    assertEquals(Duration.ofHours(9), Duration.between(now, end))
  }

  @Test
  fun untilMorningAfterMidnightMeansThisMorning() {
    // 01:00 local: the next 08:00 is today's.
    val now = Instant.parse("2026-01-15T00:00:00Z")
    assertEquals(Instant.parse("2026-01-15T07:00:00Z"), QuickDnd.untilMorning(now, BERLIN))
  }

  @Test
  fun aWindowPastMidnightCoversTheNextMorning() {
    val schedule = DndEditor(ranges = listOf(DndRange(setOf(Weekday.FRIDAY), 22 * 60, 7 * 60)))
      .toSchedule(BERLIN, Instant.parse("2026-01-15T12:00:00Z"))
    // Saturday 2026-01-17 06:00 Berlin: still Friday night's window.
    assertTrue(QuickDnd.isScheduledAt(schedule, Instant.parse("2026-01-17T05:00:00Z"), BERLIN))
    // Saturday 08:00: over.
    assertFalse(QuickDnd.isScheduledAt(schedule, Instant.parse("2026-01-17T07:00:00Z"), BERLIN))
    // Thursday 23:00: not a Friday window.
    assertFalse(QuickDnd.isScheduledAt(schedule, Instant.parse("2026-01-15T22:00:00Z"), BERLIN))
  }

  private companion object {
    val BERLIN: ZoneId = ZoneId.of("Europe/Berlin")
  }
}
