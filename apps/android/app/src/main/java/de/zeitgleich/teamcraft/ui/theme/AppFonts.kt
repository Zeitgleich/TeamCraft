package de.zeitgleich.teamcraft.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.ExperimentalTextApi
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import de.zeitgleich.teamcraft.R

/**
 * The interface fonts (#1494), bundled in `res/font` by `scripts/build-fonts.py` (subset to Latin,
 * plus Greek and Cyrillic where the font has them; text in other scripts falls back to the system
 * font). The ids are the synced setting's wire names (`teamcraft-client-api` `font.rs`); names are
 * the fonts' own and are not translated.
 *
 * Desktop and web also offer Arial, Georgia and Verdana where installed; Android has none of them,
 * so it doesn't list them and shows the system font when another device chose one.
 */
enum class AppFontGroup { Suggested, Sans, Readability, Monospace }

data class AppFont(val id: String, val name: String, val group: AppFontGroup)

/** The fonts this app offers, in the order of the picker. The first is the system font. */
val APP_FONTS: List<AppFont> = listOf(
  AppFont(AppearancePrefs.DEFAULT_FONT, "", AppFontGroup.Suggested),
  AppFont("inter", "Inter", AppFontGroup.Suggested),
  AppFont("google-sans-flex", "Google Sans Flex", AppFontGroup.Sans),
  AppFont("ibm-plex-sans", "IBM Plex Sans", AppFontGroup.Sans),
  AppFont("source-sans-3", "Source Sans 3", AppFontGroup.Sans),
  AppFont("noto-sans", "Noto Sans", AppFontGroup.Sans),
  AppFont("atkinson-hyperlegible-next", "Atkinson Hyperlegible Next", AppFontGroup.Readability),
  AppFont("lexend", "Lexend", AppFontGroup.Readability),
  AppFont("opendyslexic", "OpenDyslexic", AppFontGroup.Readability),
  AppFont("jetbrains-mono", "JetBrains Mono", AppFontGroup.Monospace),
)

/** The weights the app uses (Tokens.kt): each a named instance of the variable font. */
private val WEIGHTS = listOf(FontWeight.Normal, FontWeight.Medium, FontWeight.SemiBold, FontWeight.Bold)

/** The family of the font with wire name [id]; `null` for the system font and unknown names. */
fun appFontFamily(id: String): FontFamily? = when (id) {
  "inter" -> variable(R.font.font_inter)
  "google-sans-flex" -> variable(R.font.font_google_sans_flex)
  "ibm-plex-sans" -> variable(R.font.font_ibm_plex_sans)
  "source-sans-3" -> variable(R.font.font_source_sans_3)
  "noto-sans" -> variable(R.font.font_noto_sans)
  "atkinson-hyperlegible-next" -> variable(R.font.font_atkinson_hyperlegible_next)
  "lexend" -> variable(R.font.font_lexend)
  "jetbrains-mono" -> variable(R.font.font_jetbrains_mono)
  // Static, a file per weight; Medium and SemiBold fall to the nearest.
  "opendyslexic" -> FontFamily(
    Font(R.font.font_opendyslexic, FontWeight.Normal),
    Font(R.font.font_opendyslexic_bold, FontWeight.Bold),
  )
  else -> null
}

// Variation settings of resource fonts were experimental in older Compose versions.
@OptIn(ExperimentalTextApi::class)
private fun variable(resource: Int) = FontFamily(
  WEIGHTS.map { weight ->
    Font(resource, weight, variationSettings = FontVariation.Settings(FontVariation.weight(weight.weight)))
  },
)

/** Every style of [this] in [family], sizes and weights as they are. */
fun Typography.withFontFamily(family: FontFamily): Typography {
  fun TextStyle.inFamily() = copy(fontFamily = family)
  return copy(
    displayLarge = displayLarge.inFamily(),
    displayMedium = displayMedium.inFamily(),
    displaySmall = displaySmall.inFamily(),
    headlineLarge = headlineLarge.inFamily(),
    headlineMedium = headlineMedium.inFamily(),
    headlineSmall = headlineSmall.inFamily(),
    titleLarge = titleLarge.inFamily(),
    titleMedium = titleMedium.inFamily(),
    titleSmall = titleSmall.inFamily(),
    bodyLarge = bodyLarge.inFamily(),
    bodyMedium = bodyMedium.inFamily(),
    bodySmall = bodySmall.inFamily(),
    labelLarge = labelLarge.inFamily(),
    labelMedium = labelMedium.inFamily(),
    labelSmall = labelSmall.inFamily(),
    displayLargeEmphasized = displayLargeEmphasized.inFamily(),
    displayMediumEmphasized = displayMediumEmphasized.inFamily(),
    displaySmallEmphasized = displaySmallEmphasized.inFamily(),
    headlineLargeEmphasized = headlineLargeEmphasized.inFamily(),
    headlineMediumEmphasized = headlineMediumEmphasized.inFamily(),
    headlineSmallEmphasized = headlineSmallEmphasized.inFamily(),
    titleLargeEmphasized = titleLargeEmphasized.inFamily(),
    titleMediumEmphasized = titleMediumEmphasized.inFamily(),
    titleSmallEmphasized = titleSmallEmphasized.inFamily(),
    bodyLargeEmphasized = bodyLargeEmphasized.inFamily(),
    bodyMediumEmphasized = bodyMediumEmphasized.inFamily(),
    bodySmallEmphasized = bodySmallEmphasized.inFamily(),
    labelLargeEmphasized = labelLargeEmphasized.inFamily(),
    labelMediumEmphasized = labelMediumEmphasized.inFamily(),
    labelSmallEmphasized = labelSmallEmphasized.inFamily(),
  )
}
