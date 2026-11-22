package de.zeitgleich.teamcraft.ui.screens

import java.text.DateFormat
import java.time.Instant
import java.time.ZoneId
import java.util.Date
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.ListItemShapes
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
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ConversationNotificationSettings
import de.zeitgleich.teamcraft.core.NotificationSettings
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.BubblePrefs
import de.zeitgleich.teamcraft.notifications.ChannelMentions
import de.zeitgleich.teamcraft.notifications.DndRange
import de.zeitgleich.teamcraft.notifications.NotificationChannels
import de.zeitgleich.teamcraft.notifications.QuietHoursEditor
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.isDirect
import uniffi.teamcraft_ffi.NotificationContent

private val LEVELS = listOf("all" to R.string.notify_all, "mentions" to R.string.notify_mentions, "none" to R.string.notify_none)
/** From most to least shown: the order the stricter-of comparison relies on. */
private val CONTENT_CHOICES = listOf(
  NotificationContent.CONTENT to R.string.notification_content_full,
  NotificationContent.SENDER_ONLY to R.string.notification_content_sender,
  NotificationContent.NOTHING to R.string.notification_content_nothing,
)
private val PUSH_MODES = listOf("always" to R.string.push_always, "away_only" to R.string.push_away_only)
private val CHANNEL_MENTION_CHOICES = listOf(
  ChannelMentions.DEFAULT to R.string.notify_channel_mentions_default,
  ChannelMentions.NOTIFY to R.string.notify_channel_mentions_notify,
  ChannelMentions.IGNORE to R.string.notify_channel_mentions_ignore,
)

/** The lead times the settings offer, in minutes; `0` is no reminder (the desktop offers the same). */
private val MEETING_REMINDER_CHOICES = listOf(0, 1, 5, 10, 15)
private const val DEFAULT_MEETING_REMINDER_MINUTES = 5

/**
 * A conversation's notification level, mute and filters (`@channel`, quiet hours), as the server
 * enforces them for push.
 */
