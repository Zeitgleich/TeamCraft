package de.zeitgleich.teamcraft.ui.screens

import android.graphics.Bitmap
import androidx.annotation.StringRes
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalUriHandler
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
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.SecretClipboard
import uniffi.teamcraft_ffi.BrandLinks
import uniffi.teamcraft_ffi.LoginBrand

private const val MIN_PASSPHRASE_LENGTH = 12

/** Share of the screen width the link QR code takes. */
private const val QR_CODE_WIDTH_FRACTION = 0.7f
private const val LOGO_WIDTH_FRACTION = 0.6f
private val LOGO_MAX_HEIGHT = 96.dp

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

/**
 * Sign-in: the server's brand (logo, heading, text, links) when it sent one, and which server
 * the app signs in to, so a user with several organizations knows where they are.
 */
@Composable
fun LoginScreen(
  maybeBrand: LoginBrand?,
  maybeLogo: Bitmap?,
  serverHost: String,
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  onLogin: () -> Unit,
) {
  val name = maybeBrand?.displayName ?: stringResource(R.string.app_name)
  Centered {
    if (maybeLogo != null) {
      Image(
        maybeLogo.asImageBitmap(),
        contentDescription = name,
        modifier = Modifier.heightIn(max = LOGO_MAX_HEIGHT).fillMaxWidth(LOGO_WIDTH_FRACTION),
      )
    }
    Text(
      maybeBrand?.maybeTitle ?: name,
      style = MaterialTheme.typography.headlineMedium,
      textAlign = TextAlign.Center,
      modifier = Modifier.semantics { heading() },
    )
    Text(
      maybeBrand?.maybeText ?: stringResource(R.string.sign_in_hint),
      style = MaterialTheme.typography.bodyMedium,
      textAlign = TextAlign.Center,
    )
    maybeError?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center) }
    Button(onClick = onLogin, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
      if (isBusy) {
        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
      } else {
        Text(stringResource(R.string.sign_in))
      }
    }
    Text(
      stringResource(R.string.sign_in_server, serverHost),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    maybeBrand?.links?.let { BrandLinkRow(it) }
  }
}

/** Support, privacy, terms and imprint, as the brand names them; each opens in the browser. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun BrandLinkRow(links: BrandLinks) {
  val uriHandler = LocalUriHandler.current
  val entries = listOfNotNull(
    links.maybeSupport?.let { R.string.brand_support to it },
    links.maybePrivacy?.let { R.string.brand_privacy to it },
    links.maybeTerms?.let { R.string.brand_terms to it },
    links.maybeImprint?.let { R.string.brand_imprint to it },
  )
  FlowRow(horizontalArrangement = Arrangement.Center, modifier = Modifier.fillMaxWidth()) {
    for ((label, url) in entries) {
      TextButton(onClick = { runCatching { uriHandler.openUri(url) } }) { Text(stringResource(label)) }
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
  val maybeProblem = when {
    passphrase.length < MIN_PASSPHRASE_LENGTH -> R.string.passphrase_too_short
    passphrase != repeated -> R.string.passphrases_differ
    else -> null
  }
  Centered {
    Title(R.string.enroll_title)
    Text(stringResource(R.string.enroll_new_hint), style = MaterialTheme.typography.bodyMedium)
    PassphraseField(passphrase, R.string.passphrase, onChange = { passphrase = it })
    PassphraseField(repeated, R.string.passphrase_repeat, onChange = { repeated = it }) {
      if (!isBusy && maybeProblem == null) onEnroll(passphrase)
    }
    Text(
      stringResource(R.string.passphrase_rule, MIN_PASSPHRASE_LENGTH),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.fillMaxWidth(),
    )
    val shownError = maybeError ?: maybeProblem.takeIf { passphrase.isNotEmpty() && repeated.isNotEmpty() }
    shownError?.let { ErrorLine(it) }
    WorkingButton(R.string.enroll_button, isBusy, isEnabled = maybeProblem == null) { onEnroll(passphrase) }
  }
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
  Centered {
    Title(R.string.enroll_title)
    Text(stringResource(R.string.restore_link_hint), style = MaterialTheme.typography.bodyMedium)
    Button(onClick = onLink, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
      Text(stringResource(R.string.link_instead))
    }
    HorizontalDivider(Modifier.padding(vertical = 8.dp))
    Text(stringResource(R.string.restore_passphrase_hint), style = MaterialTheme.typography.bodyMedium)
    PassphraseField(passphrase, R.string.passphrase, onChange = { passphrase = it }) {
      if (!isBusy && passphrase.length >= MIN_PASSPHRASE_LENGTH) onRestore(passphrase)
    }
    maybeError?.let { ErrorLine(it) }
    WorkingButton(
      R.string.recover_button,
      isBusy,
      isEnabled = passphrase.length >= MIN_PASSPHRASE_LENGTH,
      isOutlined = true,
    ) { onRestore(passphrase) }
    TextButton(onClick = onOrgRecovery, enabled = !isBusy) { Text(stringResource(R.string.org_recovery_instead)) }
  }
}

/** A passphrase field with a show/hide button; keyboards don't learn or suggest what's typed. */
@Composable
private fun PassphraseField(
  value: String,
  @StringRes label: Int,
  onChange: (String) -> Unit,
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
    trailingIcon = {
      IconButton(onClick = { isVisible = !isVisible }) {
        Icon(
          painterResource(if (isVisible) R.drawable.ic_visibility_off else R.drawable.ic_visibility),
          contentDescription = stringResource(if (isVisible) R.string.passphrase_hide else R.string.passphrase_show),
        )
      }
    },
    modifier = Modifier.fillMaxWidth(),
  )
}

