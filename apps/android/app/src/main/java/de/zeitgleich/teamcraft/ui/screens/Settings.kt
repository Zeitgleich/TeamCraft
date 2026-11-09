package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.heading
import android.app.Activity
import android.view.WindowManager
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.material3.ListItem
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.Icon
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.launch
import androidx.compose.runtime.produceState
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.CustomStatus
import de.zeitgleich.teamcraft.core.HOLIDAY_REGIONS
import de.zeitgleich.teamcraft.core.HolidayRegion
import de.zeitgleich.teamcraft.core.KeyBackup
import de.zeitgleich.teamcraft.core.OwnProfile
import de.zeitgleich.teamcraft.security.ScreenPrefs
import de.zeitgleich.teamcraft.ui.AppState
import de.zeitgleich.teamcraft.ui.LinkConfirmation
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.forms.deviceLinkCodeError
import de.zeitgleich.teamcraft.ui.forms.displayNameError
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.passphraseError
import de.zeitgleich.teamcraft.ui.forms.requiredTextError
import de.zeitgleich.teamcraft.ui.forms.supportingText
import de.zeitgleich.teamcraft.ui.rememberIsOnline
import uniffi.teamcraft_ffi.Presence
import uniffi.teamcraft_ffi.PushService
import uniffi.teamcraft_ffi.TeamCraftException

/** The settings' pages; the hub lists them, each opens on top of it. */
private enum class SettingsPage(val title: Int, val summary: Int) {
  Notifications(R.string.push_title, R.string.settings_notifications_summary),
  Chats(R.string.settings_chats_title, R.string.settings_chats_summary),
  Security(R.string.settings_security_title, R.string.settings_security_summary),
  Devices(R.string.settings_devices_title, R.string.settings_devices_summary),
}

/**
 * Settings as a hub: the user's name, presence and status on top, then one entry per page
 * (notifications, chats, privacy and security, devices) and sign-out, instead of one long page.
 */
@Composable
fun SettingsScreen(
  viewModel: MainViewModel,
  account: AppState.Ready,
  onBack: () -> Unit,
  onDnd: () -> Unit = {},
  onDiagnostics: () -> Unit = {},
  onReportProblem: () -> Unit = {},
  opensSecurity: Boolean = false,
) {
  var maybePage by rememberSaveable { mutableStateOf(if (opensSecurity) SettingsPage.Security else null) }
  BackHandler(enabled = maybePage != null) { maybePage = null }
  val page = maybePage
  Scaffold(
    contentWindowInsets = ScreenInsets,
    topBar = {
      ChromeTopBar(
        title = stringResource(page?.title ?: R.string.settings_title),
        maybeOnBack = { if (page != null) maybePage = null else onBack() },
      )
    },
  ) { padding ->
    Column(
      Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(16.dp),
      verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
      when (page) {
        null -> SettingsHub(viewModel, account, onOpen = { maybePage = it }, onReportProblem = onReportProblem)
        SettingsPage.Notifications -> NotificationsPage(viewModel, onDnd, onDiagnostics)
        SettingsPage.Chats -> ChatsPage(viewModel)
        SettingsPage.Security -> SecurityPage(viewModel, account)
        SettingsPage.Devices -> ApproveDevice(viewModel)
      }
    }
  }
}

