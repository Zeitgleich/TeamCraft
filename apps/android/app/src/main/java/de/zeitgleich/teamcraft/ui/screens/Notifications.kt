package de.zeitgleich.teamcraft.ui.screens

import java.text.DateFormat
import java.util.Date
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ConversationNotificationSettings
import de.zeitgleich.teamcraft.core.NotificationSettings
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.ui.MainViewModel
import uniffi.teamcraft_ffi.NotificationContent

private val LEVELS = listOf("all" to R.string.notify_all, "mentions" to R.string.notify_mentions, "none" to R.string.notify_none)
/** From most to least shown: the order the stricter-of comparison relies on. */
private val CONTENT_CHOICES = listOf(
  NotificationContent.CONTENT to R.string.notification_content_full,
  NotificationContent.SENDER_ONLY to R.string.notification_content_sender,
  NotificationContent.NOTHING to R.string.notification_content_nothing,
)
private val PUSH_MODES = listOf("always" to R.string.push_always, "away_only" to R.string.push_away_only)

/** A conversation's notification level and mute, as the server enforces them for push. */
@Composable
fun ConversationNotificationsDialog(viewModel: MainViewModel, conversationId: String, onDismiss: () -> Unit) {
  val scope = rememberCoroutineScope()
  var settings by remember { mutableStateOf<ConversationNotificationSettings?>(null) }
  LaunchedEffect(conversationId) {
    settings = viewModel.readChat { conversationNotifications(conversationId) } ?: ConversationNotificationSettings()
  }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.notifications_conversation)) },
    text = {
      Column {
        val current = settings ?: return@Column
        for ((level, label) in LEVELS) {
          Row(verticalAlignment = Alignment.CenterVertically) {
            RadioButton(selected = current.level == level, onClick = { settings = current.copy(level = level, isInherited = false) })
            Text(stringResource(label))
          }
        }
        MutePresets(current) { settings = it }
      }
    },
    confirmButton = {
      TextButton(onClick = {
        settings?.let { chosen -> scope.launch { viewModel.readChat { setConversationNotifications(conversationId, chosen) } } }
        onDismiss()
      }) { Text(stringResource(R.string.ok)) }
    },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

/** A space's mute time; its channels follow it unless they have their own setting (#1105). */
@Composable
fun SpaceMuteDialog(viewModel: MainViewModel, spaceId: String, name: String, onDismiss: () -> Unit) {
  var maybeUntil by remember { mutableStateOf<Long?>(null) }
  var isLoaded by remember { mutableStateOf(false) }
  LaunchedEffect(spaceId) {
    maybeUntil = viewModel.spaceMutedUntil(spaceId)
    isLoaded = true
  }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.space_mute_title, name)) },
    text = {
      Column {
        if (isLoaded) MutePresets(ConversationNotificationSettings(mutedUntil = maybeUntil)) { maybeUntil = it.mutedUntil }
      }
    },
    confirmButton = {
      TextButton(
        enabled = isLoaded,
        onClick = {
          viewModel.muteSpace(spaceId, maybeUntil)
          onDismiss()
        },
      ) { Text(stringResource(R.string.ok)) }
    },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

/** Global notification preferences: when pushes come, read receipts, thread auto-follow. */
@Composable
fun NotificationSettingsSection(viewModel: MainViewModel) {
  val scope = rememberCoroutineScope()
  var settings by remember { mutableStateOf<NotificationSettings?>(null) }
  LaunchedEffect(Unit) { settings = viewModel.readChat { notificationSettings() } }
  val current = settings ?: return
  fun save(changed: NotificationSettings) {
    settings = changed
    scope.launch { viewModel.readChat { setNotificationSettings(changed) } }
  }
  Column(Modifier.fillMaxWidth()) {
    for ((mode, label) in PUSH_MODES) {
      Row(verticalAlignment = Alignment.CenterVertically) {
        RadioButton(selected = current.pushMode == mode, onClick = { save(current.copy(pushMode = mode)) })
        Text(stringResource(label))
      }
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
      Switch(checked = current.sendReadReceipts, onCheckedChange = { save(current.copy(sendReadReceipts = it)) })
      Text(stringResource(R.string.read_receipts), style = MaterialTheme.typography.bodyMedium)
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
      Switch(
        checked = current.autoFollowMentionedThreads,
        onCheckedChange = { save(current.copy(autoFollowMentionedThreads = it)) },
      )
      Text(stringResource(R.string.auto_follow_threads), style = MaterialTheme.typography.bodyMedium)
    }
  }
}

