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
import androidx.compose.material3.ListItemShapes

/**
 * App lock on or off, and after how long in the background it locks again: rows of the security
 * page's group.
 */
fun SettingsGroupScope.appLockRows(isAvailable: Boolean, isEnabled: Boolean, onEnabled: (Boolean) -> Unit) {
  row { shapes ->
    SettingsSwitchRow(
      shapes,
      stringResource(R.string.app_lock),
      isEnabled,
      maybeSummary = if (isAvailable) null else stringResource(R.string.app_lock_unavailable),
      isEnabled = isAvailable || isEnabled,
      onChange = onEnabled,
    )
  }
  if (isEnabled) row { shapes -> LockAfterRow(shapes) }
}

/** The app lock's state for [appLockRows]: switching it off deletes the key and reposts notifications. */
@Composable
fun rememberAppLock(): Pair<Boolean, (Boolean) -> Unit> {
  val context = LocalContext.current
  val prefs = remember { AppLockPrefs.prefs(context) }
  var isEnabled by remember { mutableStateOf(prefs.isEnabled) }
  return isEnabled to { isOn: Boolean ->
    isEnabled = isOn
    prefs.setEnabled(isOn)
    if (!isOn) deleteAppLockKey()
    // Notifications already shown follow the new setting at once.
    TeamCraftApplication.graph(context).notifier.repostAll()
  }
}

@Composable
private fun LockAfterRow(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { AppLockPrefs.prefs(context) }
  var lockAfter by remember { mutableStateOf(prefs.lockAfter) }
  SettingsChoiceRow(shapes, stringResource(R.string.app_lock_after), LOCK_AFTER_CHOICES, lockAfter) {
    lockAfter = it
    prefs.setLockAfter(it)
  }
}

/** Whether others see "… is typing" while the user writes. */
@Composable
fun ShareTypingSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { ComposerPrefs.prefs(context) }
  var isOn by remember { mutableStateOf(prefs.isSharingTyping) }
  SettingsSwitchRow(shapes, stringResource(R.string.share_typing), isOn) {
    isOn = it
    prefs.setSharingTyping(it)
  }
}

/** Which call status sounds play (reconnecting, reconnected, call lost); applies from the next call. */
@Composable
fun CallStatusSoundsSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { CallSoundPrefs.prefs(context) }
  var sounds by remember { mutableStateOf(prefs.statusSounds) }
  SettingsChoiceRow(
    shapes,
    stringResource(R.string.call_status_sounds),
    STATUS_SOUND_CHOICES,
    sounds,
    maybeHint = stringResource(R.string.call_status_sounds_hint),
  ) {
    sounds = it
    prefs.setStatusSounds(it)
  }
}

/**
 * When the image viewer fetches a picture's original by itself (#240): always, on Wi-Fi only, or
 * never ("Load original" then). The preview shows at once either way.
 */
@Composable
fun OriginalDownloadSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { MediaPrefs.prefs(context) }
  var choice by remember { mutableStateOf(prefs.originalDownload) }
  SettingsChoiceRow(
    shapes,
    stringResource(R.string.original_download_title),
    ORIGINAL_DOWNLOAD_CHOICES,
    choice,
    maybeHint = stringResource(R.string.original_download_hint),
  ) {
    choice = it
    prefs.setOriginalDownload(it)
  }
}

/** The quality photos and videos are sent at unless changed while sending (#1358). */
@Composable
fun SendQualitySetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { MediaPrefs.prefs(context) }
  var choice by remember { mutableStateOf(prefs.sendQuality) }
  SettingsChoiceRow(
    shapes,
    stringResource(R.string.send_quality_title),
    SEND_QUALITY_CHOICES,
    choice,
    maybeHint = stringResource(R.string.send_quality_hint),
  ) {
    choice = it
    prefs.setSendQuality(it)
  }
}

/** Whether animated images start by themselves or from their play button (#201). Takes effect at once. */
@Composable
fun AnimatedImagesSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { MediaPrefs.prefs(context) }
  val choice by prefs.animatedImages.collectAsState()
  SettingsChoiceRow(
    shapes,
    stringResource(R.string.animated_images_title),
    ANIMATED_IMAGES_CHOICES,
    choice,
    maybeHint = stringResource(R.string.animated_images_hint),
    onSelect = prefs::setAnimatedImages,
  )
}

/** One list of all conversations, or Home and Spaces apart (#1151). Takes effect at once. */
@Composable
fun ChatListLayoutSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val layout by prefs.chatListLayout.collectAsState()
  SettingsChoiceRow(
    shapes,
    stringResource(R.string.chat_list_layout),
    CHAT_LIST_LAYOUT_CHOICES,
    layout,
    maybeHint = stringResource(R.string.chat_list_layout_hint),
    onSelect = prefs::setChatListLayout,
  )
}

/** Messages as bubbles or as the desktop's comfortable or compact list (#598, #1488). A setting of this device; takes effect at once. */
@Composable
fun MessageLayoutSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { MessageLayoutPrefs.of(context) }
  val layout by prefs.layout.collectAsState()
  SettingsChoiceRow(
    shapes,
    stringResource(R.string.message_layout),
    MESSAGE_LAYOUT_CHOICES,
    layout,
    maybeHint = stringResource(R.string.message_layout_hint),
    onSelect = prefs::setLayout,
  )
}

/** In the list layout: DMs and channels together, or the channels grouped by space (#1345). */
@Composable
fun ChatListGroupingSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val grouping by prefs.chatListGrouping.collectAsState()
  SettingsChoiceRow(
    shapes,
    stringResource(R.string.chat_list_grouping),
    CHAT_LIST_GROUPING_CHOICES,
    grouping,
    maybeHint = stringResource(R.string.chat_list_grouping_hint),
    onSelect = prefs::setChatListGrouping,
  )
}

/** Whether the list layout is on, so the page shows [ChatListGroupingSetting] (only the list has an order to choose). */
@Composable
fun isChatListLayoutList(): Boolean {
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val layout by prefs.chatListLayout.collectAsState()
  return layout == ChatListLayout.List
}

/** The wallpaper's colors instead of TeamCraft's (Android 12+; the page leaves the row out before). */
@Composable
fun DynamicColorSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { AppearancePrefs.prefs(context) }
  val isOn by prefs.isDynamicColor.collectAsState()
  SettingsSwitchRow(shapes, stringResource(R.string.dynamic_color), isOn, onChange = prefs::setDynamicColor)
}

/** Enter sends (hardware keyboards); Shift+Enter is a new line then. */
@Composable
fun SendWithEnterSetting(shapes: ListItemShapes) {
  val context = LocalContext.current
  val prefs = remember { ComposerPrefs.prefs(context) }
  var isOn by remember { mutableStateOf(prefs.isSendWithEnter) }
  SettingsSwitchRow(shapes, stringResource(R.string.send_with_enter), isOn) {
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
