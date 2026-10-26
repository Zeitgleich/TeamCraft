package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import de.zeitgleich.teamcraft.core.Message

class FeedScrollTest {
  @Test
  fun opensAtTheFirstUnreadMessage() {
    val decision = decideFeedScroll(FeedScrollState(), FeedScrollEvent.Opened(maybeFocusIndex = null, maybeUnreadIndex = 7), isNearBottom = true)
    assertEquals(FeedScrollAction.ShowAtTop(7), decision.action)
    assertTrue(decision.state.isOpened)
  }

  @Test
  fun opensAtTheEndWithoutUnreadMessages() {
    val decision = decideFeedScroll(FeedScrollState(), FeedScrollEvent.Opened(maybeFocusIndex = null, maybeUnreadIndex = null), isNearBottom = true)
    assertEquals(FeedScrollAction.ToBottom(isAnimated = false), decision.action)
  }

  @Test
  fun anUnreadLineWithNothingBelowItOpensAtTheEnd() {
    val decision = decideFeedScroll(FeedScrollState(), FeedScrollEvent.Opened(maybeFocusIndex = null, maybeUnreadIndex = 0), isNearBottom = true)
    assertEquals(FeedScrollAction.ToBottom(isAnimated = false), decision.action)
  }

  @Test
  fun aMessageLinkWinsOverTheUnreadLine() {
    val decision = decideFeedScroll(FeedScrollState(), FeedScrollEvent.Opened(maybeFocusIndex = 30, maybeUnreadIndex = 7), isNearBottom = true)
    assertEquals(FeedScrollAction.ShowAtTop(30), decision.action)
  }

  @Test
  fun opensOnlyOnce() {
    val decision = decideFeedScroll(OPENED, FeedScrollEvent.Opened(maybeFocusIndex = null, maybeUnreadIndex = 7), isNearBottom = false)
    assertEquals(FeedScrollAction.None, decision.action)
    assertEquals(OPENED, decision.state)
  }

  @Test
  fun newMessagesBeforeTheOpeningDontMoveTheList() {
    val decision = decideFeedScroll(FeedScrollState(), FeedScrollEvent.NewMessages(fromOthers = 2, hasOwn = false), isNearBottom = false)
    assertEquals(FeedScrollAction.None, decision.action)
    assertEquals(0, decision.state.unseenCount)
  }

  @Test
  fun sendingCloseToTheEndGlidesThere() {
    val decision = decideFeedScroll(OPENED, FeedScrollEvent.Sent, isNearBottom = true)
    assertEquals(FeedScrollAction.ToBottom(isAnimated = true), decision.action)
    assertTrue(decision.state.isAwaitingOwnMessage)
  }

  @Test
  fun sendingFarUpJumpsThereAndClearsThePill() {
    val decision = decideFeedScroll(OPENED.copy(unseenCount = 4), FeedScrollEvent.Sent, isNearBottom = false)
    assertEquals(FeedScrollAction.ToBottom(isAnimated = false), decision.action)
    assertEquals(0, decision.state.unseenCount)
  }

  @Test
  fun theSentMessageComesIntoViewEvenAfterScrollingUp() {
    val awaiting = OPENED.copy(isAwaitingOwnMessage = true)
    val decision = decideFeedScroll(awaiting, FeedScrollEvent.NewMessages(fromOthers = 0, hasOwn = true), isNearBottom = false)
    assertEquals(FeedScrollAction.ToBottom(isAnimated = false), decision.action)
    assertFalse(decision.state.isAwaitingOwnMessage)
  }

  @Test
  fun ownMessagesFromAnotherDeviceDontPullTheReaderDown() {
    val decision = decideFeedScroll(OPENED, FeedScrollEvent.NewMessages(fromOthers = 0, hasOwn = true), isNearBottom = false)
    assertEquals(FeedScrollAction.None, decision.action)
    assertEquals(0, decision.state.unseenCount)
  }

