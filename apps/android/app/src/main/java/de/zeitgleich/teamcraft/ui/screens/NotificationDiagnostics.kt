package de.zeitgleich.teamcraft.ui.screens

import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import java.text.DateFormat
import java.time.Instant
import java.time.ZoneId
import java.util.Date
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.notifications.NotificationChannels
import de.zeitgleich.teamcraft.notifications.NotificationHealth
import de.zeitgleich.teamcraft.notifications.QuickDnd
import de.zeitgleich.teamcraft.push.UnifiedPushStatus
import de.zeitgleich.teamcraft.ui.AppState
import de.zeitgleich.teamcraft.ui.MainViewModel
import uniffi.teamcraft_ffi.PushDiagnosticsInfo
import uniffi.teamcraft_ffi.PushNotificationKind
import uniffi.teamcraft_ffi.PushService
import uniffi.teamcraft_ffi.TestPushOutcome
import uniffi.teamcraft_ffi.TestPushResult

/**
 * Notification diagnostics (notifications.md, "Diagnostics"): which push service, what the system
 * allows, battery and DND state, what arrived, and an end-to-end test push.
 */
@Composable
fun NotificationDiagnosticsScreen(
  viewModel: MainViewModel,
  account: AppState.Ready,
  onBattery: () -> Unit,
  onDnd: () -> Unit,
  onBack: () -> Unit,
) {
  val context = LocalContext.current
  var refresh by remember { mutableIntStateOf(0) }
  // System switches change in the settings app: re-read whenever the screen comes back.
  LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { refresh++ }
  val handled by viewModel.pushHandledCount.collectAsState()
  val health = remember(refresh, handled) { NotificationHealth.read(context) }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.diagnostics_title), maybeOnBack = onBack) }) { padding ->
    Column(
      Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(16.dp),
      verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
      PushServiceSection(viewModel, health, onChanged = { refresh++ })
      HorizontalDivider()
      SystemSection(context, health)
      HorizontalDivider()
      SectionTitle(R.string.battery_title)
      StatusLine(
        if (health.isAppExemptFromBatteryOptimization) R.string.battery_app_unrestricted else R.string.battery_app_optimized,
        isOk = true,
      )
      if (health.isBackgroundRestricted) StatusLine(R.string.battery_background_restricted, isOk = false)
      health.maybeCurrentDistributor?.let { distributor ->
        val text = if (distributor.isExemptFromBatteryOptimization) R.string.battery_distributor_unrestricted else R.string.battery_distributor_optimized
        StatusLine(text, isOk = distributor.isExemptFromBatteryOptimization, distributor.label)
      }
      TextButton(onClick = onBattery) { Text(stringResource(R.string.battery_more)) }
      HorizontalDivider()
      DndSection(viewModel, account, health, onDnd)
      HorizontalDivider()
      ReceivedSection(viewModel, refresh, handled)
    }
  }
}

@Composable
private fun PushServiceSection(viewModel: MainViewModel, health: NotificationHealth, onChanged: () -> Unit) {
  SectionTitle(R.string.diagnostics_service)
  when (viewModel.pushService()) {
    PushService.FCM -> Text(stringResource(R.string.diagnostics_service_fcm))
    PushService.UNIFIED_PUSH -> UnifiedPushDetails(viewModel, health, onChanged)
    // APNs is the iOS app's service; Android never registers with it.
    PushService.APNS, null -> {
      StatusLine(R.string.diagnostics_service_none, isOk = false)
      Text(stringResource(R.string.push_none), style = MaterialTheme.typography.bodySmall)
    }
  }
}

