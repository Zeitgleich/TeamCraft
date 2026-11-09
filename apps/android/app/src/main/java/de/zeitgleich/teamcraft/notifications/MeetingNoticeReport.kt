package de.zeitgleich.teamcraft.notifications

import android.util.Log
import de.zeitgleich.teamcraft.core.ChatClient

/**
 * Tells the server that a meeting's reminder or start notice was dealt with on this phone (Join, or
 * swiped away), so the user's other phones take theirs down with a `dismiss` wake-up naming the
 * meeting (docs/notifications.md, "Meeting notices"). Best effort: a phone that is offline, or an
 * occurrence the server no longer knows, changes nothing here.
 */
object MeetingNoticeReport {
  private const val TAG = "TeamCraftNotify"

  /** `POST /meetings/{id}/notices/dismiss`. */
  fun path(meetingId: String): String = "/meetings/$meetingId/notices/dismiss"

  /** The request body: the start of the occurrence the notice was for. */
  fun body(startsAtMs: Long): String = """{"starts_at":$startsAtMs}"""

  suspend fun send(chat: ChatClient, meetingId: String, startsAtMs: Long) {
    try {
      chat.api("POST", path(meetingId), body(startsAtMs))
    } catch (failure: Exception) {
      Log.w(TAG, "reporting a meeting notice failed: ${failure.javaClass.simpleName}")
    }
  }
}
