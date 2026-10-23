package de.zeitgleich.teamcraft.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext
import androidx.compose.runtime.remember
import androidx.compose.runtime.getValue
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color

/** The desktop UI's tokens (crates/teamcraft-ui/style/main.css), so both apps look alike. */
private val Light = lightColorScheme(
  primary = Color(0xFF0B7F78),
  onPrimary = Color(0xFFFFFFFF),
  primaryContainer = Color(0xFFDFF3F1),
  onPrimaryContainer = Color(0xFF096C66),
  // Selected chips and tonal buttons: the accent, not Material's default purple.
  secondaryContainer = Color(0xFFDFF3F1),
  onSecondaryContainer = Color(0xFF096C66),
  background = Color(0xFFF6F7F9),
  onBackground = Color(0xFF1B2230),
  surface = Color(0xFFFFFFFF),
  onSurface = Color(0xFF1B2230),
  onSurfaceVariant = Color(0xFF566175),
  // Sheets, menus and dialogs: neutral grays instead of Material's default lavender tint.
  surfaceVariant = Color(0xFFE9ECF1),
  surfaceContainerLowest = Color(0xFFFFFFFF),
  surfaceContainerLow = Color(0xFFF9FAFB),
  surfaceContainer = Color(0xFFF1F3F6),
  surfaceContainerHigh = Color(0xFFECEEF2),
  surfaceContainerHighest = Color(0xFFE6E9EE),
  // Field and control borders need 3:1 against the surface (WCAG 1.4.11); dividers stay light.
  outline = Color(0xFF848EA0),
  outlineVariant = Color(0xFFDCE0E7),
  error = Color(0xFFC0392B),
)

private val Dark = darkColorScheme(
  primary = Color(0xFF35C6BA),
  onPrimary = Color(0xFF04211E),
  primaryContainer = Color(0xFF123A38),
  onPrimaryContainer = Color(0xFF5FD8CE),
  secondaryContainer = Color(0xFF123A38),
  onSecondaryContainer = Color(0xFF5FD8CE),
  background = Color(0xFF0F141D),
  onBackground = Color(0xFFE4E9F2),
  surface = Color(0xFF151B27),
  onSurface = Color(0xFFE4E9F2),
  onSurfaceVariant = Color(0xFF9AA6BA),
  surfaceVariant = Color(0xFF1F2738),
  surfaceContainerLowest = Color(0xFF0B0F16),
  surfaceContainerLow = Color(0xFF131925),
  surfaceContainer = Color(0xFF182030),
  surfaceContainerHigh = Color(0xFF1D2638),
  surfaceContainerHighest = Color(0xFF232D40),
  outline = Color(0xFF626D86),
  outlineVariant = Color(0xFF28324A),
  error = Color(0xFFFF7A6B),
)

/** Colors outside Material's scheme: the dark sidebar chrome, badges, mentions. */
@Immutable
data class TeamCraftColors(
  val chrome: Color,
  val chromeText: Color,
  val chromeMuted: Color,
  val badge: Color,
  val mentionBackground: Color,
  val mentionText: Color,
)

private val LightExtra = TeamCraftColors(
  chrome = Color(0xFF1A2434),
  chromeText = Color(0xFFFFFFFF),
  chromeMuted = Color(0xFF8793A8),
  // White badge numbers need 4.5:1.
  badge = Color(0xFFC8441F),
  mentionBackground = Color(0xFFFFF2CC),
  mentionText = Color(0xFF6A4B00),
)

private val DarkExtra = LightExtra.copy(
  chrome = Color(0xFF0F1520),
  mentionBackground = Color(0xFF4A3B10),
  mentionText = Color(0xFFFFD978),
)

val LocalTeamCraftColors = staticCompositionLocalOf { LightExtra }

@Composable
fun TeamCraftTheme(content: @Composable () -> Unit) {
  val isDark = isSystemInDarkTheme()
  val context = LocalContext.current
  val isDynamic by remember { AppearancePrefs.prefs(context).isDynamicColor }.collectAsState()
  val scheme = when {
    // The wallpaper's colors (Material You); the dark chrome and badges stay TeamCraft's.
    isDynamic && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
      if (isDark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
    isDark -> Dark
    else -> Light
  }
  CompositionLocalProvider(LocalTeamCraftColors provides if (isDark) DarkExtra else LightExtra) {
    MaterialTheme(colorScheme = scheme, content = content)
  }
}
