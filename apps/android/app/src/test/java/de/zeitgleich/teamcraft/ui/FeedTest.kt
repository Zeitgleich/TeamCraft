package de.zeitgleich.teamcraft.ui

import java.time.LocalDate
import java.time.ZoneOffset
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test
import de.zeitgleich.teamcraft.core.Message

class FeedTest {
  @Test
  fun dateChipBeforeTheFirstMessageOfEachDay() {
    val items = feedItems(
      listOf(message("1", "a", DAY_ONE), message("2", "a", DAY_ONE + HOUR), message("3", "a", DAY_TWO)),
      maybeFirstUnread = null,
      zone = ZoneOffset.UTC,
    )
    assertEquals(
      listOf("day-2026-10-21", "1", "2", "day-2026-10-22", "3"),
      items.map { it.key },
    )
    assertEquals(FeedItem.Day(LocalDate.of(2026, 10, 22)), items[3])
  }

  @Test
  fun followUpsOfTheSameAuthorAreGrouped() {
    val items = entries(
      message("1", "a", DAY_ONE),
      message("2", "a", DAY_ONE + MINUTE),
      message("3", "b", DAY_ONE + 2 * MINUTE),
      message("4", "b", DAY_ONE + 2 * MINUTE + GROUP_WINDOW_MS + 1),
    )
    assertEquals(listOf(false, true, false, false), items.map { it.isContinuation })
  }

  @Test
  fun onlyTheLastMessageOfAGroupEndsIt() {
    val items = entries(
      message("1", "a", DAY_ONE),
      message("2", "a", DAY_ONE + MINUTE),
      message("3", "a", DAY_ONE + 2 * MINUTE),
      message("4", "b", DAY_ONE + 3 * MINUTE),
    )
    assertEquals(listOf(false, false, true, true), items.map { it.isGroupEnd })
  }

  @Test
  fun aDayChipEndsTheGroupBeforeIt() {
    // DAY_ONE is 08:00 UTC: one minute before and at midnight.
    val midnight = DAY_ONE + 16 * HOUR
    val items = entries(message("1", "a", midnight - MINUTE), message("2", "a", midnight))
    assertEquals(listOf(true, true), items.map { it.isGroupEnd })
    assertEquals(listOf(false, false), items.map { it.isContinuation })
  }

  @Test
  fun unreadLineBreaksAGroup() {
    val items = feedItems(
      listOf(message("1", "a", DAY_ONE), message("2", "a", DAY_ONE + MINUTE)),
      maybeFirstUnread = "2",
      zone = ZoneOffset.UTC,
    )
    assertEquals(listOf("day-2026-10-21", "1", "unread", "2"), items.map { it.key })
    assertEquals(false, (items[3] as FeedItem.Entry).isContinuation)
  }

  @Test
  fun systemEventsAreNeverGrouped() {
    val joined = message("2", "a", DAY_ONE + MINUTE, kind = JsonObject(mapOf("System" to JsonPrimitive("Joined"))))
    val items = entries(message("1", "a", DAY_ONE), joined, message("3", "a", DAY_ONE + 2 * MINUTE))
    assertEquals(listOf(false, false, false), items.map { it.isContinuation })
  }

  @Test
  fun unknownFirstUnreadAddsNoLine() {
    val items = feedItems(listOf(message("1", "a", DAY_ONE)), maybeFirstUnread = "gone", zone = ZoneOffset.UTC)
    assertEquals(listOf("day-2026-10-21", "1"), items.map { it.key })
  }

  @Test
  fun rowsOfOneLayoutShareAContentType() {
    val joined = message("3", "a", DAY_ONE + 2 * MINUTE, kind = JsonObject(mapOf("System" to JsonPrimitive("Joined"))))
    val items = feedItems(
      listOf(message("1", "a", DAY_ONE), message("2", "a", DAY_ONE + MINUTE), joined),
      maybeFirstUnread = "2",
      zone = ZoneOffset.UTC,
    )
    assertEquals(
      listOf(FeedContentType.Day, FeedContentType.Message, FeedContentType.Unread, FeedContentType.Message, FeedContentType.Event),
      items.map { it.contentType },
    )
    val grouped = feedItems(listOf(message("1", "a", DAY_ONE), message("2", "a", DAY_ONE + MINUTE)), null, ZoneOffset.UTC)
    assertEquals(FeedContentType.FollowUp, grouped.last().contentType)
  }

  @Test
  fun onlyDaysOutsideTheCurrentYearShowTheirYear() {
    val today = LocalDate.of(2026, 10, 22)
    assertEquals(false, dayShowsYear(LocalDate.of(2026, 1, 1), today))
    assertEquals(true, dayShowsYear(LocalDate.of(2025, 12, 31), today))
    assertEquals(true, dayShowsYear(LocalDate.of(2027, 1, 1), today))
    // New Year's Eve: the same day gains its year once the next year has begun.
    val eve = LocalDate.of(2026, 12, 31)
    assertEquals(false, dayShowsYear(eve, eve))
    assertEquals(true, dayShowsYear(eve, eve.plusDays(1)))
  }

  private fun entries(vararg messages: Message): List<FeedItem.Entry> =
    feedItems(messages.toList(), maybeFirstUnread = null, zone = ZoneOffset.UTC).filterIsInstance<FeedItem.Entry>()

  private fun message(id: String, author: String, sentAt: Long, kind: kotlinx.serialization.json.JsonElement = JsonPrimitive("User")) =
    Message(id = id, conversation = "c", author = author, kind = kind, sentAt = sentAt, body = "text $id")

  private companion object {
    const val MINUTE = 60_000L
    const val HOUR = 60 * MINUTE
    /** 2026-10-21 08:00 UTC. */
    const val DAY_ONE = 1_792_569_600_000L
    const val DAY_TWO = DAY_ONE + 24 * HOUR
  }
}
