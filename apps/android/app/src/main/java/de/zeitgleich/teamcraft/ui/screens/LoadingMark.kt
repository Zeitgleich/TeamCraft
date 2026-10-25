package de.zeitgleich.teamcraft.ui.screens

import android.provider.Settings
import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.animation.core.Easing
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameMillis
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Paint
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.MarkFill
import de.zeitgleich.teamcraft.ui.theme.MarkPart
import de.zeitgleich.teamcraft.ui.theme.MarkParts

// The same choreography as the web's loading screen (crates/teamcraft-ui/style/loader.css, issue
// #463): the heads swirl in, the bodies rise, the speech bubble pops and types, the team nods
// along. Times in milliseconds from the first frame.
private val MARK_SIZE = 88.dp
private val EMPHASIZED = CubicBezierEasing(0.05f, 0.7f, 0.1f, 1f)
private val POP = CubicBezierEasing(0.34f, 1.56f, 0.64f, 1f)
private val EASE_IN_OUT = CubicBezierEasing(0.42f, 0f, 0.58f, 1f)
private val EASE_OUT = CubicBezierEasing(0f, 0f, 0.58f, 1f)
private const val SWIRL_MS = 820f
private const val SWIRL_DEGREES = -150f
private const val HEAD_START_SCALE = 0.4f
private const val RISE_MS = 640f
private const val RISE_SHARE = 0.18f
private const val POP_MS = 460f
private const val POP_DELAY_MS = 420f
private const val POP_START_SCALE = 0.6f
private const val DOT_IN_MS = 160f
private const val DOT_REST_ALPHA = 0.55f
private const val HOP_PERIOD_MS = 1200f
private const val HOP_SHARE = 0.4f
private const val NOD_PERIOD_MS = 2400f
private const val NOD_SHARE = 0.06f
/** Every part rests here: assembled, the loops not started yet. */
private const val RESTING_MS = 1040f
/** The mark has assembled (the loops start later, at 1100 ms); the handover waits for this. */
private const val ASSEMBLED_MS = 900L
/** The handover: the mark rises towards the viewer and fades with the backdrop. */
private const val DEPART_MS = 320f
private const val DEPART_SCALE = 1.15f
/** Reduced motion: no movement, the mark fades in and out. */
private const val FADE_MS = 240f

private val HEADS = listOf(MarkParts.headTop, MarkParts.headLeft, MarkParts.headRight)
private val HEAD_DELAYS_MS = listOf(0f, 70f, 140f)
private val NOD_DELAYS_MS = listOf(1300f, 1100f, 1500f)
private val DOT_IN_DELAYS_MS = listOf(760f, 820f, 880f)
private val HOP_DELAYS_MS = listOf(1100f, 1250f, 1400f)

/**
 * The loading screen of a cold start, over the app: the TeamCraft mark assembles and types while
 * [isLoading], then gets out of the way once it has assembled. Removes itself when done; a
 * screen that was not loading at first never shows it. With [maybeHandoff] it goes on from where
 * the system splash left the mark (Android 12+, MainActivity), at its place and size.
 */