@Composable
private fun SettingsHub(
  viewModel: MainViewModel,
  account: AppState.Ready,
  onOpen: (SettingsPage) -> Unit,
  onReportProblem: () -> Unit,
) {
  val changes by viewModel.changeCount.collectAsState()
  // The user's own presence and custom status as the workspace shows them, not a guess.
  val maybeMe by produceState(viewModel.chat.maybeKeptSnapshot()?.me, changes) { value = viewModel.readChat { snapshot() }?.me ?: value }
  var presence by remember(maybeMe?.presence) { mutableStateOf(presenceOf(maybeMe?.presence)) }
  Row(verticalAlignment = Alignment.CenterVertically) {
    AccountAvatar(account.userId, account.displayName, size = 56.dp)
    Column(Modifier.padding(start = 16.dp).weight(1f)) {
      Text(account.displayName, style = MaterialTheme.typography.titleLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
      maybeMe?.maybeCustomStatus?.let { status ->
        Text(
          listOf(status.emoji, status.text).filter { it.isNotBlank() }.joinToString(" "),
          style = MaterialTheme.typography.bodyMedium,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
        )
      }
    }
  }
  DisplayNameSection(viewModel)
  // Guests have no meetings of their own to skip holidays in; the server refuses them anyway.
  if (maybeMe?.isGuest != true) HolidayRegionSection(viewModel)
  PresenceRow(presence) { choice ->
    presence = choice
    viewModel.setPresence(choice)
  }
  var isEditingStatus by rememberSaveable { mutableStateOf(false) }
  ListItem(
    headlineContent = { Text(stringResource(R.string.custom_status_title)) },
    supportingContent = {
      val status = maybeMe?.maybeCustomStatus
      Text(status?.let { listOf(it.emoji, it.text).filter(String::isNotBlank).joinToString(" ") } ?: stringResource(R.string.custom_status_hint))
    },
    trailingContent = { Chevron(isOpen = isEditingStatus) },
    modifier = Modifier.clickable { isEditingStatus = !isEditingStatus },
  )
  if (isEditingStatus) CustomStatusSection(viewModel)
  HorizontalDivider()
  Column {
    for (entry in SettingsPage.entries) {
      ListItem(
        headlineContent = { Text(stringResource(entry.title)) },
        supportingContent = { Text(stringResource(entry.summary)) },
        trailingContent = { Chevron() },
        modifier = Modifier.clickable { onOpen(entry) },
      )
    }
  }
  val context = LocalContext.current
  ListItem(
    headlineContent = { Text(stringResource(R.string.export_diagnostics)) },
    supportingContent = { Text(stringResource(R.string.export_diagnostics_summary)) },
    leadingContent = { Icon(Icons.Filled.Share, contentDescription = null) },
    modifier = Modifier.clickable { exportDiagnostics(context) },
  )
  // Every member can report; guests share no DM with the organization's admins.
  if (maybeMe?.isGuest != true) {
    ListItem(
      headlineContent = { Text(stringResource(R.string.report_title)) },
      supportingContent = { Text(stringResource(R.string.report_summary)) },
      leadingContent = { Icon(painterResource(R.drawable.ic_help), contentDescription = null) },
      trailingContent = { Chevron() },
      modifier = Modifier.clickable(onClick = onReportProblem),
    )
  }
  HorizontalDivider()
  OutlinedButton(onClick = viewModel::signOut, modifier = Modifier.fillMaxWidth()) {
    Text(stringResource(R.string.sign_out))
  }
}

@Composable
private fun NotificationsPage(viewModel: MainViewModel, onDnd: () -> Unit, onDiagnostics: () -> Unit) {
  NotificationSettingsSection(viewModel)
  Heading(R.string.notification_content_title)
  NotificationContentSection(viewModel)
  Heading(R.string.mention_title)
  MentionSettingsSection(viewModel)
  Text(
    stringResource(
      when (viewModel.pushService()) {
        PushService.FCM -> R.string.push_fcm
        PushService.UNIFIED_PUSH -> R.string.push_unifiedpush
        // APNs is the iOS app's service; Android never registers with it.
        PushService.APNS, null -> R.string.push_none
      },
    ),
  )
  Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    OutlinedButton(onClick = onDnd) { Text(stringResource(R.string.dnd_title)) }
    OutlinedButton(onClick = onDiagnostics) { Text(stringResource(R.string.diagnostics_title)) }
  }
}

