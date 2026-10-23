package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NotificationLinesTest {
  @Test
  fun linesAreOrderedBySendTimeNotByArrival() {
    val later = line("2000", sentAtMs = 20)
    val earlier = line("1000", sentAtMs = 10)
    val merged = NotificationLines.merge(listOf(later), earlier)
    assertEquals(listOf("1000", "2000"), merged.map { it.maybeMessageId })
  }

  @Test
  fun sameMillisecondIsOrderedByMessageId() {
    val merged = NotificationLines.merge(listOf(line("1002", sentAtMs = 10)), line("1001", sentAtMs = 10))
    assertEquals(listOf("1001", "1002"), merged.map { it.maybeMessageId })
  }

  @Test
  fun aSecondLineForTheSameMessageReplacesTheFirst() {
    val merged = NotificationLines.merge(listOf(line("1000", text = "first")), line("1000", text = "urgent again"))
    assertEquals(listOf("urgent again"), merged.map { it.text })
  }

  @Test
  fun ownRepliesNeverReplaceEachOther() {
    val merged = NotificationLines.merge(listOf(own("one", 10)), own("two", 20))
    assertEquals(listOf("one", "two"), merged.map { it.text })
  }

  @Test
  fun onlyTheNewestLinesAreKept() {
    var lines = emptyList<NotificationLine>()
    for (index in 1..NotificationLines.MAX_LINES + 3) lines = NotificationLines.merge(lines, line("$index", sentAtMs = index.toLong()))
    assertEquals(NotificationLines.MAX_LINES, lines.size)
    assertEquals("${NotificationLines.MAX_LINES + 3}", lines.last().maybeMessageId)
    assertEquals("4", lines.first().maybeMessageId)
  }

  @Test
  fun readingTheConversationRemovesItsLinesUpToTheReadMessage() {
    val lines = listOf(line("100"), line("200"), line("300"))
    val left = NotificationLines.afterRead(lines, maybeThreadId = null, maybeUpToMessageId = "200")
    assertEquals(listOf("300"), left.map { it.maybeMessageId })
  }

  @Test
  fun readingTheConversationKeepsThreadLines() {
    val lines = listOf(line("100"), line("150", threadId = "90"))
    val left = NotificationLines.afterRead(lines, maybeThreadId = null, maybeUpToMessageId = "200")
    assertEquals(listOf("150"), left.map { it.maybeMessageId })
  }

  @Test
  fun readingAThreadRemovesOnlyThatThread() {
    val lines = listOf(line("100"), line("150", threadId = "90"), line("160", threadId = "95"))
    val left = NotificationLines.afterRead(lines, maybeThreadId = "90", maybeUpToMessageId = "200")
    assertEquals(listOf("100", "160"), left.map { it.maybeMessageId })
  }

  @Test
  fun withoutAMessageEverythingOfThatScopeIsRead() {
    val left = NotificationLines.afterRead(listOf(line("100"), line("900")), maybeThreadId = null, maybeUpToMessageId = null)
    assertTrue(left.isEmpty())
  }

  @Test
  fun ownRepliesGoWithTheLastMessageOfSomebodyElse() {
    val lines = listOf(line("100"), own("thanks", 20))
    assertTrue(NotificationLines.afterRead(lines, maybeThreadId = null, maybeUpToMessageId = "100").isEmpty())
  }

  @Test
  fun idsCompareAsNumbersNotAsText() {
    val left = NotificationLines.afterRead(listOf(line("99"), line("1000")), maybeThreadId = null, maybeUpToMessageId = "100")
    assertEquals(listOf("1000"), left.map { it.maybeMessageId })
  }

  @Test
  fun snowflakesCarryTheirSendTime() {
    // 2026-01-01T00:00:01Z: one second after the epoch, worker and sequence bits below.
    val id = (1_000L shl 22) or 5
    assertEquals(1_767_225_601_000L, NotificationLines.sentAtMs(id.toString()))
    assertNull(NotificationLines.sentAtMs("not an id"))
  }

  private fun line(id: String, sentAtMs: Long = id.toLong(), text: String = "text $id", threadId: String? = null) =
    NotificationLine(
      maybeMessageId = id,
      maybeThreadId = threadId,
      maybeSenderKey = "300",
      senderName = "Alice",
      text = text,
      sentAtMs = sentAtMs,
    )

  private fun own(text: String, sentAtMs: Long) = NotificationLine(text = text, sentAtMs = sentAtMs)
}
