package de.zeitgleich.teamcraft.ui.screens

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.User
import uniffi.teamcraft_ffi.NotifiedConversation
import uniffi.teamcraft_ffi.NotifiedMessage

/** The message a notification showed is a feed message like the others until the store has it (#1371). */
class NotifiedMessageTest {
  @Test
  fun theNotifiedMessageIsAFeedMessageAtItsTime() {
    val feed = withNotified(listOf(stored("1", sentAt = 100), stored("3", sentAt = 300)), notified("2", sentAt = 200))

    assertEquals(listOf("1", "2", "3"), feed.map { it.id })
    val message = feed[1]
    assertEquals(CONVERSATION, message.conversation)
    assertEquals("user-7", message.author)
    assertEquals("Shift report", message.body)
    assertNull(message.maybePlaceholder)
  }

  /** The stored one takes its place by ID: no second copy, no jump. */
  @Test
  fun theStoredMessageReplacesIt() {
    val feed = listOf(stored("1", sentAt = 100), stored("2", sentAt = 200))
    assertSame(feed, withNotified(feed, notified("2", sentAt = 200)))
  }

  /** A source's message shows as the bot it is, with the source's name. */
  @Test
  fun aSourceKeepsItsName() {
    val feed = withNotified(emptyList(), notified("2", sentAt = 200, isSource = true))
    assertEquals("Hotline", feed.single().maybeSource?.name)
  }

  @Test
  fun nothingToShowLeavesTheFeedAlone() {
    val feed = listOf(stored("1", sentAt = 100))
    assertSame(feed, withNotified(feed, null))
    assertSame(feed, withNotified(feed, notified("2", sentAt = 200, text = null)))
    // A thread reply isn't in the conversation's feed.
    assertSame(feed, withNotified(feed, notified("2", sentAt = 200, thread = "9")))
  }

  /** Without the snapshot the feed draws nothing, not bubbles without names (#1425). */
  @Test
  fun theFeedWaitsForTheSnapshot() {
    val feed = listOf(stored("1", sentAt = 100))
    assertTrue(drawnMessages(feed, notified("2", sentAt = 200), maybeSnapshot = null).isEmpty())
    val snapshot = ChatSnapshot(me = User(id = "user-1", username = "ann", displayName = "Ann"))
    assertEquals(listOf("1", "2"), drawnMessages(feed, notified("2", sentAt = 200), snapshot).map { it.id })
  }

  private fun stored(id: String, sentAt: Long) = Message(id = id, conversation = CONVERSATION, author = "user-7", sentAt = sentAt, body = "text $id")

  private fun notified(
    id: String,
    sentAt: Long,
    text: String? = "Shift report",
    isSource: Boolean = false,
    thread: String? = null,
  ) = NotifiedConversation(
    conversationId = CONVERSATION,
    maybeName = "Project A",
    isDirect = false,
    maybeMessage = NotifiedMessage(
      messageId = id,
      maybeSenderId = "user-7",
      maybeSenderName = "Hotline",
      isSenderSource = isSource,
      maybeText = text,
      maybeFileName = null,
      maybeThreadId = thread,
      sentAtMs = sentAt,
    ),
  )

  private companion object {
    const val CONVERSATION = "4242"
  }
}
