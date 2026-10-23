package de.zeitgleich.teamcraft.ui.screens

import android.app.Activity
import android.view.WindowManager
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.CustomStatus
import de.zeitgleich.teamcraft.security.ScreenPrefs
import de.zeitgleich.teamcraft.ui.AppState
import de.zeitgleich.teamcraft.ui.MainViewModel
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

      CustomStatusSection(viewModel)

      HorizontalDivider()
      Heading(R.string.push_title)
      NotificationSettingsSection(viewModel)
      Heading(R.string.mention_title)
      MentionSettingsSection(viewModel)
      val service = viewModel.pushService()
      Text(
        stringResource(
          when (service) {
            PushService.FCM -> R.string.push_fcm
            PushService.UNIFIED_PUSH -> R.string.push_unifiedpush
            // APNs is the iOS app's service; Android never registers with it.
            PushService.APNS, null -> R.string.push_none
          },
        ),
      )

      HorizontalDivider()
      Heading(R.string.search_prefetch_title)
      SearchPrefetchSection(viewModel.segmentPrefetch)

      HorizontalDivider()
      Heading(R.string.fingerprint_title)
      Text(account.identityFingerprint, fontFamily = FontFamily.Monospace)
      Text(stringResource(R.string.fingerprint_hint), style = MaterialTheme.typography.bodySmall)

      KeyChanges(viewModel)

      HorizontalDivider()
      ApproveDevice(viewModel)

      HorizontalDivider()
      ChangePassphrase(viewModel)

      HorizontalDivider()
      SecureScreenSetting()
      AppLockSetting()
      SendWithEnterSetting()

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
  // Not saveable: a restored screen must never confirm an approval the user left.
  var code by remember { mutableStateOf("") }
  var maybeShortCode by remember { mutableStateOf<String?>(null) }
  var isDone by rememberSaveable { mutableStateOf(false) }
  var hasFailed by rememberSaveable { mutableStateOf(false) }
  var isScanning by rememberSaveable { mutableStateOf(false) }
  val approve: (String) -> Unit = { linkCode ->
    hasFailed = false
    scope.launch {
      viewModel.approveLink(linkCode).onSuccess { maybeShortCode = it }.onFailure { hasFailed = true }
    }
  }
  if (isScanning) {
    Dialog(onDismissRequest = { isScanning = false }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
      Surface(Modifier.fillMaxSize()) {
        QrScanner(
          onScanned = { scanned ->
            isScanning = false
            code = scanned
            approve(scanned)
          },
          onCancel = { isScanning = false },
        )
      }
    }
  }
  val isComparing = maybeShortCode != null && !isDone
  // Leaving the comparison unconfirmed drops the approval, so syncing resumes.
  DisposableEffect(isComparing) {
    onDispose { if (isComparing) viewModel.cancelLinkApproval() }
  }
  Heading(R.string.approve_title)
  when {
    isDone -> Text(stringResource(R.string.approve_done))
    maybeShortCode != null -> {
      Text(stringResource(R.string.approve_compare))
      Text(maybeShortCode.orEmpty(), style = MaterialTheme.typography.displaySmall, fontFamily = FontFamily.Monospace)
      Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(onClick = {
          scope.launch {
            viewModel.confirmLink().onSuccess { isDone = true }.onFailure { hasFailed = true }
          }
        }) { Text(stringResource(R.string.approve_confirm)) }
        OutlinedButton(onClick = {
          maybeShortCode = null
          code = ""
        }) { Text(stringResource(R.string.cancel)) }
      }
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
      Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(onClick = { approve(code) }, enabled = code.isNotBlank()) { Text(stringResource(R.string.approve_start)) }
        OutlinedButton(onClick = { isScanning = true }) { Text(stringResource(R.string.link_scan_qr)) }
      }
    }
  }
  if (hasFailed) Text(stringResource(R.string.error_generic), color = MaterialTheme.colorScheme.error)
}