/**
 * How much message notifications show on this device (issue #314). The workspace policy can only
 * lower it, and the app lock caps it at the sender; both are said below the choices.
 */
@Composable
fun NotificationContentSection(viewModel: MainViewModel) {
  val context = LocalContext.current
  val content by viewModel.notificationContent.content.collectAsState()
  var maybeLimit by remember { mutableStateOf<NotificationContent?>(null) }
  LaunchedEffect(Unit) { maybeLimit = viewModel.notificationContentLimit() }
  val isAppLockOn = remember { AppLockPrefs.prefs(context).isEnabled }
  Column(Modifier.fillMaxWidth()) {
    for ((choice, label) in CONTENT_CHOICES) {
      Row(verticalAlignment = Alignment.CenterVertically) {
        RadioButton(selected = content == choice, onClick = { viewModel.setNotificationContent(choice) })
        Text(stringResource(label))
      }
    }
    val limit = maybeLimit
    if (limit != null && limit.ordinal > content.ordinal) {
      ContentHint(stringResource(R.string.notification_content_limited, stringResource(CONTENT_CHOICES.first { it.first == limit }.second)))
    }
    if (isAppLockOn && content == NotificationContent.CONTENT) ContentHint(stringResource(R.string.notification_content_app_lock))
  }
}

@Composable
private fun ContentHint(text: String) {
  Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

/** Mute until a preset time from now, or not at all; shows until when it is muted. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun MutePresets(current: ConversationNotificationSettings, onChange: (ConversationNotificationSettings) -> Unit) {
  val nowMs = remember { System.currentTimeMillis() }
  val maybeUntil = current.mutedUntil?.takeIf { it > nowMs }
  Text(stringResource(R.string.notify_mute), style = MaterialTheme.typography.titleSmall)
  Text(
    when {
      maybeUntil == null -> stringResource(R.string.mute_off)
      maybeUntil - nowMs > MUTE_FOREVER_THRESHOLD_MS -> stringResource(R.string.mute_forever_state)
      else -> stringResource(
        R.string.mute_until,
        DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(maybeUntil)),
      )
    },
    style = MaterialTheme.typography.bodySmall,
  )
  FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
    FilterChip(
      selected = maybeUntil == null,
      onClick = { onChange(current.copy(mutedUntil = null, isInherited = false)) },
      label = { Text(stringResource(R.string.mute_off)) },
    )
    for ((durationMs, label) in MUTE_PRESETS) {
      FilterChip(
        selected = false,
        onClick = { onChange(current.copy(mutedUntil = System.currentTimeMillis() + durationMs, isInherited = false)) },
        label = { Text(stringResource(label)) },
      )
    }
  }
}

private const val HOUR_MS = 60L * 60 * 1000
private const val DAY_MS = 24 * HOUR_MS

/** "Muted" without an end: the server only knows times, so far in the future. */
private const val MUTE_FOREVER_MS = 100L * 365 * DAY_MS

/** A mute this far out was set as "forever". */
private const val MUTE_FOREVER_THRESHOLD_MS = 10L * 365 * DAY_MS

private val MUTE_PRESETS = listOf(
  HOUR_MS to R.string.mute_one_hour,
  8 * HOUR_MS to R.string.mute_eight_hours,
  DAY_MS to R.string.mute_one_day,
  7 * DAY_MS to R.string.mute_one_week,
  MUTE_FOREVER_MS to R.string.mute_forever,
)