@Composable
fun LoadingMarkOverlay(isLoading: Boolean, maybeHandoff: SplashHandoff? = null) {
  val reduceMotion = rememberReduceMotion()
  val loading by rememberUpdatedState(isLoading)
  val handoff by rememberUpdatedState(maybeHandoff)
  var isShown by remember { mutableStateOf(isLoading) }
  // Loading again later (a restart of the core): the mark plays again.
  LaunchedEffect(isLoading) { if (isLoading) isShown = true }
  if (!isShown) return
  // Read only while drawing, so a frame redraws without recomposing.
  val elapsed = remember { mutableLongStateOf(0L) }
  val departedAt = remember { mutableLongStateOf(-1L) }
  LaunchedEffect(Unit) {
    var origin = withFrameMillis { it }
    var maybeTakenOver: SplashHandoff? = null
    val assembled = if (reduceMotion) FADE_MS.toLong() else ASSEMBLED_MS
    val departure = if (reduceMotion) FADE_MS else DEPART_MS
    while (true) {
      val frame = withFrameMillis { it }
      // The splash handed over: the clock jumps to the splash's own.
      handoff?.takeIf { it !== maybeTakenOver }?.let {
        maybeTakenOver = it
        origin = frame - it.elapsedMs
      }
      val now = frame - origin
      elapsed.longValue = now
      if (departedAt.longValue < 0 && !loading && now >= assembled) departedAt.longValue = now
      if (departedAt.longValue >= 0 && now - departedAt.longValue >= departure) break
    }
    isShown = false
  }
  val background = MaterialTheme.colorScheme.background
  val label = stringResource(R.string.loading)
  val markSize = with(LocalDensity.current) { MARK_SIZE.toPx() }
  Canvas(
    Modifier
      .fillMaxSize()
      .graphicsLayer { alpha = 1f - departure(elapsed.longValue, departedAt.longValue, reduceMotion) }
      .background(background)
      .semantics { contentDescription = label },
  ) {
    val t = elapsed.longValue
    val gone = departure(t, departedAt.longValue, reduceMotion)
    val placed = handoff
    val centre = placed?.centre ?: center
    val side = (placed?.markSizePx ?: markSize) * if (reduceMotion) 1f else 1f + (DEPART_SCALE - 1f) * gone
    // Reduced motion fades the mark in, unless the splash already showed it.
    val alpha = if (reduceMotion && placed == null) fraction(t.toFloat(), 0f, FADE_MS) else 1f
    drawMark(t.toFloat(), reduceMotion, centre, side, alpha)
  }
}

/** The TeamCraft mark at rest, drawn like the loading screen's (the sign-in screen's tile). */
@Composable
fun StillMark(modifier: Modifier = Modifier) {
  Canvas(modifier) { drawMark(RESTING_MS, reduceMotion = false, center, size.minDimension, 1f) }
}

/** Where and when the system splash left the mark: its clock and the mark's centre and side in px. */
class SplashHandoff(val elapsedMs: Long, val centre: Offset, val markSizePx: Float)

/** Animations off in the system settings (Settings → Accessibility → Remove animations). */
@Composable
private fun rememberReduceMotion(): Boolean {
  val context = LocalContext.current
  return remember {
    Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
  }
}

private fun departure(elapsed: Long, departedAt: Long, reduceMotion: Boolean): Float {
  if (departedAt < 0) return 0f
  val duration = if (reduceMotion) FADE_MS else DEPART_MS
  return EMPHASIZED.transform(fraction((elapsed - departedAt).toFloat(), 0f, duration))
}

private fun DrawScope.drawMark(t: Float, reduceMotion: Boolean, centre: Offset, side: Float, alpha: Float) {
  val view = MarkParts.view
  val scale = side / view.width
  withTransform({
    translate(centre.x - side / 2, centre.y - side / 2)
    scale(scale, scale, pivot = Offset.Zero)
    translate(-view.left, -view.top)
  }) {
    drawContext.canvas.saveLayer(view, Paint().apply { this.alpha = alpha })
    if (reduceMotion) drawStill(t) else drawMoving(t)
    drawContext.canvas.restore()
  }
}

/** Reduced motion: the mark stands still and its dots only brighten in turn. */
private fun DrawScope.drawStill(t: Float) {
  listOf(MarkParts.bodyRight, MarkParts.bodyTop).forEach { drawPart(it) }
  drawBodyLeft()
  HEADS.forEach { drawPart(it) }
  drawPart(MarkParts.bubble)
  MarkParts.dots.forEachIndexed { index, center ->
    val phase = loopPhase(t, index * 200f, HOP_PERIOD_MS)
    val pulse = if (phase < 0.5f) phase * 2 else (1 - phase) * 2
    drawCircle(MarkParts.dotColor, MarkParts.dotRadius, center, alpha = 1f - (1f - DOT_REST_ALPHA) * pulse)
  }
}

