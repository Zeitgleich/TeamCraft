package de.zeitgleich.teamcraft.ui.screens

import android.graphics.Bitmap
import androidx.annotation.StringRes
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.CheckedServer
import uniffi.teamcraft_ffi.LoginBrand

/** Keyboard for addresses: no autocorrect or capitalisation, Go submits. */
private val ADDRESS_KEYBOARD = KeyboardOptions(
  keyboardType = KeyboardType.Uri,
  autoCorrectEnabled = false,
  imeAction = ImeAction.Go,
)

/**
 * The server entry: the user types `team.example.com`, a full address, or pastes an invite or
 * message link; the core normalises it. With [isLocked] the organization fixed the address: the
 * field is read-only and Continue checks the server again.
 */
@Composable
fun ServerEntryScreen(
  prefill: String,
  @StringRes maybeError: Int?,
  isLocked: Boolean,
  isBusy: Boolean,
  onSubmit: (String) -> Unit,
) {
  var address by rememberSaveable(prefill) { mutableStateOf(prefill) }
  val focus = remember { FocusRequester() }
  val keyboard = LocalSoftwareKeyboardController.current
  val submit = {
    if (!isBusy) {
      // The keyboard would hide the progress and the answer.
      keyboard?.hide()
      onSubmit(address)
    }
  }
  Centered {
    Title(R.string.server_entry_title)
    Text(
      stringResource(if (isLocked) R.string.server_entry_locked else R.string.server_entry_hint),
      style = MaterialTheme.typography.bodyMedium,
      modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
      value = address,
      onValueChange = { address = it },
      label = { Text(stringResource(R.string.server_entry_label)) },
      placeholder = { Text(stringResource(R.string.server_entry_placeholder)) },
      keyboardOptions = ADDRESS_KEYBOARD,
      keyboardActions = KeyboardActions(onGo = { submit() }),
      singleLine = true,
      readOnly = isLocked,
      isError = maybeError != null,
      modifier = Modifier.fillMaxWidth().focusRequester(focus),
    )
    maybeError?.let { ErrorLine(it) }
    Button(onClick = submit, enabled = !isBusy && address.isNotBlank(), modifier = Modifier.fillMaxWidth()) {
      if (isBusy) {
        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
      } else {
        Text(stringResource(if (maybeError != null && isLocked) R.string.retry else R.string.server_entry_continue))
      }
    }
  }
  LaunchedEffect(isLocked) {
    if (!isLocked) focus.requestFocus()
  }
}

/**
 * The server answered: its brand (logo and name) and host, so the user sees where they are about
 * to sign in before the browser opens. [maybeOnOtherServer] is null when the server is locked.
 */
@Composable
fun ConfirmServerScreen(
  checked: CheckedServer,
  maybeBrand: LoginBrand?,
  maybeLogo: Bitmap?,
  isBusy: Boolean,
  onContinue: () -> Unit,
  maybeOnOtherServer: (() -> Unit)?,
) {
  val name = maybeBrand?.displayName ?: checked.server.displayName
  Centered {
    if (maybeLogo != null) {
      Image(
        maybeLogo.asImageBitmap(),
        contentDescription = name,
        modifier = Modifier.heightIn(max = LOGO_MAX_HEIGHT).fillMaxWidth(LOGO_WIDTH_FRACTION),
      )
    }
    Text(
      name,
      style = MaterialTheme.typography.headlineMedium,
      textAlign = TextAlign.Center,
      modifier = Modifier.semantics { heading() },
    )
    Text(
      stringResource(R.string.sign_in_server, checked.server.address.host),
      style = MaterialTheme.typography.bodyLarge,
      textAlign = TextAlign.Center,
    )
    Text(
      stringResource(R.string.server_confirm_provider, checked.identityProviderHost),
      style = MaterialTheme.typography.bodyMedium,
      textAlign = TextAlign.Center,
    )
    if (checked.isIdentityProviderElsewhere) {
      // A server can copy another instance's sign-in settings; a provider outside the server's
      // domain gets an explicit warning (docs/e2ee.md "Sign-in through the server's provider").
      Text(
        stringResource(R.string.server_confirm_provider_elsewhere, checked.identityProviderHost),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.error,
        textAlign = TextAlign.Center,
        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
      )
    }
    Text(
      stringResource(R.string.server_confirm_hint),
      style = MaterialTheme.typography.bodyMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      textAlign = TextAlign.Center,
    )
    Button(onClick = onContinue, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
      if (isBusy) {
        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
      } else {
        Text(stringResource(R.string.server_confirm_continue))
      }
    }
    maybeOnOtherServer?.let { onOther ->
      TextButton(onClick = onOther, enabled = !isBusy) { Text(stringResource(R.string.server_confirm_other)) }
    }
  }
}
