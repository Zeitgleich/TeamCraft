package de.zeitgleich.teamcraft.ui.screens

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
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
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
import kotlinx.coroutines.launch
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
import androidx.compose.ui.semantics.contentDescription
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
import de.zeitgleich.teamcraft.ui.forms.MIN_PASSPHRASE_LENGTH
import de.zeitgleich.teamcraft.ui.forms.NewPassphraseField
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.focusFirst
import de.zeitgleich.teamcraft.ui.forms.isPassphraseRefusal
import de.zeitgleich.teamcraft.ui.forms.newPassphraseErrors
import de.zeitgleich.teamcraft.ui.forms.passphraseError
import de.zeitgleich.teamcraft.ui.forms.supportingText

internal const val LOGO_WIDTH_FRACTION = 0.6f
internal val LOGO_MAX_HEIGHT = 96.dp

@Composable
fun LoadingScreen() {
  Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
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
  onEnroll: (String) -> Unit,
  onLink: () -> Unit,
  onOrgRecovery: () -> Unit,
) {
  if (hasExistingIdentity) {
    RestoreScreen(maybeError, isBusy, onRestore = onEnroll, onLink = onLink, onOrgRecovery = onOrgRecovery)
  } else {
    NewKeysScreen(maybeError, isBusy, onEnroll)
  }
}

/** The first device of an account: choose the recovery passphrase. */
@Composable
private fun NewKeysScreen(@StringRes maybeError: Int?, isBusy: Boolean, onEnroll: (String) -> Unit) {
  var passphrase by remember { mutableStateOf("") }
  var repeated by remember { mutableStateOf("") }
  Centered {
    Title(R.string.enroll_title)
    Text(stringResource(R.string.enroll_new_hint), style = MaterialTheme.typography.bodyMedium)
    NewPassphraseFields(
      passphrase,
      repeated,
      onChange = { first, second ->
        passphrase = first
        repeated = second
      },
      maybeError = maybeError,
      isBusy = isBusy,
      submitLabel = R.string.enroll_button,
      onSubmit = onEnroll,
    )
  }
}

/**
 * A new passphrase, its repetition and the submit button. A refused submit shows every error at
 * its field and focuses the first; afterwards the errors follow the input. [maybeError] is the
 * server's or the device's failure, which belongs to no field: it sits next to the button.
 */
@Composable
private fun NewPassphraseFields(
  passphrase: String,
  repeated: String,
  onChange: (String, String) -> Unit,
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  @StringRes submitLabel: Int,
  onSubmit: (String) -> Unit,
) {
  var isSubmitted by remember { mutableStateOf(false) }
  val requesters = remember { NewPassphraseField.entries.associateWith { FocusRequester() } }
  val errors = newPassphraseErrors(passphrase, repeated)
  val shown = errors.takeIf { isSubmitted }
  val submit: () -> Unit = {
    isSubmitted = true
    if (!errors.isValid) {
      errors.focusFirst(requesters)
    } else if (!isBusy) {
      onSubmit(passphrase)
    }
  }
  PassphraseField(
    passphrase,
    R.string.passphrase,
    onChange = { onChange(it, repeated) },
    maybeError = shown?.get(NewPassphraseField.PASSPHRASE),
    focusRequester = requesters.getValue(NewPassphraseField.PASSPHRASE),
  )
  PassphraseField(
    repeated,
    R.string.passphrase_repeat,
    onChange = { onChange(passphrase, it) },
    maybeError = shown?.get(NewPassphraseField.REPEATED),
    focusRequester = requesters.getValue(NewPassphraseField.REPEATED),
    maybeOnDone = submit,
  )
  Text(
    stringResource(R.string.passphrase_rule, MIN_PASSPHRASE_LENGTH),
    style = MaterialTheme.typography.bodySmall,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier.fillMaxWidth(),
  )
  maybeError?.takeUnless { isBusy }?.let { ErrorLine(it) }
  WorkingButton(submitLabel, isBusy, onClick = submit)
}

/**
 * Another device of the account exists: linking from it is the quick way (no passphrase, the
 * history comes along), so it comes first; the passphrase and the admins are the fallbacks.
 */
