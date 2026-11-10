package de.zeitgleich.teamcraft.ui.screens

import androidx.activity.compose.BackHandler
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
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
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.error
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.AppState
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.isPassphraseRefusal
import de.zeitgleich.teamcraft.ui.forms.supportingText
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.recoveryKeyInFile
import uniffi.teamcraft_ffi.RecoveryKitDraft
import uniffi.teamcraft_ffi.KitFileCheck
import java.security.SecureRandom
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.runtime.DisposableEffect

internal const val LOGO_WIDTH_FRACTION = 0.6f
internal val LOGO_MAX_HEIGHT = 96.dp

@Composable
fun LoadingScreen() {
  Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { PageLoadingIndicator() }
}

@Composable
fun MessageScreen(@StringRes message: Int, onRetry: () -> Unit) {
  Centered {
    Text(stringResource(message), style = MaterialTheme.typography.bodyLarge)
    Button(onClick = onRetry) { Text(stringResource(R.string.retry)) }
  }
}

@Composable
fun EnrollScreen(
  hasExistingIdentity: Boolean,
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  serverHost: String,
  onEnrollWithKit: (RecoveryKitDraft) -> Unit,
  onUnlock: (String) -> Unit,
  onLink: () -> Unit,
  onOrgRecovery: () -> Unit,
) {
  if (hasExistingIdentity) {
    RestoreFlow(maybeError, isBusy, onUnlock = onUnlock, onLink = onLink, onOrgRecovery = onOrgRecovery)
  } else {
    NewKitFlow(maybeError, isBusy, serverHost, onEnrollWithKit)
  }
}

/** A recovery key's groups and their length (teamcraft-crypto's `RECOVERY_KEY_GROUPS`, e2ee.md §6.5.1). */
private const val RECOVERY_KEY_GROUPS = 8
private const val RECOVERY_KEY_GROUP_LENGTH = 4

/** The steps of a new account (issue #1701, e2ee.md §6.5.4). */
private enum class NewKitStep { INTRO, SAVE, CHECK, FINISH }

private const val NEW_KIT_STEPS = 4

/**
 * The first device of an account, one thing per step: what encryption and the recovery kit are,
 * saving the kit, checking it, and how to keep it; only then the keys are made with it. No
 * passphrase to invent.
 *
 * The key lives in the core's draft, never in saved state: after the process died a new draft is a
 * new key, so the flow resumes at saving the kit and says that a kit saved before doesn't work.
 */
