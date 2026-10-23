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

  /** A date chip above the first message of a day. */
  data class Day(val date: LocalDate) : FeedItem {
    override val key: String get() = "day-$date"
  }

  /** The "new messages" line above the first unread message. */
  data object Unread : FeedItem {
    override val key: String get() = "unread"
  }

  /** A message; `isContinuation` when it follows the same author's previous one closely. */
  data class Entry(val message: Message, val isContinuation: Boolean) : FeedItem {
    override val key: String get() = message.id
  }
}

/**
 * Lays out a conversation (oldest first) the way chat apps do: a date chip per day, the unread line
 * before `maybeFirstUnread`, and follow-up messages of the same author grouped under one name.
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
