package de.zeitgleich.teamcraft.ui.theme

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.findRootCoordinates
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.unit.LayoutDirection
import androidx.core.view.WindowCompat

/** How far (px) a bar may sit from the window's edge and still count as touching it. */
private const val EDGE_TOLERANCE_PX = 1f

/**
 * What is drawn behind the status bar, so its icons stay readable: the app is edge to edge, and
 * the system's own guess (dark icons in light mode) is wrong over the dark chrome top bar.
 * Dark backdrops register while they are composed; with none, the screen's background decides.
 */
@Stable
class StatusBarBackdrop {
  // A count, not a flag: during a navigation cross-fade both screens' bars are composed.
  private var darkCount by mutableIntStateOf(0)

  val isDark: Boolean get() = darkCount > 0

  internal fun addDark() {
    darkCount++
  }

  internal fun removeDark() {
    darkCount--
  }
}

val LocalStatusBarBackdrop = staticCompositionLocalOf { StatusBarBackdrop() }

/** Light status-bar icons over a dark theme or a dark backdrop, dark icons otherwise. */
@Composable
internal fun StatusBarIcons(isDarkTheme: Boolean, backdrop: StatusBarBackdrop) {
  val view = LocalView.current
  val isLightBackground = !isDarkTheme && !backdrop.isDark
  SideEffect {
    val window = view.context.findActivity()?.window ?: return@SideEffect
    WindowCompat.getInsetsController(window, view).isAppearanceLightStatusBars = isLightBackground
  }
}

/**
 * Marks the element as a dark backdrop behind the status bar while it lies under the bar's start,
 * where the clock is. A dark bar in the detail pane of a two-pane layout doesn't count: the
 * clock sits over the list's light bar there.
 */
@Composable
fun Modifier.darkStatusBarBackdrop(): Modifier {
  val backdrop = LocalStatusBarBackdrop.current
  val direction = LocalLayoutDirection.current
  var isUnderClock by remember { mutableStateOf(false) }
  // The value at registration: onDispose runs after the state has changed.
  val isRegistered = isUnderClock
  DisposableEffect(backdrop, isRegistered) {
    if (isRegistered) backdrop.addDark()
    onDispose { if (isRegistered) backdrop.removeDark() }
  }
  return onGloballyPositioned { isUnderClock = it.isUnderStatusBarStart(direction) }
}

private fun LayoutCoordinates.isUnderStatusBarStart(direction: LayoutDirection): Boolean {
  val bounds = boundsInWindow()
  val windowWidth = findRootCoordinates().size.width
  val touchesStart = when (direction) {
    LayoutDirection.Ltr -> bounds.left <= EDGE_TOLERANCE_PX
    LayoutDirection.Rtl -> bounds.right >= windowWidth - EDGE_TOLERANCE_PX
  }
  return bounds.top <= EDGE_TOLERANCE_PX && touchesStart
}

private tailrec fun Context.findActivity(): Activity? = when (this) {
  is Activity -> this
  is ContextWrapper -> baseContext.findActivity()
  else -> null
}
