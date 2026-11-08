package de.zeitgleich.teamcraft.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.AppearancePrefs
import de.zeitgleich.teamcraft.ui.theme.brandArgb
import de.zeitgleich.teamcraft.ui.theme.rgb
import uniffi.teamcraft_ffi.AppThemePreset
import uniffi.teamcraft_ffi.colorThemeColors
import uniffi.teamcraft_ffi.colorThemeCustom
import uniffi.teamcraft_ffi.colorThemeFromCode
import uniffi.teamcraft_ffi.colorThemePresets
import uniffi.teamcraft_ffi.colorThemeShareCode
import uniffi.teamcraft_ffi.colorThemeSurprise
import uniffi.teamcraft_ffi.colorThemeTokens
import kotlin.random.Random

/** The app's own colours (navigation, selection, presence, notifications), where custom starts. */
private val PRODUCT_COLORS = listOf(0x1A2434u, 0x0064FCu, 0x2EB67Du, 0xC8441Du)

/**
 * The colour theme (#202): the server's default, a preset, or a custom theme of four colours. It
 * colours the accent, selections, badges and the online dot; message surfaces stay neutral. It
 * follows the account; light or dark stays with the device.
 */
@Composable
fun ColorThemeSetting() {
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val value by prefs.colorThemeValue.collectAsState()
  var isCustom by remember { mutableStateOf(value.startsWith(CUSTOM_PREFIX)) }
  Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
    Text(
      stringResource(R.string.color_theme),
      style = MaterialTheme.typography.titleMedium,
      modifier = Modifier.semantics { heading() },
    )
    Text(
      stringResource(R.string.color_theme_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      FilterChip(
        selected = !isCustom,
        onClick = { isCustom = false },
        label = { Text(stringResource(R.string.color_theme_presets)) },
      )
      FilterChip(
        selected = isCustom,
        onClick = {
          isCustom = true
          // Custom starts from what is on screen, so the switch itself changes nothing.
          if (!value.startsWith(CUSTOM_PREFIX)) {
            colorThemeCustom(colorThemeColors(value) ?: PRODUCT_COLORS)?.let(prefs::setColorTheme)
          }
        },
        label = { Text(stringResource(R.string.color_theme_custom)) },
      )
    }
    if (isCustom) CustomTheme(value, prefs::setColorTheme) else PresetGallery(value, prefs::setColorTheme)
  }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun PresetGallery(value: String, onChoose: (String) -> Unit) {
  val presets = remember { colorThemePresets() }
  val isDark = isSystemInDarkTheme()
  Column(Modifier.selectableGroup(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
    Swatch(
      name = stringResource(R.string.color_theme_brand),
      colors = SwatchColors(
        navigation = if (isDark) Color(0xFF141414) else Color(0xFF1A2434),
        selected = Color(0xFF0064FC),
        presence = Color(0xFF2EB67D),
        badge = Color(0xFFC8441F),
      ),
      isSelected = value == AppearancePrefs.DEFAULT_COLOR_THEME,
      onClick = { onChoose(AppearancePrefs.DEFAULT_COLOR_THEME) },
    )
    for ((group, label) in GROUPS) {
      Text(stringResource(label), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
      FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        for (preset in presets.filter { it.group == group }) {
          val tokens = remember(preset.value, isDark) { colorThemeTokens(preset.value, isDark) } ?: continue
          Swatch(
            name = stringResource(presetName(preset)),
            colors = SwatchColors(rgb(tokens.chrome), rgb(tokens.selected), rgb(tokens.presence), rgb(tokens.badge)),
            isSelected = value == preset.value,
            onClick = { onChoose(preset.value) },
          )
        }
      }
    }
  }
}

private data class SwatchColors(val navigation: Color, val selected: Color, val presence: Color, val badge: Color)

/** A tiny sidebar in the theme's colours (a selected row, the online dot, a badge) and its name. */
@Composable
private fun Swatch(name: String, colors: SwatchColors, isSelected: Boolean, onClick: () -> Unit) {
  val ring = if (isSelected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outlineVariant
  Column(
    Modifier
      .width(76.dp)
      .selectable(selected = isSelected, role = Role.RadioButton, onClick = onClick),
    horizontalAlignment = Alignment.CenterHorizontally,
    verticalArrangement = Arrangement.spacedBy(4.dp),
  ) {
    Box(
      Modifier
        .size(76.dp, 50.dp)
        .border(if (isSelected) 2.dp else 1.dp, ring, RoundedCornerShape(12.dp))
        .padding(if (isSelected) 3.dp else 1.dp)
        .background(colors.navigation, RoundedCornerShape(10.dp))
        .padding(8.dp),
    ) {
      Box(Modifier.fillMaxWidth().height(8.dp).background(colors.selected, RoundedCornerShape(3.dp)))
      Box(Modifier.align(Alignment.BottomStart).size(8.dp).background(colors.presence, CircleShape))
      Box(Modifier.align(Alignment.BottomEnd).size(14.dp, 8.dp).background(colors.badge, CircleShape))
    }
    Text(
      name,
      style = if (isSelected) MaterialTheme.typography.labelMedium else MaterialTheme.typography.labelSmall,
      maxLines = 2,
      textAlign = TextAlign.Center,
      overflow = TextOverflow.Ellipsis,
    )
  }
}

/**
 * Four hex fields; a valid change applies at once. A field shows its error once it was left
 * (forms.md); the theme code can be copied, pasted, or rolled with "Surprise me".
 */
@Composable
private fun CustomTheme(value: String, onChoose: (String) -> Unit) {
  val context = LocalContext.current
  val drafts = remember { mutableStateListOf<String>() }
  val wasLeft = remember { mutableStateListOf(false, false, false, false) }
  // The theme changed elsewhere (another device, the code, "Surprise me"): show its colours.
  LaunchedEffect(value) {
    val colors = (colorThemeColors(value) ?: PRODUCT_COLORS).map(::hex)
    if (drafts.size != colors.size) {
      drafts.clear()
      drafts.addAll(colors)
    } else {
      colors.forEachIndexed { index, color -> if (parseHex(drafts[index]) != parseHex(color)) drafts[index] = color }
    }
  }
  if (drafts.size != SLOTS.size) return
  Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
    Text(
      stringResource(R.string.color_theme_custom_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    SLOTS.forEachIndexed { index, label ->
      val maybeColor = parseHex(drafts[index])
      val isError = wasLeft[index] && maybeColor == null
      OutlinedTextField(
        value = drafts[index],
        onValueChange = { text ->
          drafts[index] = text.take(7)
          val colors = drafts.map(::parseHex)
          if (colors.all { it != null }) colorThemeCustom(colors.filterNotNull())?.let(onChoose)
        },
        label = { Text(stringResource(label)) },
        leadingIcon = {
          Box(
            Modifier
              .size(20.dp)
              .background(maybeColor?.let(::rgb) ?: Color.Transparent, CircleShape)
              .border(1.dp, MaterialTheme.colorScheme.outline, CircleShape),
          )
        },
        isError = isError,
        supportingText = if (isError) {
          { Text(stringResource(R.string.color_theme_hex_invalid)) }
        } else {
          null
        },
        singleLine = true,
        textStyle = MaterialTheme.typography.bodyLarge.copy(fontFamily = FontFamily.Monospace),
        modifier = Modifier
          .fillMaxWidth()
          .onFocusChanged { if (!it.isFocused && drafts[index].isNotEmpty()) wasLeft[index] = true },
      )
    }
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 4.dp)) {
      OutlinedButton(onClick = {
        colorThemeShareCode(value)?.let { code ->
          context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(code, code))
        }
      }) { Text(stringResource(R.string.color_theme_copy)) }
      OutlinedButton(onClick = {
        wasLeft.indices.forEach { wasLeft[it] = false }
        onChoose(colorThemeSurprise(Random.nextDouble(360.0)))
      }) { Text(stringResource(R.string.color_theme_surprise)) }
    }
    ThemeCodeImport(onChoose)
  }
}

/** Paste a theme code someone shared; a code that isn't one is refused at the field. */
@Composable
private fun ThemeCodeImport(onChoose: (String) -> Unit) {
  var code by remember { mutableStateOf("") }
  var isRefused by remember { mutableStateOf(false) }
  val focus = remember { FocusRequester() }
  val submit: () -> Unit = {
    val maybeValue = colorThemeFromCode(code)
    if (maybeValue == null) {
      isRefused = true
      focus.requestFocus()
    } else {
      onChoose(maybeValue)
      code = ""
      isRefused = false
    }
  }
  Row(verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 4.dp)) {
    OutlinedTextField(
      value = code,
      onValueChange = {
        code = it.take(64)
        isRefused = false
      },
      label = { Text(stringResource(R.string.color_theme_import)) },
      placeholder = { Text("#1b2233,#2f66e0,#2eb67d,#d1453b") },
      isError = isRefused,
      supportingText = if (isRefused) {
        { Text(stringResource(R.string.color_theme_import_invalid)) }
      } else {
        null
      },
      singleLine = true,
      keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
      keyboardActions = KeyboardActions(onDone = { submit() }),
      modifier = Modifier.weight(1f).focusRequester(focus),
    )
    OutlinedButton(onClick = submit, enabled = code.isNotBlank(), modifier = Modifier.padding(top = 8.dp)) {
      Text(stringResource(R.string.color_theme_import_apply))
    }
  }
}

