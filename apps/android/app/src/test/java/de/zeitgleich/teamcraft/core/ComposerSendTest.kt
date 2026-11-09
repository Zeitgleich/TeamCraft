package de.zeitgleich.teamcraft.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** A composer's send is never silent and a retry is one message (#1579). */
class ComposerSendTest {
  private val photos = SendContent("", listOf("/staging/1-a.jpg", "/staging/2-b.jpg"), emptyList())

  @Test
  fun aSendIsPendingUntilTheCoreHasIt() {
    val send = ComposerSend(MemoryStore(), ids())
    val attempt = send.start(KEY, photos)!!
    assertEquals(ComposerSendState.Sending(attempt), send.state(KEY))
    send.succeeded(KEY, attempt)
    assertEquals(ComposerSendState.Idle, send.state(KEY))
  }

  @Test
  fun aSecondTapWhileSendingSendsNothing() {
    val send = ComposerSend(MemoryStore(), ids())
    send.start(KEY, photos)
    assertNull(send.start(KEY, photos))
  }

  @Test
  fun aFailureStaysWithItsReason() {
    val send = ComposerSend(MemoryStore(), ids())
    val attempt = send.start(KEY, photos)!!
    send.failed(KEY, attempt, REASON)
    assertEquals(ComposerSendState.Failed(attempt, REASON), send.state(KEY))
  }

  @Test
  fun aRetryOfTheSameContentIsTheSameMessage() {
    val send = ComposerSend(MemoryStore(), ids())
    val first = send.start(KEY, photos)!!
    send.failed(KEY, first, REASON)
    val retry = send.start(KEY, photos)!!
    assertEquals(first.clientMessageId, retry.clientMessageId)
    assertEquals(ComposerSendState.Sending(retry), send.state(KEY))
  }

  @Test
  fun changedContentAfterAFailureIsANewMessage() {
    val send = ComposerSend(MemoryStore(), ids())
    val first = send.start(KEY, photos)!!
    send.failed(KEY, first, REASON)
    val changed = send.start(KEY, photos.copy(text = "and a caption"))!!
    assertNotEquals(first.clientMessageId, changed.clientMessageId)
  }

  @Test
  fun aSendCutShortByARestartKeepsItsIdForTheRetry() {
    val store = MemoryStore()
    val first = ComposerSend(store, ids()).start(KEY, photos)!!
    // The process died before the core answered: a new composer state, the same store.
    val afterRestart = ComposerSend(store, ids(start = 100))
    assertEquals(ComposerSendState.Idle, afterRestart.state(KEY))
    assertEquals(first.clientMessageId, afterRestart.start(KEY, photos)!!.clientMessageId)
  }

  @Test
  fun aSentMessageLeavesNothingForTheNextOne() {
    val store = MemoryStore()
    val send = ComposerSend(store, ids())
    val first = send.start(KEY, photos)!!
    send.succeeded(KEY, first)
    assertNull(store.load(KEY))
    // The same pictures sent again on purpose are another message.
    assertNotEquals(first.clientMessageId, send.start(KEY, photos)!!.clientMessageId)
  }

  @Test
  fun aDismissedFailureStillRetriesAsTheSameMessage() {
    val send = ComposerSend(MemoryStore(), ids())
    val first = send.start(KEY, photos)!!
    send.failed(KEY, first, REASON)
    send.dismiss(KEY)
    assertEquals(ComposerSendState.Idle, send.state(KEY))
    assertEquals(first.clientMessageId, send.start(KEY, photos)!!.clientMessageId)
  }

  @Test
  fun aLateAnswerOfAnOlderAttemptChangesNothing() {
    val send = ComposerSend(MemoryStore(), ids())
    val first = send.start(KEY, photos)!!
    send.failed(KEY, first, REASON)
    val other = send.start(KEY, photos.copy(text = "new"))!!
    send.succeeded(KEY, first)
    send.failed(KEY, first, REASON)
    assertEquals(ComposerSendState.Sending(other), send.state(KEY))
  }

  @Test
  fun composersSendOnTheirOwn() {
    val send = ComposerSend(MemoryStore(), ids())
    send.start(KEY, photos)
    assertTrue(send.start("other/", photos) != null)
  }

  @Test
  fun theFingerprintTellsFilesFromText() {
    val asFile = SendContent("", listOf("a"), emptyList()).fingerprint()
    val asText = SendContent("a", emptyList(), emptyList()).fingerprint()
    val asDraft = SendContent("", emptyList(), listOf("a")).fingerprint()
    assertEquals(3, setOf(asFile, asText, asDraft).size)
    assertTrue(asFile.matches(Regex("[0-9a-f]{64}")))
  }

  private fun ids(start: Int = 0): () -> String {
    var next = start
    return { (next++).toString().padStart(32, '0') }
  }

  private class MemoryStore : SendAttemptStore {
    private val attempts = HashMap<String, SendAttempt>()

    override fun load(key: String): SendAttempt? = attempts[key]

    override fun save(key: String, attempt: SendAttempt) {
      attempts[key] = attempt
    }

    override fun clear(key: String) {
      attempts.remove(key)
    }
  }

  private companion object {
    const val KEY = "7/"
    const val REASON = 42
  }
}
