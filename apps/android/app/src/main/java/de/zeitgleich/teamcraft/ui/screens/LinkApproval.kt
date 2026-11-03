package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.key
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.forms.LINK_CODE_LENGTH
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.linkCodeError
import de.zeitgleich.teamcraft.ui.forms.supportingText
import kotlinx.coroutines.delay
import uniffi.teamcraft_ffi.LinkPlatform
import uniffi.teamcraft_ffi.LinkRequestItem

/**
 * Prompts for new devices of this account that ask to be linked, over the signed-in UI. A card,
 * not a dialog: it takes no focus and blocks nothing. It never approves by itself and never shows
 * the code; the user types the code the new device shows.
 */
@Composable
fun LinkRequestPrompts(viewModel: MainViewModel, modifier: Modifier = Modifier) {
  val changeCount by viewModel.changeCount.collectAsState()
  val requests by viewModel.linkRequests.collectAsState()
  val maybeReview by viewModel.linkReview.collectAsState()
  val maybeFailure by viewModel.linkPromptFailure.collectAsState()
  // The core tells the app about every change, including a request that arrived or was taken by
  // another device; reading the list is cheap.
  LaunchedEffect(changeCount) { viewModel.refreshLinkRequests() }
  Column(modifier.statusBarsPadding().padding(horizontal = 12.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
    maybeReview?.let { review ->
      Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
          if (review.isDone) {
            Text(stringResource(R.string.approve_done))
            Button(onClick = viewModel::closeLinkReview) { Text(stringResource(R.string.back)) }
          } else {
            val isEnded = review.maybeError == R.string.approve_attempt_ended
            if (isEnded) {
              Text(stringResource(R.string.approve_attempt_ended), color = MaterialTheme.colorScheme.error)
              OutlinedButton(onClick = viewModel::closeLinkReview) { Text(stringResource(R.string.back)) }
            } else {
              LinkCodeStep(
                maybeError = review.maybeError,
                isConfirming = review.isConfirming,
                onApprove = viewModel::confirmLinkReview,
                onCancel = viewModel::closeLinkReview,
              )
            }
          }
        }
      }
    }
    // Another request waits behind an open approval; its prompt shows once that one is done.
    if (maybeReview == null) {
      for (request in requests) {
        key(request.id) { LinkRequestCard(request, maybeFailure, viewModel) }
      }
    }
  }
}

@Composable
private fun LinkRequestCard(request: LinkRequestItem, @StringRes maybeFailure: Int?, viewModel: MainViewModel) {
  // The request ends with its relay; the clock only decides when to take the prompt away.
  LaunchedEffect(request.id, request.secondsLeft) {
    delay(request.secondsLeft.toLong() * MILLIS_PER_SECOND)
    viewModel.expireLinkRequest(request.id)
  }
  Card(Modifier.fillMaxWidth()) {
    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
      Text(stringResource(R.string.link_request_title), style = MaterialTheme.typography.titleSmall)
      Text(
        stringResource(R.string.link_request_body, request.deviceName, stringResource(platformLabel(request.platform))),
        style = MaterialTheme.typography.bodyMedium,
      )
      Text(stringResource(R.string.link_request_warning), style = MaterialTheme.typography.bodySmall)
      maybeFailure?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error) }
      Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = { viewModel.rejectLinkRequest(request.id) }) { Text(stringResource(R.string.link_request_deny)) }
        Button(onClick = { viewModel.reviewLinkRequest(request) }) { Text(stringResource(R.string.link_request_review)) }
      }
    }
  }
}

/**
 * The step that decides a link: the user types the code the new device shows. A missing or short
 * code and the other device's refusal ([maybeError]) are shown at the field; the refusal until the
 * user edits the code. [isConfirming] while the code is checked.
 */
@Composable
fun LinkCodeStep(
  @StringRes maybeError: Int?,
  isConfirming: Boolean,
  onApprove: (String) -> Unit,
  onCancel: () -> Unit,
) {
  // Not saveable: a restored screen must never keep a typed code of an approval the user left.
  var typed by remember { mutableStateOf("") }
  var hasTried by remember { mutableStateOf(false) }
  var isRefusalEdited by remember(maybeError) { mutableStateOf(false) }
  val focus = remember { FocusRequester() }
  val maybeShown = linkCodeError(typed).takeIf { hasTried } ?: maybeError?.takeUnless { isRefusalEdited || isConfirming }
  val approve: () -> Unit = {
    hasTried = true
    isRefusalEdited = false
    if (linkCodeError(typed) != null) {
      focus.requestFocus()
    } else if (!isConfirming) {
      onApprove(typed)
    }
  }
  LaunchedEffect(maybeError, isConfirming) {
    if (maybeError != null && !isConfirming) focus.requestFocus()
  }
  Text(
    stringResource(R.string.approve_type_code),
    style = MaterialTheme.typography.bodyMedium,
  )
  OutlinedTextField(
    value = typed,
    onValueChange = {
      typed = it.filter(Char::isDigit).take(LINK_CODE_LENGTH)
      isRefusalEdited = true
    },
    label = { Text(stringResource(R.string.approve_typed_code)) },
    singleLine = true,
    isError = maybeShown != null,
    supportingText = supportingText(maybeShown, R.string.approve_code_digits),
    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
    keyboardActions = KeyboardActions(onDone = { approve() }),
    modifier = Modifier.fillMaxWidth().focusRequester(focus).fieldError(maybeShown),
  )
  Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    Button(onClick = approve, enabled = !isConfirming) {
      Text(stringResource(R.string.approve_confirm))
    }
    OutlinedButton(onClick = onCancel) { Text(stringResource(R.string.cancel)) }
  }
}

@StringRes
private fun platformLabel(platform: LinkPlatform): Int = when (platform) {
  LinkPlatform.ANDROID -> R.string.link_platform_android
  LinkPlatform.IOS -> R.string.link_platform_ios
  LinkPlatform.MACOS -> R.string.link_platform_macos
  LinkPlatform.WINDOWS -> R.string.link_platform_windows
  LinkPlatform.LINUX -> R.string.link_platform_linux
  LinkPlatform.WEB -> R.string.link_platform_web
  LinkPlatform.OTHER -> R.string.link_platform_other
}

private const val MILLIS_PER_SECOND = 1000L
