package de.zeitgleich.teamcraft.notifications

import android.content.Context
import android.util.Log
import uniffi.teamcraft_ffi.NotificationGap

/**
 * Counts for the notification diagnostics the core doesn't keep: wake-ups this device kept quiet
 * by its own mute/DND check, unknown kinds, wake-ups that could not be opened, and why the last
 * message notification showed less than the message. Counts, times and reasons only.
 */
class PushStats(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val suppressedCount: Int get() = prefs.getInt(KEY_SUPPRESSED, 0)
  val unknownCount: Int get() = prefs.getInt(KEY_UNKNOWN, 0)
  val maybeLastSuppressedAtMs: Long? get() = prefs.getLong(KEY_LAST_SUPPRESSED_AT, 0).takeIf { it > 0 }

  val unreadableCount: Int get() = prefs.getInt(KEY_UNREADABLE, 0)

  /** Why the last message notification showed less than the message; `null`: it showed it all. */
  val maybeLastGap: NotificationGap?
    get() = prefs.getString(KEY_LAST_GAP, null)?.let { name -> NotificationGap.entries.firstOrNull { it.name == name } }

  fun recordPreview(maybeGap: NotificationGap?) {
    // The reason only, never what the message said.
    maybeGap?.let { Log.i(TAG, "message notification shows less: $it") }
    prefs.edit().putString(KEY_LAST_GAP, maybeGap?.name).apply()
  }

  fun recordUnreadable() {
    prefs.edit().putInt(KEY_UNREADABLE, unreadableCount + 1).apply()
  }

  fun recordIgnored(reason: IgnoreReason) {
    val editor = prefs.edit()
    when (reason) {
      IgnoreReason.SUPPRESSED ->
        editor
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
    const val KEY_UNREADABLE = "unreadable"
    const val KEY_LAST_GAP = "last_gap"
    const val TAG = "TeamCraftPush"
  }
}
