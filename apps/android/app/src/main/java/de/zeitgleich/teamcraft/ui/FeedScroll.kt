package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message

/**
 * How a conversation's feed follows the conversation, as in every messenger (issue #755): it opens
 * at the first unread message (or the end), jumps to the end when the user sends, stays pinned to
 * the end while new messages arrive and the user reads there, and otherwise counts them for the
 * "N new" pill instead of moving the list under the user's thumb.
 *
 * Pure: the feed reports what happened and where it was, this decides what the list does.
 */
fun decideFeedScroll(state: FeedScrollState, event: FeedScrollEvent, isNearBottom: Boolean): FeedScrollDecision =
  when (event) {
    is FeedScrollEvent.Opened -> opened(state, event)
    FeedScrollEvent.Sent -> FeedScrollDecision(
      state.copy(isAwaitingOwnMessage = true, unseenCount = 0),
      FeedScrollAction.ToBottom(isAnimated = isNearBottom),
    )
    is FeedScrollEvent.NewMessages -> newMessages(state, event, isNearBottom)
    FeedScrollEvent.ReachedBottom -> FeedScrollDecision(state.copy(unseenCount = 0), FeedScrollAction.None)
  }

/** Where the first unread message stands; `Loading` until the conversation has been asked. */
sealed interface FirstUnread {
  data object Loading : FirstUnread

  data object None : FirstUnread

  data class At(val messageId: String) : FirstUnread

  val maybeMessageId: String? get() = (this as? At)?.messageId
}

/** What the feed remembers between events. */
data class FeedScrollState(
  /** The opening position was taken; later loads don't move the list again. */
  val isOpened: Boolean = false,
  /** The user sent a message from this screen that hasn't shown up in the list yet. */
  val isAwaitingOwnMessage: Boolean = false,
  /** Others' messages that arrived while the user was reading further up. */
  val unseenCount: Int = 0,
)

sealed interface FeedScrollEvent {
  /**
   * The first messages are there and the unread position is known. Indices are into the
   * newest-first list; `null` when there is no such row.
   */
  data class Opened(val maybeFocusIndex: Int?, val maybeUnreadIndex: Int?) : FeedScrollEvent

  /** The user sent a message from this screen's composer. */
  data object Sent : FeedScrollEvent

  /** Messages newer than the previous newest one appeared. */
  data class NewMessages(val fromOthers: Int, val hasOwn: Boolean) : FeedScrollEvent

  /** The newest message came into view. */
  data object ReachedBottom : FeedScrollEvent
}

sealed interface FeedScrollAction {
  data object None : FeedScrollAction

  /** Show the row at this index of the newest-first list at the top of the screen. */
  data class ShowAtTop(val index: Int) : FeedScrollAction

  /** Go to the newest message: glide there when it is close, jump when it is far. */
  data class ToBottom(val isAnimated: Boolean) : FeedScrollAction
}

data class FeedScrollDecision(val state: FeedScrollState, val action: FeedScrollAction)

/**
 * Whether the newest message is within about one screen. The rows below the first visible one
 * aren't measured, so they count at the average height of the visible rows.
 */
fun isNearBottom(firstVisibleIndex: Int, firstVisibleOffsetPx: Int, averageRowPx: Int, viewportPx: Int): Boolean {
  if (viewportPx <= 0) return firstVisibleIndex == 0
  val distancePx = firstVisibleIndex.toLong() * averageRowPx + firstVisibleOffsetPx
  return distancePx <= viewportPx
}

/**
 * The messages that are newer than `maybePreviousNewest` (oldest first). An ID that left the list
 * (a pending message the server accepted under its final ID, a deleted message) falls back to the
 * time it was sent, so neither counts as "new" twice.
 */
fun newerMessages(messages: List<Message>, maybePreviousNewest: Message?): List<Message> {
  val previous = maybePreviousNewest ?: return emptyList()
  val index = messages.indexOfLast { it.id == previous.id }
  return if (index >= 0) messages.subList(index + 1, messages.size) else messages.filter { it.sentAt > previous.sentAt }
}

private fun opened(state: FeedScrollState, event: FeedScrollEvent.Opened): FeedScrollDecision {
  if (state.isOpened) return FeedScrollDecision(state, FeedScrollAction.None)
  val action = when {
    // A message link opens at its message.
    event.maybeFocusIndex != null -> FeedScrollAction.ShowAtTop(event.maybeFocusIndex)
    // The unread line at index 0 would have nothing below it; that only happens while loading.
    event.maybeUnreadIndex != null && event.maybeUnreadIndex > 0 -> FeedScrollAction.ShowAtTop(event.maybeUnreadIndex)
    else -> FeedScrollAction.ToBottom(isAnimated = false)
  }
  return FeedScrollDecision(state.copy(isOpened = true), action)
}

private fun newMessages(state: FeedScrollState, event: FeedScrollEvent.NewMessages, isNearBottom: Boolean): FeedScrollDecision {
  // Before the opening position is taken, the opening decides.
  if (!state.isOpened) return FeedScrollDecision(state, FeedScrollAction.None)
  return when {
    // The message the user just sent: bring it into view wherever the list is.
    event.hasOwn && state.isAwaitingOwnMessage -> FeedScrollDecision(
      state.copy(isAwaitingOwnMessage = false, unseenCount = 0),
      FeedScrollAction.ToBottom(isAnimated = isNearBottom),
    )
    // Reading at the end: stay there.
    isNearBottom -> FeedScrollDecision(state.copy(unseenCount = 0), FeedScrollAction.ToBottom(isAnimated = true))
    // Reading further up: don't move, count them for the pill.
    else -> FeedScrollDecision(state.copy(unseenCount = state.unseenCount + event.fromOthers), FeedScrollAction.None)
  }
}
