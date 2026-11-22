package de.zeitgleich.teamcraft.ui.screens

import android.content.Intent
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.ui.ActionAnswer
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.SecretClipboard
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch

/** The one-time link as the sheet has it. */
internal sealed interface GuestLinkState {
  data object None : GuestLinkState

  data object Making : GuestLinkState

  data class Made(val link: String) : GuestLinkState

  data class Failed(@StringRes val reason: Int) : GuestLinkState
}

/**
 * "Invite guests" of a meeting the user organizes or co-hosts (#1905), as desktop's: a personal
 * one-time link for one guest, the organizer's code guests may compare, and for the organizer a
 * new meeting link that ends the current one. Every link carries the organizer's key fingerprint as
 * this device pinned it (e2ee.md §4.6); without it the core makes none.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MeetingGuestsSheet(viewModel: MainViewModel, meeting: Meeting, canRenewLink: Boolean, onRenewed: () -> Unit, onDismiss: () -> Unit) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  var guestLink by remember { mutableStateOf<GuestLinkState>(GuestLinkState.None) }
  var maybeCode by remember { mutableStateOf<String?>(null) }
  var isRenewing by remember { mutableStateOf(false) }
  var isConfirmingRenew by remember { mutableStateOf(false) }
  var maybeRenewNote by remember { mutableStateOf<Int?>(null) }
  LaunchedEffect(meeting.id) {
    // The code shows once the core pinned the organizer's key; offline it stays away.
    maybeCode = (viewModel.tryMeetingAction { meetingOrganizerCode(meeting.id) } as? ActionAnswer.Answered)?.value
  }
  val makeLink: () -> Unit = makeLink@{
    if (guestLink == GuestLinkState.Making) return@makeLink
    guestLink = GuestLinkState.Making
    scope.launch {
      guestLink = when (val answer = viewModel.tryMeetingAction { meetingGuestLink(meeting.id) }) {
        is ActionAnswer.Answered -> GuestLinkState.Made(answer.value.link)
        is ActionAnswer.Failed -> GuestLinkState.Failed(R.string.meeting_guest_link_failed)
      }
    }
  }
  val renew: () -> Unit = renew@{
    if (isRenewing) return@renew
    isRenewing = true
    maybeRenewNote = null
    scope.launch {
      maybeRenewNote = when (val answer = viewModel.tryMeetingAction { renewMeetingLink(meeting.id) }) {
        is ActionAnswer.Answered -> {
          onRenewed()
          R.string.meeting_link_renewed
        }
        is ActionAnswer.Failed -> answer.text
      }
      isRenewing = false
      isConfirmingRenew = false
    }
  }
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
    MeetingGuestsContent(
      title = meeting.title,
      guestLink = guestLink,
      maybeCode = maybeCode,
      canRenewLink = canRenewLink,
      isRenewing = isRenewing,
      maybeRenewNote = maybeRenewNote,
      onMakeLink = makeLink,
      // A one-time link is a credential: copied as a secret, cleared from the clipboard later.
      onCopy = { link -> SecretClipboard.copy(context, link) },
      onShare = { link ->
        val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, link)
        context.startActivity(Intent.createChooser(send, null))
      },
      onRenew = { isConfirmingRenew = true },
      onDismiss = onDismiss,
    )
  }
  if (isConfirmingRenew) {
    AlertDialog(
      onDismissRequest = { if (!isRenewing) isConfirmingRenew = false },
      title = { Text(stringResource(R.string.meeting_new_link_confirm_title)) },
      text = { Text(stringResource(R.string.meeting_new_link_hint)) },
      confirmButton = {
        TextButton(onClick = renew, enabled = !isRenewing) { Text(stringResource(R.string.meeting_new_link_confirm)) }
      },
      dismissButton = {
        TextButton(onClick = { isConfirmingRenew = false }, enabled = !isRenewing) { Text(stringResource(R.string.meeting_new_link_keep)) }
      },
    )
  }
}

/** What the sheet shows, for each state of the one-time link. */
@Composable
internal fun MeetingGuestsContent(
  title: String,
  guestLink: GuestLinkState,
  maybeCode: String?,
  canRenewLink: Boolean,
  isRenewing: Boolean,
  maybeRenewNote: Int?,
  onMakeLink: () -> Unit,
  onCopy: (String) -> Unit,
  onShare: (String) -> Unit,
  onRenew: () -> Unit,
  onDismiss: () -> Unit,
) {
  var isCopied by remember(guestLink) { mutableStateOf(false) }
  Column(
    Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).navigationBarsPadding()
      .padding(horizontal = Spacing.l).padding(bottom = Spacing.l),
    verticalArrangement = Arrangement.spacedBy(Spacing.m),
  ) {
    Column {
      Text(
        stringResource(R.string.meeting_invite_guests),
        style = MaterialTheme.typography.titleLarge,
        modifier = Modifier.semantics { heading() },
      )
      Text(title, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
    Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
      when (guestLink) {
        GuestLinkState.None, is GuestLinkState.Failed, GuestLinkState.Making -> Row(
          verticalAlignment = Alignment.CenterVertically,
          horizontalArrangement = Arrangement.spacedBy(Spacing.s),
        ) {
          Button(onClick = onMakeLink, enabled = guestLink != GuestLinkState.Making) {
            Text(stringResource(R.string.meeting_guest_link_make))
          }
          if (guestLink == GuestLinkState.Making) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        }
        is GuestLinkState.Made -> {
          Text(stringResource(R.string.meeting_guest_link_label), style = MaterialTheme.typography.labelLarge)
          SelectionContainer {
            Text(guestLink.link, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
          }
          Row(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
            OutlinedButton(onClick = {
              onCopy(guestLink.link)
              isCopied = true
            }) {
              Text(
                stringResource(if (isCopied) R.string.meeting_link_copied else R.string.meeting_copy_link),
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
              )
            }
            OutlinedButton(onClick = { onShare(guestLink.link) }) {
              Icon(Icons.Filled.Share, contentDescription = null, modifier = Modifier.padding(end = Spacing.xs).size(18.dp))
              Text(stringResource(R.string.meeting_share_link))
            }
          }
        }
      }
      (guestLink as? GuestLinkState.Failed)?.let { failed ->
        Text(
          stringResource(failed.reason),
          color = MaterialTheme.colorScheme.error,
          style = MaterialTheme.typography.bodySmall,
          modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
      }
      Text(
        stringResource(R.string.meeting_guest_link_hint),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    maybeCode?.let { code ->
      val label = stringResource(R.string.meeting_organizer_code)
      Column(
        verticalArrangement = Arrangement.spacedBy(Spacing.xs),
        // Read out group by group, not as one word.
        modifier = Modifier.semantics(mergeDescendants = true) { contentDescription = "$label ${code.toList().joinToString(" ")}" },
      ) {
        Text(label, style = MaterialTheme.typography.labelLarge)
        Text(code, style = MaterialTheme.typography.titleMedium, fontFamily = FontFamily.Monospace)
        Text(
          stringResource(R.string.meeting_organizer_code_hint),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
      }
    }
    if (canRenewLink) {
      HorizontalDivider()
      Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
          OutlinedButton(onClick = onRenew, enabled = !isRenewing) { Text(stringResource(R.string.meeting_new_link)) }
          if (isRenewing) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        }
        Text(
          stringResource(R.string.meeting_new_link_hint),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        maybeRenewNote?.let { note ->
          Text(
            stringResource(note),
            style = MaterialTheme.typography.bodySmall,
            color = if (note == R.string.meeting_link_renewed) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.error,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
          )
        }
      }
    }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
      TextButton(onClick = onDismiss) { Text(stringResource(R.string.close)) }
    }
  }
}
