package de.zeitgleich.teamcraft.ui.theme

import androidx.activity.compose.PredictiveBackHandler
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.ContentTransform
import androidx.compose.animation.core.SeekableTransitionState
import androidx.compose.animation.core.rememberTransition
import androidx.compose.animation.EnterTransition
import androidx.compose.animation.ExitTransition
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlin.coroutines.cancellation.CancellationException

/** Material's shared-axis distance: screens move a short way while they cross-fade. */
private val SHARED_AXIS_OFFSET = 30.dp

/** The outgoing screen of a back gesture shrinks to this, as Material's predictive back does. */
private const val BACK_PREVIEW_SCALE = 0.9f

/**
 * Transitions between the app's screens (issue #454): Material's horizontal shared axis, driven by
 * the theme's motion scheme (expressive springs, or standard ones with "remove animations").
 * Navigation (and [SeekablePages]) seek the pop transitions with the predictive back gesture, so the
 * screen being left shrinks and slides away under the finger and settles with the same spring when
 * released.
 */
@Immutable
class ScreenTransitions(
  val enter: EnterTransition,
  val exit: ExitTransition,
  val popEnter: EnterTransition,
  val popExit: ExitTransition,
) {
  /** Forward ([isForward]) or back between two pages of one screen, e.g. settings and a sub-page. */
  fun between(isForward: Boolean): ContentTransform =
    if (isForward) {
      enter togetherWith exit
    } else {
      // The page being left stays above the one returned to.
      (popEnter togetherWith popExit).apply { targetContentZIndex = -1f }
    }
}

/** The [ScreenTransitions] of the current theme and layout direction. */
@Composable
fun rememberScreenTransitions(): ScreenTransitions {
  val motion = MaterialTheme.motionScheme
  val offsetPx = with(LocalDensity.current) { SHARED_AXIS_OFFSET.roundToPx() }
  // Forward is to the start in right-to-left layouts.
  val forward = if (LocalLayoutDirection.current == LayoutDirection.Rtl) -1 else 1
  return remember(motion, offsetPx, forward) {
    val spatial = motion.defaultSpatialSpec<IntOffset>()
    ScreenTransitions(
      enter = slideInHorizontally(spatial) { forward * offsetPx } + fadeIn(motion.defaultEffectsSpec()),
      exit = slideOutHorizontally(spatial) { -forward * offsetPx } + fadeOut(motion.fastEffectsSpec()),
      // Going back, the screen being left stays on top, shrinks and slides away with the finger
      // (the back gesture seeks this transition); the one returned to fades in beneath it.
      popEnter = slideInHorizontally(spatial) { -forward * offsetPx } + fadeIn(motion.defaultSpatialSpec()),
      popExit = scaleOut(motion.defaultSpatialSpec(), targetScale = BACK_PREVIEW_SCALE) +
        slideOutHorizontally(spatial) { fullWidth -> forward * fullWidth },
    )
  }
}

/**
 * Pages inside one screen (settings and its sub-pages): [page] slides in with the app's screen
 * transition, and the system back gesture goes back to [rootPage] with a predictive preview: the
 * page follows the finger, settles back when the gesture is cancelled and finishes with the spring
 * when released.
 */
@Composable
fun <T> SeekablePages(
  page: T,
  rootPage: T,
  onBack: () -> Unit,
  modifier: Modifier = Modifier,
  content: @Composable (T) -> Unit,
) {
  val transitions = rememberScreenTransitions()
  val state = remember { SeekableTransitionState(page) }
  val transition = rememberTransition(state, label = "pages")
  PredictiveBackHandler(enabled = page != rootPage) { progress ->
    try {
      progress.collect { event -> state.seekTo(event.progress, targetState = rootPage) }
      onBack()
    } catch (cancelled: CancellationException) {
      // The gesture was cancelled: the page settles back where it was (this coroutine is being
      // cancelled, so the settling runs outside of it).
      withContext(NonCancellable) { state.animateTo(state.currentState) }
      throw cancelled
    }
  }
  LaunchedEffect(page) { state.animateTo(page) }
  transition.AnimatedContent(
    modifier = modifier,
    transitionSpec = { transitions.between(isForward = targetState != rootPage) },
  ) { shown ->
    // Opaque, so the page beneath doesn't show through while the back gesture moves this one.
    Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) { content(shown) }
  }
}