@Composable
private fun UnifiedPushDetails(viewModel: MainViewModel, health: NotificationHealth, onChanged: () -> Unit) {
  val current = health.maybeCurrentDistributor
  Text(stringResource(R.string.diagnostics_service_unifiedpush, current?.label ?: "–"))
  val status = viewModel.unifiedPushStatus()
  val (text, isOk) = when (status.state) {
    UnifiedPushStatus.State.REGISTERED -> R.string.unifiedpush_registered to true
    UnifiedPushStatus.State.FAILED -> R.string.unifiedpush_failed to false
    UnifiedPushStatus.State.UNREGISTERED -> R.string.unifiedpush_unregistered to false
    UnifiedPushStatus.State.TEMPORARILY_UNAVAILABLE -> R.string.unifiedpush_unavailable to false
    UnifiedPushStatus.State.UNKNOWN -> R.string.unifiedpush_waiting to false
  }
  StatusLine(text, isOk, status.maybeFailureReason.orEmpty())
  if (health.distributors.size > 1) {
    Text(stringResource(R.string.unifiedpush_choose), style = MaterialTheme.typography.bodySmall)
    for (distributor in health.distributors) {
      Row(verticalAlignment = Alignment.CenterVertically) {
        RadioButton(
          selected = distributor.packageName == current?.packageName,
          onClick = {
            viewModel.selectDistributor(distributor.packageName)
            onChanged()
          },
        )
        Text(distributor.label)
      }
    }
  }
  if (status.state == UnifiedPushStatus.State.UNREGISTERED || status.state == UnifiedPushStatus.State.FAILED) {
    OutlinedButton(onClick = {
      viewModel.registerPush()
      onChanged()
    }) { Text(stringResource(R.string.unifiedpush_register_again)) }
  }
}

@Composable
private fun SystemSection(context: Context, health: NotificationHealth) {
  SectionTitle(R.string.diagnostics_system)
  StatusLine(if (health.hasPermission) R.string.diagnostics_permission_granted else R.string.diagnostics_permission_missing, health.hasPermission)
  StatusLine(if (health.areEnabled) R.string.diagnostics_enabled else R.string.diagnostics_disabled, health.areEnabled)
  if (!health.hasPermission || !health.areEnabled) {
    OutlinedButton(onClick = { context.startSafely(NotificationChannels.appSettingsIntent(context)) }) {
      Text(stringResource(R.string.diagnostics_open_settings))
    }
  }
  for (state in health.channels) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
      Column(Modifier.weight(1f)) {
        Text(stringResource(state.channel.name), style = MaterialTheme.typography.bodyMedium)
        Text(stringResource(importanceText(state.importance)), style = MaterialTheme.typography.bodySmall)
      }
      TextButton(onClick = { context.startSafely(NotificationChannels.settingsIntent(context, state.channel.id)) }) {
        Text(stringResource(R.string.diagnostics_change))
      }
    }
  }
  StatusLine(
    if (health.canUseFullScreenIntent) R.string.diagnostics_full_screen_allowed else R.string.diagnostics_full_screen_denied,
    health.canUseFullScreenIntent,
  )
  if (!health.canUseFullScreenIntent) {
    OutlinedButton(onClick = { context.startSafely(NotificationHealth.fullScreenIntentSettings(context)) }) {
      Text(stringResource(R.string.diagnostics_full_screen_allow))
    }
  }
  if (health.isSystemDndOn) StatusLine(R.string.diagnostics_system_dnd, isOk = false)
}

@Composable
private fun DndSection(viewModel: MainViewModel, account: AppState.Ready, health: NotificationHealth, onDnd: () -> Unit) {
  SectionTitle(R.string.dnd_title)
  var maybePresence by remember { mutableStateOf<String?>(null) }
  var isScheduled by remember { mutableStateOf(false) }
  LaunchedEffect(health) {
    maybePresence = viewModel.readChat { presenceState(account.userId) }
    isScheduled = viewModel.readChat { notificationSettings() }?.maybeDndSchedule
      ?.let { QuickDnd.isScheduledAt(it, Instant.now(), ZoneId.systemDefault()) } == true
  }
  val text = when {
    maybePresence == DND_STATE -> R.string.dnd_state_on
    isScheduled -> R.string.dnd_state_scheduled
    else -> R.string.dnd_state_off
  }
  StatusLine(text, isOk = text == R.string.dnd_state_off)
  TextButton(onClick = onDnd) { Text(stringResource(R.string.dnd_open)) }
}

