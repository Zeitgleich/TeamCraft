package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.notifications.MentionPrefs
import de.zeitgleich.teamcraft.search.SegmentPrefetch
import de.zeitgleich.teamcraft.ui.MainViewModel

/** Mentions decided on this device, as on desktop: keywords, first name, `@channel`. */
@Composable
fun MentionSettingsSection(viewModel: MainViewModel) {
  val prefs = viewModel.mentionPrefs
  val settings by prefs.settings.collectAsState()
  var keywords by remember { mutableStateOf(settings.keywords.joinToString(", ")) }
  var confirmOver by remember { mutableStateOf(prefs.confirmChannelOver.toString()) }
  Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
    SwitchRow(R.string.mention_first_name, settings.isFirstNameMention) {
      prefs.update(settings.copy(isFirstNameMention = it))
    }
    SwitchRow(R.string.mention_channel, settings.doChannelMentionsNotify) {
      prefs.update(settings.copy(doChannelMentionsNotify = it))
    }
    OutlinedTextField(
      value = keywords,
      onValueChange = {
        keywords = it
        prefs.update(settings.copy(keywords = MentionPrefs.parseKeywords(it)))
      },
      label = { Text(stringResource(R.string.mention_keywords)) },
      supportingText = { Text(stringResource(R.string.mention_keywords_hint)) },
      modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
      value = confirmOver,
      onValueChange = { text ->
        confirmOver = text.filter { it.isDigit() }.take(MAX_COUNT_DIGITS)
        prefs.setConfirmChannelOver(confirmOver.toIntOrNull() ?: 0)
      },
      label = { Text(stringResource(R.string.mention_confirm_over)) },
      supportingText = { Text(stringResource(R.string.mention_confirm_over_hint)) },
      keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
      singleLine = true,
      modifier = Modifier.fillMaxWidth(),
    )
  }
}

/** Older history searchable offline: segments downloaded on Wi-Fi while charging, with a cap. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun SearchPrefetchSection(prefetch: SegmentPrefetch) {
  var isEnabled by remember { mutableStateOf(prefetch.isEnabled) }
  var maxMegabytes by remember { mutableStateOf(prefetch.maxMegabytes) }
  Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
    SwitchRow(R.string.search_prefetch, isEnabled) {
      isEnabled = it
      prefetch.update(it, maxMegabytes)
    }
    Text(stringResource(R.string.search_prefetch_hint), style = MaterialTheme.typography.bodySmall)
    if (isEnabled) {
      FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for (choice in SegmentPrefetch.CHOICES_MB) {
          FilterChip(
            selected = maxMegabytes == choice,
            onClick = {
              maxMegabytes = choice
              prefetch.update(isEnabled, choice)
            },
            label = { Text(stringResource(R.string.search_prefetch_cap, choice)) },
          )
        }
      }
    }
  }
}

@Composable
private fun SwitchRow(label: Int, isChecked: Boolean, onChange: (Boolean) -> Unit) {
  Row(verticalAlignment = Alignment.CenterVertically) {
    Switch(checked = isChecked, onCheckedChange = onChange)
    Text(stringResource(label), style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(start = 8.dp))
  }
}

private const val MAX_COUNT_DIGITS = 5