@Composable
private fun ChatsPage(viewModel: MainViewModel) {
  SendWithEnterSetting()
  ShareTypingSetting()
  DynamicColorSetting()
  HorizontalDivider()
  FontSetting()
  HorizontalDivider()
  ColorThemeSetting()
  HorizontalDivider()
  ChatListLayoutSetting()
  ChatListGroupingSetting()
  HorizontalDivider()
  MessageLayoutSetting()
  HorizontalDivider()
  CallStatusSoundsSetting()
  HorizontalDivider()
  OriginalDownloadSetting()
  HorizontalDivider()
  SendQualitySetting()
  HorizontalDivider()
  AnimatedImagesSetting()
  HorizontalDivider()
  Heading(R.string.search_prefetch_title)
  SearchPrefetchSection(viewModel.segmentPrefetch)
}

@Composable
private fun SecurityPage(viewModel: MainViewModel, account: AppState.Ready) {
  Heading(R.string.fingerprint_title)
  Text(account.identityFingerprint, fontFamily = FontFamily.Monospace)
  Text(stringResource(R.string.fingerprint_hint), style = MaterialTheme.typography.bodySmall)
  KeyChanges(viewModel)
  HorizontalDivider()
  SecureScreenSetting()
  AppLockSetting()
  CrashReportsSetting(viewModel.crashReports)
  HorizontalDivider()
  KeyBackupStatus(viewModel)
  ChangePassphrase(viewModel)
}

/** The key backup in plain words (issue #410): stored or not, and how it is protected. */
@Composable
private fun KeyBackupStatus(viewModel: MainViewModel) {
  val changes by viewModel.changeCount.collectAsState()
  var backup by remember { mutableStateOf<KeyBackup?>(null) }
  LaunchedEffect(changes) { backup = viewModel.readChat { snapshot() }?.security?.backup }
  val state = backup ?: return
  Heading(R.string.backup_title)
  val title = when {
    !state.isStored -> R.string.backup_missing
    state.hasPassphrase -> R.string.backup_ok
    else -> R.string.backup_no_recovery
  }
  Text(stringResource(title), style = MaterialTheme.typography.titleSmall)
  val setAt = state.passphraseSetAt
  val detail = when {
    !state.isStored -> stringResource(R.string.backup_missing_hint)
    !state.hasPassphrase -> stringResource(R.string.backup_no_recovery_hint)
    setAt != null -> stringResource(
      R.string.backup_passphrase_set,
      DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(setAt)),
    )
    else -> stringResource(R.string.backup_passphrase)
  }
  Text(detail, style = MaterialTheme.typography.bodySmall)
  if (state.hasOrganization) {
    Text(stringResource(R.string.backup_organization), style = MaterialTheme.typography.bodySmall)
  }
}

