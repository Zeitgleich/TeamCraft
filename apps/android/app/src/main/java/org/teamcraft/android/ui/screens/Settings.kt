package org.teamcraft.android.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import org.teamcraft.android.R
import org.teamcraft.android.ui.AppState
import org.teamcraft.android.ui.MainViewModel
import uniffi.teamcraft_ffi.Presence
import uniffi.teamcraft_ffi.PushService

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun SettingsScreen(viewModel: MainViewModel, account: AppState.Ready, onBack: () -> Unit) {
  var presence by rememberSaveable { mutableStateOf(Presence.ONLINE) }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.settings_title), maybeOnBack = onBack) }) { padding ->
    Column(
      Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(16.dp),
      verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
      Text(account.displayName, style = MaterialTheme.typography.titleLarge)

      Heading(R.string.presence_title)
      FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for ((choice, label) in PRESENCE_CHOICES) {
          FilterChip(
            selected = presence == choice,
            onClick = {
              presence = choice
              viewModel.setPresence(choice)
            },
            label = { Text(stringResource(label)) },
          )
        }
      }

      HorizontalDivider()
      Heading(R.string.push_title)
      val service = viewModel.pushService()
      Text(
        stringResource(
          when (service) {
            PushService.FCM -> R.string.push_fcm
            PushService.UNIFIED_PUSH -> R.string.push_unifiedpush
            null -> R.string.push_none
          },
        ),
      )

      HorizontalDivider()
      Heading(R.string.fingerprint_title)
      Text(account.identityFingerprint, fontFamily = FontFamily.Monospace)
      Text(stringResource(R.string.fingerprint_hint), style = MaterialTheme.typography.bodySmall)

      HorizontalDivider()
      ApproveDevice(viewModel)

      HorizontalDivider()
      OutlinedButton(onClick = viewModel::signOut, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.sign_out))
      }
    }
  }
}

/** This device approves a new one: enter its code, compare the short codes, confirm. */
@Composable
private fun ApproveDevice(viewModel: MainViewModel) {
  val scope = rememberCoroutineScope()
  var code by rememberSaveable { mutableStateOf("") }
  var maybeShortCode by rememberSaveable { mutableStateOf<String?>(null) }
  var isDone by rememberSaveable { mutableStateOf(false) }
  var hasFailed by rememberSaveable { mutableStateOf(false) }
  Heading(R.string.approve_title)
  when {
    isDone -> Text(stringResource(R.string.approve_done))
    maybeShortCode != null -> {
      Text(stringResource(R.string.approve_compare))
      Text(maybeShortCode.orEmpty(), style = MaterialTheme.typography.displaySmall, fontFamily = FontFamily.Monospace)
      Button(onClick = {
        scope.launch {
          viewModel.confirmLink().onSuccess { isDone = true }.onFailure { hasFailed = true }
        }
      }) { Text(stringResource(R.string.approve_confirm)) }
    }
    else -> {
      Text(stringResource(R.string.approve_hint), style = MaterialTheme.typography.bodySmall)
      OutlinedTextField(
        value = code,
        onValueChange = { code = it },
        label = { Text(stringResource(R.string.approve_code)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth(),
      )
      Button(
        onClick = {
          scope.launch {
            viewModel.approveLink(code).onSuccess { maybeShortCode = it }.onFailure { hasFailed = true }
          }
        },
        enabled = code.isNotBlank(),
      ) { Text(stringResource(R.string.approve_start)) }
    }
  }
  if (hasFailed) Text(stringResource(R.string.error_generic), color = MaterialTheme.colorScheme.error)
}

@Composable
private fun Heading(text: Int) {
  Text(stringResource(text), style = MaterialTheme.typography.titleMedium)
}

private val PRESENCE_CHOICES = listOf(
  Presence.ONLINE to R.string.presence_online,
  Presence.AWAY to R.string.presence_away,
  Presence.DO_NOT_DISTURB to R.string.presence_dnd,
  Presence.OFFLINE to R.string.presence_offline,
)