/** The main button of a step; while the work runs (key derivation takes seconds) it says so. */
@Composable
private fun WorkingButton(
  @StringRes label: Int,
  isBusy: Boolean,
  isEnabled: Boolean,
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
    OutlinedButton(onClick = click, enabled = !isBusy && isEnabled, modifier = Modifier.fillMaxWidth()) { content() }
  } else {
    Button(onClick = click, enabled = !isBusy && isEnabled, modifier = Modifier.fillMaxWidth()) { content() }
  }
}

@Composable
private fun Title(@StringRes text: Int) {
  Text(stringResource(text), style = MaterialTheme.typography.headlineSmall, modifier = Modifier.semantics { heading() })
}

/** An error, read out by TalkBack as soon as it appears. */
@Composable
private fun ErrorLine(@StringRes text: Int) {
  Text(
    stringResource(text),
    color = MaterialTheme.colorScheme.error,
    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
  )
}

@Composable
fun LinkThisDeviceScreen(code: String, maybeShortCode: String?, onCancel: () -> Unit) {
  Centered {
    Title(R.string.link_new_title)
    if (maybeShortCode == null) {
      Text(stringResource(R.string.link_steps_phone), style = MaterialTheme.typography.bodyMedium, modifier = Modifier.fillMaxWidth())
      // The code holds the handshake key: shown on this screen only, never sent anywhere else.
      QrCode(code, Modifier.fillMaxWidth(QR_CODE_WIDTH_FRACTION))
      Text(stringResource(R.string.link_steps_computer), style = MaterialTheme.typography.bodyMedium, modifier = Modifier.fillMaxWidth())
      // Not selectable: a copy goes through the button, marked sensitive and cleared again.
      Text(code, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
      CopySecretButton(code)
      Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        Text(stringResource(R.string.link_waiting))
      }
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
  val isValid = passphrase.length >= MIN_PASSPHRASE_LENGTH && passphrase == repeated
  Centered {
    Title(R.string.org_recovery_title)
    Text(stringResource(R.string.org_recovery_hint), style = MaterialTheme.typography.bodyMedium)
    // Read aloud to an admin; not selectable, so it stays out of clipboard histories.
    Text(code, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.titleMedium)
    Text(stringResource(R.string.org_recovery_new_passphrase), style = MaterialTheme.typography.bodyMedium)
    PassphraseField(passphrase, R.string.passphrase, onChange = { passphrase = it })
    PassphraseField(repeated, R.string.passphrase_repeat, onChange = { repeated = it })
    maybeError?.let { ErrorLine(it) }
    WorkingButton(R.string.org_recovery_finish, isBusy, isEnabled = isValid) { onFinish(passphrase) }
    OutlinedButton(onClick = onCancel, enabled = !isBusy) { Text(stringResource(R.string.cancel)) }
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