/** This device approves a new one: scan or enter its link code, then type the code it shows. */
@Composable
private fun ApproveDevice(viewModel: MainViewModel) {
  val scope = rememberCoroutineScope()
  // Not saveable: a restored screen must never confirm an approval the user left.
  var code by remember { mutableStateOf("") }
  var isTyping by remember { mutableStateOf(false) }
  var maybeError by remember { mutableStateOf<Int?>(null) }
  var isDone by rememberSaveable { mutableStateOf(false) }
  var maybeFailure by rememberSaveable { mutableStateOf<Int?>(null) }
  var isScanning by rememberSaveable { mutableStateOf(false) }
  var hasTriedStart by remember { mutableStateOf(false) }
  var isStarting by remember { mutableStateOf(false) }
  var isConfirming by remember { mutableStateOf(false) }
  val codeFocus = remember { FocusRequester() }
  // The server's refusal of the link code goes to the code field (forms.md), a missing code too.
  val maybeCodeError = deviceLinkCodeError(code).takeIf { hasTriedStart } ?: maybeFailure
  val approve: (String) -> Unit = { linkCode ->
    maybeFailure = null
    isStarting = true
    scope.launch {
      viewModel.approveLink(linkCode)
        .onSuccess { isTyping = true }
        .onFailure { failure ->
          // Says what to do, e.g. that a computer's "Link a phone" code belongs on the new phone.
          maybeFailure = (failure as? TeamCraftException)?.let(MainViewModel::errorText) ?: R.string.error_generic
          codeFocus.requestFocus()
        }
      isStarting = false
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
  val isOpen = isTyping && !isDone
  // Leaving the code step unconfirmed drops the approval, so syncing resumes.
  DisposableEffect(isOpen) {
    onDispose { if (isOpen) viewModel.cancelLinkApproval() }
  }
  Heading(R.string.approve_title)
  when {
    isDone -> Text(stringResource(R.string.approve_done))
    isTyping -> LinkCodeStep(
      maybeError = maybeError,
      isConfirming = isConfirming,
      onApprove = { typed ->
        isConfirming = true
        scope.launch {
          when (viewModel.confirmLink(typed)) {
            LinkConfirmation.CONFIRMED -> {
              maybeError = null
              isDone = true
            }
            LinkConfirmation.WRONG_CODE -> maybeError = R.string.approve_code_wrong
            LinkConfirmation.ATTEMPT_ENDED -> {
              // The new device has to start again: back to the first step.
              isTyping = false
              code = ""
              hasTriedStart = false
              maybeFailure = null
              maybeError = R.string.approve_attempt_ended
            }
            LinkConfirmation.FAILED -> maybeError = R.string.error_generic
          }
          isConfirming = false
        }
      },
      onCancel = {
        isTyping = false
        maybeError = null
        code = ""
        hasTriedStart = false
      },
    )
    else -> {
      Text(stringResource(R.string.approve_hint), style = MaterialTheme.typography.bodySmall)
      // The previous attempt ended on the new device: says so above the fresh start.
      maybeError?.let { ErrorLine(it) }
      OutlinedTextField(
        value = code,
        onValueChange = {
          code = it
          maybeFailure = null
        },
        label = { Text(stringResource(R.string.approve_code)) },
        singleLine = true,
        isError = maybeCodeError != null,
        supportingText = supportingText(maybeCodeError),
        modifier = Modifier.fillMaxWidth().focusRequester(codeFocus).fieldError(maybeCodeError),
      )
      // A new computer shows a QR code: scanning it is the usual way, typing its code the fallback.
      Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(onClick = {
          maybeError = null
          isScanning = true
        }, enabled = !isStarting) { Text(stringResource(R.string.link_scan_qr)) }
        OutlinedButton(onClick = {
          maybeError = null
          hasTriedStart = true
          if (deviceLinkCodeError(code) != null) codeFocus.requestFocus() else approve(code)
        }, enabled = !isStarting) { Text(stringResource(R.string.approve_start)) }
      }
      Text(stringResource(R.string.approve_new_phone_hint), style = MaterialTheme.typography.bodySmall)
    }
  }
}

/**
 * Changing the name under the picture, when the workspace policy lets people (issue #408): a
 * button that opens a field, and "Use name from sign-in" while a name of one's own is set. The new
 * name reaches everyone through the profile update event; this screen's name follows the same way.
 */
@Composable
private fun DisplayNameSection(viewModel: MainViewModel) {
  val scope = rememberCoroutineScope()
  val changes by viewModel.changeCount.collectAsState()
  var maybeProfile by remember { mutableStateOf<OwnProfile?>(null) }
  LaunchedEffect(changes) { maybeProfile = viewModel.readChat { ownProfile() } }
  val profile = maybeProfile?.takeIf { it.canOverrideDisplayName } ?: return
  var isEditing by rememberSaveable { mutableStateOf(false) }
  var name by rememberSaveable { mutableStateOf("") }
  var hasTried by remember { mutableStateOf(false) }
  var isSaving by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val nameFocus = remember { FocusRequester() }
  val maybeError = displayNameError(name).takeIf { hasTried } ?: maybeFailure
  fun save(maybeName: String?) {
    isSaving = true
    scope.launch {
      maybeFailure = viewModel.trySetDisplayName(profile, maybeName)
      if (maybeFailure == null) {
        isEditing = false
        hasTried = false
      } else if (isEditing) {
        nameFocus.requestFocus()
      }
      isSaving = false
    }
  }
  val resetButton: @Composable () -> Unit = {
    if (profile.maybeDisplayNameOverride != null) {
      OutlinedButton(onClick = { save(null) }, enabled = !isSaving) {
        Text(stringResource(R.string.profile_name_reset))
      }
    }
  }
  if (!isEditing) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      OutlinedButton(onClick = {
        name = profile.maybeDisplayNameOverride.orEmpty()
        maybeFailure = null
        isEditing = true
      }) { Text(stringResource(R.string.profile_name_change)) }
      resetButton()
    }
    return
  }
  OutlinedTextField(
    value = name,
    onValueChange = {
      name = it
      maybeFailure = null
    },
    label = { Text(stringResource(R.string.profile_name_label)) },
    singleLine = true,
    isError = maybeError != null,
    supportingText = supportingText(maybeError),
    modifier = Modifier.fillMaxWidth().focusRequester(nameFocus).fieldError(maybeError),
  )
  Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    Button(
      onClick = {
        hasTried = true
        if (displayNameError(name) != null) nameFocus.requestFocus() else save(name.trim())
      },
      enabled = !isSaving,
    ) { Text(stringResource(R.string.profile_name_save)) }
    OutlinedButton(onClick = {
      isEditing = false
      hasTried = false
      maybeFailure = null
    }, enabled = !isSaving) { Text(stringResource(R.string.cancel)) }
    resetButton()
  }
}

