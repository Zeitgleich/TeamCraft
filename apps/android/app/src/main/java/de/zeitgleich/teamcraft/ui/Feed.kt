package de.zeitgleich.teamcraft.ui

import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import de.zeitgleich.teamcraft.core.Message

/** Messages of one author this close together are shown as one block (no repeated name). */
const val GROUP_WINDOW_MS = 5 * 60 * 1000L

/** One row of a conversation's message list. */
sealed interface FeedItem {
  val key: String

  /** Which rows share a layout, for the list's reuse of composed rows. */
  val contentType: FeedContentType

  /** A date chip above the first message of a day. */
  data class Day(val date: LocalDate) : FeedItem {
    override val key: String get() = "day-$date"
    override val contentType: FeedContentType get() = FeedContentType.Day
  }

  /** The "new messages" line above the first unread message. */
  data object Unread : FeedItem {
    override val key: String get() = "unread"
    override val contentType: FeedContentType get() = FeedContentType.Unread
  }

  /**
   * A message; `isContinuation` when it follows the same author's previous one closely,
   * `isGroupEnd` when the next one doesn't continue it (the group's last bubble carries the avatar).
   */
  data class Entry(val message: Message, val isContinuation: Boolean, val isGroupEnd: Boolean = true) : FeedItem {
    override val key: String get() = message.id
    override val contentType: FeedContentType
      get() = when {
        message.maybeSystemEvent != null -> FeedContentType.Event
        isContinuation -> FeedContentType.FollowUp
        else -> FeedContentType.Message
      }
  }
}

enum class FeedContentType { Day, Unread, Event, Message, FollowUp }

/** Whether a day's label names its year: only outside the current year (#1086). */
fun dayShowsYear(date: LocalDate, today: LocalDate): Boolean = date.year != today.year

/**
 * Lays out a conversation (oldest first) the way chat apps do: a date chip per day, the unread line
 * before `maybeFirstUnread`, and follow-up messages of the same author grouped under one name and
 * one avatar.
 */
fun feedItems(messages: List<Message>, maybeFirstUnread: String?, zone: ZoneId = ZoneId.systemDefault()): List<FeedItem> {
  val items = ArrayList<FeedItem>(messages.size + 8)
  var maybePrevious: Message? = null
  var maybeDay: LocalDate? = null
  for (message in messages) {
    val day = Instant.ofEpochMilli(message.sentAt).atZone(zone).toLocalDate()
    val isNewDay = day != maybeDay
    if (isNewDay) items += FeedItem.Day(day)
    val isUnreadStart = message.id == maybeFirstUnread
    if (isUnreadStart) items += FeedItem.Unread
    val isContinuation = !isNewDay && !isUnreadStart && maybePrevious?.let { continues(it, message) } == true
    if (isContinuation) {
      // The entry just before is the previous message: no chip or line came between them.
      val previous = items.removeAt(items.lastIndex) as FeedItem.Entry
      items += previous.copy(isGroupEnd = false)
    }
    items += FeedItem.Entry(message, isContinuation)
    maybePrevious = message
    maybeDay = day
  }
  return items
}

private fun continues(previous: Message, message: Message): Boolean =
  previous.author == message.author &&
    previous.maybeSystemEvent == null &&
    message.maybeSystemEvent == null &&
    message.sentAt - previous.sentAt in 0..GROUP_WINDOW_MS

/**
 * The messages among the rows on screen (`visible`, indices into `items`) whose reactions wait
 * unseen (`waiting`), and not already `reported`: they are seen now (issue #1009).
 */
fun reactionsInView(items: List<FeedItem>, visible: List<Int>, waiting: List<String>, reported: Set<String>): List<String> =
  visible
    .mapNotNull { (items.getOrNull(it) as? FeedItem.Entry)?.message?.id }
    .filter { it in waiting && it !in reported }
    .distinct()

/** Where the message is among the rows, or `null` while it isn't loaded. */
fun rowOf(items: List<FeedItem>, messageId: String): Int? =
  items.indexOfFirst { it is FeedItem.Entry && it.message.id == messageId }.takeIf { it >= 0 }
