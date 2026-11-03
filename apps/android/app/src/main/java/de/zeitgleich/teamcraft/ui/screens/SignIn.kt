package de.zeitgleich.teamcraft.ui.screens

import android.graphics.Bitmap
import android.icu.text.BreakIterator
import androidx.annotation.StringRes
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.autofill.ContentType
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.contentType
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.auth.SignInItem
import de.zeitgleich.teamcraft.auth.cleanTotpCode
import de.zeitgleich.teamcraft.auth.SignInField
import de.zeitgleich.teamcraft.auth.localFormErrors
import de.zeitgleich.teamcraft.auth.localSignInErrorField
import de.zeitgleich.teamcraft.auth.signInItems
import de.zeitgleich.teamcraft.auth.totpCodeToSend
import de.zeitgleich.teamcraft.ui.forms.FieldErrors
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.focusFirst
import de.zeitgleich.teamcraft.ui.forms.supportingText
import uniffi.teamcraft_ffi.BrandLinks
import uniffi.teamcraft_ffi.LoginBrand
import uniffi.teamcraft_ffi.SignInPageInfo

/** Widest the sign-in card gets on tablets and in landscape. */
private val CARD_MAX_WIDTH = 440.dp
private val SSO_ICON_SIZE = 20.dp
private val BUTTON_MIN_HEIGHT = 48.dp
/** The brand tiles: app-icon sized squares, the web's 64 px with its 16 px corners. */
private val TILE_SIZE = 64.dp
private val TILE_SHAPE = RoundedCornerShape(16.dp)
private val TILE_GAP = 12.dp
private val TILE_PADDING = 8.dp
private val TILE_ELEVATION = 4.dp

private val EMAIL_KEYBOARD = KeyboardOptions(
  keyboardType = KeyboardType.Email,
  autoCorrectEnabled = false,
  imeAction = ImeAction.Next,
)
private val CODE_KEYBOARD = KeyboardOptions(
  keyboardType = KeyboardType.NumberPassword,
  autoCorrectEnabled = false,
  imeAction = ImeAction.Done,
)

/**
 * Sign-in: the server's brand, then the ways the server offers in its order ("Sign in with …"
 * through the identity provider, and e-mail and password of a local account), help, and which
 * server the app signs in to, so a user with several organizations knows where they are.
 * [maybePage] is null while the server's page is not known yet; [maybeOnChangeServer] is null when
 * the organization locked the server.
 */
@Composable
fun LoginScreen(
  maybeBrand: LoginBrand?,
  maybeLogo: Bitmap?,
  maybeIcon: Bitmap?,
  maybePage: SignInPageInfo?,
  maybeSsoIcon: Bitmap?,
  serverHost: String,
  @StringRes maybeError: Int?,
  maybeErrorCode: String?,
  isBusy: Boolean,
  onSso: () -> Unit,
  onLocal: (email: String, password: String, maybeCode: String?) -> Unit,
  maybeOnChangeServer: (() -> Unit)?,
) {
  val name = maybeBrand?.displayName ?: stringResource(R.string.app_name)
  Centered {
    Column(
      modifier = Modifier.widthIn(max = CARD_MAX_WIDTH).fillMaxWidth(),
      verticalArrangement = Arrangement.spacedBy(16.dp),
      horizontalAlignment = Alignment.CenterHorizontally,
    ) {
      BrandHeader(maybeBrand, maybeLogo, maybeIcon, name)
      // A refused e-mail or password is shown at the local form's field, everything else up here.
      maybeError?.takeIf { localSignInErrorField(it) == null }?.let { SignInError(it, maybeErrorCode) }
      if (maybePage == null) {
        val loading = stringResource(R.string.sign_in_loading)
        CircularProgressIndicator(Modifier.size(32.dp).semantics { contentDescription = loading })
      } else {
        SignInMethods(maybePage, maybeSsoIcon, maybeError, isBusy, onSso, onLocal)
        SignInHelp(maybePage)
      }
      EncryptionNote()
      Text(
        stringResource(R.string.sign_in_server, serverHost),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
      )
      maybeOnChangeServer?.let { onChange ->
        TextButton(onClick = onChange, enabled = !isBusy) { Text(stringResource(R.string.server_change)) }
      }
      maybeBrand?.links?.let { BrandLinkRow(it) }
    }
  }
}

/** Tiles, logo, heading and the brand's text (or the default hint). */
@Composable
private fun BrandHeader(maybeBrand: LoginBrand?, maybeLogo: Bitmap?, maybeIcon: Bitmap?, name: String) {
  if (maybeBrand != null && !maybeBrand.isProduct) BrandTiles(maybeIcon, name)
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
}

/**
 * A server brand: TeamCraft's tile and the brand's beside it, like two app icons (issue #463, as
 * on the web). Without an icon, the brand's initial on its accent.
 */
