package de.zeitgleich.teamcraft.ui

import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.spring
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import kotlin.math.abs
import kotlin.math.sign
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/** A drag that ends this far from the start (of the viewer's height) closes the viewer. */
private const val DISMISS_FRACTION = 0.3f

/** Where the backdrop is fully faded: this share of the viewer's height from the start. */
private const val FADE_FRACTION = 0.5f

/** The same deceleration as a scroll, to project where a fling would come to rest. */
private const val DECELERATION_RATE = 0.998f

/** A fling at least this fast (px/s) counts as a fling and may bounce back; slower settles without overshoot. */
private const val BOUNCE_VELOCITY = 500f

/** Where a fling of this velocity (px/s) would travel before it stops. */
internal fun projectedTravel(velocity: Float): Float = velocity / 1000f * DECELERATION_RATE / (1f - DECELERATION_RATE)

/** Whether a drag released at [offset] with [velocity] closes the viewer, [extent] being its height. */
internal fun shouldDismiss(offset: Float, velocity: Float, extent: Float): Boolean {
  val projected = offset + projectedTravel(velocity)
  // A fling back past the start is a change of mind, not a dismissal the other way.
  val isTurnedBack = offset != 0f && sign(projected) != sign(offset)
  return extent > 0f && !isTurnedBack && abs(projected) >= extent * DISMISS_FRACTION
}

/** How far the backdrop has faded (0 = not at all, 1 = gone) at [offset]. */
internal fun dismissProgress(offset: Float, extent: Float): Float =
  if (extent <= 0f) 0f else (abs(offset) / (extent * FADE_FRACTION)).coerceIn(0f, 1f)

/**
 * The state of a vertical swipe that closes a full-screen viewer (#1999): how far the content is
 * dragged and how much of the backdrop is left. Content uses [swipeDismissLayer], the backdrop and
 * bars use [progress].
 */
@Stable
class SwipeDismissState {
  internal var offset by mutableFloatStateOf(0f)
  internal var extent by mutableFloatStateOf(0f)
  internal var job: Job? = null

  /** 0 at rest, 1 once the backdrop is gone. */
  val progress: Float get() = dismissProgress(offset, extent)
}

@Composable
fun rememberSwipeDismissState(): SwipeDismissState = remember { SwipeDismissState() }

/**
 * Follows a vertical drag (up or down) of the content: it moves with the finger, past a distance or
 * with a fast fling it flies off and [onDismiss] runs, otherwise it springs back. Not active while
 * [isEnabled] is false (a zoomed picture pans instead), with a second finger, or when the first
 * movement is horizontal (paging, a scrubber). Reduced motion: no spring, it closes at once or
 * snaps back.
 *
 * Watches in the initial pass, so a video's view can't swallow the drag; nothing is consumed
 * before the movement is clearly vertical, so taps and controls keep working.
 */
@Composable
fun Modifier.swipeDismiss(
  state: SwipeDismissState,
  isEnabled: Boolean,
  isMotionReduced: Boolean,
  onDismiss: () -> Unit,
): Modifier {
  val scope = rememberCoroutineScope()
  val currentOnDismiss by rememberUpdatedState(onDismiss)
  return pointerInput(state, isEnabled, isMotionReduced) {
    if (!isEnabled) return@pointerInput
    awaitEachGesture {
      val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
      val tracker = VelocityTracker().apply { addPosition(down.uptimeMillis, down.position) }
      state.extent = size.height.toFloat()
      var moved = Offset.Zero
      var isDragging = false
      while (true) {
        val event = awaitPointerEvent(PointerEventPass.Initial)
        if (event.changes.size > 1 && !isDragging) return@awaitEachGesture
        val change = event.changes.firstOrNull { it.id == down.id } ?: return@awaitEachGesture
        if (change.changedToUp() || !change.pressed) {
          if (isDragging) {
            val velocity = tracker.calculateVelocity().y
            state.job = scope.launch { settle(state, velocity, isMotionReduced) { currentOnDismiss() } }
          }
          return@awaitEachGesture
        }
        tracker.addPosition(change.uptimeMillis, change.position)
        val delta = change.positionChange()
        if (!isDragging) {
          moved += delta
          // Horizontal first: paging or a scrubber's, not ours. Another handler took it: leave it.
          if (change.isConsumed || (abs(moved.x) > viewConfiguration.touchSlop && abs(moved.x) > abs(moved.y))) {
            return@awaitEachGesture
          }
          if (abs(moved.y) > viewConfiguration.touchSlop) {
            isDragging = true
            state.job?.cancel()
            // The slop is not part of the movement: the content starts under the finger.
            state.offset += moved.y - sign(moved.y) * viewConfiguration.touchSlop
            change.consume()
          }
        } else {
          state.offset += delta.y
          change.consume()
        }
      }
    }
  }
}

/** Moves the content with the drag (apply after [swipeDismiss] so the gesture's coordinates stay put). */
fun Modifier.swipeDismissLayer(state: SwipeDismissState): Modifier = graphicsLayer { translationY = state.offset }

private suspend fun settle(state: SwipeDismissState, velocity: Float, isMotionReduced: Boolean, onDismiss: () -> Unit) {
  if (shouldDismiss(state.offset, velocity, state.extent)) {
    if (!isMotionReduced) {
      // Out the way it was going, continuing at the finger's speed (the direction of the drag, when the finger rested).
      val direction = if (abs(velocity) > BOUNCE_VELOCITY) sign(velocity) else sign(state.offset)
      animate(
        initialValue = state.offset,
        targetValue = direction * state.extent,
        initialVelocity = velocity,
        animationSpec = spring(dampingRatio = Spring.DampingRatioNoBouncy, stiffness = Spring.StiffnessMedium),
      ) { value, _ -> state.offset = value }
    }
    onDismiss()
  } else if (isMotionReduced) {
    state.offset = 0f
  } else {
    // Bounce only when a fling carried momentum into the spring.
    val damping = if (abs(velocity) > BOUNCE_VELOCITY) 0.8f else Spring.DampingRatioNoBouncy
    animate(
      initialValue = state.offset,
      targetValue = 0f,
      initialVelocity = velocity,
      animationSpec = spring(dampingRatio = damping, stiffness = Spring.StiffnessMedium),
    ) { value, _ -> state.offset = value }
  }
}
