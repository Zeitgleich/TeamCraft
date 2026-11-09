package de.zeitgleich.teamcraft.ui

import androidx.annotation.StringRes
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import java.time.DayOfWeek
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.time.ZonedDateTime
import java.time.temporal.TemporalAdjusters
import uniffi.teamcraft_ffi.Presence

/**
 * Presence as every screen shows it (#1578): one dot per status, in the colours and shapes of the
 * desktop and web app, and one list of statuses in the same order everywhere.
 */
object PresenceStyle {
  val AWAY = Color(0xFFE0A030)
  val RED = Color(0xFFD0453A)
  val OFFLINE = Color(0xFF8A8F98)

  /** The status list's order: available, busy, do not disturb, be right back, away, offline. */
  val ALL = listOf(
    Presence.ONLINE,
    Presence.BUSY,
    Presence.DO_NOT_DISTURB,
    Presence.BE_RIGHT_BACK,
    Presence.AWAY,
    Presence.OFFLINE,
  )
}

/** The core's name of a presence (`Online`, `Busy`, `DoNotDisturb`, …), as snapshots carry it. */
fun presenceOf(maybeName: String?): Presence = when (maybeName) {
  "Busy" -> Presence.BUSY
  "DoNotDisturb" -> Presence.DO_NOT_DISTURB
  "BeRightBack" -> Presence.BE_RIGHT_BACK
  "Away" -> Presence.AWAY
  "Offline" -> Presence.OFFLINE
  else -> Presence.ONLINE
}

/**
 * The coloured dot of a presence. Busy and do not disturb share red, do not disturb carries a bar;
 * be right back is the away colour with clock hands; offline is a grey ring. No emoji (design.md).
 */
@Composable
fun PresenceDot(presence: Presence, modifier: Modifier = Modifier, size: Dp = 10.dp) {
  val online = LocalTeamCraftColors.current.presence
  Canvas(modifier.size(size)) {
    val radius = this.size.minDimension / 2
    val mark = Color.White
    val line = radius * 0.3f
    when (presence) {
      Presence.ONLINE -> drawCircle(online)
      Presence.BUSY -> drawCircle(PresenceStyle.RED)
      Presence.DO_NOT_DISTURB -> {
        drawCircle(PresenceStyle.RED)
        drawRect(mark, Offset(center.x - radius * 0.6f, center.y - line / 2), Size(radius * 1.2f, line))
      }
      Presence.BE_RIGHT_BACK -> {
        drawCircle(PresenceStyle.AWAY)
        drawLine(mark, center, Offset(center.x, center.y - radius * 0.6f), strokeWidth = line)
        drawLine(mark, center, Offset(center.x + radius * 0.5f, center.y), strokeWidth = line)
      }
      Presence.AWAY -> drawCircle(PresenceStyle.AWAY)
      Presence.OFFLINE -> drawCircle(PresenceStyle.OFFLINE, radius - line / 2, style = Stroke(width = line))
    }
  }
}

/** What others see: "Online", "Busy", … */
@StringRes
fun presenceLabel(presence: Presence): Int = when (presence) {
  Presence.ONLINE -> R.string.presence_online
  Presence.BUSY -> R.string.presence_busy
  Presence.DO_NOT_DISTURB -> R.string.presence_dnd
  Presence.BE_RIGHT_BACK -> R.string.presence_brb
  Presence.AWAY -> R.string.presence_away
  Presence.OFFLINE -> R.string.presence_peer_offline
}

/** The status list's wording: what the user does ("Available", "Appear away", "Appear offline"). */
@StringRes
fun presencePickLabel(presence: Presence): Int = when (presence) {
  Presence.ONLINE -> R.string.status_pick_online
  Presence.BUSY -> R.string.status_pick_busy
  Presence.DO_NOT_DISTURB -> R.string.status_pick_dnd
  Presence.BE_RIGHT_BACK -> R.string.status_pick_brb
  Presence.AWAY -> R.string.status_pick_away
  Presence.OFFLINE -> R.string.status_pick_offline
}

/** "Reset after" of a manual status (#1578); [UNTIL_TIME] asks for a date and time. */
enum class ResetAfter(@StringRes val label: Int) {
  THIRTY_MINUTES(R.string.status_duration_30m),
  ONE_HOUR(R.string.status_duration_1h),
  TWO_HOURS(R.string.status_duration_2h),
  TODAY(R.string.status_duration_today),
  THIS_WEEK(R.string.status_duration_week),
  UNTIL_TIME(R.string.status_duration_until),
  NEVER(R.string.status_duration_never);

  /** When a status set at [now] ends; null for never and for a time the user picks. */
  fun end(now: Instant, zone: ZoneId): Instant? {
    val local = ZonedDateTime.ofInstant(now, zone)
    return when (this) {
      THIRTY_MINUTES -> now.plus(Duration.ofMinutes(30))
      ONE_HOUR -> now.plus(Duration.ofHours(1))
      TWO_HOURS -> now.plus(Duration.ofHours(2))
      TODAY -> local.toLocalDate().plusDays(1).atStartOfDay(zone).toInstant()
      THIS_WEEK -> local.toLocalDate().with(TemporalAdjusters.next(DayOfWeek.MONDAY)).atStartOfDay(zone).toInstant()
      UNTIL_TIME, NEVER -> null
    }
  }
}