/** An emoji and text shown next to the user's name, cleared after the chosen time. */
@Composable
private fun CustomStatusSection(viewModel: MainViewModel) {
  var emoji by rememberSaveable { mutableStateOf("💬") }
  var text by rememberSaveable { mutableStateOf("") }
  Heading(R.string.custom_status_title)
  Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    OutlinedTextField(value = emoji, onValueChange = { emoji = it.take(4) }, singleLine = true, modifier = Modifier.width(72.dp))
    OutlinedTextField(
      value = text,
      onValueChange = { text = it },
      singleLine = true,
      placeholder = { Text(stringResource(R.string.custom_status_hint)) },
      modifier = Modifier.weight(1f),
    )
  }
  Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    Button(
      onClick = { viewModel.act(ChatActions.setCustomStatus(CustomStatus(emoji, text.trim(), "Today"))) },
      enabled = text.isNotBlank(),
    ) { Text(stringResource(R.string.custom_status_set)) }
    OutlinedButton(onClick = {
      text = ""
      viewModel.act(ChatActions.setCustomStatus(null))
    }) { Text(stringResource(R.string.custom_status_clear)) }
  }
}

/** Contacts whose identity key the server now shows differently: compare, then accept. */
@Composable
private fun KeyChanges(viewModel: MainViewModel) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  LaunchedEffect(changes) { snapshot = viewModel.readChat { snapshot() } }
  val changed = snapshot?.security?.contacts.orEmpty().filter { it.maybeNewFingerprint != null }
  if (changed.isEmpty()) return
  HorizontalDivider()
  Heading(R.string.key_changes_title)
  for (contact in changed) {
    Text(snapshot?.name(contact.user).orEmpty(), style = MaterialTheme.typography.titleSmall)
    Text(stringResource(R.string.key_changes_new, contact.maybeNewFingerprint.orEmpty()), fontFamily = FontFamily.Monospace)
    OutlinedButton(onClick = { viewModel.act(ChatActions.acceptKeyChange(contact.user)) }) {
      Text(stringResource(R.string.key_changes_accept))
    }
  }
}

@Composable
private fun ChangePassphrase(viewModel: MainViewModel) {
  var passphrase by remember { mutableStateOf("") }
  var isDone by rememberSaveable { mutableStateOf(false) }
  Heading(R.string.change_passphrase_title)
  OutlinedTextField(
    value = passphrase,
    onValueChange = {
      passphrase = it
      isDone = false
    },
    label = { Text(stringResource(R.string.passphrase)) },
    singleLine = true,
    visualTransformation = PasswordVisualTransformation(),
    keyboardOptions = PASSPHRASE_KEYBOARD,
    modifier = Modifier.fillMaxWidth(),
  )
  Button(
    onClick = {
      viewModel.act(ChatActions.changePassphrase(passphrase))
      passphrase = ""
      isDone = true
    },
    enabled = passphrase.length >= MIN_PASSPHRASE_LENGTH,
  ) { Text(stringResource(R.string.change_passphrase_button)) }
  if (isDone) Text(stringResource(R.string.change_passphrase_done), style = MaterialTheme.typography.bodySmall)
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

/** Same floor as the enrollment screen. */
private const val MIN_PASSPHRASE_LENGTH = 12

/** No screenshots or app-switcher thumbnails; applies the next time a screen opens. */
@Composable
private fun SecureScreenSetting() {
  val context = LocalContext.current
  val prefs = remember { ScreenPrefs.prefs(context) }
  var isSecure by remember { mutableStateOf(prefs.isSecureScreen) }
  Row(verticalAlignment = Alignment.CenterVertically) {
    Switch(checked = isSecure, onCheckedChange = {
      isSecure = it
      prefs.setSecureScreen(it)
      (context as? Activity)?.let { activity ->
        if (it) {
          activity.window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        } else {
          activity.window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
      }
    })
    Text(stringResource(R.string.secure_screen), modifier = Modifier.padding(start = 8.dp))
  }
}
