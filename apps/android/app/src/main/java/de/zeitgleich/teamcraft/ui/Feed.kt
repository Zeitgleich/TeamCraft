package de.zeitgleich.teamcraft.ui

import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import de.zeitgleich.teamcraft.core.HistoryGap
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
   * History this device hasn't read yet between two messages (issues #1156, #1438): a catch-up
   * after a long absence read the newest messages ahead of the rest, and a reader who comes near
   * asks for them.
   */
  data class Gap(val gap: HistoryGap) : FeedItem {
    override val key: String get() = "gap-${gap.after}"
    override val contentType: FeedContentType get() = FeedContentType.Gap
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

enum class FeedContentType { Day, Unread, Gap, Event, Message, FollowUp }

/** A hole in the history and the message right before it among those a feed shows (`null`: above the first). */
data class PlacedGap(val gap: HistoryGap, val maybeAfterMessage: String?)

/**
 * Places the holes the core counted among [messages] (a [de.zeitgleich.teamcraft.core.MessageTail]'s,
 * `index` messages before each) by the message before them, so a feed that adds a row of its own (the
 * notified message, [de.zeitgleich.teamcraft.ui.screens.withNotified]) keeps them in place.
 */
fun placeGaps(messages: List<Message>, gaps: List<HistoryGap>): List<PlacedGap> =
  gaps.filter { it.index in 0..messages.size }.map { PlacedGap(it, messages.getOrNull(it.index - 1)?.id) }

/** Whether a day's label names its year: only outside the current year (#1086). */
fun dayShowsYear(date: LocalDate, today: LocalDate): Boolean = date.year != today.year

/**
 * Lays out a conversation (oldest first) the way chat apps do: a date chip per day, the unread line
 * before `maybeFirstUnread`, and follow-up messages of the same author grouped under one name and
 * one avatar. The holes in the history ([gaps]) sit after the message before them; one never
 * groups the messages around it.
 */
fun feedItems(
  messages: List<Message>,
  maybeFirstUnread: String?,
  zone: ZoneId = ZoneId.systemDefault(),
  gaps: List<PlacedGap> = emptyList(),
): List<FeedItem> {
  val items = ArrayList<FeedItem>(messages.size + gaps.size + 8)
  val gapsAfter = gaps.groupBy { it.maybeAfterMessage }
  if (messages.isNotEmpty()) gapsAfter[null]?.forEach { items += FeedItem.Gap(it.gap) }
  var maybePrevious: Message? = null
  var maybeDay: LocalDate? = null
  for (message in messages) {
    val day = Instant.ofEpochMilli(message.sentAt).atZone(zone).toLocalDate()
    val isNewDay = day != maybeDay
    if (isNewDay) items += FeedItem.Day(day)
    val isUnreadStart = message.id == maybeFirstUnread
    if (isUnreadStart) items += FeedItem.Unread
    val isAfterGap = items.lastOrNull() is FeedItem.Gap
    val isContinuation = !isNewDay && !isUnreadStart && !isAfterGap && maybePrevious?.let { continues(it, message) } == true
    if (isContinuation) {
      // The entry just before is the previous message: no chip, line or hole came between them.
      val previous = items.removeAt(items.lastIndex) as FeedItem.Entry
      items += previous.copy(isGroupEnd = false)
    }
    items += FeedItem.Entry(message, isContinuation)
    gapsAfter[message.id]?.forEach { items += FeedItem.Gap(it.gap) }
    maybePrevious = message
    maybeDay = day
  }
  return items
}

private fun continues(previous: Message, message: Message): Boolean =
  previous.author == message.author &&
    // One bot posts for several sources.
    previous.maybeSource == message.maybeSource &&
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