@Composable
private fun ReceivedSection(viewModel: MainViewModel, refresh: Int, handled: Long) {
  val scope = rememberCoroutineScope()
  var maybeInfo by remember { mutableStateOf<PushDiagnosticsInfo?>(null) }
  var maybeTest by remember { mutableStateOf<TestRun?>(null) }
  LaunchedEffect(refresh, handled) { maybeInfo = viewModel.pushDiagnostics() }
  SectionTitle(R.string.diagnostics_received)
  val info = maybeInfo
  if (info == null || info.receivedCount == 0UL) {
    Text(stringResource(R.string.diagnostics_none_received))
  } else {
    Text(stringResource(R.string.diagnostics_received_count, info.receivedCount.toLong()))
    info.maybeLastReceivedAtMs?.let { Text(stringResource(R.string.diagnostics_last_received, formatTime(it))) }
    info.maybeLastKind?.let { Text(stringResource(R.string.diagnostics_last_kind, stringResource(kindText(it)))) }
    info.maybeLastDelayMs?.let { Text(stringResource(R.string.diagnostics_last_delay, seconds(it))) }
  }
  val suppressed = viewModel.pushStats.suppressedCount
  if (suppressed > 0) Text(stringResource(R.string.diagnostics_suppressed, suppressed))

  Button(
    onClick = {
      val requestedAtMs = System.currentTimeMillis()
      maybeTest = TestRun(requestedAtMs, maybeResult = null, hasFailed = false)
      scope.launch {
        val result = viewModel.sendTestPush()
        maybeTest = TestRun(requestedAtMs, result.getOrNull(), hasFailed = result.isFailure)
      }
    },
    enabled = maybeTest?.let { it.maybeResult == null && !it.hasFailed } != true,
  ) { Text(stringResource(R.string.diagnostics_send_test)) }
  maybeTest?.let { test -> TestOutcome(test, info?.maybeLastTestAtMs) }
}

@Composable
private fun TestOutcome(test: TestRun, maybeLastTestAtMs: Long?) {
  val result = test.maybeResult
  when {
    test.hasFailed -> StatusLine(R.string.test_push_request_failed, isOk = false)
    result == null -> Text(stringResource(R.string.test_push_requesting))
    result.outcome != TestPushOutcome.SENT -> StatusLine(outcomeText(result.outcome), isOk = false)
    maybeLastTestAtMs != null && maybeLastTestAtMs >= test.requestedAtMs ->
      StatusLine(R.string.test_push_arrived, isOk = true, seconds(maybeLastTestAtMs - test.requestedAtMs))
    else -> Text(stringResource(R.string.test_push_waiting, result.maybeProvider ?: "–"))
  }
}

/** A test push asked for at `requestedAtMs` (the device's clock, compared with its own arrival time). */
private data class TestRun(val requestedAtMs: Long, val maybeResult: TestPushResult?, val hasFailed: Boolean)

@Composable
private fun SectionTitle(@StringRes text: Int) {
  Text(stringResource(text), style = MaterialTheme.typography.titleMedium)
}

@Composable
private fun StatusLine(@StringRes text: Int, isOk: Boolean, vararg arguments: Any) {
  Text(
    stringResource(text, *arguments),
    color = if (isOk) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.error,
  )
}

private fun Context.startSafely(intent: Intent) {
  // Some vendors drop settings pages; then the screen just stays.
  runCatching { startActivity(intent) }
}

@StringRes
private fun importanceText(importance: Int): Int = when (importance) {
  NotificationManager.IMPORTANCE_NONE -> R.string.importance_off
  NotificationManager.IMPORTANCE_MIN -> R.string.importance_min
  NotificationManager.IMPORTANCE_LOW -> R.string.importance_low
  NotificationManager.IMPORTANCE_DEFAULT -> R.string.importance_default
  else -> R.string.importance_high
}

@StringRes
private fun kindText(kind: PushNotificationKind): Int = when (kind) {
  PushNotificationKind.MESSAGE -> R.string.push_kind_message
  PushNotificationKind.CALL -> R.string.push_kind_call
  PushNotificationKind.DISMISS -> R.string.push_kind_dismiss
  PushNotificationKind.TEST -> R.string.push_kind_test
  PushNotificationKind.UNKNOWN -> R.string.push_kind_unknown
}

@StringRes
private fun outcomeText(outcome: TestPushOutcome): Int = when (outcome) {
  TestPushOutcome.SENT -> R.string.test_push_waiting
  TestPushOutcome.NOT_REGISTERED -> R.string.test_push_not_registered
  TestPushOutcome.NOT_CONFIGURED -> R.string.test_push_not_configured
  TestPushOutcome.UNAVAILABLE -> R.string.test_push_unavailable
  TestPushOutcome.REJECTED -> R.string.test_push_rejected
  TestPushOutcome.GONE -> R.string.test_push_gone
  TestPushOutcome.UNKNOWN -> R.string.test_push_unknown
}

private fun formatTime(ms: Long): String = DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.MEDIUM).format(Date(ms))

/** Tenths of a second are enough to tell Doze delays from instant delivery. */
private fun seconds(ms: Long): String = "%.1f".format(ms.coerceAtLeast(0) / 1000.0)

private const val DND_STATE = "dnd"