/**
 * Whose public holidays the user's repeating meetings skip (#1450): the workspace's region or one
 * of their own. Saved as soon as it is picked, the row waits while it saves, and a refusal puts
 * the old choice back and says so. Offline the row is off: it is a setting, not content. The
 * meeting list reads afresh whenever it is opened, so it shows the skipped occurrences.
 */
@Composable
private fun HolidayRegionSection(viewModel: MainViewModel) {
  val scope = rememberCoroutineScope()
  val isOnline = rememberIsOnline()
  var maybeLoaded by remember { mutableStateOf<HolidayRegion?>(null) }
  var hasFailedToLoad by remember { mutableStateOf(false) }
  // The choice being saved, shown until the server answers; `""` is the workspace's region.
  var maybePicked by remember { mutableStateOf<String?>(null) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  var isOpen by remember { mutableStateOf(false) }
  LaunchedEffect(isOnline) {
    if (isOnline && maybeLoaded == null) {
      val read = viewModel.loadHolidayRegion()
      maybeLoaded = read
      hasFailedToLoad = read == null
    }
  }
  val loaded = maybeLoaded
  val isSaving = maybePicked != null
  val current = maybePicked ?: loaded?.maybeRegion.orEmpty()
  val isEnabled = isOnline && loaded != null && !isSaving
  val hint = when {
    !isOnline -> stringResource(R.string.holiday_offline)
    hasFailedToLoad -> stringResource(R.string.holiday_unavailable)
    loaded == null -> ""
    else -> holidayHint(loaded)
  }
  Box {
    ListItem(
      headlineContent = { Text(stringResource(R.string.holiday_title)) },
      supportingContent = {
        Column {
          Text(HOLIDAY_REGION_LABELS[current]?.let { stringResource(it) } ?: current)
          if (hint.isNotEmpty()) Text(hint, style = MaterialTheme.typography.bodySmall)
        }
      },
      trailingContent = { Chevron(isOpen = isOpen) },
      modifier = Modifier.clickable(
        enabled = isEnabled,
        onClick = {
          maybeFailure = null
          isOpen = true
        },
      ),
    )
    DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
      for (code in listOf("") + HOLIDAY_REGIONS) {
        DropdownMenuItem(
          text = {
            Text(
              stringResource(if (code.isEmpty()) R.string.holiday_region_instance else HOLIDAY_REGION_LABELS.getValue(code)),
              color = if (code == current) MaterialTheme.colorScheme.primary else Color.Unspecified,
            )
          },
          onClick = {
            isOpen = false
            if (code == current) return@DropdownMenuItem
            maybePicked = code
            scope.launch {
              val (saved, maybeError) = viewModel.trySetHolidayRegion(code.ifEmpty { null })
              if (saved != null) maybeLoaded = saved else maybeFailure = maybeError
              // On a refusal the old choice is back: it was only held while saving.
              maybePicked = null
            }
          },
        )
      }
    }
  }
  maybeFailure?.let { ErrorLine(it) }
}