  @Test
  fun staysPinnedToTheEnd() {
    val decision = decideFeedScroll(OPENED, FeedScrollEvent.NewMessages(fromOthers = 1, hasOwn = false), isNearBottom = true)
    assertEquals(FeedScrollAction.ToBottom(isAnimated = true), decision.action)
    assertEquals(0, decision.state.unseenCount)
  }

  @Test
  fun scrolledUpCountsThemInsteadOfMoving() {
    var state = OPENED
    for (count in listOf(1, 3)) {
      val decision = decideFeedScroll(state, FeedScrollEvent.NewMessages(fromOthers = count, hasOwn = false), isNearBottom = false)
      assertEquals(FeedScrollAction.None, decision.action)
      state = decision.state
    }
    assertEquals(4, state.unseenCount)
  }

  @Test
  fun reachingTheEndClearsThePill() {
    val decision = decideFeedScroll(OPENED.copy(unseenCount = 5), FeedScrollEvent.ReachedBottom, isNearBottom = true)
    assertEquals(FeedScrollAction.None, decision.action)
    assertEquals(0, decision.state.unseenCount)
  }

  @Test
  fun theNewestRowInViewIsNear() {
    assertTrue(isNearBottom(firstVisibleIndex = 0, firstVisibleOffsetPx = 0, averageRowPx = 100, viewportPx = 1000))
  }

  @Test
  fun withinAboutOneScreenIsNear() {
    assertTrue(isNearBottom(firstVisibleIndex = 9, firstVisibleOffsetPx = 50, averageRowPx = 100, viewportPx = 1000))
    assertFalse(isNearBottom(firstVisibleIndex = 10, firstVisibleOffsetPx = 50, averageRowPx = 100, viewportPx = 1000))
  }

  @Test
  fun beforeTheFirstLayoutOnlyTheStartIsNear() {
    assertTrue(isNearBottom(firstVisibleIndex = 0, firstVisibleOffsetPx = 0, averageRowPx = 0, viewportPx = 0))
    assertFalse(isNearBottom(firstVisibleIndex = 3, firstVisibleOffsetPx = 0, averageRowPx = 0, viewportPx = 0))
  }

  @Test
  fun hugeListsDontOverflow() {
    assertFalse(isNearBottom(firstVisibleIndex = Int.MAX_VALUE, firstVisibleOffsetPx = 0, averageRowPx = Int.MAX_VALUE, viewportPx = 1000))
  }

  @Test
  fun theMessagesAfterThePreviousNewest() {
    val messages = listOf(message("1", 1), message("2", 2), message("3", 3))
    assertEquals(listOf("3"), newerMessages(messages, message("2", 2)).map { it.id })
  }

  @Test
  fun nothingBeforeTheFirstLoad() {
    assertEquals(emptyList<Message>(), newerMessages(listOf(message("1", 1)), maybePreviousNewest = null))
  }

  @Test
  fun anAcceptedPendingMessageIsNotNewAgain() {
    // The outbox ID is replaced by the server's; the message keeps its place in time.
    val messages = listOf(message("1", 1), message("2", 2))
    assertEquals(emptyList<Message>(), newerMessages(messages, message("pending-2", 2)).map { it.id })
  }

  @Test
  fun aDeletedNewestMessageFallsBackToItsTime() {
    val messages = listOf(message("1", 1), message("3", 3))
    assertEquals(listOf("3"), newerMessages(messages, message("2", 2)).map { it.id })
  }

  @Test
  fun theUnreadMarkerKnowsItsMessage() {
    assertEquals("7", FirstUnread.At("7").maybeMessageId)
    assertEquals(null, FirstUnread.None.maybeMessageId)
    assertEquals(null, FirstUnread.Loading.maybeMessageId)
  }

  private companion object {
    val OPENED = FeedScrollState(isOpened = true)

    fun message(id: String, sentAt: Long) = Message(id = id, conversation = "c", author = "a", sentAt = sentAt, body = "text $id")
  }
}
