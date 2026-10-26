package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class BubblesTest {
  @Test
  fun aSingleBubbleIsRoundAllOver() {
    val corners = bubbleCorners(isMine = false, isContinuation = false, isGroupEnd = true)
    assertEquals(BubbleCorners(BUBBLE_CORNER, BUBBLE_CORNER, BUBBLE_CORNER, BUBBLE_CORNER), corners)
  }

  @Test
  fun othersJoinOnTheStartSide() {
    val first = bubbleCorners(isMine = false, isContinuation = false, isGroupEnd = false)
    val middle = bubbleCorners(isMine = false, isContinuation = true, isGroupEnd = false)
    val last = bubbleCorners(isMine = false, isContinuation = true, isGroupEnd = true)
    assertEquals(BubbleCorners(BUBBLE_CORNER, BUBBLE_CORNER, BUBBLE_CORNER, BUBBLE_JOINED_CORNER), first)
    assertEquals(BubbleCorners(BUBBLE_JOINED_CORNER, BUBBLE_CORNER, BUBBLE_CORNER, BUBBLE_JOINED_CORNER), middle)
    assertEquals(BubbleCorners(BUBBLE_JOINED_CORNER, BUBBLE_CORNER, BUBBLE_CORNER, BUBBLE_CORNER), last)
  }

  @Test
  fun ownMessagesJoinOnTheEndSide() {
    val middle = bubbleCorners(isMine = true, isContinuation = true, isGroupEnd = false)
    assertEquals(BubbleCorners(BUBBLE_CORNER, BUBBLE_JOINED_CORNER, BUBBLE_JOINED_CORNER, BUBBLE_CORNER), middle)
  }

  @Test
  fun sendersShowOutsideOneToOneConversations() {
    assertEquals(false, showsSenders("Direct"))
    assertEquals(true, showsSenders("Group"))
    assertEquals(true, showsSenders("Public"))
    assertEquals(true, showsSenders(null))
  }
}
