package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.awaitLongPressOrCancellation
import androidx.compose.foundation.gestures.drag
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.hapticfeedback.HapticFeedback
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import de.zeitgleich.teamcraft.ui.FavoriteOrder
import kotlin.math.abs

/**
 * Drag to reorder a favorite (#2057). The row follows the finger; each time it has moved past half a
 * row's height, [FavoriteReorder.onMove] puts it one place further and the offset gives that row back,
 * so the row stays under the finger while its neighbours slide. "Move up/down" in the row's menu and
 * accessibility actions do the same without a drag.
 */
internal class FavoriteDragState {
  var offset by mutableFloatStateOf(0f)
  var height by mutableIntStateOf(0)
  var isDragging by mutableStateOf(false)

  /** Takes [delta] pixels of drag; moves the row by whole places while it is past half a row. */
  fun dragBy(delta: Float, onMove: (up: Boolean) -> Boolean) {
    if (height <= 0) return
    offset += delta
    val half = height / 2f
    while (offset > half) {
      if (!onMove(false)) {
        offset = half
        return
      }
      offset -= height
    }
    while (offset < -half) {
      if (!onMove(true)) {
        offset = -half
        return
      }
      offset += height
    }
  }

  fun end() {
    isDragging = false
    offset = 0f
  }
}

/** A favorite row that can move: its drag state and the step to a neighbouring place (false at an end). */
internal class FavoriteReorder(val state: FavoriteDragState, val onMove: (up: Boolean) -> Boolean)

/**
 * Long-press then move to drag a favorite (#2057): the row lifts, follows the finger and drops into
 * place. A long-press released without moving past touch slop calls [onLongPress] (the entry menu);
 * a swipe before the long-press time is the list's scroll. Sits inside the row's click handling and
 * consumes the gesture once it has taken it, so the release is no tap and the list doesn't scroll.
 * Its job for assistive tech is the row's move actions.
 */
internal fun Modifier.favoriteLongPressDrag(reorder: FavoriteReorder, onLongPress: () -> Unit): Modifier =
  composed {
    val haptics = LocalHapticFeedback.current
    val onMove by rememberUpdatedState(reorder.onMove)
    val onLongPressNow by rememberUpdatedState(onLongPress)
    pointerInput(reorder.state) {
      awaitEachGesture {
        val down = awaitFirstDown(requireUnconsumed = false)
        val longPress = awaitLongPressOrCancellation(down.id) ?: return@awaitEachGesture
        haptics.performHapticFeedback(HapticFeedbackType.LongPress)
        longPress.consume()
        var moved = 0f
        var isDragging = false
        drag(longPress.id) { change ->
          change.consume()
          val delta = change.positionChange().y
          moved += delta
          if (!isDragging && abs(moved) > viewConfiguration.touchSlop) {
            isDragging = true
            reorder.state.isDragging = true
            // The slop already travelled counts, so the row doesn't jump when it picks up.
            reorder.state.dragBy(moved) { up -> stepWithTick(haptics, onMove, up) }
          } else if (isDragging) {
            reorder.state.dragBy(delta) { up -> stepWithTick(haptics, onMove, up) }
          }
        }
        // The release (or cancel) belongs to this gesture: no tap, no fling.
        currentEvent.changes.forEach { it.consume() }
        if (isDragging) reorder.state.end() else onLongPressNow()
      }
    }
  }

/** One place further, with a tick each time the row passes a neighbour. */
private fun stepWithTick(haptics: HapticFeedback, onMove: (Boolean) -> Boolean, up: Boolean): Boolean {
  val didMove = onMove(up)
  if (didMove) haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
  return didMove
}

/**
 * The reorder of favorite [id] among [shown] (the favorites as listed). Each step starts from the
 * latest order this row wrote, so two drag events before the list has recomposed don't repeat a step.
 */
@Composable
internal fun rememberFavoriteReorder(id: String, shown: List<String>): FavoriteReorder {
  val context = LocalContext.current
  val state = remember(id) { FavoriteDragState() }
  val latest = remember(shown) { arrayOf(shown) }
  return remember(id, shown) {
    FavoriteReorder(state) { up ->
      val moved = FavoriteOrder.moved(latest[0], id, up) ?: return@FavoriteReorder false
      latest[0] = moved
      FavoriteOrder.of(context).set(moved)
      true
    }
  }
}