@Composable
private fun BrandTiles(maybeIcon: Bitmap?, name: String) {
  Row(horizontalArrangement = Arrangement.spacedBy(TILE_GAP), modifier = Modifier.clearAndSetSemantics {}) {
    Tile(MaterialTheme.colorScheme.surfaceVariant) { StillMark(Modifier.padding(TILE_PADDING).fillMaxSize()) }
    if (maybeIcon != null) {
      Tile(MaterialTheme.colorScheme.surfaceVariant) {
        Image(
          maybeIcon.asImageBitmap(),
          contentDescription = null,
          contentScale = ContentScale.Crop,
          modifier = Modifier.fillMaxSize(),
        )
      }
    } else {
      Tile(MaterialTheme.colorScheme.primary) {
        Text(
          firstGrapheme(name).uppercase(),
          style = MaterialTheme.typography.headlineSmall,
          color = MaterialTheme.colorScheme.onPrimary,
        )
      }
    }
  }
}

/** The first user-perceived character (an emoji or accented letter stays whole). */
private fun firstGrapheme(text: String): String {
  val breaks = BreakIterator.getCharacterInstance().apply { setText(text) }
  val end = breaks.next()
  return if (end == BreakIterator.DONE) "" else text.substring(0, end)
}

@Composable
private fun Tile(background: Color, content: @Composable () -> Unit) {
  Box(
    Modifier
      .size(TILE_SIZE)
      .shadow(TILE_ELEVATION, TILE_SHAPE)
      .clip(TILE_SHAPE)
      .background(background)
      .border(1.dp, MaterialTheme.colorScheme.outlineVariant, TILE_SHAPE),
    contentAlignment = Alignment.Center,
  ) { content() }
}

/** The methods in the server's order, with "or" between them. */
@Composable
private fun SignInMethods(
  page: SignInPageInfo,
  maybeSsoIcon: Bitmap?,
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  onSso: () -> Unit,
  onLocal: (String, String, String?) -> Unit,
) {
  for (item in signInItems(page.methods, page.localRequiresCode)) {
    when (item) {
      SignInItem.SsoButton -> SsoButton(page.ssoLabel, maybeSsoIcon, isBusy, onSso)
      SignInItem.Divider -> OrDivider()
      is SignInItem.LocalForm -> LocalForm(item.isCodeRequired, maybeError, isBusy, onLocal)
    }
  }
}

/** "Sign in with <provider>", with the provider's icon when the server has one. */
@Composable
private fun SsoButton(label: String, maybeIcon: Bitmap?, isBusy: Boolean, onClick: () -> Unit) {
  Button(
    onClick = onClick,
    enabled = !isBusy,
    modifier = Modifier.fillMaxWidth().heightIn(min = BUTTON_MIN_HEIGHT),
  ) {
    if (maybeIcon != null) {
      // Decorative: the label names the provider.
      Image(maybeIcon.asImageBitmap(), contentDescription = null, modifier = Modifier.size(SSO_ICON_SIZE))
      Spacer(Modifier.width(8.dp))
    }
    Text(stringResource(R.string.sign_in_with, label), textAlign = TextAlign.Center)
  }
}

@Composable
private fun OrDivider() {
  Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
    HorizontalDivider(Modifier.weight(1f))
    Text(
      stringResource(R.string.sign_in_or),
      style = MaterialTheme.typography.labelMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(horizontal = 12.dp),
    )
    HorizontalDivider(Modifier.weight(1f))
  }
}

/**
 * E-mail, password and one-time code of a local account. The password and the code live only in
 * memory (never in saved state); the code is cleared after each attempt, since it expires anyway.
 * A refused submit shows every missing field's error at once and focuses the first; the server's
 * refusal ([maybeError], when it belongs to a field) stays at its field until the user edits.
 */
