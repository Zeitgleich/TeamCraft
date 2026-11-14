package de.zeitgleich.teamcraft.ui.screens

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.MeetingForm
import uniffi.teamcraft_ffi.MeetingFormProblem
import uniffi.teamcraft_ffi.MeetingRepeat
import uniffi.teamcraft_ffi.MeetingRepeatForm
import uniffi.teamcraft_ffi.MeetingSeriesEnd
import java.time.LocalDate
import java.time.ZoneId
import java.time.ZonedDateTime

private val BERLIN: ZoneId = ZoneId.of("Europe/Berlin")

class MeetingFormSheetTest {
  @Test
  fun theOffsetFollowsTheStartAcrossDaylightSaving() {
    val winter = ZonedDateTime.of(2027, 1, 18, 10, 0, 0, 0, BERLIN).toInstant().toEpochMilli()
    val summer = ZonedDateTime.of(2027, 7, 19, 10, 0, 0, 0, BERLIN).toInstant().toEpochMilli()
    assertEquals(60, form().startingAt(winter, BERLIN).utcOffsetMinutes)
    val moved = form().startingAt(summer, BERLIN)
    assertEquals(summer, moved.startsAtMs)
    assertEquals(120, moved.utcOffsetMinutes)
  }

  @Test
  fun aSeriesThatEndsOnADayStillMeetsThatDay() {
    val end = endOfDayMs(LocalDate.of(2027, 1, 22), BERLIN)
    assertEquals(ZonedDateTime.of(2027, 1, 22, 23, 59, 59, 0, BERLIN).toInstant().toEpochMilli(), end)
  }

  @Test
  fun eachProblemBelongsToItsFieldInTheOrderOfTheSheet() {
    val problems = listOf(
      MeetingFormProblem.InvalidCount,
      MeetingFormProblem.InvalidEmail("nobody"),
      MeetingFormProblem.StartOffWeekdays,
    )
    assertEquals(MeetingField.EXTERNALS, problems.minOf { it.field })
    assertEquals(MeetingField.WEEKDAYS, MeetingFormProblem.NoWeekday.field)
    assertEquals(MeetingField.UNTIL, MeetingFormProblem.UntilBeforeStart.field)
  }

  private fun form() = MeetingForm(
    title = "Planning",
    description = "",
    startsAtMs = 0,
    utcOffsetMinutes = 0,
    durationMinutes = 30u,
    inviteeUserIds = emptyList(),
    externalInvitees = emptyList(),
    repeatRule = MeetingRepeatForm(MeetingRepeat.NEVER, 1u, List(7) { true }, MeetingSeriesEnd.NEVER, 10u, null, false),
    hasLobby = true,
    maybeNotetakerUserId = null,
    coHostUserIds = emptyList(),
    maybeRequestKey = null,
  )
}
