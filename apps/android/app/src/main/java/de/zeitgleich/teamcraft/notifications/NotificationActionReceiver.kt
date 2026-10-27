package de.zeitgleich.teamcraft.notifications

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.app.RemoteInput
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.ConversationNotificationSettings
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
    if (intent.action == ACTION_DISMISSED) {
      // Swiped away: the next message starts a fresh notification.
      graph.notifier.dismiss(conversationId)
      return
    }
    val pending = goAsync()
    CoroutineScope(Dispatchers.Default).launch {
      try {
        when (intent.action) {
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
          ACTION_APPROVE_JOIN, ACTION_DECLINE_JOIN -> {
            val user = intent.getStringExtra(EXTRA_USER_ID) ?: return@launch
            val tag = intent.getStringExtra(EXTRA_TAG) ?: return@launch
            val step = if (intent.action == ACTION_APPROVE_JOIN) JoinStep.Approve else JoinStep.Decline
            graph.joinRequestNotifier.decide(graph.core, step, conversationId, user, tag)
          }
        }
      } catch (failure: Exception) {
        Log.w(TAG, "notification action failed: ${failure.javaClass.simpleName}")
      } finally {
        pending.finish()
      }
    }
  }

  companion object {
    const val ACTION_REPLY = "de.zeitgleich.teamcraft.REPLY"
    const val ACTION_MARK_READ = "de.zeitgleich.teamcraft.MARK_READ"
    const val ACTION_MUTE = "de.zeitgleich.teamcraft.MUTE"
    const val ACTION_DISMISSED = "de.zeitgleich.teamcraft.DISMISSED"
    const val ACTION_APPROVE_JOIN = "de.zeitgleich.teamcraft.APPROVE_JOIN"
    const val ACTION_DECLINE_JOIN = "de.zeitgleich.teamcraft.DECLINE_JOIN"
    const val KEY_REPLY = "reply"
    private const val EXTRA_CONVERSATION_ID = "conversation_id"
    private const val EXTRA_THREAD_ID = "thread_id"
    private const val EXTRA_USER_ID = "user_id"
    private const val EXTRA_TAG = "tag"
    private const val MUTE_DURATION_MS = 8L * 60 * 60 * 1000
    private const val TAG = "TeamCraftNotify"

    fun intent(context: Context, action: String, conversationId: String, maybeThreadId: String? = null): Intent =
      Intent(context, NotificationActionReceiver::class.java)
        .setAction(action)
        .putExtra(EXTRA_CONVERSATION_ID, conversationId)
        .apply { maybeThreadId?.let { putExtra(EXTRA_THREAD_ID, it) } }

    /** Approve or Decline of [request] ([ACTION_APPROVE_JOIN], [ACTION_DECLINE_JOIN]). */
    fun joinRequestIntent(context: Context, action: String, request: JoinRequestItem): Intent =
      intent(context, action, request.conversationId)
        .putExtra(EXTRA_USER_ID, request.userId)
        .putExtra(EXTRA_TAG, request.notificationTag)
  }
}
