package de.zeitgleich.teamcraft.ui

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ScheduledOverview
import de.zeitgleich.teamcraft.ui.forms.FieldErrors
import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDate
import java.time.LocalTime
import java.time.ZoneId
import java.time.temporal.TemporalAdjusters

/** The fields of editing a scheduled message, top to bottom. */
enum class ScheduleField { BODY, TIME }

/**
 * The times "send later" offers and what it refuses, as desktop and web do
 * (teamcraft-client-api `scheduled`). No Compose: unit tests cover them.
 */
object ScheduleTimes {
  private const val HOUR_MS = 3_600_000L
  private const val DAY_MS = 24 * HOUR_MS
  private val MORNING: LocalTime = LocalTime.of(8, 0)

  /** Counts from the server's clock, so a wrong device clock doesn't shift it. */
  fun inOneHour(serverNowMs: Long): Long = serverNowMs + HOUR_MS

  fun tomorrowMorning(nowMs: Long, zone: ZoneId): Long =
    at(Instant.ofEpochMilli(nowMs).atZone(zone).toLocalDate().plusDays(1), MORNING, zone)

  /** The next Monday's morning; on a Monday, a week ahead. */
  fun mondayMorning(nowMs: Long, zone: ZoneId): Long = at(
    Instant.ofEpochMilli(nowMs).atZone(zone).toLocalDate().with(TemporalAdjusters.next(DayOfWeek.MONDAY)),
    MORNING,
    zone,
  )

  fun at(date: LocalDate, time: LocalTime, zone: ZoneId): Long = date.atTime(time).atZone(zone).toInstant().toEpochMilli()

  /** Whether a time can be offered at all. */
  fun isWithinRules(dueAtMs: Long, overview: ScheduledOverview): Boolean = timeError(dueAtMs, overview) == null

  /** What is wrong with the text and the time; every problem at once, at its field. */
  fun errors(body: String, dueAtMs: Long, overview: ScheduledOverview): FieldErrors<ScheduleField> = FieldErrors.of(
    ScheduleField.BODY to when {
      body.isBlank() -> R.string.schedule_error_empty
      else -> null
    },
    ScheduleField.TIME to timeError(dueAtMs, overview),
  )

  @StringRes
  fun timeError(dueAtMs: Long, overview: ScheduledOverview): Int? = when {
    dueAtMs <= overview.serverNow -> R.string.schedule_error_past
    dueAtMs > overview.serverNow + overview.policy.maxHorizonDays * DAY_MS -> R.string.schedule_error_far
    else -> null
  }
}
