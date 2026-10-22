package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.foundation.layout.RowScope
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors

/** The dark top bar of the desktop's chrome. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChromeTopBar(
  title: String,
  maybeSubtitle: String? = null,
  maybeOnBack: (() -> Unit)? = null,
  actions: @Composable RowScope.() -> Unit = {},
) {
  val colors = LocalTeamCraftColors.current
  TopAppBar(
    title = {
      androidx.compose.foundation.layout.Column {
        Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis)
        maybeSubtitle?.takeIf { it.isNotBlank() }?.let {
          Text(
            it,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            style = androidx.compose.material3.MaterialTheme.typography.bodySmall,
            color = colors.chromeMuted,
          )
        }
      }
    },
    navigationIcon = {
      if (maybeOnBack != null) {
        IconButton(onClick = maybeOnBack) {
          Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.back))
        }
      }
    },
    actions = actions,
    colors = TopAppBarDefaults.topAppBarColors(
      containerColor = colors.chrome,
      titleContentColor = colors.chromeText,
      navigationIconContentColor = colors.chromeText,
      actionIconContentColor = colors.chromeText,
    ),
  )
}
