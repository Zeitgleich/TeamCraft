package de.zeitgleich.teamcraft.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.MotionScheme
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
import androidx.compose.material3.ColorScheme
import androidx.compose.ui.graphics.Color
import de.zeitgleich.teamcraft.ui.rememberMotionReduced
import uniffi.teamcraft_ffi.AppThemePolicy
import uniffi.teamcraft_ffi.AppThemeTokens
import uniffi.teamcraft_ffi.BrandAccent
import uniffi.teamcraft_ffi.colorThemeEffective
import uniffi.teamcraft_ffi.colorThemeTokens
import uniffi.teamcraft_ffi.LoginBrand

/** The desktop UI's tokens (crates/teamcraft-ui/style/main/000-tokens.css), so both apps look alike. */
private val Light = lightColorScheme(
  primary = Color(0xFF005DFC),
  onPrimary = Color(0xFFFFFFFF),
  primaryContainer = Color(0xFFE3EDFF),
  onPrimaryContainer = Color(0xFF004BD3),
  // Selected chips and tonal buttons: the accent, not Material's default purple.
  secondaryContainer = Color(0xFFE3EDFF),
  onSecondaryContainer = Color(0xFF004BD3),
  background = Color(0xFFF5F5F5),
  onBackground = Color(0xFF1D1D1D),
  surface = Color(0xFFFFFFFF),
  onSurface = Color(0xFF1D1D1D),
  onSurfaceVariant = Color(0xFF545454),
  // Sheets, menus and dialogs: neutral grays instead of Material's default lavender tint.
  surfaceVariant = Color(0xFFEBEBEB),
  surfaceContainerLowest = Color(0xFFFFFFFF),
  surfaceContainerLow = Color(0xFFFAFAFA),
  surfaceContainer = Color(0xFFF2F2F2),
  surfaceContainerHigh = Color(0xFFEDEDED),
  surfaceContainerHighest = Color(0xFFE6E6E6),
  // Field and control borders need 3:1 against the surface (WCAG 1.4.11); dividers stay light.
  outline = Color(0xFF858585),
  outlineVariant = Color(0xFFDDDDDD),
  error = Color(0xFFC0392B),
  errorContainer = Color(0xFFFFDAD5),
  onErrorContainer = Color(0xFF8C1D12),
  // The roles below are Material's lavender by default; expressive components (toggle buttons,
  // FAB menus, toolbars, loading indicators) use them, so they derive from the brand blue too.
  inversePrimary = Color(0xFFB2C5FF),
  secondary = Color(0xFF545F71),
  onSecondary = Color(0xFFFFFFFF),
  tertiary = Color(0xFF00677F),
  onTertiary = Color(0xFFFFFFFF),
  tertiaryContainer = Color(0xFFB7EAFF),
  onTertiaryContainer = Color(0xFF004D61),
  surfaceTint = Color(0xFF005DFC),
  inverseSurface = Color(0xFF2F2F2F),
  inverseOnSurface = Color(0xFFF2F2F2),
  surfaceBright = Color(0xFFFFFFFF),
  surfaceDim = Color(0xFFDCDCDC),
  primaryFixed = FixedRoles.primaryFixed,
  primaryFixedDim = FixedRoles.primaryFixedDim,
  onPrimaryFixed = FixedRoles.onPrimaryFixed,
  onPrimaryFixedVariant = FixedRoles.onPrimaryFixedVariant,
  secondaryFixed = FixedRoles.secondaryFixed,
  secondaryFixedDim = FixedRoles.secondaryFixedDim,
  onSecondaryFixed = FixedRoles.onSecondaryFixed,
  onSecondaryFixedVariant = FixedRoles.onSecondaryFixedVariant,
  tertiaryFixed = FixedRoles.tertiaryFixed,
  tertiaryFixedDim = FixedRoles.tertiaryFixedDim,
  onTertiaryFixed = FixedRoles.onTertiaryFixed,
  onTertiaryFixedVariant = FixedRoles.onTertiaryFixedVariant,
)

