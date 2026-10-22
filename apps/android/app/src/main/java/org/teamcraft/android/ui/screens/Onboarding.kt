package org.teamcraft.android.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import org.teamcraft.android.R

private const val MIN_PASSPHRASE_LENGTH = 12

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
  var passphrase by rememberSaveable { mutableStateOf("") }
  var repeated by rememberSaveable { mutableStateOf("") }
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
      singleLine = true,
      modifier = Modifier.fillMaxWidth(),
    )
    if (!hasExistingIdentity) {
      OutlinedTextField(
        value = repeated,
        onValueChange = { repeated = it },
        label = { Text(stringResource(R.string.passphrase_repeat)) },
        visualTransformation = PasswordVisualTransformation(),
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
    SelectionContainer {
      Text(code, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
    }
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
  var passphrase by rememberSaveable { mutableStateOf("") }
  var repeated by rememberSaveable { mutableStateOf("") }
  val isValid = passphrase.length >= MIN_PASSPHRASE_LENGTH && passphrase == repeated
  Centered {
    Text(stringResource(R.string.org_recovery_title), style = MaterialTheme.typography.headlineSmall)
    Text(stringResource(R.string.org_recovery_hint), style = MaterialTheme.typography.bodyMedium)
    SelectionContainer {
      Text(code, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.titleMedium)
    }
    Text(stringResource(R.string.org_recovery_new_passphrase), style = MaterialTheme.typography.bodyMedium)
    OutlinedTextField(
      value = passphrase,
      onValueChange = { passphrase = it },
      label = { Text(stringResource(R.string.passphrase)) },
      visualTransformation = PasswordVisualTransformation(),
      singleLine = true,
      modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
      value = repeated,
      onValueChange = { repeated = it },
      label = { Text(stringResource(R.string.passphrase_repeat)) },
      visualTransformation = PasswordVisualTransformation(),
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