@Composable
private fun holidayHint(region: HolidayRegion): String {
  val source = if (region.maybeRegion != null) {
    stringResource(R.string.holiday_own_hint)
  } else {
    val name = HOLIDAY_REGION_LABELS[region.instanceRegion]?.let { stringResource(it) } ?: region.instanceRegion
    stringResource(R.string.holiday_instance_hint, name)
  }
  return if (region.hasInstanceFeed) "$source ${stringResource(R.string.holiday_feed_hint)}" else source
}

/** The user's presence as one row; a tap opens the choices. */
@Composable
private fun PresenceRow(presence: Presence, onChoose: (Presence) -> Unit) {
  var isOpen by remember { mutableStateOf(false) }
  Box {
    ListItem(
      headlineContent = { Text(stringResource(R.string.presence_title)) },
      supportingContent = { Text(stringResource(PRESENCE_CHOICES.first { it.first == presence }.second)) },
      leadingContent = { Box(Modifier.size(12.dp).background(presenceDot(presence), CircleShape)) },
      trailingContent = { Chevron() },
      modifier = Modifier.clickable { isOpen = true },
    )
    DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
      for ((choice, label) in PRESENCE_CHOICES) {
        DropdownMenuItem(
          text = { Text(stringResource(label)) },
          leadingIcon = { Box(Modifier.size(10.dp).background(presenceDot(choice), CircleShape)) },
          onClick = {
            isOpen = false
            onChoose(choice)
          },
        )
      }
    }
  }
}

/** A row's "more behind this" mark; pointing down while what it opens is shown below. */
@Composable
private fun Chevron(isOpen: Boolean = false) {
  Icon(
    if (isOpen) Icons.Filled.KeyboardArrowDown else Icons.AutoMirrored.Filled.KeyboardArrowRight,
    contentDescription = null,
    tint = MaterialTheme.colorScheme.onSurfaceVariant,
  )
}

private fun presenceOf(maybeWire: String?): Presence = when (maybeWire) {
  "Away" -> Presence.AWAY
  "DoNotDisturb" -> Presence.DO_NOT_DISTURB
  "Offline" -> Presence.OFFLINE
  else -> Presence.ONLINE
}

private fun presenceDot(presence: Presence): Color = when (presence) {
  Presence.ONLINE -> Color(0xFF2E9E5B)
  Presence.AWAY -> Color(0xFFE0A030)
  Presence.DO_NOT_DISTURB -> Color(0xFFD0453A)
  Presence.OFFLINE -> Color(0xFF8A8F98)
}

/** An emoji and text shown next to the user's name, cleared after the chosen time. */
@Composable
private fun CustomStatusSection(viewModel: MainViewModel) {
  val scope = rememberCoroutineScope()
  var emoji by rememberSaveable { mutableStateOf("") }
  var text by rememberSaveable { mutableStateOf("") }
  var hasTried by remember { mutableStateOf(false) }
  var isSaving by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val textFocus = remember { FocusRequester() }
  val maybeTextError = requiredTextError(text, R.string.custom_status_missing).takeIf { hasTried } ?: maybeFailure
  Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    OutlinedTextField(
      value = emoji,
      onValueChange = { emoji = it.take(4) },
      label = { Text(stringResource(R.string.custom_status_emoji)) },
      singleLine = true,
      modifier = Modifier.width(96.dp),
    )
    OutlinedTextField(
      value = text,
      onValueChange = {
        text = it
        maybeFailure = null
      },
      label = { Text(stringResource(R.string.custom_status_text)) },
      singleLine = true,
      placeholder = { Text(stringResource(R.string.custom_status_hint)) },
      isError = maybeTextError != null,
      supportingText = supportingText(maybeTextError),
      modifier = Modifier.weight(1f).focusRequester(textFocus).fieldError(maybeTextError),
    )
  }
  Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    Button(
      onClick = {
        hasTried = true
        if (text.isBlank()) {
          textFocus.requestFocus()
        } else {
          isSaving = true
          scope.launch {
            maybeFailure = viewModel.tryAct(ChatActions.setCustomStatus(CustomStatus(emoji, text.trim(), "Today")))
            if (maybeFailure != null) textFocus.requestFocus()
            isSaving = false
          }
        }
      },
      enabled = !isSaving,
    ) { Text(stringResource(R.string.custom_status_set)) }
    OutlinedButton(onClick = {
      text = ""
      hasTried = false
      maybeFailure = null
      viewModel.act(ChatActions.setCustomStatus(null))
    }, enabled = !isSaving) { Text(stringResource(R.string.custom_status_clear)) }
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

