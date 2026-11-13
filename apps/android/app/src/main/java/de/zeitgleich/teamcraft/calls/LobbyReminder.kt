package de.zeitgleich.teamcraft.calls

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.core.CoreRepository
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * After the organizer left a meeting's call from the system's call UI (the call notification,
 * Telecom, the car), where no question can be asked (#1859): if people still wait in the lobby and
 * nobody else can let them in, a notification says so, and its "Hand over" opens
 * [HandoverActivity]. The core decides when (`lobbyLeftWaiting`), as the in-call leave question.
 */
internal object LobbyReminder {
  private const val TAG = "TeamCraftCalls"

  /** The scheme of the reminder intent's data: only this app's explicit intent carries it. */
  private const val LOBBY_LEFT_SCHEME = "teamcraft-lobby-left"

  /** Leaving never waits on this; a slow network just means no reminder. */
  private const val CHECK_TIMEOUT_MILLIS = 15_000L

  suspend fun check(context: Context, core: CoreRepository, conversationId: String) {
    val left = try {
      withTimeoutOrNull(CHECK_TIMEOUT_MILLIS) { core.core().lobbyLeftWaiting(conversationId) }
    } catch (error: TeamCraftException) {
      Log.w(TAG, "could not check the lobby after leaving", error)
      null
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
      null
    } ?: return
    TeamCraftApplication.graph(context).notifier.showLobbyLeftWaiting(
      conversationId,
      left.meetingTitle,
      left.waiting.toInt(),
      handOverIntent(context, conversationId),
    )
  }

  /**
   * The reminder's intent. Its data names the meeting chat, so the system keeps one intent per
   * meeting (intents differing in data are different, whatever the request code): a second
   * meeting's reminder never replaces the first's extras, also across process restarts.
   */
  private fun handOverIntent(context: Context, conversationId: String): PendingIntent =
    PendingIntent.getActivity(
      context,
      0,
      Intent(context, HandoverActivity::class.java)
        .setData(Uri.fromParts(LOBBY_LEFT_SCHEME, conversationId, null))
        .putExtra(HandoverActivity.EXTRA_CONVERSATION_ID, conversationId)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
}