@Composable
fun ConversationNotificationsDialog(viewModel: MainViewModel, conversationId: String, onDismiss: () -> Unit) {
  val scope = rememberCoroutineScope()
  var settings by remember { mutableStateOf<ConversationNotificationSettings?>(null) }
  var quiet by remember { mutableStateOf(QuietHoursEditor()) }
  // Bubbles are a device setting of direct messages (Android 11+, #591), kept apart from the server's.
  val context = LocalContext.current
  val bubbles = remember { BubblePrefs(context) }
  var isBubble by remember(conversationId) { mutableStateOf(bubbles.isEnabled(conversationId)) }
  var canBubble by remember(conversationId) { mutableStateOf(false) }
  var isDirectConversation by remember(conversationId) { mutableStateOf(false) }
  LaunchedEffect(conversationId) {
    isDirectConversation = viewModel.conversations().firstOrNull { it.id == conversationId }?.isDirect == true
    canBubble = BubblePrefs.isSupported && isDirectConversation
    val loaded = viewModel.readChat { conversationNotifications(conversationId) } ?: ConversationNotificationSettings()
    settings = loaded
    quiet = QuietHoursEditor.from(loaded.quietHours)
  }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.notifications_conversation)) },
    text = {
      Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        val current = settings ?: return@Column
        Column(Modifier.selectableGroup()) {
          for ((level, label) in LEVELS) {
            Row(
              Modifier
                .fillMaxWidth()
                .selectable(
                  selected = current.level == level,
                  role = Role.RadioButton,
                  onClick = { settings = current.copy(level = level, isInherited = false) },
                ),
              verticalAlignment = Alignment.CenterVertically,
            ) {
              RadioButton(selected = current.level == level, onClick = null)
              Text(stringResource(label))
            }
          }
        }
        MutePresets(current) { settings = it }
        if (canBubble) {
          // Read again on every return to the app: the user may come back from the system settings.
          var isAllowed by remember { mutableStateOf(bubbles.isAllowedBySystem()) }
          LifecycleResumeEffect(Unit) {
            isAllowed = bubbles.isAllowedBySystem()
            onPauseOrDispose {}
          }
          Row(
            Modifier.fillMaxWidth().toggleable(value = isBubble, role = Role.Switch, onValueChange = { isBubble = it }),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
          ) {
            Text(stringResource(R.string.bubble_setting), style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            Switch(checked = isBubble, onCheckedChange = null)
          }
          Text(
            stringResource(if (isAllowed) R.string.bubble_setting_hint else R.string.bubble_setting_system_off),
            style = MaterialTheme.typography.bodySmall,
          )
          if (!isAllowed) {
            TextButton(onClick = { context.startActivity(bubbles.systemSettingsIntent()) }) {
              Text(stringResource(R.string.bubble_open_system_settings))
            }
          }
        }
        Text(stringResource(R.string.notify_channel_mentions), style = MaterialTheme.typography.titleSmall)
        Column(Modifier.selectableGroup()) {
          for ((choice, label) in CHANNEL_MENTION_CHOICES) {
            val isChosen = ChannelMentions.shown(current.channelMentions) == choice
            Row(
              Modifier
                .fillMaxWidth()
                .selectable(selected = isChosen, role = Role.RadioButton, onClick = { settings = current.copy(channelMentions = choice) }),
              verticalAlignment = Alignment.CenterVertically,
            ) {
              RadioButton(selected = isChosen, onClick = null)
              Text(stringResource(label))
            }
          }
        }
        Text(stringResource(R.string.notify_quiet_hours), style = MaterialTheme.typography.titleSmall)
        Text(stringResource(R.string.notify_quiet_hours_hint, ZoneId.systemDefault().id), style = MaterialTheme.typography.bodySmall)
        quiet.ranges.forEachIndexed { index, range ->
          RangeRow(
            range = range,
            onChange = { changed -> quiet = quiet.copy(ranges = quiet.ranges.toMutableList().also { it[index] = changed }) },
            onRemove = { quiet = quiet.copy(ranges = quiet.ranges.filterIndexed { other, _ -> other != index }) },
          )
        }
        if (quiet.canAddRange) {
          OutlinedButton(onClick = { quiet = quiet.copy(ranges = quiet.ranges + DndRange.DEFAULT) }) {
            Text(stringResource(R.string.dnd_add_range))
          }
        }
        // How loud, its sound and whether it is a priority conversation: the system's own settings.
        Text(stringResource(R.string.notify_system_settings_hint), style = MaterialTheme.typography.bodySmall)
        TextButton(onClick = {
          // Some vendors drop settings pages; then the dialog just stays.
          runCatching { context.startActivity(NotificationChannels.conversationSettingsIntent(context, conversationId, isDirectConversation)) }
        }) {
          Text(stringResource(R.string.notify_system_settings))
        }
      }
    },
    confirmButton = {
      TextButton(
        // A range that isn't complete says so at the range (RangeRow); nothing is saved until it is.
        enabled = settings != null && quiet.isValid,
        onClick = {
          settings?.let { loaded ->
            val chosen = loaded.copy(
              channelMentions = ChannelMentions.toSend(ChannelMentions.shown(loaded.channelMentions)),
              quietHours = quiet.toQuietHours(ZoneId.systemDefault(), Instant.now()),
            )
            scope.launch { viewModel.readChat { setConversationNotifications(conversationId, chosen) } }
          }
          if (canBubble && isBubble != bubbles.isEnabled(conversationId)) {
            bubbles.setEnabled(conversationId, isBubble)
            // The notification on screen gains or loses its bubble now, without sounding again.
            val notifier = TeamCraftApplication.graph(context).notifier
            scope.launch(Dispatchers.Default) { notifier.repostAll() }
          }
          onDismiss()
        },
      ) { Text(stringResource(R.string.ok)) }
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

/**
 * Global notification preferences as settings groups: when pushes come, receipts and thread
 * auto-follow, meeting reminders. Nothing shows until the settings are read.
 */
@Composable
fun NotificationSettingsGroups(viewModel: MainViewModel, pushRows: SettingsGroupScope.() -> Unit) {
  val scope = rememberCoroutineScope()
  var settings by remember { mutableStateOf<NotificationSettings?>(null) }
  LaunchedEffect(Unit) { settings = viewModel.readChat { notificationSettings() } }
  val current = settings ?: return
  fun save(changed: NotificationSettings) {
    settings = changed
    scope.launch { viewModel.readChat { setNotificationSettings(changed) } }
  }
  SettingsGroup(R.string.settings_group_push) {
    row { shapes ->
      SettingsChoiceRow(shapes, stringResource(R.string.push_mode_title), PUSH_MODES, current.pushMode) {
        save(current.copy(pushMode = it))
      }
    }
    pushRows()
  }
  SettingsGroup(R.string.settings_group_messages) {
    row { shapes ->
      SettingsSwitchRow(shapes, stringResource(R.string.read_receipts), current.sendReadReceipts) {
        save(current.copy(sendReadReceipts = it))
      }
    }
    row { shapes ->
      SettingsSwitchRow(shapes, stringResource(R.string.delivery_receipts), current.maybeSendDeliveryReceipts ?: true) {
        save(current.copy(maybeSendDeliveryReceipts = it))
      }
    }
    row { shapes ->
      SettingsSwitchRow(shapes, stringResource(R.string.auto_follow_threads), current.autoFollowMentionedThreads) {
        save(current.copy(autoFollowMentionedThreads = it))
      }
    }
  }
  SettingsGroup(R.string.settings_group_meetings) {
    row { shapes ->
      SettingsChoiceRow(
        shapes,
        stringResource(R.string.meeting_reminder_title),
        MEETING_REMINDER_CHOICES,
        current.maybeMeetingReminderMinutes ?: DEFAULT_MEETING_REMINDER_MINUTES,
        label = { minutes ->
          if (minutes == 0) {
            stringResource(R.string.meeting_reminder_off)
          } else {
            pluralStringResource(R.plurals.meeting_reminder_minutes, minutes, minutes)
          }
        },
      ) { save(current.copy(maybeMeetingReminderMinutes = it)) }
    }
    row { shapes ->
      SettingsSwitchRow(shapes, stringResource(R.string.meeting_notify_start), current.maybeNotifiesMeetingStart ?: true) {
        save(current.copy(maybeNotifiesMeetingStart = it))
      }
    }
  }
}

/**
 * How much message notifications show on this device (issue #314). The workspace policy can only
 * lower it, and the app lock caps it at the sender; both are said below the group.
 */
@Composable
fun NotificationContentGroup(viewModel: MainViewModel) {
  val context = LocalContext.current
  val content by viewModel.notificationContent.content.collectAsState()
  var maybeLimit by remember { mutableStateOf<NotificationContent?>(null) }
  LaunchedEffect(Unit) { maybeLimit = viewModel.notificationContentLimit() }
  val isAppLockOn = remember { AppLockPrefs.prefs(context).isEnabled }
  val limit = maybeLimit
  val hints = listOfNotNull(
    if (limit != null && limit.ordinal > content.ordinal) {
      stringResource(R.string.notification_content_limited, stringResource(CONTENT_CHOICES.first { it.first == limit }.second))
    } else {
      null
    },
    if (isAppLockOn && content == NotificationContent.CONTENT) stringResource(R.string.notification_content_app_lock) else null,
  )
  SettingsGroup(maybeFooter = hints.joinToString("\n").ifEmpty { null }) {
    row { shapes ->
      SettingsChoiceRow(shapes, stringResource(R.string.notification_content_title), CONTENT_CHOICES, content) {
        viewModel.setNotificationContent(it)
      }
    }
    row { shapes ->
      val isWorkspaceShowingContent = limit == null || limit == NotificationContent.CONTENT
      WidgetPreviewRow(shapes, isAllowed = content == NotificationContent.CONTENT && isWorkspaceShowingContent && !isAppLockOn)
    }
  }
}

/**
 * Message previews in the home screen widget (#2076): off by default, and only where notifications
 * show content and the app lock is off. Names and unread counts show either way.
 */
@Composable
private fun WidgetPreviewRow(shapes: ListItemShapes, isAllowed: Boolean) {
  val context = LocalContext.current
  val widgets = remember { TeamCraftApplication.graph(context).homeWidgets }
  val isWanted by widgets.isPreviewWanted.collectAsState()
  SettingsSwitchRow(
    shapes,
    stringResource(R.string.widget_previews),
    isOn = isWanted && isAllowed,
    maybeSummary = stringResource(if (isAllowed) R.string.widget_previews_summary else R.string.widget_previews_unavailable),
    isEnabled = isAllowed,
    onChange = widgets::setPreviewWanted,
  )
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
