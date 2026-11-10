package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
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
import de.zeitgleich.teamcraft.security.deleteAppLockKey
import de.zeitgleich.teamcraft.security.LockAfter
import de.zeitgleich.teamcraft.ui.AnimatedImagesMode
import de.zeitgleich.teamcraft.ui.ComposerPrefs
import de.zeitgleich.teamcraft.ui.MediaPrefs
import de.zeitgleich.teamcraft.ui.media.MediaQuality
import de.zeitgleich.teamcraft.ui.theme.AppearancePrefs
import de.zeitgleich.teamcraft.ui.theme.ChatListGrouping
import de.zeitgleich.teamcraft.ui.theme.ChatListLayout
import de.zeitgleich.teamcraft.ui.MessageLayout
import de.zeitgleich.teamcraft.ui.MessageLayoutPrefs
import uniffi.teamcraft_ffi.CallStatusSounds
import uniffi.teamcraft_ffi.OriginalDownload

/** App lock on or off, and after how long in the background it locks again. */
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
      if (!it) deleteAppLockKey()
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
      ChoiceButtonGroup(LOCK_AFTER_CHOICES, selected = lockAfter) { choice ->
        lockAfter = choice
        prefs.setLockAfter(choice)
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
@Composable
fun CallStatusSoundsSetting() {
  val context = LocalContext.current
  val prefs = remember { CallSoundPrefs.prefs(context) }
  var sounds by remember { mutableStateOf(prefs.statusSounds) }
  Column {
    Text(stringResource(R.string.call_status_sounds))
    ChoiceButtonGroup(STATUS_SOUND_CHOICES, selected = sounds) { choice ->
      sounds = choice
      prefs.setStatusSounds(choice)
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
@Composable
fun OriginalDownloadSetting() {
  val context = LocalContext.current
  val prefs = remember { MediaPrefs.prefs(context) }
  var choice by remember { mutableStateOf(prefs.originalDownload) }
  Column {
    Text(stringResource(R.string.original_download_title))
    ChoiceButtonGroup(ORIGINAL_DOWNLOAD_CHOICES, selected = choice) { option ->
      choice = option
      prefs.setOriginalDownload(option)
    }
    Text(
      stringResource(R.string.original_download_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** The quality photos and videos are sent at unless changed while sending (#1358). */
@Composable
fun SendQualitySetting() {
  val context = LocalContext.current
  val prefs = remember { MediaPrefs.prefs(context) }
  var choice by remember { mutableStateOf(prefs.sendQuality) }
  Column {
    Text(stringResource(R.string.send_quality_title))
    ChoiceButtonGroup(SEND_QUALITY_CHOICES, selected = choice) { option ->
      choice = option
      prefs.setSendQuality(option)
    }
    Text(
      stringResource(R.string.send_quality_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** Whether animated images start by themselves or from their play button (#201). Takes effect at once. */
@Composable
fun AnimatedImagesSetting() {
  val context = LocalContext.current
  val prefs = remember { MediaPrefs.prefs(context) }
  val choice by prefs.animatedImages.collectAsState()
  Column {
    Text(stringResource(R.string.animated_images_title))
    ChoiceButtonGroup(ANIMATED_IMAGES_CHOICES, selected = choice) { option -> prefs.setAnimatedImages(option) }
    Text(
      stringResource(R.string.animated_images_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** One list of all conversations, or Home and Spaces apart (#1151). Takes effect at once. */
@Composable
fun ChatListLayoutSetting() {
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val layout by prefs.chatListLayout.collectAsState()
  Column {
    Text(stringResource(R.string.chat_list_layout))
    ChoiceButtonGroup(CHAT_LIST_LAYOUT_CHOICES, selected = layout) { choice -> prefs.setChatListLayout(choice) }
    Text(
      stringResource(R.string.chat_list_layout_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** Messages as bubbles or as the desktop's comfortable or compact list (#598, #1488). A setting of this device; takes effect at once. */
@Composable
fun MessageLayoutSetting() {
  val context = LocalContext.current
  val prefs = remember { MessageLayoutPrefs.of(context) }
  val layout by prefs.layout.collectAsState()
  Column {
    Text(stringResource(R.string.message_layout))
    ChoiceButtonGroup(MESSAGE_LAYOUT_CHOICES, selected = layout) { choice -> prefs.setLayout(choice) }
    Text(
      stringResource(R.string.message_layout_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** In the list layout: DMs and channels together, or the channels grouped by space (#1345). */
@Composable
fun ChatListGroupingSetting() {
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val layout by prefs.chatListLayout.collectAsState()
  val grouping by prefs.chatListGrouping.collectAsState()
  // Only the list has this order to choose.
  if (layout != ChatListLayout.List) return
  Column {
    Text(stringResource(R.string.chat_list_grouping))
    ChoiceButtonGroup(CHAT_LIST_GROUPING_CHOICES, selected = grouping) { choice -> prefs.setChatListGrouping(choice) }
    Text(
      stringResource(R.string.chat_list_grouping_hint),
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

private val SEND_QUALITY_CHOICES = listOf(
  MediaQuality.Standard to R.string.send_quality_standard,
  MediaQuality.High to R.string.send_quality_high,
)

private val CHAT_LIST_LAYOUT_CHOICES = listOf(
  ChatListLayout.Spaces to R.string.nav_spaces,
  ChatListLayout.List to R.string.chat_list_layout_list,
)

private val MESSAGE_LAYOUT_CHOICES = listOf(
  MessageLayout.Bubbles to R.string.message_layout_bubbles,
  MessageLayout.Comfortable to R.string.message_layout_comfortable,
  MessageLayout.Compact to R.string.message_layout_compact,
)

private val CHAT_LIST_GROUPING_CHOICES = listOf(
  ChatListGrouping.Mixed to R.string.chat_list_grouping_mixed,
  ChatListGrouping.BySpace to R.string.chat_list_grouping_by_space,
)

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

private val ANIMATED_IMAGES_CHOICES = listOf(
  AnimatedImagesMode.AUTOPLAY to R.string.animated_images_autoplay,
  AnimatedImagesMode.ON_TAP to R.string.animated_images_tap,
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
  val haptics = LocalHapticFeedback.current
  Row(
    Modifier
      .fillMaxWidth()
      .toggleable(value = isOn, enabled = isEnabled, role = Role.Switch) {
        // Material haptics: a switch confirms its new state.
        haptics.performHapticFeedback(if (it) HapticFeedbackType.ToggleOn else HapticFeedbackType.ToggleOff)
        onChange(it)
      }
      .padding(vertical = 4.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Switch(checked = isOn, onCheckedChange = null, enabled = isEnabled)
    Text(stringResource(label), modifier = Modifier.padding(start = 8.dp))
  }
}
