package de.zeitgleich.teamcraft.ui.theme

import androidx.compose.animation.AnimatedContentTransitionScope
import androidx.compose.animation.EnterTransition
import androidx.compose.animation.ExitTransition
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.navigation.NavBackStackEntry

/** Material's shared-axis distance: screens move a short way while they cross-fade. */
private val SHARED_AXIS_OFFSET = 30.dp

/** The outgoing screen of a back gesture shrinks to this, as Material's predictive back does. */
private const val BACK_PREVIEW_SCALE = 0.9f

/**
 * Transitions between the app's screens (issue #454): Material's horizontal shared axis, driven by
 * the theme's motion scheme (expressive springs, or standard ones with "remove animations").
 * Navigation seeks the pop transitions with the predictive back gesture, so the screen being left
 * shrinks and slides away under the finger and settles with the same spring when released.
 */
@Immutable
class ScreenTransitions(
  val enter: AnimatedContentTransitionScope<NavBackStackEntry>.() -> EnterTransition,
  val exit: AnimatedContentTransitionScope<NavBackStackEntry>.() -> ExitTransition,
  val popEnter: AnimatedContentTransitionScope<NavBackStackEntry>.() -> EnterTransition,
  val popExit: AnimatedContentTransitionScope<NavBackStackEntry>.() -> ExitTransition,
)

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
      enter = {
        slideInHorizontally(spatial) { forward * offsetPx } + fadeIn(motion.defaultEffectsSpec())
      },
      exit = {
        slideOutHorizontally(spatial) { -forward * offsetPx } + fadeOut(motion.fastEffectsSpec())
      },
      popEnter = {
        slideInHorizontally(spatial) { -forward * offsetPx } + fadeIn(motion.defaultEffectsSpec())
      },
      popExit = {
        scaleOut(motion.defaultSpatialSpec(), targetScale = BACK_PREVIEW_SCALE) +
          slideOutHorizontally(spatial) { forward * offsetPx } +
          fadeOut(motion.fastEffectsSpec())
      },
    )
  }
}
