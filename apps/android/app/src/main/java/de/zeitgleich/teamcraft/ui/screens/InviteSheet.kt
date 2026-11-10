package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
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
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.Space
import de.zeitgleich.teamcraft.ui.ActionAnswer
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.forms.FieldErrors
import de.zeitgleich.teamcraft.ui.forms.InviteField
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.focusFirst
import de.zeitgleich.teamcraft.ui.forms.inviteErrors
import de.zeitgleich.teamcraft.ui.forms.supportingText
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.InvitationOutcome
import uniffi.teamcraft_ffi.InviteOutcome
import uniffi.teamcraft_ffi.InvitePeopleInput

/**
 * "Invite people" (#1337): an e-mail address into one of [spaces] (those the user may invite to),
 * as a member or a guest where the user may invite that way. The sheet stays until the server took
 * the invitation; a refused address shows at its field (forms.md).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun InvitePeopleSheet(viewModel: MainViewModel, spaces: List<Space>, onSent: () -> Unit, onDismiss: () -> Unit) {
  val scope = rememberCoroutineScope()
  var spaceId by rememberSaveable { mutableStateOf(spaces.first().id) }
  val space = spaces.firstOrNull { it.id == spaceId } ?: spaces.first()
  var email by rememberSaveable { mutableStateOf("") }
  var note by rememberSaveable { mutableStateOf("") }
  var wantsGuest by rememberSaveable { mutableStateOf(false) }
  // Only the roles the user may give in this space; a member invitation where both are allowed.
  val isGuest = when {
    !space.canAddMembers -> true
    !space.canInviteGuests -> false
    else -> wantsGuest
  }
  var hasTried by rememberSaveable { mutableStateOf(false) }
  var isSending by remember { mutableStateOf(false) }
  var maybeEmailRefusal by remember { mutableStateOf<Int?>(null) }
  // The invitation exists, but its e-mail did not go out: the sheet says so, with a resend where one can help.
  var maybeUnsent by remember { mutableStateOf<InviteOutcome.MailNotSent?>(null) }
  var maybeResendNote by remember { mutableStateOf<Int?>(null) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val requesters = remember { InviteField.entries.associateWith { FocusRequester() } }
  val checked = inviteErrors(email, note)
  val shown = (if (hasTried) checked else FieldErrors.of<InviteField>()).withServerError(InviteField.EMAIL, maybeEmailRefusal)
  val send: () -> Unit = send@{
    hasTried = true
    if (!checked.isValid) {
      checked.focusFirst(requesters)
      return@send
    }
    if (isSending) return@send
    isSending = true
    maybeFailure = null
    scope.launch {
      when (val answer = viewModel.tryDirectoryAction { invitePeople(InvitePeopleInput(space.id, email.trim(), isGuest, note.trim())) }) {
        is ActionAnswer.Answered -> when (answer.value) {
          is InviteOutcome.Sent -> onSent()
          is InviteOutcome.MailNotSent -> maybeUnsent = answer.value as InviteOutcome.MailNotSent
          is InviteOutcome.AlreadyInvited, is InviteOutcome.EmailRefused -> {
            maybeEmailRefusal = if (answer.value is InviteOutcome.AlreadyInvited) R.string.invite_already_invited else R.string.invite_email_refused
            requesters.getValue(InviteField.EMAIL).requestFocus()
          }
          is InviteOutcome.NotAllowed -> maybeFailure = R.string.create_not_allowed
        }
        is ActionAnswer.Failed -> maybeFailure = answer.text
      }
      isSending = false
    }
  }
  val resend: (InviteOutcome.MailNotSent) -> Unit = resend@{ unsent ->
    if (isSending) return@resend
    isSending = true
    maybeResendNote = null
    scope.launch {
      when (val answer = viewModel.tryDirectoryAction { resendInvitation(unsent.spaceId, unsent.invitationId) }) {
        is ActionAnswer.Answered -> when (answer.value) {
          InvitationOutcome.DONE -> onSent()
          InvitationOutcome.TOO_SOON -> maybeResendNote = R.string.directory_resend_later
          InvitationOutcome.GONE, InvitationOutcome.NOT_ALLOWED -> maybeResendNote = R.string.create_not_allowed
        }
        is ActionAnswer.Failed -> maybeResendNote = answer.text
      }
      isSending = false
    }
  }
  LaunchedEffect(Unit) { requesters.getValue(InviteField.EMAIL).requestFocus() }
  ModalBottomSheet(
    onDismissRequest = { if (!isSending) onDismiss() },
    sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
  ) {
    Column(
      Modifier
        .fillMaxWidth()
        .imePadding()
        .navigationBarsPadding()
        .verticalScroll(rememberScrollState())
        .padding(horizontal = Spacing.l)
        .padding(bottom = Spacing.l),
      verticalArrangement = Arrangement.spacedBy(Spacing.m),
    ) {
      Text(stringResource(R.string.invite_title), style = MaterialTheme.typography.titleLarge, modifier = Modifier.semantics { heading() })
      val unsent = maybeUnsent
      if (unsent != null) {
        Text(
          stringResource(R.string.invite_mail_not_sent, email.trim()),
          style = MaterialTheme.typography.bodyLarge,
          modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
        maybeResendNote?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium) }
        if (unsent.canResend) {
          Button(onClick = { resend(unsent) }, enabled = !isSending, modifier = Modifier.fillMaxWidth()) {
            Text(stringResource(R.string.directory_resend))
          }
        }
        TextButton(onClick = onSent, enabled = !isSending, modifier = Modifier.fillMaxWidth()) {
          Text(stringResource(R.string.invite_mail_done))
        }
      } else {
        if (spaces.size > 1) {
          ChoiceGroup(R.string.invite_space_label) {
            for (candidate in spaces) {
              Choice(candidate.name, maybeHint = null, isSelected = candidate.id == space.id) { spaceId = candidate.id }
            }
          }
        }
        OutlinedTextField(
          value = email,
          onValueChange = {
            email = it
            maybeEmailRefusal = null
          },
          label = { Text(stringResource(R.string.invite_email_label)) },
          singleLine = true,
          isError = shown[InviteField.EMAIL] != null,
          supportingText = supportingText(shown[InviteField.EMAIL]),
          keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Email, imeAction = ImeAction.Next),
          modifier = Modifier.fillMaxWidth().focusRequester(requesters.getValue(InviteField.EMAIL)).fieldError(shown[InviteField.EMAIL]),
        )
        if (space.canAddMembers && space.canInviteGuests) {
          ChoiceGroup(R.string.invite_role_label) {
            Choice(stringResource(R.string.invite_role_member), stringResource(R.string.invite_role_member_hint), isSelected = !isGuest) { wantsGuest = false }
            Choice(stringResource(R.string.invite_role_guest), stringResource(R.string.invite_role_guest_hint), isSelected = isGuest) { wantsGuest = true }
          }
        }
        OutlinedTextField(
          value = note,
          onValueChange = { note = it },
          label = { Text(stringResource(R.string.invite_message_label)) },
          minLines = 2,
          isError = shown[InviteField.NOTE] != null,
          supportingText = supportingText(shown[InviteField.NOTE]),
          keyboardOptions = textKeyboard(),
          modifier = Modifier.fillMaxWidth().focusRequester(requesters.getValue(InviteField.NOTE)).fieldError(shown[InviteField.NOTE]),
        )
        maybeFailure?.let { failure ->
          Text(
            stringResource(failure),
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
          )
        }
        Button(onClick = send, enabled = !isSending, modifier = Modifier.fillMaxWidth()) {
          if (isSending) {
            CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.onPrimary)
          } else {
            Text(stringResource(R.string.invite_submit))
          }
        }
      }
    }
  }
}

@Composable
private fun ChoiceGroup(@StringRes label: Int, content: @Composable () -> Unit) {
  Column(Modifier.fillMaxWidth().selectableGroup()) {
    Text(
      stringResource(label),
      style = MaterialTheme.typography.labelLarge,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(bottom = Spacing.xs),
    )
    content()
  }
}

@Composable
private fun Choice(title: String, maybeHint: String?, isSelected: Boolean, onSelect: () -> Unit) {
  Row(
    Modifier
      .fillMaxWidth()
      .selectable(selected = isSelected, role = Role.RadioButton, onClick = onSelect)
      .padding(vertical = Spacing.xs),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    RadioButton(selected = isSelected, onClick = null, modifier = Modifier.padding(horizontal = Spacing.s))
    Column(Modifier.weight(1f)) {
      Text(title, style = MaterialTheme.typography.bodyLarge)
      maybeHint?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
  }
}
