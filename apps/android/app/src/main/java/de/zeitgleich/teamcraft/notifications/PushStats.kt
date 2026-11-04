package de.zeitgleich.teamcraft.notifications

import android.content.Context
import android.util.Log
import uniffi.teamcraft_ffi.NotificationGap
import uniffi.teamcraft_ffi.PushNotification

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

  /** Counts a path a wake-up took, and logs the counts so far: how often each path is taken. */
  fun record(path: PushPath) {
    val key = KEY_PATH_PREFIX + path.name
    prefs.edit().putInt(key, prefs.getInt(key, 0) + 1).apply()
    Log.i(TAG, "notification path ${path.name}; ${summary()}")
  }

  /** The counts by path, e.g. `TEXT=12 HIDDEN=3`, for the log and the diagnostics export. */
  fun summary(): String = PushPath.entries
    .mapNotNull { path -> prefs.getInt(KEY_PATH_PREFIX + path.name, 0).takeIf { it > 0 }?.let { "${path.name}=$it" } }
    .joinToString(" ")

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
    const val KEY_PATH_PREFIX = "path_"
    const val TAG = "TeamCraftPush"
  }
}

/**
 * Every way a wake-up ends up in a notification (issue #1249), counted so that "New message" can be
 * told apart by cause on a real phone, with no content in it.
 */
enum class PushPath {
  /** Showed what was written, or a file. */
  SHOWN,

  /** A policy or setting shows less: as chosen. */
  LIMITED,

  /** Read before it was opened, or before a later look: says nothing. */
  ALREADY_READ,

  /** A kind this version doesn't know, from a newer client: says "New message", phone only. */
  NOT_SHOWABLE,

  /** A reaction, an edit or another change: says nothing. */
  HIDDEN,

  /** Placeholders by why the first try could not read the message. */
  PLACEHOLDER_NO_SESSION,
  PLACEHOLDER_NOT_DECRYPTED,
  PLACEHOLDER_TIMED_OUT,

  /** How a placeholder got its text: the try right away, a later retry, the sync. */
  FILLED_HOLD,
  FILLED_RETRY,
  FILLED_STORE,

  /** The hold ended without the text: the phone alerted for the placeholder. */
  ALERT_RELEASED,

  /** The last retry did not read it either; what the sync brings can still fill it. */
  GAVE_UP,

  /** Too many chains of retries waited already: this wake-up got none. */
  RETRY_CAPPED,

  /** The wake-up could not be opened at all: a generic "New message", or nothing when not visible. */
  UNREADABLE,
  UNREADABLE_QUIET,

  /** A wake-up that could not be opened at first opened on a retry. */
  OPENED_LATE,
  ;

  companion object {
    fun of(notification: PushNotification): PushPath = when (notification.maybeGap) {
      null -> SHOWN
      NotificationGap.NO_SESSION -> PLACEHOLDER_NO_SESSION
      NotificationGap.NOT_DECRYPTED -> PLACEHOLDER_NOT_DECRYPTED
      NotificationGap.TIMED_OUT -> PLACEHOLDER_TIMED_OUT
      NotificationGap.NOT_SHOWABLE -> NOT_SHOWABLE
      NotificationGap.HIDDEN -> HIDDEN
      NotificationGap.WORKSPACE_POLICY, NotificationGap.DEVICE_SETTING -> LIMITED
    }
  }
}
