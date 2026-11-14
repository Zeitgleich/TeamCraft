package de.zeitgleich.teamcraft.notifications

import java.util.concurrent.ConcurrentHashMap
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.ConversationNotificationSettings
import uniffi.teamcraft_ffi.ConversationOutboxItem
import uniffi.teamcraft_ffi.PendingWorkItem
import uniffi.teamcraft_ffi.TeamCraft

/**
 * Reply, mark as read and mute from a message notification, on the phone or in Android Auto
 * (#1972). Each shows its state on the notification right away, so the user sees that something
 * happens and whether it worked:
 *
 * - **Mark as read, mute:** the button says "Marking as read…" and does nothing more while it runs;
 *   the notification goes once the action is done, or stays with "Couldn't mark as read. Tap to
 *   open".
 * - **Reply:** the reply shows as sending (the remote-input spinner) until the server took it, then
 *   as sent; one the server refused shows "Couldn't send. Tap to retry".
 *
 * In the background the core's sync is stopped, so a reply or a read marker only waited in the
 * outbox until the next start or push (an Android Auto reply arrived an hour late). Every action
 * therefore runs the sync until its work is out ([NotificationCore.flush]), and hands what is left
 * to [de.zeitgleich.teamcraft.core.OutboxWork] at once, which sends it when a network is there and
 * then calls [settleReplies].
 */
class NotificationActions(
  private val core: suspend () -> NotificationCore,
  private val status: ActionStatusSink,
  /** Schedules [de.zeitgleich.teamcraft.core.OutboxWork] (expedited, network constraint, backoff). */
  private val enqueueOutbox: () -> Unit,
) {
  /**
   * Conversations whose reply still shows as sending, with how many of its messages the server had
   * refused before it: one more refused means this reply failed.
   */
  private val sendingReplies = ConcurrentHashMap<String, UInt>()

  suspend fun reply(conversationId: String, maybeThreadId: String?, text: String) {
    val core = core()
    val before = core.conversationOutbox(conversationId)
    // Through the core's outbox: kept across a restart, and retried while the phone is offline.
    core.sendMessage(conversationId, maybeThreadId, text)
    sendingReplies.putIfAbsent(conversationId, before.failed)
    status.showOwnReply(conversationId, text)
    // Scheduled first: the process may go before the flush below is done.
    enqueueOutbox()
    runCatching { core.flush(REPLY_FLUSH_MS) }
    settleReplies(core, isLastTry = false)
  }

  suspend fun markRead(conversationId: String) =
    quickAction(conversationId, ActionStatus.MARKING_READ, ActionStatus.MARK_READ_FAILED) { core ->
      core.markRead(conversationId)
      // Queued in the core's store, so it survives the process; sent now, or by the outbox work.
      enqueueOutbox()
      runCatching { core.flush(ACTION_FLUSH_MS) }
    }

  suspend fun mute(conversationId: String) =
    quickAction(conversationId, ActionStatus.MUTING, ActionStatus.MUTE_FAILED) { core -> core.mute(conversationId) }

  /**
   * Shows how the replies still sending ended: sent once none of the conversation's messages waits
   * any more, failed once the server refused one. `isLastTry`: nothing sends them before the app
   * opens again (the outbox work gave up), so those still waiting show as failed too.
   */
  suspend fun settleReplies(isLastTry: Boolean) {
    if (sendingReplies.isEmpty()) return
    settleReplies(core(), isLastTry)
  }

  private suspend fun settleReplies(core: NotificationCore, isLastTry: Boolean) {
    for ((conversationId, failedBefore) in sendingReplies.entries.toList()) {
      val outbox = runCatching { core.conversationOutbox(conversationId) }.getOrNull() ?: continue
      when (replyOutcome(failedBefore, outbox, isLastTry)) {
        ReplyOutcome.SENT -> {
          sendingReplies.remove(conversationId)
          status.clearActionStatus(conversationId)
        }
        ReplyOutcome.FAILED -> {
          sendingReplies.remove(conversationId)
          status.showActionStatus(conversationId, ActionStatus.REPLY_FAILED)
        }
        ReplyOutcome.SENDING -> Unit
      }
    }
  }

  private suspend fun quickAction(
    conversationId: String,
    pending: ActionStatus,
    failed: ActionStatus,
    act: suspend (NotificationCore) -> Unit,
  ) {
    status.showActionStatus(conversationId, pending)
    try {
      act(core())
      status.dismiss(conversationId)
    } catch (failure: Exception) {
      status.showActionStatus(conversationId, failed)
      throw failure
    }
  }

  companion object {
    /**
     * How long a reply keeps the sync running in the receiver's process. The broadcast itself ends
     * sooner ([de.zeitgleich.teamcraft.push.WakeUpWork.RECEIVER_BUDGET_MS]); the outbox work sends
     * what is left if the process goes.
     */
    const val REPLY_FLUSH_MS = 20_000L

    /** Mark as read shows "Marking as read…" this long at most when the server can't be reached. */
    const val ACTION_FLUSH_MS = 10_000L

    /** How a reply stands, from its conversation's outbox now and the refused count before it. */
    fun replyOutcome(failedBefore: UInt, outbox: ConversationOutboxItem, isLastTry: Boolean): ReplyOutcome = when {
      outbox.failed > failedBefore -> ReplyOutcome.FAILED
      outbox.sending == 0u -> ReplyOutcome.SENT
      isLastTry -> ReplyOutcome.FAILED
      else -> ReplyOutcome.SENDING
    }
  }
}

