package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxWidth
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

/** Over this height at its top the scrim fades from clear to nearly the background. */
private val SCRIM_FADE = 24.dp

/** How much of the background the scrim lets through below its fade: messages stay a hint. */
private const val SCRIM_BODY_ALPHA = 0.88f

/**
 * The composer floating over the end of the message list (#1966): no band of its own, the list runs
 * on behind it to the bottom of the screen and fades out under a scrim from the background colour.
 * The scrim is drawn once per size, not per scroll. Reports its full height, the keyboard and the
 * gesture bar included, so the list's bottom inset keeps the newest message above it.
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
  Column(
    modifier
      .fillMaxWidth()
      .onSizeChanged { size -> onHeightChange(with(density) { size.height.toDp() }) }
      .drawBehind {
        val fade = (SCRIM_FADE.toPx() / size.height).coerceIn(0f, 1f)
        drawRect(
          Brush.verticalGradient(
            0f to background.copy(alpha = 0f),
            fade to background.copy(alpha = SCRIM_BODY_ALPHA),
            1f to background,
          ),
        )
      }
      .windowInsetsPadding(ScreenInsets.only(WindowInsetsSides.Bottom))
      .padding(top = SCRIM_FADE),
  ) {
    Column(
      Modifier.fillMaxWidth().then(if (isMotionReduced) Modifier else Modifier.animateContentSize()),
      content = content,
    )
  }
}
