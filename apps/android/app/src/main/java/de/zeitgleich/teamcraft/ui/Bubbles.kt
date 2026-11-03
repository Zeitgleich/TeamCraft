package de.zeitgleich.teamcraft.ui

import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** The corner radius of a chat bubble. */
val BUBBLE_CORNER = 18.dp

/** The corner on the sender's side where a bubble joins the next or previous one of a group. */
val BUBBLE_JOINED_CORNER = 5.dp

/** The corners of one bubble, start and end as the layout direction reads them. */
data class BubbleCorners(val topStart: Dp, val topEnd: Dp, val bottomEnd: Dp, val bottomStart: Dp)

/**
 * The corners of a message's bubble in a group of one sender (issue #704): round all over, except
 * on the sender's side (end for own messages, start for others) where it joins the bubble above
 * ([isContinuation]) or below (not [isGroupEnd]), as in the common messengers.
 */
fun bubbleCorners(isMine: Boolean, isContinuation: Boolean, isGroupEnd: Boolean): BubbleCorners {
  val top = if (isContinuation) BUBBLE_JOINED_CORNER else BUBBLE_CORNER
  val bottom = if (isGroupEnd) BUBBLE_CORNER else BUBBLE_JOINED_CORNER
  return if (isMine) {
    BubbleCorners(topStart = BUBBLE_CORNER, topEnd = top, bottomEnd = bottom, bottomStart = BUBBLE_CORNER)
  } else {
    BubbleCorners(topStart = top, topEnd = BUBBLE_CORNER, bottomEnd = BUBBLE_CORNER, bottomStart = bottom)
  }
}

/**
 * Whether bubbles carry the sender's name and avatar: in channels and group messages, not in a
 * conversation with one other person, where it is always the same.
 */
fun showsSenders(maybeConversationKind: String?): Boolean = maybeConversationKind != "Direct"
