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

/**
 * "Reply" and "mark as read" from a notification, on the phone or in Android Auto, and the
 * notification being swiped away.
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
          ACTION_MARK_READ -> {
            graph.core.core().markRead(conversationId)
            graph.notifier.dismiss(conversationId)
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
    const val ACTION_DISMISSED = "de.zeitgleich.teamcraft.DISMISSED"
    const val KEY_REPLY = "reply"
    private const val EXTRA_CONVERSATION_ID = "conversation_id"
    private const val EXTRA_THREAD_ID = "thread_id"
    private const val TAG = "TeamCraftNotify"

    fun intent(context: Context, action: String, conversationId: String, maybeThreadId: String? = null): Intent =
      Intent(context, NotificationActionReceiver::class.java)
        .setAction(action)
        .putExtra(EXTRA_CONVERSATION_ID, conversationId)
        .apply { maybeThreadId?.let { putExtra(EXTRA_THREAD_ID, it) } }
  }
}
