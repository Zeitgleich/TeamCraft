package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.notifications.NotificationHealth
import de.zeitgleich.teamcraft.ui.MainViewModel
import uniffi.teamcraft_ffi.PushService

/**
 * Battery guidance. With FCM the app relies on high-priority messages and never asks to be exempt
 * (as large messengers do); the state is only shown. With UnifiedPush it is the distributor that
 * has to keep its connection open, so the guidance is about the distributor, not this app: the
 * distributor raises the app to the foreground for each message (connector `RaiseToForeground`).
 */
@Composable
fun PushBatteryScreen(viewModel: MainViewModel, onBack: () -> Unit) {
  val context = LocalContext.current
  var refresh by remember { mutableIntStateOf(0) }
  LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { refresh++ }
  val health = remember(refresh) { NotificationHealth.read(context) }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.battery_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    Column(
      Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(16.dp),
      verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
      Text(
        stringResource(
          if (health.isAppExemptFromBatteryOptimization) R.string.battery_app_unrestricted else R.string.battery_app_optimized,
        ),
      )
      if (health.isBackgroundRestricted) {
        Text(stringResource(R.string.battery_background_restricted), color = MaterialTheme.colorScheme.error)
      }
      when (viewModel.pushService()) {
        PushService.UNIFIED_PUSH -> DistributorGuidance(health)
        PushService.FCM -> Text(stringResource(R.string.battery_fcm_explained))
        PushService.APNS, null -> Text(stringResource(R.string.push_none))
      }
      Text(stringResource(R.string.battery_vendor_hint), style = MaterialTheme.typography.bodySmall)
    }
  }
}

@Composable
private fun DistributorGuidance(health: NotificationHealth) {
  val context = LocalContext.current
  Text(stringResource(R.string.battery_unifiedpush_explained))
  val distributor = health.maybeCurrentDistributor ?: run {
    Text(stringResource(R.string.push_none), color = MaterialTheme.colorScheme.error)
    return
  }
  val state = if (distributor.isExemptFromBatteryOptimization) R.string.battery_distributor_unrestricted else R.string.battery_distributor_optimized
  Text(
    stringResource(state, distributor.label),
    color = if (distributor.isExemptFromBatteryOptimization) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.error,
  )
  OutlinedButton(onClick = { runCatching { context.startActivity(NotificationHealth.appInfo(distributor.packageName)) } }) {
    Text(stringResource(R.string.battery_open_distributor, distributor.label))
  }
}