/** `#rgb` or `#rrggbb`, the `#` optional, as `0xRRGGBB`; null for anything else. */
private fun parseHex(text: String): UInt? {
  val trimmed = text.trim()
  val withHash = if (trimmed.startsWith("#")) trimmed else "#$trimmed"
  return brandArgb(withHash)?.let { (it and 0xFFFFFFL).toUInt() }
}

private fun hex(value: UInt) = "#%06x".format(value.toInt() and 0xFFFFFF)

private const val CUSTOM_PREFIX = "custom:"

private val SLOTS = listOf(
  R.string.color_theme_navigation,
  R.string.color_theme_selection,
  R.string.color_theme_presence,
  R.string.color_theme_notification,
)

private val GROUPS = listOf(
  "calm" to R.string.color_theme_group_calm,
  "bright" to R.string.color_theme_group_bright,
  "vivid" to R.string.color_theme_group_vivid,
  "accessible" to R.string.color_theme_group_accessible,
)

private fun presetName(preset: AppThemePreset): Int = when (preset.id) {
  "harbor" -> R.string.color_theme_harbor
  "graphite" -> R.string.color_theme_graphite
  "pine" -> R.string.color_theme_pine
  "deep-sea" -> R.string.color_theme_deep_sea
  "heather" -> R.string.color_theme_heather
  "paper" -> R.string.color_theme_paper
  "dune" -> R.string.color_theme_dune
  "meadow" -> R.string.color_theme_meadow
  "lavender" -> R.string.color_theme_lavender
  "ember" -> R.string.color_theme_ember
  "orchid" -> R.string.color_theme_orchid
  "lagoon" -> R.string.color_theme_lagoon
  "indigo" -> R.string.color_theme_indigo
  "red-green-safe" -> R.string.color_theme_red_green_safe
  "blue-yellow-safe" -> R.string.color_theme_blue_yellow_safe
  "high-contrast" -> R.string.color_theme_high_contrast
  else -> R.string.color_theme_custom
}
