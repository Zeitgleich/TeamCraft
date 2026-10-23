package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.security.LockAfter
import de.zeitgleich.teamcraft.ui.ComposerPrefs

/** App lock on or off, and after how long in the background it locks again. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun AppLockSetting() {
  val context = LocalContext.current
  val prefs = remember { AppLockPrefs.prefs(context) }
  val isAvailable = remember { AppLockPrefs.isAvailable(context) }
  var isEnabled by remember { mutableStateOf(prefs.isEnabled) }
  var lockAfter by remember { mutableStateOf(prefs.lockAfter) }
  Column {
    Row(verticalAlignment = Alignment.CenterVertically) {
      Switch(
        checked = isEnabled,
        enabled = isAvailable || isEnabled,
        onCheckedChange = {
          isEnabled = it
          prefs.setEnabled(it)
          // Notifications already shown follow the new setting at once.
          TeamCraftApplication.graph(context).notifier.repostAll()
        },
      )
      Text(stringResource(R.string.app_lock), modifier = Modifier.padding(start = 8.dp))
    }
    if (!isAvailable) {
      Text(
        stringResource(R.string.app_lock_unavailable),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    if (isEnabled) {
      Text(stringResource(R.string.app_lock_after), style = MaterialTheme.typography.bodySmall)
      FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for ((choice, label) in LOCK_AFTER_CHOICES) {
          FilterChip(
            selected = lockAfter == choice,
            onClick = {
              lockAfter = choice
              prefs.setLockAfter(choice)
            },
            label = { Text(stringResource(label)) },
          )
        }
      }
    }
  }
}

/** Enter sends (hardware keyboards); Shift+Enter is a new line then. */
@Composable
fun SendWithEnterSetting() {
  val context = LocalContext.current
  val prefs = remember { ComposerPrefs.prefs(context) }
  var isOn by remember { mutableStateOf(prefs.isSendWithEnter) }
  Row(verticalAlignment = Alignment.CenterVertically) {
    Switch(checked = isOn, onCheckedChange = {
      isOn = it
      prefs.setSendWithEnter(it)
    })
    Text(stringResource(R.string.send_with_enter), modifier = Modifier.padding(start = 8.dp))
  }
}

private val LOCK_AFTER_CHOICES = listOf(
  LockAfter.Immediately to R.string.app_lock_immediately,
  LockAfter.OneMinute to R.string.app_lock_one_minute,
  LockAfter.FiveMinutes to R.string.app_lock_five_minutes,
  LockAfter.FifteenMinutes to R.string.app_lock_fifteen_minutes,
)
