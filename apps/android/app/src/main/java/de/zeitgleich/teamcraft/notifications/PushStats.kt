package de.zeitgleich.teamcraft.notifications

import android.content.Context

/**
 * Counts for the notification diagnostics the core doesn't keep: wake-ups this device kept quiet
 * by its own mute/DND check, and unknown kinds. Counts and times only.
 */
class PushStats(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val suppressedCount: Int get() = prefs.getInt(KEY_SUPPRESSED, 0)
  val unknownCount: Int get() = prefs.getInt(KEY_UNKNOWN, 0)
  val maybeLastSuppressedAtMs: Long? get() = prefs.getLong(KEY_LAST_SUPPRESSED_AT, 0).takeIf { it > 0 }

  fun recordIgnored(reason: IgnoreReason) {
    val editor = prefs.edit()
    when (reason) {
      IgnoreReason.SUPPRESSED -> editor
        .putInt(KEY_SUPPRESSED, suppressedCount + 1)
        .putLong(KEY_LAST_SUPPRESSED_AT, System.currentTimeMillis())
      IgnoreReason.UNKNOWN_KIND -> editor.putInt(KEY_UNKNOWN, unknownCount + 1)
    }
    editor.apply()
  }

  fun clear() {
    prefs.edit().clear().apply()
  }

  private companion object {
    const val FILE = "push_stats"
    const val KEY_SUPPRESSED = "suppressed"
    const val KEY_UNKNOWN = "unknown"
    const val KEY_LAST_SUPPRESSED_AT = "last_suppressed_at"
  }
}
