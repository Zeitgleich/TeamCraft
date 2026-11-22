package de.zeitgleich.teamcraft.notifications

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log
import androidx.core.app.RemoteInput
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.push.WakeUpWork
import de.zeitgleich.teamcraft.ui.JoinStep
import uniffi.teamcraft_ffi.JoinRequestItem
import uniffi.teamcraft_ffi.LinkArrival

/**
 * "Reply", "mark as read" and "mute" from a notification ([NotificationActions]), on the phone or in Android Auto, the
 * notification being swiped away, Approve or Decline of an admin's request to join, and "Remove" on somebody let
 * into a meeting without a click.
 */
class NotificationActionReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    val conversationId = intent.getStringExtra(EXTRA_CONVERSATION_ID) ?: return
    val graph = TeamCraftApplication.graph(context)
    val pending = goAsync()
    // On the process's scope, and the broadcast ends after [WakeUpWork.RECEIVER_BUDGET_MS] at most:
    // a core waiting behind a busy store kept the broadcast open until Android declared the app not
    // responding and killed it (a background ANR, with nothing on the main thread). The action goes
    // on without the broadcast while the process lives. A swipe too: the notifier's lock can wait.
    // The action keeps going after the broadcast's budget: a reply runs the sync until it is out.
    val work = graph.scope.launch { act(context, intent, conversationId) }
    graph.scope.launch {
      try {
        withTimeoutOrNull(WakeUpWork.RECEIVER_BUDGET_MS) { work.join() }
      } finally {
        pending.finish()
      }
    }
  }

  private suspend fun act(context: Context, intent: Intent, conversationId: String) {
    val graph = TeamCraftApplication.graph(context)
    try {
      when (intent.action) {
        // Swiped away: the next message starts a fresh notification.
        ACTION_DISMISSED -> graph.notifier.dismiss(conversationId)
        // Each shows its state on the notification at once and runs the sync until it is out
        // (#1972): in the background the sync is stopped, so a reply only waited in the outbox.
        ACTION_REPLY -> {
          val reply = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(KEY_REPLY)
          if (!reply.isNullOrBlank()) {
            graph.notificationActions.reply(conversationId, intent.getStringExtra(EXTRA_THREAD_ID), reply.toString())
          }
        }
        ACTION_MUTE -> graph.notificationActions.mute(conversationId)
        ACTION_MARK_READ ->
          graph.notificationActions.markRead(conversationId, intent.getStringExtra(EXTRA_THROUGH_MESSAGE_ID))
        ACTION_MEETING_DISMISSED -> {
          val meetingId = intent.getStringExtra(EXTRA_MEETING_ID) ?: return
          val startsAtMs = intent.getLongExtra(EXTRA_MEETING_STARTS_AT_MS, -1L).takeIf { it >= 0 } ?: return
          MeetingNoticeReport.send(ChatClient { graph.core.core() }, meetingId, startsAtMs)
        }
        ACTION_APPROVE_JOIN, ACTION_DECLINE_JOIN -> {
          val user = intent.getStringExtra(EXTRA_USER_ID) ?: return
          val tag = intent.getStringExtra(EXTRA_TAG) ?: return
          val step = if (intent.action == ACTION_APPROVE_JOIN) JoinStep.Approve else JoinStep.Decline
          graph.joinRequestNotifier.decide(graph.core, step, conversationId, user, tag)
        }
        ACTION_REMOVE_LINK_ARRIVAL -> {
          val arrival = linkArrival(intent, conversationId) ?: return
          graph.linkArrivalNotifier.remove(graph.core, arrival)
        }
      }
    } catch (failure: Exception) {
      Log.w(TAG, "notification action failed: ${failure.javaClass.simpleName}")
    }
  }

  companion object {
    const val ACTION_REPLY = "de.zeitgleich.teamcraft.REPLY"
    const val ACTION_MARK_READ = "de.zeitgleich.teamcraft.MARK_READ"
    const val ACTION_MUTE = "de.zeitgleich.teamcraft.MUTE"
    const val ACTION_DISMISSED = "de.zeitgleich.teamcraft.DISMISSED"
    /** A meeting's notice was swiped away: reported, so the user's other phones take theirs down. */
    const val ACTION_MEETING_DISMISSED = "de.zeitgleich.teamcraft.MEETING_DISMISSED"
    const val ACTION_APPROVE_JOIN = "de.zeitgleich.teamcraft.APPROVE_JOIN"
    const val ACTION_DECLINE_JOIN = "de.zeitgleich.teamcraft.DECLINE_JOIN"
    /** "Remove" on somebody let into a meeting by this device without a click (#2258). */
    const val ACTION_REMOVE_LINK_ARRIVAL = "de.zeitgleich.teamcraft.REMOVE_LINK_ARRIVAL"
    const val KEY_REPLY = "reply"
    private const val EXTRA_CONVERSATION_ID = "conversation_id"
    private const val EXTRA_THREAD_ID = "thread_id"
    /** The newest message a "Mark as read" notification shows (#2079). */
    const val EXTRA_THROUGH_MESSAGE_ID = "through_message_id"
    private const val EXTRA_USER_ID = "user_id"
    private const val EXTRA_TAG = "tag"
    private const val EXTRA_MEETING_ID = "meeting_id"
    private const val EXTRA_MEETING_STARTS_AT_MS = "meeting_starts_at_ms"
    private const val EXTRA_MEETING_TITLE = "meeting_title"
    private const val EXTRA_DISPLAY_NAME = "display_name"
    private const val TAG = "TeamCraftNotify"

    fun intent(
      context: Context,
      action: String,
      conversationId: String,
      maybeThreadId: String? = null,
      maybeThroughMessageId: String? = null,
    ): Intent =
      Intent(context, NotificationActionReceiver::class.java)
        .setAction(action)
        .putExtra(EXTRA_CONVERSATION_ID, conversationId)
        .apply {
          maybeThreadId?.let { putExtra(EXTRA_THREAD_ID, it) }
          maybeThroughMessageId?.let { putExtra(EXTRA_THROUGH_MESSAGE_ID, it) }
        }

    /** The meeting notice of [meetingId] (the occurrence starting at [startsAtMs]) was swiped away. */
    fun meetingDismissedIntent(context: Context, conversationId: String, meetingId: String, startsAtMs: Long): Intent =
      intent(context, ACTION_MEETING_DISMISSED, conversationId)
        .setData(Uri.parse("teamcraft-meeting-notice://$meetingId/$startsAtMs"))
        .putExtra(EXTRA_MEETING_ID, meetingId)
        .putExtra(EXTRA_MEETING_STARTS_AT_MS, startsAtMs)

    /** "Remove" on [arrival] ([ACTION_REMOVE_LINK_ARRIVAL]); it carries what shows it again on a failure. */
    fun linkArrivalIntent(context: Context, arrival: LinkArrival): Intent =
      intent(context, ACTION_REMOVE_LINK_ARRIVAL, arrival.conversationId)
        .putExtra(EXTRA_MEETING_ID, arrival.meetingId)
        .putExtra(EXTRA_MEETING_TITLE, arrival.meetingTitle)
        .putExtra(EXTRA_USER_ID, arrival.userId)
        .putExtra(EXTRA_DISPLAY_NAME, arrival.displayName)

    private fun linkArrival(intent: Intent, conversationId: String): LinkArrival? {
      val meetingId = intent.getStringExtra(EXTRA_MEETING_ID) ?: return null
      val userId = intent.getStringExtra(EXTRA_USER_ID) ?: return null
      return LinkArrival(
        meetingId = meetingId,
        meetingTitle = intent.getStringExtra(EXTRA_MEETING_TITLE).orEmpty(),
        conversationId = conversationId,
        userId = userId,
        displayName = intent.getStringExtra(EXTRA_DISPLAY_NAME).orEmpty(),
      )
    }

    /** Approve or Decline of [request] ([ACTION_APPROVE_JOIN], [ACTION_DECLINE_JOIN]). */
    fun joinRequestIntent(context: Context, action: String, request: JoinRequestItem): Intent =
      intent(context, action, request.conversationId)
        .putExtra(EXTRA_USER_ID, request.userId)
        .putExtra(EXTRA_TAG, request.notificationTag)
  }
}