enum class ReplyOutcome { SENDING, SENT, FAILED }

/** What [NotificationActions] needs of the core; [CoreNotificationActions] is the real one. */
interface NotificationCore {
  suspend fun conversationOutbox(conversationId: String): ConversationOutboxItem

  suspend fun sendMessage(conversationId: String, maybeThreadId: String?, body: String)

  suspend fun markRead(conversationId: String)

  /** Quiet for a while, through the server's per-conversation setting (all devices follow). */
  suspend fun mute(conversationId: String)

  /** Runs the sync until nothing waits for the server or `maxWaitMs` passed. */
  suspend fun flush(maxWaitMs: Long): PendingWorkItem
}

/** Where an action's state shows: the conversation's notification ([MessageNotifier]). */
interface ActionStatusSink {
  /** Adds the user's reply to the notification, as sending. */
  fun showOwnReply(conversationId: String, reply: CharSequence)

  fun showActionStatus(conversationId: String, status: ActionStatus)

  fun clearActionStatus(conversationId: String)

  /** The action is done: the notification goes. */
  fun dismiss(conversationId: String)
}

/** [NotificationCore] on the app's core; mute goes through the server's per-conversation setting. */
class CoreNotificationActions(private val core: TeamCraft) : NotificationCore {
  override suspend fun conversationOutbox(conversationId: String): ConversationOutboxItem = core.conversationOutbox(conversationId)

  override suspend fun sendMessage(conversationId: String, maybeThreadId: String?, body: String) =
    core.sendMessage(conversationId, maybeThreadId, body)

  override suspend fun markRead(conversationId: String) = core.markRead(conversationId)

  override suspend fun mute(conversationId: String) {
    val chat = ChatClient { core }
    val current = chat.conversationNotifications(conversationId)
    val mutedUntil = System.currentTimeMillis() + MUTE_DURATION_MS
    chat.setConversationNotifications(conversationId, ConversationNotificationSettings(level = current.level, mutedUntil = mutedUntil))
  }

  override suspend fun flush(maxWaitMs: Long): PendingWorkItem = core.flushPendingWork(maxWaitMs.toULong())

  private companion object {
    const val MUTE_DURATION_MS = 8L * 60 * 60 * 1000
  }
}