private val Dark = darkColorScheme(
  primary = Color(0xFF5C9DFF),
  onPrimary = Color(0xFF04122E),
  primaryContainer = Color(0xFF182A47),
  onPrimaryContainer = Color(0xFF8AB8FF),
  secondaryContainer = Color(0xFF182A47),
  onSecondaryContainer = Color(0xFF8AB8FF),
  background = Color(0xFF161616),
  onBackground = Color(0xFFF5F5F5),
  surface = Color(0xFF1E1E1E),
  onSurface = Color(0xFFF5F5F5),
  onSurfaceVariant = Color(0xFFB0B0B0),
  surfaceVariant = Color(0xFF2A2A2A),
  surfaceContainerLowest = Color(0xFF121212),
  surfaceContainerLow = Color(0xFF1B1B1B),
  surfaceContainer = Color(0xFF222222),
  surfaceContainerHigh = Color(0xFF282828),
  surfaceContainerHighest = Color(0xFF303030),
  outline = Color(0xFF7A7A7A),
  outlineVariant = Color(0xFF363636),
  error = Color(0xFFFF7A6B),
  errorContainer = Color(0xFF5C1A13),
  onErrorContainer = Color(0xFFFFDAD5),
  inversePrimary = Color(0xFF005DFC),
  secondary = Color(0xFFBCC7DC),
  onSecondary = Color(0xFF263141),
  tertiary = Color(0xFF5DD5FC),
  onTertiary = Color(0xFF003544),
  tertiaryContainer = Color(0xFF004D61),
  onTertiaryContainer = Color(0xFFB7EAFF),
  surfaceTint = Color(0xFF5C9DFF),
  inverseSurface = Color(0xFFE6E6E6),
  inverseOnSurface = Color(0xFF2A2A2A),
  surfaceBright = Color(0xFF3A3A3A),
  surfaceDim = Color(0xFF161616),
  primaryFixed = FixedRoles.primaryFixed,
  primaryFixedDim = FixedRoles.primaryFixedDim,
  onPrimaryFixed = FixedRoles.onPrimaryFixed,
  onPrimaryFixedVariant = FixedRoles.onPrimaryFixedVariant,
  secondaryFixed = FixedRoles.secondaryFixed,
  secondaryFixedDim = FixedRoles.secondaryFixedDim,
  onSecondaryFixed = FixedRoles.onSecondaryFixed,
  onSecondaryFixedVariant = FixedRoles.onSecondaryFixedVariant,
  tertiaryFixed = FixedRoles.tertiaryFixed,
  tertiaryFixedDim = FixedRoles.tertiaryFixedDim,
  onTertiaryFixed = FixedRoles.onTertiaryFixed,
  onTertiaryFixedVariant = FixedRoles.onTertiaryFixedVariant,
)

/**
 * Material's "fixed" roles keep their tone in light and dark (FAB menus, vibrant toolbars); tones of
 * the brand blue (primary), a blue gray (secondary) and a cyan (tertiary).
 */
private object FixedRoles {
  val primaryFixed = Color(0xFFDAE2FF)
  val primaryFixedDim = Color(0xFFB2C5FF)
  val onPrimaryFixed = Color(0xFF001849)
  val onPrimaryFixedVariant = Color(0xFF0040A8)
  val secondaryFixed = Color(0xFFD8E3F8)
  val secondaryFixedDim = Color(0xFFBCC7DC)
  val onSecondaryFixed = Color(0xFF111C2B)
  val onSecondaryFixedVariant = Color(0xFF3C4758)
  val tertiaryFixed = Color(0xFFB7EAFF)
  val tertiaryFixedDim = Color(0xFF5DD5FC)
  val onTertiaryFixed = Color(0xFF001F28)
  val onTertiaryFixedVariant = Color(0xFF004D61)
}

/** Colors outside Material's scheme: the dark sidebar chrome, badges, mentions. */
@Immutable
data class TeamCraftColors(
  /** The main screen's header and the status bar behind it; a colour theme (#202) sets it. */
  val chrome: Color,
  /** White text on strong fills (badges). */
  val chromeText: Color,
  /** Text and icons on [chrome]: 4.5:1. */
  val onChrome: Color,
  val chromeMuted: Color,
  val badge: Color,
  /** The "online" dot. */
  val presence: Color,
  val mentionBackground: Color,
  val mentionText: Color,
)

private val LightExtra = TeamCraftColors(
  chrome = Color(0xFF1A2434),
  chromeText = Color(0xFFFFFFFF),
  onChrome = Color(0xFFFFFFFF),
  chromeMuted = Color(0xFF8793A8),
  // White badge numbers need 4.5:1.
  badge = Color(0xFFC8441F),
  presence = Color(0xFF2E9E5B),
  mentionBackground = Color(0xFFFFF2CC),
  mentionText = Color(0xFF6A4B00),
)

private val DarkExtra = LightExtra.copy(
  chrome = Color(0xFF141414),
  mentionBackground = Color(0xFF4A3B10),
  mentionText = Color(0xFFFFD978),
  presence = Color(0xFF34C759),
)

val LocalTeamCraftColors = staticCompositionLocalOf { LightExtra }

/** The brand's rules for colour themes (branding.md §5.1); `null` without a brand. */
val LocalThemePolicy = staticCompositionLocalOf<AppThemePolicy?> { null }

/**
 * The app's theme: Material 3 Expressive (issue #454) with TeamCraft's colours. Motion is the
 * expressive spring scheme, or the standard one without overshoot when the system's "remove
 * animations" is on. [maybeBrand]: the server's brand, whose accent colors (branding.md §4) replace
 * TeamCraft's teal; status, danger and text colors stay the app's own. [isAlwaysDark]: the dark
 * scheme whatever the system's mode, for screens that are dark by design (the call), so their
 * components and the status bar's icons match the dark stage.
 */
