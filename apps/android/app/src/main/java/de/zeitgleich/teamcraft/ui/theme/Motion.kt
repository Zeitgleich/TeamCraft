package de.zeitgleich.teamcraft.ui.theme

import androidx.compose.animation.EnterTransition
import androidx.compose.animation.ExitTransition
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.animation.core.FiniteAnimationSpec
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.foundation.lazy.LazyItemScope
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.unit.IntOffset

/**
 * The motion tokens of the web and desktop UI (`crates/teamcraft-ui/style/main/000-tokens.css`,
 * docs/motion-audit.md, issue #302): three durations, a short exit, and four curves. Screen
 * transitions and Material's own components keep the theme's [androidx.compose.material3.MotionScheme]
 * springs ([rememberScreenTransitions]); these are for the app's own fades, pops and list motion, so
 * they feel the same on every platform.
 */
object MotionTokens {
  const val FAST_MS = 120
  const val BASE_MS = 180
  const val SLOW_MS = 260
  const val EXIT_MS = 100

  /** `--ease-standard`: state changes on screen. */
  val Standard = CubicBezierEasing(0.2f, 0f, 0f, 1f)

  /** `--ease-emphasized`: entrances that should land softly (a new message). */
  val Emphasized = CubicBezierEasing(0.05f, 0.7f, 0.1f, 1f)

  /** `--ease-exit`: a strong ease-out, so leaving starts at once (motion-audit.md finding 1). */
  val Exit = CubicBezierEasing(0.23f, 1f, 0.32f, 1f)

  /** `--ease-pop`: its own overshoot is the pop of reactions and badges (finding 2). */
  val Pop = CubicBezierEasing(0.34f, 1.56f, 0.64f, 1f)

  /** `motion-pop` starts at this scale; never from zero. */
  const val POP_FROM_SCALE = 0.7f

  /** Popups and floating buttons grow from this scale while they fade in. */
  const val ENTER_SCALE = 0.95f
}

/** The [MotionSpecs] of the current "Remove animations" setting; [TeamCraftTheme] provides it. */
val LocalMotion = staticCompositionLocalOf { MotionSpecs(isReduced = false) }

/**
 * Animation specs from [MotionTokens] for one motion setting. With [isReduced] ("Remove animations",
 * which also turns the system's own animations off), fades stay as short cross-fades that help
 * reading, and everything that moves or scales snaps, as the web's reduced-motion block does.
 */
@Immutable
class MotionSpecs(val isReduced: Boolean) {
  /** Small state changes: a fade, a colour. */
  fun <T> fast(): FiniteAnimationSpec<T> = tween(MotionTokens.FAST_MS, easing = MotionTokens.Standard)

  /** Movement on screen: rows making room, a bar growing. */
  fun <T> base(): FiniteAnimationSpec<T> = if (isReduced) snap() else tween(MotionTokens.BASE_MS, easing = MotionTokens.Standard)

  /** Larger entrances: a new message rising into place. */
  fun <T> slow(): FiniteAnimationSpec<T> = if (isReduced) snap() else tween(MotionTokens.SLOW_MS, easing = MotionTokens.Emphasized)

  /** Leaving: short and fast from the first frame. */
  fun <T> exit(): FiniteAnimationSpec<T> = tween(MotionTokens.EXIT_MS, easing = MotionTokens.Exit)

  /** The pop of a reaction or badge that changed. */
  fun pop(): FiniteAnimationSpec<Float> = if (isReduced) snap() else tween(MotionTokens.SLOW_MS, easing = MotionTokens.Pop)

  /** Something small appearing in place (a jump button, a pill): fades in and grows a little. */
  val enter: EnterTransition =
    fadeIn(fast()) + if (isReduced) EnterTransition.None else scaleIn(base(), initialScale = MotionTokens.ENTER_SCALE)

  /** The way back of [enter]. */
  val exitTransition: ExitTransition =
    fadeOut(exit()) + if (isReduced) ExitTransition.None else scaleOut(exit(), targetScale = MotionTokens.ENTER_SCALE)

  /** Only a fade, for overlays where growing would distract (bars over a picture, the day pill). */
  val fadeIn: EnterTransition = fadeIn(fast())

  /** The way back of [fadeIn]. */
  val fadeOut: ExitTransition = fadeOut(exit())

  /** Where list rows slide when others arrive or leave; null without motion. */
  val maybePlacement: FiniteAnimationSpec<IntOffset>? = if (isReduced) null else base()
}

/**
 * List rows arriving, leaving and making room with the tokens: a fade, and a slide of the rows
 * around them (none with reduced motion).
 */
@Composable
fun LazyItemScope.rowMotion(): Modifier {
  val motion = LocalMotion.current
  return Modifier.animateItem(fadeInSpec = motion.fast(), placementSpec = motion.maybePlacement, fadeOutSpec = motion.exit())
}

/**
 * Pops (scales from [MotionTokens.POP_FROM_SCALE] back to full size) whenever [key] changes after
 * the first composition, e.g. a reaction's count or a badge's number. With [popsOnEnter] it also
 * pops when it first appears (a reaction just added). Drawing only: no layout pass, nothing runs
 * while it rests. Nothing pops with reduced motion.
 */
@Composable
fun Modifier.popOnChange(key: Any?, popsOnEnter: Boolean = false): Modifier {
  val motion = LocalMotion.current
  // A pill that pops in starts small, so its first frame isn't drawn at full size.
  val scale = remember { Animatable(if (popsOnEnter && !motion.isReduced) MotionTokens.POP_FROM_SCALE else 1f) }
  val last = remember { LastKey(key, isFirst = true) }
  LaunchedEffect(key) {
    val isChange = if (last.isFirst) popsOnEnter else last.key != key
    last.key = key
    last.isFirst = false
    if (!isChange || motion.isReduced) {
      scale.snapTo(1f)
      return@LaunchedEffect
    }
    scale.snapTo(MotionTokens.POP_FROM_SCALE)
    scale.animateTo(1f, motion.pop())
  }
  return graphicsLayer {
    scaleX = scale.value
    scaleY = scale.value
  }
}

/** The key [popOnChange] saw last; not state, nothing is drawn from it. */
private class LastKey(var key: Any?, var isFirst: Boolean)