private fun DrawScope.drawMoving(t: Float) {
  val bodies = listOf(MarkParts.bodyRight to 280f, MarkParts.bodyTop to 160f, null to 220f)
  bodies.forEach { (part, delay) ->
    val progress = EMPHASIZED.transform(fraction(t, delay, RISE_MS))
    val box = (part ?: MarkParts.bodyLeft).box
    withTransform({ translate(0f, box.height * RISE_SHARE * (1 - progress)) }) {
      if (part == null) drawBodyLeft(progress) else drawPart(part, progress)
    }
  }
  HEADS.forEachIndexed { index, head -> drawHead(head, t, HEAD_DELAYS_MS[index], NOD_DELAYS_MS[index]) }
  drawBubble(t)
}

/** A head swirls in around the mark's centre and grows into place, then nods now and then. */
private fun DrawScope.drawHead(head: MarkPart, t: Float, delay: Float, nodDelay: Float) {
  val linear = fraction(t, delay, SWIRL_MS)
  val progress = EMPHASIZED.transform(linear)
  val alpha = EMPHASIZED.transform((linear / 0.4f).coerceAtMost(1f))
  val grow = HEAD_START_SCALE + (1 - HEAD_START_SCALE) * progress
  val nod = bump(loopPhase(t, nodDelay, NOD_PERIOD_MS), 0.12f) * head.box.height * NOD_SHARE
  val view = MarkParts.view
  withTransform({
    rotate(SWIRL_DEGREES * (1 - progress), pivot = view.center)
    translate(0f, -nod)
    scale(grow, grow, pivot = head.box.center)
  }) {
    drawPart(head, alpha)
  }
}

/** The speech bubble pops from its tail, then its dots fade in and hop in turn. */
private fun DrawScope.drawBubble(t: Float) {
  val bubble = MarkParts.bubble
  val linear = fraction(t, POP_DELAY_MS, POP_MS)
  val pop = POP_START_SCALE + (1 - POP_START_SCALE) * POP.transform(linear)
  val alpha = POP.transform((linear / 0.4f).coerceAtMost(1f)).coerceIn(0f, 1f)
  val tail = Offset(bubble.box.left + bubble.box.width * 0.48f, bubble.box.bottom)
  withTransform({ scale(pop, pop, pivot = tail) }) {
    drawPart(bubble, alpha)
    MarkParts.dots.forEachIndexed { index, center ->
      val appear = EASE_OUT.transform(fraction(t, DOT_IN_DELAYS_MS[index], DOT_IN_MS)) * DOT_REST_ALPHA
      val hop = bump(loopPhase(t, HOP_DELAYS_MS[index], HOP_PERIOD_MS), 0.3f)
      val dotAlpha = if (t < HOP_DELAYS_MS[index]) appear else DOT_REST_ALPHA + (1 - DOT_REST_ALPHA) * hop
      val lift = hop * MarkParts.dotRadius * 2 * HOP_SHARE
      drawCircle(MarkParts.dotColor, MarkParts.dotRadius, center - Offset(0f, lift), alpha = dotAlpha * alpha)
    }
  }
}

/** The left body with its darker upper face. */
private fun DrawScope.drawBodyLeft(alpha: Float = 1f) {
  drawPart(MarkParts.bodyLeft, alpha)
  clipPath(MarkParts.bodyLeftShadeClip) { drawPart(MarkParts.bodyLeftShade, alpha) }
}

private fun DrawScope.drawPart(part: MarkPart, alpha: Float = 1f) {
  when (val fill = part.fill) {
    is MarkFill.Solid -> drawPath(part.path, fill.color, alpha = alpha)
    is MarkFill.Gradient -> drawPath(part.path, Brush.linearGradient(fill.colors, fill.start, fill.end), alpha = alpha)
  }
}

/** 0 before [delay], 1 after it plus [duration], linear between. */
private fun fraction(t: Float, delay: Float, duration: Float): Float = ((t - delay) / duration).coerceIn(0f, 1f)

/** Where a loop that starts at [delay] stands in its period, 0 until it starts. */
private fun loopPhase(t: Float, delay: Float, period: Float): Float = if (t < delay) 0f else ((t - delay) % period) / period

/** Up and back down within the first 2 × [half] of the loop (eased both ways), at rest after. */
private fun bump(phase: Float, half: Float, easing: Easing = EASE_IN_OUT): Float = when {
  phase < half -> easing.transform(phase / half)
  phase < 2 * half -> 1f - easing.transform((phase - half) / half)
  else -> 0f
}
