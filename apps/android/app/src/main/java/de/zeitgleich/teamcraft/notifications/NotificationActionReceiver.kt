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
import de.zeitgleich.teamcraft.core.ConversationNotificationSettings
import de.zeitgleich.teamcraft.push.WakeUpWork
import de.zeitgleich.teamcraft.ui.JoinStep
import uniffi.teamcraft_ffi.JoinRequestItem

/**
 * "Reply" and "mark as read" from a notification, on the phone or in Android Auto, the
 * notification being swiped away, and Approve or Decline of an admin's request to join.
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
        ACTION_REPLY -> {
          val reply = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(KEY_REPLY)
          if (!reply.isNullOrBlank()) {
            // Through the core's outbox: queued and retried if the phone is offline.
            graph.core.core().sendMessage(conversationId, intent.getStringExtra(EXTRA_THREAD_ID), reply.toString())
            graph.notifier.showOwnReply(conversationId, reply)
          }
        }
        ACTION_MUTE -> {
          // Quiet for a while through the server's per-conversation setting (all devices follow).
          val mutedUntil = System.currentTimeMillis() + MUTE_DURATION_MS
          val chat = ChatClient { graph.core.core() }
          val current = chat.conversationNotifications(conversationId)
          chat.setConversationNotifications(
            conversationId,
            ConversationNotificationSettings(level = current.level, mutedUntil = mutedUntil),
          )
          graph.notifier.dismiss(conversationId)
        }
        ACTION_MARK_READ -> {
          graph.core.core().markRead(conversationId)
          graph.notifier.dismiss(conversationId)
        }
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
    const val KEY_REPLY = "reply"
    private const val EXTRA_CONVERSATION_ID = "conversation_id"
    private const val EXTRA_THREAD_ID = "thread_id"
    private const val EXTRA_USER_ID = "user_id"
    private const val EXTRA_TAG = "tag"
    private const val EXTRA_MEETING_ID = "meeting_id"
    private const val EXTRA_MEETING_STARTS_AT_MS = "meeting_starts_at_ms"
    private const val MUTE_DURATION_MS = 8L * 60 * 60 * 1000
    private const val TAG = "TeamCraftNotify"

    fun intent(context: Context, action: String, conversationId: String, maybeThreadId: String? = null): Intent =
      Intent(context, NotificationActionReceiver::class.java)
        .setAction(action)
        .putExtra(EXTRA_CONVERSATION_ID, conversationId)
        .apply { maybeThreadId?.let { putExtra(EXTRA_THREAD_ID, it) } }

    /** The meeting notice of [meetingId] (the occurrence starting at [startsAtMs]) was swiped away. */
    fun meetingDismissedIntent(context: Context, conversationId: String, meetingId: String, startsAtMs: Long): Intent =
      intent(context, ACTION_MEETING_DISMISSED, conversationId)
        .setData(Uri.parse("teamcraft-meeting-notice://$meetingId/$startsAtMs"))
        .putExtra(EXTRA_MEETING_ID, meetingId)
        .putExtra(EXTRA_MEETING_STARTS_AT_MS, startsAtMs)

    /** Approve or Decline of [request] ([ACTION_APPROVE_JOIN], [ACTION_DECLINE_JOIN]). */
    fun joinRequestIntent(context: Context, action: String, request: JoinRequestItem): Intent =
      intent(context, action, request.conversationId)
        .putExtra(EXTRA_USER_ID, request.userId)
        .putExtra(EXTRA_TAG, request.notificationTag)
  }
}
