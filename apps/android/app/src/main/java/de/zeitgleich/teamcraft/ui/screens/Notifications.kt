package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ConversationNotificationSettings
import de.zeitgleich.teamcraft.core.NotificationSettings
import de.zeitgleich.teamcraft.ui.MainViewModel

private val LEVELS = listOf("all" to R.string.notify_all, "mentions" to R.string.notify_mentions, "none" to R.string.notify_none)
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
            RadioButton(selected = current.level == level, onClick = { settings = current.copy(level = level) })
            Text(stringResource(label))
          }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
          Switch(
            checked = current.mutedUntil != null,
            onCheckedChange = { isMuted ->
              settings = current.copy(mutedUntil = if (isMuted) System.currentTimeMillis() + MUTE_FOREVER_MS else null)
            },
          )
          Text(stringResource(R.string.notify_mute))
        }
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

/** "Muted" without an end: the server only knows times, so far in the future. */
private const val MUTE_FOREVER_MS = 100L * 365 * 24 * 60 * 60 * 1000
