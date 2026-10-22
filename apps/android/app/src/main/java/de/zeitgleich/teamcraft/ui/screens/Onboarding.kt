package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.SecretClipboard

private const val MIN_PASSPHRASE_LENGTH = 12

/** Share of the screen width the link QR code takes. */
private const val QR_CODE_WIDTH_FRACTION = 0.7f

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
fun LoginScreen(@StringRes maybeError: Int?, isBusy: Boolean, onLogin: () -> Unit) {
  Centered {
    Text(stringResource(R.string.app_name), style = MaterialTheme.typography.headlineMedium)
    Text(stringResource(R.string.sign_in_hint), style = MaterialTheme.typography.bodyMedium)
    maybeError?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error) }
    Button(onClick = onLogin, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
      Text(stringResource(R.string.sign_in))
    }
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
  var passphrase by remember { mutableStateOf("") }
  var repeated by remember { mutableStateOf("") }
  val maybeProblem = when {
    passphrase.length < MIN_PASSPHRASE_LENGTH -> R.string.passphrase_too_short
    !hasExistingIdentity && passphrase != repeated -> R.string.passphrases_differ
    else -> null
  }
  Centered {
    Text(stringResource(R.string.enroll_title), style = MaterialTheme.typography.headlineSmall)
    Text(
      stringResource(if (hasExistingIdentity) R.string.enroll_recover_hint else R.string.enroll_new_hint),
      style = MaterialTheme.typography.bodyMedium,
    )
    OutlinedTextField(
      value = passphrase,
      onValueChange = { passphrase = it },
      label = { Text(stringResource(R.string.passphrase)) },
      visualTransformation = PasswordVisualTransformation(),
      keyboardOptions = PASSPHRASE_KEYBOARD,
      singleLine = true,
      modifier = Modifier.fillMaxWidth(),
    )
    if (!hasExistingIdentity) {
      OutlinedTextField(
        value = repeated,
        onValueChange = { repeated = it },
        label = { Text(stringResource(R.string.passphrase_repeat)) },
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = PASSPHRASE_KEYBOARD,
        singleLine = true,
        modifier = Modifier.fillMaxWidth(),
      )
    }
    val shownError = maybeError ?: maybeProblem.takeIf { passphrase.isNotEmpty() }
    shownError?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error) }
    Button(
      onClick = { onEnroll(passphrase) },
      enabled = !isBusy && maybeProblem == null,
      modifier = Modifier.fillMaxWidth(),
    ) {
      Text(stringResource(if (hasExistingIdentity) R.string.recover_button else R.string.enroll_button))
    }
    if (hasExistingIdentity) {
      OutlinedButton(onClick = onLink, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.link_instead))
      }
      OutlinedButton(onClick = onOrgRecovery, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.org_recovery_instead))
      }
    }
    if (isBusy) CircularProgressIndicator()
  }
}

@Composable
fun LinkThisDeviceScreen(code: String, maybeShortCode: String?, onCancel: () -> Unit) {
  Centered {
    Text(stringResource(R.string.link_new_title), style = MaterialTheme.typography.headlineSmall)
    Text(stringResource(R.string.link_new_hint), style = MaterialTheme.typography.bodyMedium)
    // The code holds the handshake key: shown on this screen only, never sent anywhere else.
    QrCode(code, Modifier.fillMaxWidth(QR_CODE_WIDTH_FRACTION))
    Text(stringResource(R.string.link_qr_hint), style = MaterialTheme.typography.bodySmall)
    // Not selectable: a copy goes through the button, marked sensitive and cleared again.
    Text(code, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
    CopySecretButton(code)
    if (maybeShortCode == null) {
      Text(stringResource(R.string.link_waiting))
      CircularProgressIndicator()
    } else {
      Text(stringResource(R.string.link_compare))
      Text(maybeShortCode, style = MaterialTheme.typography.displaySmall, fontFamily = FontFamily.Monospace)
      Text(stringResource(R.string.link_confirm_on_other))
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
  val isValid = passphrase.length >= MIN_PASSPHRASE_LENGTH && passphrase == repeated
  Centered {
    Text(stringResource(R.string.org_recovery_title), style = MaterialTheme.typography.headlineSmall)
    Text(stringResource(R.string.org_recovery_hint), style = MaterialTheme.typography.bodyMedium)
    // Read aloud to an admin; not selectable, so it stays out of clipboard histories.
    Text(code, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.titleMedium)
    Text(stringResource(R.string.org_recovery_new_passphrase), style = MaterialTheme.typography.bodyMedium)
    OutlinedTextField(
      value = passphrase,
      onValueChange = { passphrase = it },
      label = { Text(stringResource(R.string.passphrase)) },
      visualTransformation = PasswordVisualTransformation(),
      keyboardOptions = PASSPHRASE_KEYBOARD,
      singleLine = true,
      modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
      value = repeated,
      onValueChange = { repeated = it },
      label = { Text(stringResource(R.string.passphrase_repeat)) },
      visualTransformation = PasswordVisualTransformation(),
      keyboardOptions = PASSPHRASE_KEYBOARD,
      singleLine = true,
      modifier = Modifier.fillMaxWidth(),
    )
    maybeError?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error) }
    Button(onClick = { onFinish(passphrase) }, enabled = !isBusy && isValid, modifier = Modifier.fillMaxWidth()) {
      Text(stringResource(R.string.org_recovery_finish))
    }
    OutlinedButton(onClick = onCancel) { Text(stringResource(R.string.cancel)) }
    if (isBusy) CircularProgressIndicator()
  }
}

@Composable
private fun Centered(content: @Composable () -> Unit) {
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

/** Copies a secret code, marked sensitive, and clears the clipboard again after a minute. */
@Composable
private fun CopySecretButton(secret: String) {
  val context = LocalContext.current
  TextButton(onClick = { SecretClipboard.copy(context.applicationContext, secret) }) {
    Text(stringResource(R.string.link_copy_code))
  }
}