@Composable
private fun RestoreScreen(
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  onRestore: (String) -> Unit,
  onLink: () -> Unit,
  onOrgRecovery: () -> Unit,
) {
  var passphrase by remember { mutableStateOf("") }
  var isSubmitted by remember { mutableStateOf(false) }
  // A wrong passphrase is about the typed one: shown at the field until the user edits it.
  var isErrorEdited by remember(maybeError) { mutableStateOf(false) }
  val focus = remember { FocusRequester() }
  val maybeRefusal = maybeError?.takeIf { isPassphraseRefusal(it) && !isErrorEdited && !isBusy }
  val maybeFieldError = passphraseError(passphrase).takeIf { isSubmitted } ?: maybeRefusal
  val submit: () -> Unit = {
    isSubmitted = true
    isErrorEdited = false
    if (passphraseError(passphrase) != null) {
      focus.requestFocus()
    } else if (!isBusy) {
      onRestore(passphrase)
    }
  }
  LaunchedEffect(maybeError, isBusy) {
    if (maybeRefusal != null) focus.requestFocus()
  }
  Centered {
    Title(R.string.enroll_title)
    Text(stringResource(R.string.restore_link_hint), style = MaterialTheme.typography.bodyMedium)
    Button(onClick = onLink, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
      Text(stringResource(R.string.link_instead))
    }
    HorizontalDivider(Modifier.padding(vertical = 8.dp))
    Text(stringResource(R.string.restore_passphrase_hint), style = MaterialTheme.typography.bodyMedium)
    PassphraseField(
      passphrase,
      R.string.passphrase,
      onChange = {
        passphrase = it
        isErrorEdited = true
      },
      maybeError = maybeFieldError,
      focusRequester = focus,
      maybeOnDone = submit,
    )
    maybeError?.takeUnless { isPassphraseRefusal(it) || isBusy }?.let { ErrorLine(it) }
    WorkingButton(R.string.recover_button, isBusy, isOutlined = true, onClick = submit)
    TextButton(onClick = onOrgRecovery, enabled = !isBusy) { Text(stringResource(R.string.org_recovery_instead)) }
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
    supportingText = supportingText(maybeError),
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

/** The main button of a step; while the work runs (key derivation takes seconds) it says so. */
@Composable
private fun WorkingButton(
  @StringRes label: Int,
  isBusy: Boolean,
  isOutlined: Boolean = false,
  onClick: () -> Unit,
) {
  val keyboard = LocalSoftwareKeyboardController.current
  val content: @Composable () -> Unit = {
    if (isBusy) {
      CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
      Text(stringResource(R.string.enroll_working), modifier = Modifier.padding(start = 8.dp))
    } else {
      Text(stringResource(label))
    }
  }
  val click = {
    // The keyboard would hide the progress.
    keyboard?.hide()
    onClick()
  }
  if (isOutlined) {
    OutlinedButton(onClick = click, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) { content() }
  } else {
    Button(onClick = click, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) { content() }
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
    Dialog(onDismissRequest = { isScanning = false }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
      Surface(Modifier.fillMaxSize()) {
        // Without a camera the prompt above is the way: it needs none.
        QrScanner(
          onScanned = { scanned ->
            isScanning = false
            onScanned(scanned)
          },
          onCancel = { isScanning = false },
        )
      }
    }
  }
  Centered {
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
      CircularProgressIndicator()
    }
    OutlinedButton(onClick = onCancel) { Text(stringResource(R.string.cancel)) }
  }
}

/** Recovery by the organization's admins: read the code to an admin, then choose a new passphrase. */
@Composable
fun OrgRecoveryScreen(
  code: String,
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  onFinish: (String) -> Unit,
  onCancel: () -> Unit,
) {
  var passphrase by remember { mutableStateOf("") }
  var repeated by remember { mutableStateOf("") }
  Centered {
    Title(R.string.org_recovery_title)
    Text(stringResource(R.string.org_recovery_hint), style = MaterialTheme.typography.bodyMedium)
    // Read aloud to an admin; not selectable, so it stays out of clipboard histories.
    Text(code, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.titleMedium)
    Text(stringResource(R.string.org_recovery_new_passphrase), style = MaterialTheme.typography.bodyMedium)
    NewPassphraseFields(
      passphrase,
      repeated,
      onChange = { first, second ->
        passphrase = first
        repeated = second
      },
      maybeError = maybeError,
      isBusy = isBusy,
      submitLabel = R.string.org_recovery_finish,
      onSubmit = onFinish,
    )
    OutlinedButton(onClick = onCancel, enabled = !isBusy) { Text(stringResource(R.string.cancel)) }
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