@Composable
fun TeamCraftTheme(maybeBrand: LoginBrand? = null, isAlwaysDark: Boolean = false, content: @Composable () -> Unit) {
  val isDark = isAlwaysDark || isSystemInDarkTheme()
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val isDynamic by prefs.isDynamicColor.collectAsState()
  val colorTheme by prefs.colorThemeValue.collectAsState()
  val font by prefs.fontValue.collectAsState()
  val isMotionReduced = rememberMotionReduced()
  val typography = remember(font) { appFontFamily(font)?.let(TeamCraftTypography::withFontFamily) ?: TeamCraftTypography }
  val maybePolicy = maybeBrand?.themePolicy
  // A chosen colour theme (#202): the accent, selection, badge and presence; surfaces stay neutral.
  // One the brand doesn't allow shows its default; the stored value stays (#1495).
  val maybeTheme = remember(colorTheme, isDark, maybePolicy) {
    colorThemeTokens(maybePolicy?.let { colorThemeEffective(colorTheme, it) } ?: colorTheme, isDark)
  }
  val scheme = when {
    maybeTheme != null -> (if (isDark) Dark else Light).withTheme(maybeTheme)
    // The wallpaper's colors (Material You); the dark chrome and badges stay TeamCraft's.
    isDynamic && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
      if (isDark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
    isDark -> Dark.withAccent(maybeBrand?.dark)
    else -> Light.withAccent(maybeBrand?.light)
  }
  val extra = (if (isDark) DarkExtra else LightExtra).let { base ->
    maybeTheme?.let { base.withTheme(it) } ?: base
  }
  val backdrop = remember { StatusBarBackdrop() }
  StatusBarIcons(isDarkTheme = isDark, backdrop = backdrop)
  CompositionLocalProvider(
    LocalTeamCraftColors provides extra,
    LocalStatusBarBackdrop provides backdrop,
    LocalThemePolicy provides maybePolicy,
  ) {
    MaterialExpressiveTheme(
      colorScheme = scheme,
      motionScheme = if (isMotionReduced) MotionScheme.standard() else MotionScheme.expressive(),
      typography = typography,
      // No shapes: Material's own, as Google's apps use them (segmented lists, dialogs, buttons).
      content = content,
    )
  }
}

/** A colour theme's accent tokens over the scheme (the core checked their contrast). */
private fun ColorScheme.withTheme(theme: AppThemeTokens): ColorScheme = copy(
  primary = rgb(theme.accent),
  surfaceTint = rgb(theme.accent),
  onPrimary = rgb(theme.onAccent),
  primaryContainer = rgb(theme.accentSoft),
  onPrimaryContainer = rgb(theme.accentStrong),
  secondaryContainer = rgb(theme.accentSoft),
  onSecondaryContainer = rgb(theme.accentStrong),
)

private fun TeamCraftColors.withTheme(theme: AppThemeTokens) = copy(
  chrome = rgb(theme.chrome),
  onChrome = rgb(theme.onChrome),
  badge = rgb(theme.badge),
  presence = rgb(theme.presence),
)

/** An opaque color from the core's `0xRRGGBB`. */
fun rgb(value: UInt): Color = Color(0xFF000000L or value.toLong())

/** The brand's accent tokens over the scheme; each one only where the brand sets it. */
private fun ColorScheme.withAccent(maybeAccent: BrandAccent?): ColorScheme {
  val accent = maybeAccent ?: return this
  fun pick(maybeHex: String?, fallback: Color) = maybeHex?.let(::brandArgb)?.let { Color(it) } ?: fallback
  val soft = pick(accent.maybeAccentSoft, primaryContainer)
  val strong = pick(accent.maybeAccentStrong, onPrimaryContainer)
  val accentColor = pick(accent.maybeAccent, primary)
  return copy(
    primary = accentColor,
    surfaceTint = accentColor,
    onPrimary = pick(accent.maybeOnAccent, onPrimary),
    primaryContainer = soft,
    onPrimaryContainer = strong,
    secondaryContainer = soft,
    onSecondaryContainer = strong,
  )
}

/**
 * An opaque ARGB value from a brand color (`#rgb` or `#rrggbb`, already checked by the core);
 * null for anything else.
 */
fun brandArgb(hex: String): Long? {
  if (!hex.startsWith("#")) return null
  val digits = hex.substring(1)
  if (digits.any { Character.digit(it, 16) < 0 }) return null
  val full = when (digits.length) {
    3 -> digits.map { "$it$it" }.joinToString("")
    6 -> digits
    else -> return null
  }
  return 0xFF000000L or full.toLong(16)
}
