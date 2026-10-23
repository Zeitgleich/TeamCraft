package de.zeitgleich.teamcraft.notifications

import java.time.Duration
import java.time.Instant
import java.time.LocalTime
import java.time.ZoneId
import java.time.ZonedDateTime
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * The do-not-disturb schedule as the server stores it (`/me/notification-settings`,
 * teamcraft-proto `DndSchedule`): weekly windows in the user's IANA time zone (the fixed offset is
 * the fallback for evaluators without a time zone database), plus who and what breaks through.
 */
@Serializable
data class DndSchedule(
  @SerialName("utc_offset_minutes") val utcOffsetMinutes: Int = 0,
  @SerialName("time_zone") val maybeTimeZone: String? = null,
  val windows: List<DndWindow> = emptyList(),
  @SerialName("allow_user_ids") val allowUserIds: List<String> = emptyList(),
  @SerialName("allows_urgent") val allowsUrgent: Boolean = false,
)

/** A weekly window: starts on each of `weekdays`; an end before the start runs past midnight. */
@Serializable
data class DndWindow(
  /** Wire values `mon` … `sun` (see [Weekday]). */
  val weekdays: List<String>,
  @SerialName("start_minute") val startMinute: Int,
  @SerialName("end_minute") val endMinute: Int,
)

enum class Weekday(val wire: String) {
  MONDAY("mon"),
  TUESDAY("tue"),
  WEDNESDAY("wed"),
  THURSDAY("thu"),
  FRIDAY("fri"),
  SATURDAY("sat"),
  SUNDAY("sun"),
  ;

  companion object {
    fun fromWire(value: String): Weekday? = entries.firstOrNull { it.wire == value }

    fun of(day: java.time.DayOfWeek): Weekday = entries[day.value - 1]
  }
}

/** One editable row of the schedule editor. */
data class DndRange(val weekdays: Set<Weekday>, val startMinute: Int, val endMinute: Int) {
  /** Saveable: at least one day, and a window of some length. */
  val isValid: Boolean get() = weekdays.isNotEmpty() && startMinute != endMinute && startMinute in MINUTES && endMinute in MINUTES

  fun toWindow(): DndWindow = DndWindow(
    weekdays = Weekday.entries.filter { it in weekdays }.map { it.wire },
    startMinute = startMinute,
    endMinute = endMinute,
  )

  companion object {
    val MINUTES = 0 until 24 * 60

    /** Weeknights 22:00 to 07:00, the row a new range starts with. */
    val DEFAULT = DndRange(
      weekdays = setOf(Weekday.MONDAY, Weekday.TUESDAY, Weekday.WEDNESDAY, Weekday.THURSDAY, Weekday.FRIDAY),
      startMinute = 22 * 60,
      endMinute = 7 * 60,
    )

    /** Unknown weekdays (a newer server) are dropped rather than failing the editor. */
    fun fromWindow(window: DndWindow): DndRange = DndRange(
      weekdays = window.weekdays.mapNotNull(Weekday::fromWire).toSet(),
      startMinute = window.startMinute,
      endMinute = window.endMinute,
    )
  }
}

/** The editor's state: what the screen edits, turned into the server's schedule on save. */
data class DndEditor(
  val ranges: List<DndRange> = emptyList(),
  val allowUserIds: List<String> = emptyList(),
  val allowsUrgent: Boolean = false,
) {
  val canAddRange: Boolean get() = ranges.size < MAX_WINDOWS
  val isValid: Boolean get() = ranges.size <= MAX_WINDOWS && ranges.all { it.isValid }

  /** The schedule to save, in the device's zone and its offset at `now`. */
  fun toSchedule(zone: ZoneId, now: Instant): DndSchedule = DndSchedule(
    utcOffsetMinutes = zone.rules.getOffset(now).totalSeconds / SECONDS_PER_MINUTE,
    maybeTimeZone = zone.id,
    windows = ranges.map { it.toWindow() },
    allowUserIds = allowUserIds.distinct(),
    allowsUrgent = allowsUrgent,
  )

  companion object {
    /** The server's limit (teamcraft-proto). */
    const val MAX_WINDOWS = 14
    private const val SECONDS_PER_MINUTE = 60

    fun from(maybeSchedule: DndSchedule?): DndEditor {
      val schedule = maybeSchedule ?: return DndEditor()
      return DndEditor(
        ranges = schedule.windows.map(DndRange::fromWindow),
        allowUserIds = schedule.allowUserIds,
        allowsUrgent = schedule.allowsUrgent,
      )
    }
  }
}

/** When quick DND ends, computed in the device's zone so DST changes land on the wall clock. */
object QuickDnd {
  /** "Until tomorrow morning" ends at this local time. */
  val MORNING: LocalTime = LocalTime.of(8, 0)

  fun forOneHour(now: Instant): Instant = now.plus(Duration.ofHours(1))

  /** The next 08:00: today's if it is still before (after midnight), else tomorrow's. */
  fun untilMorning(now: Instant, zone: ZoneId): Instant {
    val local = ZonedDateTime.ofInstant(now, zone)
    val today = local.toLocalDate().atTime(MORNING).atZone(zone)
    val end = if (local.isBefore(today)) today else local.toLocalDate().plusDays(1).atTime(MORNING).atZone(zone)
    return end.toInstant()
  }

  /** Whether the schedule's windows cover `now` (the core decides for pushes; this is for display). */
  fun isScheduledAt(schedule: DndSchedule, now: Instant, zone: ZoneId): Boolean {
    val local = ZonedDateTime.ofInstant(now, schedule.maybeTimeZone?.let(::zoneOrNull) ?: zone)
    val minute = local.hour * 60 + local.minute
    val today = Weekday.of(local.dayOfWeek)
    val yesterday = Weekday.of(local.dayOfWeek.minus(1))
    return schedule.windows.map(DndRange::fromWindow).any { range ->
      if (range.startMinute < range.endMinute) {
        today in range.weekdays && minute >= range.startMinute && minute < range.endMinute
      } else {
        (today in range.weekdays && minute >= range.startMinute) || (yesterday in range.weekdays && minute < range.endMinute)
      }
    }
  }

  private fun zoneOrNull(id: String): ZoneId? = runCatching { ZoneId.of(id) }.getOrNull()
}