@Composable
private fun NewKitFlow(@StringRes maybeError: Int?, isBusy: Boolean, serverHost: String, onEnroll: (RecoveryKitDraft) -> Unit) {
  val kit = remember { RecoveryKitDraft() }
  DisposableEffect(kit) { onDispose { kit.close() } }
  var savedStep by rememberSaveable { mutableStateOf(NewKitStep.INTRO) }
  var wasKeyShown by rememberSaveable { mutableStateOf(false) }
  // Saved past the intro, but this draft is new: its key was never shown.
  val wasRestarted = remember { wasKeyShown }
  var isKeyConfirmed by remember { mutableStateOf(false) }
  // Of this draft's key: not saveable, a new draft after the process died is unsaved.
  var maybeSaved by remember { mutableStateOf<Boolean?>(null) }
  val step = when {
    savedStep > NewKitStep.SAVE && !isKeyConfirmed && wasRestarted -> NewKitStep.SAVE
    savedStep == NewKitStep.FINISH && !isKeyConfirmed -> NewKitStep.CHECK
    else -> savedStep
  }
  LaunchedEffect(step) { if (step == NewKitStep.SAVE) wasKeyShown = true }
  BackHandler(enabled = step != NewKitStep.INTRO && !isBusy) { savedStep = NewKitStep.entries[step.ordinal - 1] }
  when (step) {
    NewKitStep.INTRO -> StepPage(
      step = 1,
      steps = NEW_KIT_STEPS,
      title = stringResource(R.string.onboarding_intro_title),
      maybeLead = R.string.onboarding_intro_lead,
      actions = { primary { Button(onClick = { savedStep = NewKitStep.SAVE }) { Text(stringResource(R.string.onboarding_start)) } } },
    ) {
      StepPoint(R.drawable.ic_lock, R.string.onboarding_intro_locked, R.string.onboarding_intro_locked_text)
      StepPoint(R.drawable.ic_key, R.string.onboarding_intro_kit, R.string.onboarding_intro_kit_text)
      StepPoint(R.drawable.ic_verified_user, R.string.onboarding_intro_keep, R.string.onboarding_intro_keep_text)
    }
    NewKitStep.SAVE -> SaveKitStep(
      kit = kit,
      serverHost = serverHost,
      wasRestarted = wasRestarted,
      maybeSaved = maybeSaved,
      onSaved = { maybeSaved = it },
      onBack = { savedStep = NewKitStep.INTRO },
      onContinue = { savedStep = NewKitStep.CHECK },
    )
    NewKitStep.CHECK -> CheckKitStep(
      kit = kit,
      onBack = { savedStep = NewKitStep.SAVE },
      onConfirmed = {
        isKeyConfirmed = true
        savedStep = NewKitStep.FINISH
      },
    )
    NewKitStep.FINISH -> StepPage(
      step = 4,
      steps = NEW_KIT_STEPS,
      title = stringResource(R.string.onboarding_finish_title),
      maybeLead = R.string.onboarding_finish_lead,
      actions = {
        secondary { TextButton(onClick = { savedStep = NewKitStep.CHECK }, enabled = !isBusy) { Text(stringResource(R.string.onboarding_back)) } }
        primary { WorkingButton(R.string.onboarding_finish, isBusy, isFullWidth = false) { onEnroll(kit) } }
      },
    ) {
      StepPoint(R.drawable.ic_description, R.string.onboarding_finish_where, R.string.onboarding_finish_where_text)
      StepPoint(R.drawable.ic_lock, R.string.onboarding_finish_share, R.string.onboarding_finish_share_text)
      StepPoint(R.drawable.ic_verified_user, R.string.onboarding_finish_ask, R.string.onboarding_finish_ask_text)
      // The device's or the server's failure belongs to no field: it sits above the buttons.
      maybeError?.takeUnless { isBusy }?.let { ErrorLine(it) }
    }
  }
}

/**
 * Step 2: the key, and saving the kit file; once saved, the main action moves on. [maybeSaved] is
 * the flow's (`null`: not tried yet), so coming back to the step still says the kit was saved.
 */
@Composable
private fun SaveKitStep(
  kit: RecoveryKitDraft,
  serverHost: String,
  wasRestarted: Boolean,
  maybeSaved: Boolean?,
  onSaved: (Boolean) -> Unit,
  onBack: () -> Unit,
  onContinue: () -> Unit,
  step: Int = 2,
  steps: Int = NEW_KIT_STEPS,
) {
  val save = rememberKitSaver(kit, serverHost, onSaved)
  val groups = remember(kit) { kit.groups() }
  StepPage(
    step = step,
    steps = steps,
    title = stringResource(R.string.onboarding_save_title),
    maybeLead = R.string.onboarding_save_lead,
    actions = {
      secondary { TextButton(onClick = onBack) { Text(stringResource(R.string.onboarding_back)) } }
      primary {
        if (maybeSaved == true) {
          Button(onClick = onContinue) { Text(stringResource(R.string.onboarding_continue)) }
        } else {
          Button(onClick = save) {
            Icon(painterResource(R.drawable.ic_download), contentDescription = null, modifier = Modifier.size(18.dp))
            Text(stringResource(R.string.kit_save), modifier = Modifier.padding(start = 8.dp))
          }
        }
      }
    },
  ) {
    if (wasRestarted) ErrorLine(R.string.onboarding_save_new_key)
    KitKeyCard(groups)
    when (maybeSaved) {
      true -> Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
          stringResource(R.string.kit_saved),
          style = MaterialTheme.typography.bodyMedium,
          modifier = Modifier.weight(1f).semantics { liveRegion = LiveRegionMode.Polite },
        )
        TextButton(onClick = save) { Text(stringResource(R.string.kit_save_again)) }
      }
      false -> ErrorLine(R.string.kit_save_failed)
      null -> Text(stringResource(R.string.kit_save_hint), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
    if (maybeSaved != true) {
      TextButton(onClick = onContinue) { Text(stringResource(R.string.kit_wrote_down)) }
    }
  }
}