@Composable
private fun LocalForm(
  isCodeRequired: Boolean,
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  onSubmit: (String, String, String?) -> Unit,
) {
  var email by rememberSaveable { mutableStateOf("") }
  var password by remember { mutableStateOf("") }
  var code by remember { mutableStateOf("") }
  var isPasswordVisible by remember { mutableStateOf(false) }
  var hasTried by remember { mutableStateOf(false) }
  var hasSubmitted by remember { mutableStateOf(false) }
  var isRefusalEdited by remember(maybeError) { mutableStateOf(false) }
  val requesters = remember { SignInField.entries.associateWith { FocusRequester() } }
  val keyboard = LocalSoftwareKeyboardController.current
  val focus = LocalFocusManager.current
  val errors = localFormErrors(email, password, code, isCodeRequired)
  val maybeRefusal = maybeError?.takeUnless { isRefusalEdited || isBusy }
  val maybeRefusedField = maybeRefusal?.let(::localSignInErrorField)
  val shown = (if (hasTried) errors else FieldErrors<SignInField>(emptyMap()))
    .let { if (maybeRefusedField != null) it.withServerError(maybeRefusedField, maybeRefusal) else it }
  val edit: (() -> Unit) -> Unit = { change ->
    change()
    isRefusalEdited = true
  }
  val submit: () -> Unit = {
    hasTried = true
    if (!errors.isValid) {
      errors.focusFirst(requesters)
    } else if (!isBusy) {
      // The keyboard would hide the progress and the answer.
      keyboard?.hide()
      isRefusalEdited = false
      hasSubmitted = true
      onSubmit(email, password, totpCodeToSend(code))
    }
  }
  LaunchedEffect(isBusy) {
    if (!isBusy && hasSubmitted) {
      code = ""
      hasSubmitted = false
    }
  }
  LaunchedEffect(maybeRefusedField) {
    maybeRefusedField?.let { requesters[it]?.requestFocus() }
  }
  OutlinedTextField(
    value = email,
    onValueChange = { edit { email = it } },
    label = { Text(stringResource(R.string.local_email)) },
    keyboardOptions = EMAIL_KEYBOARD,
    keyboardActions = KeyboardActions(onNext = { focus.moveFocus(FocusDirection.Down) }),
    singleLine = true,
    enabled = !isBusy,
    isError = shown[SignInField.EMAIL] != null,
    supportingText = supportingText(shown[SignInField.EMAIL]),
    modifier = Modifier
      .fillMaxWidth()
      .focusRequester(requesters.getValue(SignInField.EMAIL))
      .fieldError(shown[SignInField.EMAIL])
      .semantics { contentType = ContentType.Username + ContentType.EmailAddress },
  )
  OutlinedTextField(
    value = password,
    onValueChange = { edit { password = it } },
    label = { Text(stringResource(R.string.local_password)) },
    visualTransformation = if (isPasswordVisible) VisualTransformation.None else PasswordVisualTransformation(),
    // Next goes on to the code; Done sends the form when the code field is optional.
    keyboardOptions = PASSPHRASE_KEYBOARD.copy(imeAction = if (isCodeRequired) ImeAction.Next else ImeAction.Done),
    keyboardActions = KeyboardActions(
      onNext = { focus.moveFocus(FocusDirection.Down) },
      onDone = { submit() },
    ),
    singleLine = true,
    enabled = !isBusy,
    isError = shown[SignInField.PASSWORD] != null,
    supportingText = supportingText(shown[SignInField.PASSWORD]),
    trailingIcon = {
      IconButton(onClick = { isPasswordVisible = !isPasswordVisible }) {
        Icon(
          painterResource(if (isPasswordVisible) R.drawable.ic_visibility_off else R.drawable.ic_visibility),
          contentDescription = stringResource(if (isPasswordVisible) R.string.local_password_hide else R.string.local_password_show),
        )
      }
    },
    modifier = Modifier
      .fillMaxWidth()
      .focusRequester(requesters.getValue(SignInField.PASSWORD))
      .fieldError(shown[SignInField.PASSWORD])
      .semantics { contentType = ContentType.Password },
  )
  OutlinedTextField(
    value = code,
    onValueChange = { edit { code = cleanTotpCode(it) } },
    label = { Text(stringResource(if (isCodeRequired) R.string.local_code else R.string.local_code_optional)) },
    keyboardOptions = CODE_KEYBOARD,
    keyboardActions = KeyboardActions(onDone = { submit() }),
    singleLine = true,
    enabled = !isBusy,
    isError = shown[SignInField.CODE] != null,
    supportingText = supportingText(shown[SignInField.CODE]),
    modifier = Modifier
      .fillMaxWidth()
      .focusRequester(requesters.getValue(SignInField.CODE))
      .fieldError(shown[SignInField.CODE])
      .semantics { contentType = ContentType.SmsOtpCode },
  )
  Button(
    onClick = submit,
    enabled = !isBusy,
    modifier = Modifier.fillMaxWidth().heightIn(min = BUTTON_MIN_HEIGHT),
  ) {
    if (isBusy && hasSubmitted) {
      CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
    } else {
      Text(stringResource(R.string.sign_in))
    }
  }
}

/** The organization's help for signing in, and its link (opens in the browser). */
@Composable
private fun SignInHelp(page: SignInPageInfo) {
  val uriHandler = LocalUriHandler.current
  page.maybeHelpText?.let {
    Text(
      it,
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      textAlign = TextAlign.Center,
    )
  }
  page.maybeHelpLink?.let { link ->
    TextButton(onClick = { runCatching { uriHandler.openUri(link) } }) { Text(stringResource(R.string.sign_in_help_link)) }
  }
}

@Composable
private fun EncryptionNote() {
  Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    Icon(
      Icons.Filled.Lock,
      contentDescription = null,
      tint = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.size(16.dp),
    )
    Text(
      stringResource(R.string.sign_in_e2ee_note),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** An error, centred and read out by TalkBack as soon as it appears. */
@Composable
private fun SignInError(@StringRes text: Int, maybeCode: String? = null) {
  Text(
    maybeCode?.let { stringResource(text, it) } ?: stringResource(text),
    color = MaterialTheme.colorScheme.error,
    textAlign = TextAlign.Center,
    modifier = Modifier.fillMaxWidth().semantics { liveRegion = LiveRegionMode.Polite },
  )
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
