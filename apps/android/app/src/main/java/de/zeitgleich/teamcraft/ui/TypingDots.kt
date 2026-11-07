package de.zeitgleich.teamcraft.ui

import android.provider.Settings
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.keyframes
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** One pulse of a dot, and the delay between neighbours (#882). */
private const val DOT_CYCLE_MS = 1_200
private const val DOT_STAGGER_MS = 160
private const val DOTS = 3

/** Alpha of a dot at rest: all three at this level when motion is reduced. */
internal const val DOT_REST_ALPHA = 0.6f

/**
 * Three dots that pulse one after the other while someone types, in the chat list's row and the
 * header. They stay still when the user turned animations off (accessibility or developer
 * settings). Decorative: the text beside them says what is going on.
 */
@Composable
fun TypingDots(modifier: Modifier = Modifier, color: Color = MaterialTheme.colorScheme.primary, dotSize: Dp = 5.dp) {
  val isReduced = rememberReducedMotion()
  val transition = if (isReduced) null else rememberInfiniteTransition(label = "typing-dots")
  val alphas = List(DOTS) { index ->
    if (transition == null) {
      StillAlpha
    } else {
      transition.animateFloat(
        initialValue = DOT_REST_ALPHA / 2,
        targetValue = DOT_REST_ALPHA / 2,
        animationSpec = infiniteRepeatable(
          keyframes<Float> {
            durationMillis = DOT_CYCLE_MS
            DOT_REST_ALPHA / 2 at index * DOT_STAGGER_MS using LinearEasing
            1f at index * DOT_STAGGER_MS + DOT_CYCLE_MS / 4 using LinearEasing
            DOT_REST_ALPHA / 2 at index * DOT_STAGGER_MS + DOT_CYCLE_MS / 2 using LinearEasing
          },
          RepeatMode.Restart,
        ),
        label = "typing-dot-$index",
      )
    }
  }
  Row(
    modifier.clearAndSetSemantics {}.height(dotSize),
    horizontalArrangement = Arrangement.spacedBy(dotSize / 2),
  ) {
    for (alpha in alphas) {
      // The alpha is read while drawing: a pulse redraws the dots, it doesn't recompose the row.
      Canvas(Modifier.size(dotSize)) { drawCircle(color.copy(alpha = alpha.value)) }
    }
  }
}

private val StillAlpha: State<Float> = object : State<Float> {
  override val value: Float get() = DOT_REST_ALPHA
}

/** Animations are off when the user turned them off (accessibility or developer settings). */
@Composable
internal fun rememberReducedMotion(): Boolean {
  val resolver = LocalContext.current.contentResolver
  return remember(resolver) {
    Settings.Global.getFloat(resolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
  }
}
