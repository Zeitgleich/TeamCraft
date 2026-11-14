package de.zeitgleich.teamcraft.notifications

import de.zeitgleich.teamcraft.notifications.MessageNotificationSpecTest.Companion.CONVERSATION
import de.zeitgleich.teamcraft.notifications.MessageNotificationSpecTest.Companion.LABELS
import de.zeitgleich.teamcraft.notifications.MessageNotificationSpecTest.Companion.content
import de.zeitgleich.teamcraft.notifications.MessageNotificationSpecTest.Companion.direct
import de.zeitgleich.teamcraft.notifications.MessageNotificationSpecTest.Companion.line
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationOutboxItem
import uniffi.teamcraft_ffi.NotificationContent
import uniffi.teamcraft_ffi.PendingWorkItem

class NotificationActionsTest {
  @Test
  fun aReplyWhileTheCoreIsStoppedIsHandedToTheOutboxWorkAtOnce() = runTest {
    // The sync is stopped and the phone offline: the flush ends with the reply still queued.
    val core = FakeCore(sendsGoOut = false)
    val sink = RecordingSink()
    var enqueued = 0
    val actions = NotificationActions({ core }, sink, enqueueOutbox = { enqueued++ })

    actions.reply(CONVERSATION, null, "on my way")

    assertEquals(listOf("on my way"), core.sent)
    assertEquals("scheduled right after the send", 1, enqueued)
    assertEquals("the sync ran for it", listOf(NotificationActions.REPLY_FLUSH_MS), core.flushes)
    assertEquals(listOf("reply:on my way"), sink.events)
  }

  @Test
  fun aReplyShowsAsSentOnceItWentOut() = runTest {
    val core = FakeCore(sendsGoOut = true)
    val sink = RecordingSink()
    NotificationActions({ core }, sink, enqueueOutbox = {}).reply(CONVERSATION, null, "on my way")
    assertEquals(listOf("reply:on my way", "clear"), sink.events)
  }

  @Test
  fun aReplyTheServerRefusedShowsAsFailed() = runTest {
    val core = FakeCore(sendsGoOut = true, refusesSends = true)
    val sink = RecordingSink()
    NotificationActions({ core }, sink, enqueueOutbox = {}).reply(CONVERSATION, null, "on my way")
    assertEquals(listOf("reply:on my way", "status:REPLY_FAILED"), sink.events)
  }

  @Test
  fun aReplyStillQueuedSettlesWhenTheOutboxWorkSentIt() = runTest {
    val core = FakeCore(sendsGoOut = false)
    val sink = RecordingSink()
    val actions = NotificationActions({ core }, sink, enqueueOutbox = {})
    actions.reply(CONVERSATION, null, "on my way")

    core.sendQueued()
    actions.settleReplies(isLastTry = false)
    assertEquals(listOf("reply:on my way", "clear"), sink.events)

    actions.settleReplies(isLastTry = false)
    assertEquals("settled once", 2, sink.events.size)
  }

  @Test
  fun aReplyTheOutboxWorkGaveUpOnShowsAsFailed() = runTest {
    val core = FakeCore(sendsGoOut = false)
    val sink = RecordingSink()
    val actions = NotificationActions({ core }, sink, enqueueOutbox = {})
    actions.reply(CONVERSATION, null, "on my way")
    actions.settleReplies(isLastTry = false)
    assertEquals("still sending", listOf("reply:on my way"), sink.events)
    actions.settleReplies(isLastTry = true)
    assertEquals(listOf("reply:on my way", "status:REPLY_FAILED"), sink.events)
  }

  @Test
  fun markAsReadShowsItRunsAndTheNotificationGoesOnlyOnceDone() = runTest {
    val core = FakeCore(sendsGoOut = true)
    val sink = RecordingSink()
    var enqueued = 0
    NotificationActions({ core }, sink, enqueueOutbox = { enqueued++ }).markRead(CONVERSATION)
    assertEquals(listOf("status:MARKING_READ", "dismiss"), sink.events)
    assertEquals(listOf(CONVERSATION), core.read)
    assertEquals("the read marker goes out also with the sync stopped", 1, enqueued)
    assertEquals(listOf(NotificationActions.ACTION_FLUSH_MS), core.flushes)
  }

  @Test
  fun aFailedMarkAsReadOrMuteStaysAndSaysSo() = runTest {
    val core = FakeCore(sendsGoOut = true, failsActions = true)
    val sink = RecordingSink()
    val actions = NotificationActions({ core }, sink, enqueueOutbox = {})
    runCatching { actions.markRead(CONVERSATION) }
    runCatching { actions.mute(CONVERSATION) }
    assertEquals(
      listOf("status:MARKING_READ", "status:MARK_READ_FAILED", "status:MUTING", "status:MUTE_FAILED"),
      sink.events,
    )
  }