/**
 * Step 3: one group of the key (chosen at random, the key is hidden here) or the saved file. A
 * refused check says what to do at the field and focuses it (forms.md).
 */
@Composable
private fun CheckKitStep(kit: RecoveryKitDraft, onBack: () -> Unit, onConfirmed: () -> Unit, step: Int = 3, steps: Int = NEW_KIT_STEPS) {
  // Not a secret: which group the user types back, so they look at their saved kit.
  val groupIndex = remember { SecureRandom().nextInt(RECOVERY_KEY_GROUPS) }
  val groupNumber = groupIndex + 1
  var typed by remember { mutableStateOf("") }
  var isSubmitted by remember { mutableStateOf(false) }
  var maybeFileCheck by remember { mutableStateOf<KitFileCheck?>(null) }
  val focus = remember { FocusRequester() }
  val isGroupRight = remember(typed) { kit.isGroup(groupIndex.toUInt(), typed) }
  val isConfirmed = isGroupRight || maybeFileCheck == KitFileCheck.MATCHES
  val maybeFieldError = when {
    isConfirmed -> null
    typed.trim().length >= RECOVERY_KEY_GROUP_LENGTH -> stringResource(R.string.kit_check_wrong, groupNumber)
    isSubmitted -> stringResource(R.string.kit_check_missing, groupNumber)
    else -> null
  }
  val open = rememberKitOpener { maybeText -> maybeFileCheck = maybeText?.let { kit.checkFile(it) } ?: KitFileCheck.NO_KEY }
  val submit: () -> Unit = {
    isSubmitted = true
    if (isConfirmed) onConfirmed() else focus.requestFocus()
  }
  StepPage(
    step = step,
    steps = steps,
    title = stringResource(R.string.onboarding_check_title),
    maybeLead = R.string.onboarding_check_lead,
    actions = {
      secondary { TextButton(onClick = onBack) { Text(stringResource(R.string.kit_check_show_again)) } }
      primary { Button(onClick = submit) { Text(stringResource(R.string.onboarding_continue)) } }
    },
  ) {
    OutlinedTextField(
      value = typed,
      onValueChange = { typed = it.take(RECOVERY_KEY_GROUP_LENGTH + 2) },
      label = { Text(stringResource(R.string.kit_check_group, groupNumber)) },
      singleLine = true,
      isError = maybeFieldError != null,
      supportingText = when {
        maybeFieldError != null -> ({ Text(maybeFieldError) })
        isGroupRight -> ({ Text(stringResource(R.string.kit_check_group_ok)) })
        else -> null
      },
      textStyle = MaterialTheme.typography.titleLarge.copy(fontFamily = FontFamily.Monospace),
      keyboardOptions = KeyboardOptions(
        capitalization = KeyboardCapitalization.Characters,
        autoCorrectEnabled = false,
        keyboardType = KeyboardType.Password,
        imeAction = ImeAction.Done,
      ),
      keyboardActions = KeyboardActions(onDone = { submit() }),
      modifier = Modifier
        .fillMaxWidth()
        .focusRequester(focus)
        .semantics { maybeFieldError?.let { error(it) } },
    )
    OutlinedButton(onClick = open) {
      Icon(painterResource(R.drawable.ic_description), contentDescription = null, modifier = Modifier.size(18.dp))
      Text(stringResource(R.string.kit_open), modifier = Modifier.padding(start = 8.dp))
    }
    when (maybeFileCheck) {
      KitFileCheck.MATCHES -> Text(stringResource(R.string.kit_check_file_ok), modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
      KitFileCheck.OTHER_KEY -> ErrorLine(R.string.kit_check_file_other)
      KitFileCheck.NO_KEY -> ErrorLine(R.string.kit_file_no_key)
      null -> Unit
    }
  }
}

/** The steps of an account that already has keys: the choice, then the kit or the IT department. */
private enum class RestoreStep { CHOOSE, KIT, IT }

private const val RESTORE_STEPS = 2

/**
 * Another device of the account exists: choose how to bring the key here. Linking from it is the
 * quick way (the history comes along), so it comes first and is marked; the recovery kit and the
 * admins are the fallbacks. Linking and the admins continue on their own screens (step 2 there too).
 */
@Composable
private fun RestoreFlow(
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  onUnlock: (String) -> Unit,
  onLink: () -> Unit,
  onOrgRecovery: () -> Unit,
) {
  // A refused key brings the user back to its field, also after the process was restored.
  var step by rememberSaveable { mutableStateOf(RestoreStep.CHOOSE) }
  LaunchedEffect(maybeError) {
    if (maybeError != null && isPassphraseRefusal(maybeError)) step = RestoreStep.KIT
  }
  BackHandler(enabled = step != RestoreStep.CHOOSE && !isBusy) { step = RestoreStep.CHOOSE }
  when (step) {
    RestoreStep.CHOOSE -> StepPage(
      step = 1,
      steps = RESTORE_STEPS,
      title = stringResource(R.string.onboarding_unlock_title),
      maybeLead = R.string.onboarding_unlock_lead,
      actions = {},
    ) {
      StepChoices(
        listOf(
          StepChoice(R.drawable.ic_devices, R.string.onboarding_way_link, R.string.onboarding_way_link_text, isRecommended = true, isEnabled = !isBusy, onClick = onLink),
          StepChoice(R.drawable.ic_key, R.string.onboarding_way_kit, R.string.onboarding_way_kit_text, isEnabled = !isBusy) {
            step = RestoreStep.KIT
          },
          StepChoice(R.drawable.ic_supervisor_account, R.string.onboarding_way_admins, R.string.onboarding_way_admins_text, isEnabled = !isBusy) {
            step = RestoreStep.IT
          },
        ),
      )
      // A refused key belongs to the kit step, "recovery not set up" to the IT step.
      maybeError?.takeUnless { isPassphraseRefusal(it) || it == R.string.error_org_recovery_unavailable || isBusy }?.let { ErrorLine(it) }
    }
    RestoreStep.KIT -> UnlockWithKitStep(maybeError, isBusy, onUnlock, onBack = { step = RestoreStep.CHOOSE })
    RestoreStep.IT -> AskTheItDepartmentStep(maybeError, isBusy, onShowCode = onOrgRecovery, onBack = { step = RestoreStep.CHOOSE })
  }
}

/**
 * No other device and no kit: what to tell the IT department. Not a self-service: the IT department
 * resets the account, or (if the organization set up recovery) asks for the code this phone shows
 * (e2ee.md §6.3.1); without recovery set up, asking for the code says so.
 */
@Composable
private fun AskTheItDepartmentStep(@StringRes maybeError: Int?, isBusy: Boolean, onShowCode: () -> Unit, onBack: () -> Unit) {
  StepPage(
    step = 2,
    steps = RESTORE_STEPS,
    title = stringResource(R.string.onboarding_it_title),
    maybeLead = R.string.onboarding_it_lead,
    actions = { secondary { TextButton(onClick = onBack, enabled = !isBusy) { Text(stringResource(R.string.onboarding_back)) } } },
  ) {
    Text(stringResource(R.string.onboarding_it_code), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    OutlinedButton(onClick = onShowCode, enabled = !isBusy) { Text(stringResource(R.string.onboarding_it_show_code)) }
    maybeError?.takeUnless { isPassphraseRefusal(it) || isBusy }?.let { ErrorLine(it) }
  }
}

/**
 * Step 2 with the kit: open its file (the key is read from it on the phone) or type the key; a
 * legacy passphrase works in the same field (the core tries a text that reads as a key as one,
 * e2ee.md §6.5.5). A refused one shows at the field until the user edits it.
 */
@Composable
private fun UnlockWithKitStep(
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  onUnlock: (String) -> Unit,
  onBack: () -> Unit,
) {
  var secret by remember { mutableStateOf("") }
  var isSubmitted by remember { mutableStateOf(false) }
  var isErrorEdited by remember(maybeError) { mutableStateOf(false) }
  var maybeFileNote by remember { mutableStateOf<Int?>(null) }
  val focus = remember { FocusRequester() }
  val maybeRefusal = maybeError?.takeIf { isPassphraseRefusal(it) && !isErrorEdited && !isBusy }
  val maybeFieldError = R.string.kit_secret_missing.takeIf { isSubmitted && secret.isBlank() } ?: maybeRefusal
  val open = rememberKitOpener { maybeText ->
    val maybeKey = maybeText?.let { recoveryKeyInFile(it) }
    if (maybeKey != null) {
      secret = maybeKey
      isErrorEdited = true
      maybeFileNote = R.string.kit_read
    } else {
      maybeFileNote = R.string.kit_file_no_key
    }
  }
  val submit: () -> Unit = {
    isSubmitted = true
    isErrorEdited = false
    if (secret.isBlank()) {
      focus.requestFocus()
    } else if (!isBusy) {
      onUnlock(secret.trim())
    }
  }
  LaunchedEffect(maybeError, isBusy) {
    if (maybeRefusal != null) focus.requestFocus()
  }
  StepPage(
    step = 2,
    steps = RESTORE_STEPS,
    title = stringResource(R.string.onboarding_kit_title),
    maybeLead = R.string.onboarding_kit_lead,
    actions = {
      secondary { TextButton(onClick = onBack, enabled = !isBusy) { Text(stringResource(R.string.onboarding_back)) } }
      primary { WorkingButton(R.string.onboarding_unlock, isBusy, isFullWidth = false, onClick = submit) }
    },
  ) {
    OutlinedButton(onClick = open, enabled = !isBusy) {
      Icon(painterResource(R.drawable.ic_description), contentDescription = null, modifier = Modifier.size(18.dp))
      Text(stringResource(R.string.kit_open), modifier = Modifier.padding(start = 8.dp))
    }
    maybeFileNote?.let { note ->
      if (note == R.string.kit_read) {
        Text(stringResource(note), modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
      } else {
        ErrorLine(note)
      }
    }
    PassphraseField(
      secret,
      R.string.kit_secret_label,
      onChange = {
        secret = it
        isErrorEdited = true
        maybeFileNote = null
      },
      maybeError = maybeFieldError,
      focusRequester = focus,
      maybeOnDone = submit,
      maybeHint = R.string.kit_secret_hint,
    )
    maybeError?.takeUnless { isPassphraseRefusal(it) || isBusy }?.let { ErrorLine(it) }
  }
}

/** A passphrase field with a show/hide button; keyboards don't learn or suggest what's typed. */
@Composable
private fun PassphraseField(
  value: String,
  @StringRes label: Int,
  onChange: (String) -> Unit,
  @StringRes maybeError: Int?,
  focusRequester: FocusRequester,
  maybeOnDone: (() -> Unit)? = null,
  @StringRes maybeHint: Int? = null,
) {
  var isVisible by rememberSaveable { mutableStateOf(false) }
  OutlinedTextField(
    value = value,
    onValueChange = onChange,
    label = { Text(stringResource(label)) },
    visualTransformation = if (isVisible) VisualTransformation.None else PasswordVisualTransformation(),
    // The last field's Done key submits; the others move on with Next.
    keyboardOptions = PASSPHRASE_KEYBOARD.copy(imeAction = if (maybeOnDone == null) ImeAction.Next else ImeAction.Done),
    keyboardActions = KeyboardActions(onDone = { maybeOnDone?.invoke() }),
    singleLine = true,
    isError = maybeError != null,
    supportingText = supportingText(maybeError, maybeHint),
    trailingIcon = {
      IconButton(onClick = { isVisible = !isVisible }) {
        Icon(
          painterResource(if (isVisible) R.drawable.ic_visibility_off else R.drawable.ic_visibility),
          contentDescription = stringResource(if (isVisible) R.string.passphrase_hide else R.string.passphrase_show),
        )
      }
    },
    modifier = Modifier.fillMaxWidth().focusRequester(focusRequester).fieldError(maybeError),
  )
}

/**
 * The main button of a step; while the work runs (key derivation takes seconds) it says so. In a
 * step's action bar it takes its own width ([isFullWidth] false).
 */
@Composable
private fun WorkingButton(
  @StringRes label: Int,
  isBusy: Boolean,
  isFullWidth: Boolean = true,
  onClick: () -> Unit,
) {
  val keyboard = LocalSoftwareKeyboardController.current
  Button(
    onClick = {
      // The keyboard would hide the progress.
      keyboard?.hide()
      onClick()
    },
    enabled = !isBusy,
    modifier = if (isFullWidth) Modifier.fillMaxWidth() else Modifier,
  ) {
    if (isBusy) {
      CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
      Text(stringResource(R.string.enroll_working), modifier = Modifier.padding(start = 8.dp))
    } else {
      Text(stringResource(label))
    }
  }
}

@Composable
internal fun Title(@StringRes text: Int) {
  Text(stringResource(text), style = MaterialTheme.typography.headlineSmall, modifier = Modifier.semantics { heading() })
}

/** An error, read out by TalkBack as soon as it appears. */
@Composable
internal fun ErrorLine(@StringRes text: Int) {
  Text(
    stringResource(text),
    color = MaterialTheme.colorScheme.error,
    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
  )
}

private enum class LinkAsk { IDLE, ASKING, ASKED, FAILED }

/**
 * New phone: no QR code, since nobody could scan it (issue #998). The user approves this phone from
 * a prompt on a signed-in device, or scans the code a computer shows under "Link a device"; either
 * way the other device then asks for the number shown here.
 */
@Composable
fun LinkThisDeviceScreen(
  state: AppState.LinkingThisDevice,
  onAskOtherDevices: suspend () -> Result<Unit>,
  onScanned: (String) -> Unit,
  onCancel: () -> Unit,
) {
  val scope = rememberCoroutineScope()
  // Not saveable: asking hands the request's key to the server and must be a fresh choice each time.
  var ask by remember(state.attempt) { mutableStateOf(LinkAsk.IDLE) }
  var isScanning by rememberSaveable(state.attempt) { mutableStateOf(false) }
  if (isScanning) {
    // Full screen and edge to edge: the surface fills the system bars, the scanner keeps clear of them.
    Dialog(onDismissRequest = { isScanning = false }, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
      Surface(Modifier.fillMaxSize()) {
        // Without a camera the prompt above is the way: it needs none.
        QrScanner(
          onScanned = { scanned ->
            isScanning = false
            onScanned(scanned)
          },
          onCancel = { isScanning = false },
          modifier = Modifier.safeDrawingPadding(),
        )
      }
    }
  }
  Centered {
    StepProgress(step = 2, steps = 2)
    Title(R.string.link_new_title)
    val maybeShortCode = state.maybeShortCode
    if (maybeShortCode == null) {
      state.maybeNotice?.let { ErrorLine(it) }
      Text(stringResource(R.string.link_phone_intro), style = MaterialTheme.typography.bodyMedium, modifier = Modifier.fillMaxWidth())
      // The prompt on the other devices needs the request's key at the server; scanning does not.
      when (ask) {
        LinkAsk.ASKED -> {
          Text(stringResource(R.string.link_asked), modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
          Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
            Text(stringResource(R.string.link_waiting))
          }
        }
        else -> {
          Button(
            enabled = ask != LinkAsk.ASKING,
            onClick = {
              ask = LinkAsk.ASKING
              scope.launch { ask = if (onAskOtherDevices().isSuccess) LinkAsk.ASKED else LinkAsk.FAILED }
            },
            modifier = Modifier.fillMaxWidth(),
          ) { Text(stringResource(R.string.link_ask)) }
          Text(stringResource(R.string.link_ask_hint), style = MaterialTheme.typography.bodySmall)
          if (ask == LinkAsk.FAILED) ErrorLine(R.string.link_ask_failed)
        }
      }
      HorizontalDivider()
      OutlinedButton(onClick = { isScanning = true }, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.link_scan_code))
      }
      Text(stringResource(R.string.link_scan_code_hint), style = MaterialTheme.typography.bodySmall)
      HorizontalDivider()
      Text(stringResource(R.string.link_no_other_device), style = MaterialTheme.typography.bodySmall)
    } else {
      // Connected: only the comparison matters now, large and read out at once.
      Text(stringResource(R.string.link_compare))
      Text(
        maybeShortCode,
        style = MaterialTheme.typography.displaySmall,
        fontFamily = FontFamily.Monospace,
        modifier = Modifier.semantics {
          liveRegion = LiveRegionMode.Polite
          contentDescription = maybeShortCode.toCharArray().joinToString(" ")
        },
      )
      Text(stringResource(R.string.link_confirm_on_other))
      PageLoadingIndicator()
    }
    OutlinedButton(onClick = onCancel) { Text(stringResource(R.string.cancel)) }
  }
}

/** The steps of a recovery through the admins (e2ee.md §6.3.1). */
private enum class OrgStep { CODE, SAVE, CHECK, FINISH }

private const val ORG_STEPS = 4

/**
 * Recovery by the organization's admins, one thing per step: read the code to an admin, then save
 * and check a new recovery kit (the admins saw the old backup key, so the account moves to a new
 * one, protected by this kit), then finish once two admins approved. Finishing too early says so
 * and keeps the user on the step.
 */
@Composable
fun OrgRecoveryScreen(
  code: String,
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  serverHost: String,
  onFinish: (RecoveryKitDraft) -> Unit,
  onCancel: () -> Unit,
) {
  val kit = remember { RecoveryKitDraft() }
  DisposableEffect(kit) { onDispose { kit.close() } }
  var savedStep by rememberSaveable { mutableStateOf(OrgStep.CODE) }
  var isKeyConfirmed by remember { mutableStateOf(false) }
  var maybeSaved by remember { mutableStateOf<Boolean?>(null) }
  // The kit's key lives only in this draft: without a confirmed one the flow is at the kit again.
  val step = if (savedStep > OrgStep.SAVE && !isKeyConfirmed) OrgStep.SAVE else savedStep
  BackHandler(enabled = !isBusy) {
    if (step == OrgStep.CODE) onCancel() else savedStep = OrgStep.entries[step.ordinal - 1]
  }
  when (step) {
    OrgStep.CODE -> StepPage(
      step = 1,
      steps = ORG_STEPS,
      title = stringResource(R.string.org_recovery_title),
      maybeLead = R.string.org_recovery_hint,
      actions = {
        secondary { TextButton(onClick = onCancel) { Text(stringResource(R.string.cancel)) } }
        primary { Button(onClick = { savedStep = OrgStep.SAVE }) { Text(stringResource(R.string.onboarding_continue)) } }
      },
    ) {
      // Read aloud to an admin; not selectable, so it stays out of clipboard histories.
      Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = MaterialTheme.shapes.large, modifier = Modifier.fillMaxWidth()) {
        Text(
          code,
          fontFamily = FontFamily.Monospace,
          style = MaterialTheme.typography.headlineSmall,
          modifier = Modifier
            .padding(Spacing.l)
            .semantics { contentDescription = code.toCharArray().joinToString(" ") },
        )
      }
      Text(stringResource(R.string.org_recovery_next), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
    OrgStep.SAVE -> SaveKitStep(
      kit = kit,
      serverHost = serverHost,
      wasRestarted = false,
      maybeSaved = maybeSaved,
      onSaved = { maybeSaved = it },
      onBack = { savedStep = OrgStep.CODE },
      onContinue = { savedStep = OrgStep.CHECK },
      step = 2,
      steps = ORG_STEPS,
    )
    OrgStep.CHECK -> CheckKitStep(
      kit = kit,
      onBack = { savedStep = OrgStep.SAVE },
      onConfirmed = {
        isKeyConfirmed = true
        savedStep = OrgStep.FINISH
      },
      step = 3,
      steps = ORG_STEPS,
    )
    OrgStep.FINISH -> StepPage(
      step = 4,
      steps = ORG_STEPS,
      title = stringResource(R.string.org_recovery_finish_title),
      maybeLead = R.string.org_recovery_finish_lead,
      actions = {
        secondary { TextButton(onClick = { savedStep = OrgStep.CHECK }, enabled = !isBusy) { Text(stringResource(R.string.onboarding_back)) } }
        primary { WorkingButton(R.string.org_recovery_finish, isBusy, isFullWidth = false) { onFinish(kit) } }
      },
    ) {
      StepPoint(R.drawable.ic_description, R.string.onboarding_finish_where, R.string.onboarding_finish_where_text)
      StepPoint(R.drawable.ic_lock, R.string.onboarding_finish_share, R.string.onboarding_finish_share_text)
      // "Not approved yet" and the device's or the server's failures: above the buttons.
      maybeError?.takeUnless { isBusy }?.let { ErrorLine(it) }
    }
  }
}

@Composable
internal fun Centered(content: @Composable () -> Unit) {
  Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
    Column(
      modifier = Modifier
        .safeDrawingPadding()
        .verticalScroll(rememberScrollState())
        .padding(24.dp),
      verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
      horizontalAlignment = Alignment.CenterHorizontally,
    ) { content() }
  }
}

/** Passphrases: keyboards must not learn, suggest or sync them. */
internal val PASSPHRASE_KEYBOARD = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false)
