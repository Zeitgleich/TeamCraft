package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.ui.semantics.Role
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.calls.CallSoundPrefs
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.security.LockAfter
import de.zeitgleich.teamcraft.ui.ComposerPrefs
import de.zeitgleich.teamcraft.ui.MediaPrefs
import de.zeitgleich.teamcraft.ui.theme.AppearancePrefs
import uniffi.teamcraft_ffi.CallStatusSounds
import uniffi.teamcraft_ffi.OriginalDownload

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
    SettingSwitch(R.string.app_lock, isEnabled, isEnabled = isAvailable || isEnabled) {
      isEnabled = it
      prefs.setEnabled(it)
      // Notifications already shown follow the new setting at once.
      TeamCraftApplication.graph(context).notifier.repostAll()
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

/** Whether others see "… is typing" while the user writes. */
@Composable
fun ShareTypingSetting() {
  val context = LocalContext.current
  val prefs = remember { ComposerPrefs.prefs(context) }
  var isOn by remember { mutableStateOf(prefs.isSharingTyping) }
  SettingSwitch(R.string.share_typing, isOn) {
    isOn = it
    prefs.setSharingTyping(it)
  }
}

/** Which call status sounds play (reconnecting, reconnected, call lost); applies from the next call. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun CallStatusSoundsSetting() {
  val context = LocalContext.current
  val prefs = remember { CallSoundPrefs.prefs(context) }
  var sounds by remember { mutableStateOf(prefs.statusSounds) }
  Column {
    Text(stringResource(R.string.call_status_sounds))
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      for ((choice, label) in STATUS_SOUND_CHOICES) {
        FilterChip(
          selected = sounds == choice,
          onClick = {
            sounds = choice
            prefs.setStatusSounds(choice)
          },
          label = { Text(stringResource(label)) },
        )
      }
    }
    Text(
      stringResource(R.string.call_status_sounds_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/**
 * When the image viewer fetches a picture's original by itself (#240): always, on Wi-Fi only, or
 * never ("Load original" then). The preview shows at once either way.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun OriginalDownloadSetting() {
  val context = LocalContext.current
  val prefs = remember { MediaPrefs.prefs(context) }
  var choice by remember { mutableStateOf(prefs.originalDownload) }
  Column {
    Text(stringResource(R.string.original_download_title))
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      for ((option, label) in ORIGINAL_DOWNLOAD_CHOICES) {
        FilterChip(
          selected = choice == option,
          onClick = {
            choice = option
            prefs.setOriginalDownload(option)
          },
          label = { Text(stringResource(label)) },
        )
      }
    }
    Text(
      stringResource(R.string.original_download_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** The wallpaper's colors instead of TeamCraft's (Android 12+). */
@Composable
fun DynamicColorSetting() {
  if (!AppearancePrefs.isSupported) return
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val isOn by prefs.isDynamicColor.collectAsState()
  SettingSwitch(R.string.dynamic_color, isOn, onChange = prefs::setDynamicColor)
}

/** Enter sends (hardware keyboards); Shift+Enter is a new line then. */
@Composable
fun SendWithEnterSetting() {
  val context = LocalContext.current
  val prefs = remember { ComposerPrefs.prefs(context) }
  var isOn by remember { mutableStateOf(prefs.isSendWithEnter) }
  SettingSwitch(R.string.send_with_enter, isOn) {
    isOn = it
    prefs.setSendWithEnter(it)
  }
}

private val STATUS_SOUND_CHOICES = listOf(
  CallStatusSounds.ALL to R.string.call_status_sounds_all,
  CallStatusSounds.OWN_ONLY to R.string.call_status_sounds_own_only,
  CallStatusSounds.OFF to R.string.call_status_sounds_off,
)

private val ORIGINAL_DOWNLOAD_CHOICES = listOf(
  OriginalDownload.ALWAYS to R.string.original_download_always,
  OriginalDownload.UNMETERED_ONLY to R.string.original_download_unmetered,
  OriginalDownload.NEVER to R.string.original_download_never,
)

private val LOCK_AFTER_CHOICES = listOf(
  LockAfter.Immediately to R.string.app_lock_immediately,
  LockAfter.OneMinute to R.string.app_lock_one_minute,
  LockAfter.FiveMinutes to R.string.app_lock_five_minutes,
  LockAfter.FifteenMinutes to R.string.app_lock_fifteen_minutes,
)

/** A switch with its label; the whole row toggles it (a bigger target than the switch alone). */
@Composable
fun SettingSwitch(label: Int, isOn: Boolean, isEnabled: Boolean = true, onChange: (Boolean) -> Unit) {
  Row(
    Modifier
      .fillMaxWidth()
      .toggleable(value = isOn, enabled = isEnabled, role = Role.Switch, onValueChange = onChange)
      .padding(vertical = 4.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Switch(checked = isOn, onCheckedChange = null, enabled = isEnabled)
    Text(stringResource(label), modifier = Modifier.padding(start = 8.dp))
  }
}