/**
 * A new recovery passphrase for the key backup. Too short, or refused by the core (the backup
 * couldn't be re-encrypted or uploaded), is said at the field; the field keeps the input then.
 */
@Composable
private fun ChangePassphrase(viewModel: MainViewModel) {
  val scope = rememberCoroutineScope()
  var passphrase by remember { mutableStateOf("") }
  var isDone by rememberSaveable { mutableStateOf(false) }
  var hasTried by remember { mutableStateOf(false) }
  var isSaving by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val focus = remember { FocusRequester() }
  val maybeError = passphraseError(passphrase).takeIf { hasTried } ?: maybeFailure
  val submit: () -> Unit = {
    hasTried = true
    if (passphraseError(passphrase) != null) {
      focus.requestFocus()
    } else if (!isSaving) {
      isSaving = true
      scope.launch {
        maybeFailure = viewModel.tryAct(ChatActions.changePassphrase(passphrase))
        if (maybeFailure == null) {
          passphrase = ""
          hasTried = false
          isDone = true
        } else {
          focus.requestFocus()
        }
        isSaving = false
      }
    }
  }
  Heading(R.string.change_passphrase_title)
  OutlinedTextField(
    value = passphrase,
    onValueChange = {
      passphrase = it
      isDone = false
      maybeFailure = null
    },
    label = { Text(stringResource(R.string.passphrase)) },
    singleLine = true,
    visualTransformation = PasswordVisualTransformation(),
    keyboardOptions = PASSPHRASE_KEYBOARD.copy(imeAction = ImeAction.Done),
    keyboardActions = KeyboardActions(onDone = { submit() }),
    isError = maybeError != null,
    supportingText = supportingText(maybeError, R.string.passphrase_rule_short),
    modifier = Modifier.fillMaxWidth().focusRequester(focus).fieldError(maybeError),
  )
  Button(onClick = submit, enabled = !isSaving) { Text(stringResource(R.string.change_passphrase_button)) }
  if (isDone) Text(stringResource(R.string.change_passphrase_done), style = MaterialTheme.typography.bodySmall)
}

@Composable
private fun Heading(text: Int) {
  Text(stringResource(text), style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
}

private val PRESENCE_CHOICES = listOf(
  Presence.ONLINE to R.string.presence_online,
  Presence.AWAY to R.string.presence_away,
  Presence.DO_NOT_DISTURB to R.string.presence_dnd,
  Presence.OFFLINE to R.string.presence_offline,
)

/** No screenshots or app-switcher thumbnails; applies the next time a screen opens. */
@Composable
private fun SecureScreenSetting() {
  val context = LocalContext.current
  val prefs = remember { ScreenPrefs.prefs(context) }
  var isSecure by remember { mutableStateOf(prefs.isSecureScreen) }
  SettingSwitch(R.string.secure_screen, isSecure) {
    isSecure = it
    prefs.setSecureScreen(it)
    (context as? Activity)?.let { activity ->
      if (it) {
        activity.window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
      } else {
        activity.window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
      }
    }
  }
}
