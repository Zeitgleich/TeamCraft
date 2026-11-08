package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.APP_FONTS
import de.zeitgleich.teamcraft.ui.theme.AppFont
import de.zeitgleich.teamcraft.ui.theme.AppFontGroup
import de.zeitgleich.teamcraft.ui.theme.AppearancePrefs
import de.zeitgleich.teamcraft.ui.theme.appFontFamily

/**
 * The interface font (#1494): a row showing the chosen font in its own face, and a menu of the
 * fonts, each in its own face, grouped. Follows the account; applies at once (Theme.kt).
 */
@Composable
fun FontSetting() {
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val value by prefs.fontValue.collectAsState()
  var isOpen by remember { mutableStateOf(false) }
  val chosen = APP_FONTS.firstOrNull { it.id == value } ?: APP_FONTS.first()
  Box {
    ListItem(
      headlineContent = { Text(stringResource(R.string.font)) },
      supportingContent = {
        Column {
          Text(fontName(chosen), style = inFont(chosen, MaterialTheme.typography.bodyLarge))
          Text(
            stringResource(R.string.font_hint),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
          )
        }
      },
      modifier = Modifier.clickable(role = Role.DropdownList) { isOpen = true },
    )
    DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
      AppFontGroup.entries.forEachIndexed { index, group ->
        if (index > 0) HorizontalDivider()
        Text(
          stringResource(groupTitle(group)),
          style = MaterialTheme.typography.labelMedium,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp).semantics { heading() },
        )
        for (font in APP_FONTS.filter { it.group == group }) {
          val isChosen = font == chosen
          DropdownMenuItem(
            text = {
              Text(
                fontName(font),
                style = inFont(font, MaterialTheme.typography.bodyLarge),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
              )
            },
            trailingIcon = if (isChosen) {
              { Icon(Icons.Filled.Check, contentDescription = null) }
            } else {
              null
            },
            onClick = {
              prefs.setFont(font.id)
              isOpen = false
            },
            modifier = Modifier.semantics { selected = isChosen },
          )
        }
      }
    }
  }
}

@Composable
private fun fontName(font: AppFont): String =
  if (font.id == AppearancePrefs.DEFAULT_FONT) stringResource(R.string.font_system) else font.name

private fun inFont(font: AppFont, style: TextStyle): TextStyle =
  appFontFamily(font.id)?.let { style.copy(fontFamily = it) } ?: style

private fun groupTitle(group: AppFontGroup): Int = when (group) {
  AppFontGroup.Suggested -> R.string.font_group_suggested
  AppFontGroup.Sans -> R.string.font_group_sans
  AppFontGroup.Readability -> R.string.font_group_readability
  AppFontGroup.Monospace -> R.string.font_group_monospace
}
