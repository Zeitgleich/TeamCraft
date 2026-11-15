package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.ui.FavoriteOrder

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

/** The handle at the row's end that starts a drag; its job for assistive tech is the row's move actions. */
@Composable
internal fun FavoriteDragHandle(reorder: FavoriteReorder, modifier: Modifier = Modifier) {
  val haptics = LocalHapticFeedback.current
  val onMove by rememberUpdatedState(reorder.onMove)
  Icon(
    Icons.Filled.Menu,
    contentDescription = null,
    tint = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = modifier
      .size(40.dp)
      .pointerInput(reorder.state) {
        detectVerticalDragGestures(
          onDragStart = {
            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
            reorder.state.isDragging = true
          },
          onDragEnd = { reorder.state.end() },
          onDragCancel = { reorder.state.end() },
        ) { change, delta ->
          change.consume()
          reorder.state.dragBy(delta) { up -> onMove(up) }
        }
      },
  )
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
