package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ScheduledMessage
import de.zeitgleich.teamcraft.core.ScheduledOverview
import de.zeitgleich.teamcraft.core.SchedulePolicy
import de.zeitgleich.teamcraft.core.SendOptions
import de.zeitgleich.teamcraft.core.decodeAnswer
import java.time.LocalDate
import java.time.ZoneId
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ScheduleTimesTest {
  private val zone = ZoneId.of("Europe/Berlin")

  /** Wednesday 2026-11-04, 12:00 in Berlin. */
  private val wednesdayNoon = ScheduleTimes.at(LocalDate.of(2026, 11, 4), java.time.LocalTime.NOON, zone)

  @Test
  fun inOneHourCountsFromTheServersClock() {
    assertEquals(3_600_000L + 500L, ScheduleTimes.inOneHour(500L))
  }

  @Test
  fun tomorrowMorningIsTheNextDayAtEight() {
    val expected = ScheduleTimes.at(LocalDate.of(2026, 11, 5), java.time.LocalTime.of(8, 0), zone)
    assertEquals(expected, ScheduleTimes.tomorrowMorning(wednesdayNoon, zone))
  }

  @Test
  fun mondayMorningOnAMondayIsAWeekAhead() {
    val monday = ScheduleTimes.at(LocalDate.of(2026, 11, 2), java.time.LocalTime.of(7, 0), zone)
    val expected = ScheduleTimes.at(LocalDate.of(2026, 11, 9), java.time.LocalTime.of(8, 0), zone)
    assertEquals(expected, ScheduleTimes.mondayMorning(monday, zone))
  }

  @Test
  fun everyProblemShowsAtItsField() {
    val errors = ScheduleTimes.errors("  ", wednesdayNoon, overview(serverNow = wednesdayNoon))
    assertEquals(R.string.schedule_error_empty, errors[ScheduleField.BODY])
    assertEquals(R.string.schedule_error_past, errors[ScheduleField.TIME])
    assertEquals(ScheduleField.BODY, errors.maybeFirst)
  }

  @Test
  fun theHorizonEndsExactlyAtItsDay() {
    val overview = overview(serverNow = 0, horizonDays = 2)
    assertNull(ScheduleTimes.timeError(2 * 86_400_000L, overview))
    assertEquals(R.string.schedule_error_far, ScheduleTimes.timeError(2 * 86_400_000L + 1, overview))
  }

  @Test
  fun onlyWhatWaitsForTheUserAsksForAttention() {
    assertTrue(message(ScheduledMessage.STATUS_NOT_A_MEMBER).needsAttention)
    assertFalse(message(ScheduledMessage.STATUS_STALE).needsAttention)
    assertFalse(message(ScheduledMessage.STATUS_NOT_A_MEMBER).canGoOut)
    assertTrue(message(ScheduledMessage.STATUS_POLICY_OFF).canGoOut)
    assertFalse(message(ScheduledMessage.STATUS_POLICY_OFF).canEdit)
  }

  @Test
  fun theActionsHaveTheCoresShape() {
    val json = Json.parseToJsonElement(ChatActions.schedule("7", "later", SendOptions(), 123)).jsonObject
    assertEquals("schedule_message", json["action"]!!.jsonPrimitive.content)
    assertEquals("123", json["due_at"]!!.jsonPrimitive.content)
    assertEquals("later", json["message"]!!.jsonObject["body"]!!.jsonPrimitive.content)
    assertEquals("""{"action":"cancel_scheduled","id":"ab"}""", ChatActions.cancelScheduled("ab"))
    assertEquals("""{"action":"send_scheduled_now","id":"ab"}""", ChatActions.sendScheduledNow("ab"))
  }

  @Test
  fun theCoresAnswerDecodes() {
    val answer = decodeAnswer(
      """{"answer":"scheduled_messages","overview":{"messages":[{"id":"ab","conversation":"7","conversation_name":"general",
      "maybe_thread":null,"body":"later","due_at":5,"status":"policy_off","priority":"normal"}],
      "policy":{"is_enabled":true,"max_horizon_days":30,"max_per_user":50},"server_now":3}}""",
    )
    val overview = answer.overview!!
    assertEquals(30, overview.policy.maxHorizonDays)
    assertEquals(ScheduledMessage.STATUS_POLICY_OFF, overview.messages[0].status)
    assertEquals(3L, overview.serverNow)
  }

  private fun overview(serverNow: Long, horizonDays: Int = 365) =
    ScheduledOverview(policy = SchedulePolicy(isEnabled = true, maxHorizonDays = horizonDays), serverNow = serverNow)

  private fun message(status: String, dueAt: Long = 0) =
    ScheduledMessage(id = "ab", conversation = "7", body = "later", dueAt = dueAt, status = status)
}
