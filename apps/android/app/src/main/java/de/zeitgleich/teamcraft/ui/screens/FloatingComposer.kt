package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.ui.rememberMotionReduced

/** Room above the composer where the scrim begins, still clear. */
private val SCRIM_LEAD = 24.dp

/**
 * The scrim's opacity down its height, as (fraction of the visible height, alpha): clear at the
 * top, denser toward the bottom edge, never solid. Messages stay legible as a fading hint behind
 * the composer and down into the navigation bar, as in common chat apps.
 */
private val SCRIM_STOPS = listOf(0f to 0f, 0.3f to 0.25f, 0.65f to 0.6f, 1f to 0.9f)

/**
 * The composer floating over the end of the message list (#1966): no band of its own, the list runs
 * on behind it to the bottom of the screen and fades out under a scrim from the background colour.
 * The scrim is one gradient from clear above the composer to nearly opaque at the screen's bottom
 * edge (or the keyboard's top): no solid band behind the gesture or button bar. It is redrawn per
 * size and keyboard position, not per scroll. Reports its full height, the keyboard and the
 * navigation bar included, so the list's bottom inset keeps the newest message above it.
 *
 * Only the composer's own height change animates (a second line, a reply bar, attachments); the
 * keyboard's inset follows the keyboard itself.
 */
@Composable
fun FloatingComposer(
  background: Color,
  onHeightChange: (Dp) -> Unit,
  modifier: Modifier = Modifier,
  content: @Composable ColumnScope.() -> Unit,
) {
  val density = LocalDensity.current
  val isMotionReduced = rememberMotionReduced()
  val ime = WindowInsets.ime
  Column(
    modifier
      .fillMaxWidth()
      .onSizeChanged { size -> onHeightChange(with(density) { size.height.toDp() }) }
      .drawBehind {
        // The keyboard covers the bottom of this column: the gradient ends at its top edge.
        val visibleBottom = (size.height - ime.getBottom(this)).coerceAtLeast(1f)
        val stops = SCRIM_STOPS.map { (at, alpha) -> at to background.copy(alpha = alpha) }.toTypedArray()
        drawRect(Brush.verticalGradient(*stops, startY = 0f, endY = visibleBottom))
      }
      .windowInsetsPadding(ScreenInsets.only(WindowInsetsSides.Bottom))
      .padding(top = SCRIM_LEAD),
  ) {
    Column(
      Modifier.fillMaxWidth().then(if (isMotionReduced) Modifier else Modifier.animateContentSize()),
      content = content,
    )
  }
}