  @Test
  fun aRunningActionsButtonDoesNothingAndAFailedOneSaysWhat() {
    val entry = direct(line("1000", "Alice", "lunch?"), NotificationLine(text = "on my way", sentAtMs = 2000))
    fun spec(status: ActionStatus?) = MessageNotificationSpecs.conversation(entry, content(), false, LABELS, maybeActionStatus = status)!!

    assertNull(spec(null).maybeRunningAction)
    assertEquals(MessageAction.MARK_READ, spec(ActionStatus.MARKING_READ).maybeRunningAction)
    assertEquals(MessageAction.MUTE, spec(ActionStatus.MUTING).maybeRunningAction)
    assertNull("another reply may go while one sends", spec(ActionStatus.SENDING_REPLY).maybeRunningAction)

    val sending = spec(ActionStatus.SENDING_REPLY)
    assertTrue(sending.isReplySending)
    assertEquals("on my way", sending.maybeOwnReply)

    assertEquals(MessageAction.MARK_READ, spec(ActionStatus.MARK_READ_FAILED).maybeFailedAction)
    assertEquals(MessageAction.REPLY, spec(ActionStatus.REPLY_FAILED).maybeFailedAction)
    assertFalse(spec(ActionStatus.REPLY_FAILED).isReplySending)
    assertNull(spec(ActionStatus.REPLY_FAILED).maybeRunningAction)
  }

  @Test
  fun aNotificationWithoutActionsShowsNoState() {
    val entry = direct(line("1000", "Alice", "lunch?"))
    val hidden = MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = true, isVerified = true)
    val spec = MessageNotificationSpecs.conversation(entry, hidden, false, LABELS, maybeActionStatus = ActionStatus.MARKING_READ)!!
    assertNull(spec.maybeActionStatus)
  }

  @Test
  fun aReplysOutcomeFollowsItsConversationsOutbox() {
    fun outcome(sending: UInt, failed: UInt, isLastTry: Boolean = false) =
      NotificationActions.replyOutcome(failedBefore = 1u, ConversationOutboxItem(sending, failed), isLastTry)
    assertEquals(ReplyOutcome.SENDING, outcome(sending = 1u, failed = 1u))
    assertEquals(ReplyOutcome.SENT, outcome(sending = 0u, failed = 1u))
    assertEquals("an older failure is not this reply's", ReplyOutcome.SENT, outcome(sending = 0u, failed = 1u, isLastTry = true))
    assertEquals(ReplyOutcome.FAILED, outcome(sending = 0u, failed = 2u))
    assertEquals(ReplyOutcome.FAILED, outcome(sending = 1u, failed = 1u, isLastTry = true))
  }

  /** The core's outbox for one conversation: replies are queued, then go out or are refused. */
  private class FakeCore(
    private val sendsGoOut: Boolean,
    private val refusesSends: Boolean = false,
    private val failsActions: Boolean = false,
  ) : NotificationCore {
    val sent = mutableListOf<String>()
    val read = mutableListOf<String>()
    val flushes = mutableListOf<Long>()
    private var queued = 0u
    private var refused = 0u

    fun sendQueued() {
      queued = 0u
    }

    override suspend fun conversationOutbox(conversationId: String) = ConversationOutboxItem(sending = queued, failed = refused)

    override suspend fun sendMessage(conversationId: String, maybeThreadId: String?, body: String) {
      sent += body
      queued++
    }

    override suspend fun markRead(conversationId: String) {
      if (failsActions) error("store busy")
      read += conversationId
    }

    override suspend fun mute(conversationId: String) {
      if (failsActions) error("offline")
    }

    override suspend fun flush(maxWaitMs: Long): PendingWorkItem {
      flushes += maxWaitMs
      if (sendsGoOut) {
        if (refusesSends) refused += queued
        queued = 0u
      }
      return PendingWorkItem(sends = queued, uploads = 0u, actions = 0u, isEmpty = queued == 0u)
    }
  }

  private class RecordingSink : ActionStatusSink {
    val events = mutableListOf<String>()

    override fun showOwnReply(conversationId: String, reply: CharSequence) {
      events += "reply:$reply"
    }

    override fun showActionStatus(conversationId: String, status: ActionStatus) {
      events += "status:$status"
    }

    override fun clearActionStatus(conversationId: String) {
      events += "clear"
    }

    override fun dismiss(conversationId: String) {
      events += "dismiss"
    }
  }
}
