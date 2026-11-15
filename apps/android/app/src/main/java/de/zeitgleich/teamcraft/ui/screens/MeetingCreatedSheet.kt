package de.zeitgleich.teamcraft.ui.screens

import android.content.Intent
import android.text.format.DateUtils
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.ui.ActionAnswer
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.Spacing
import uniffi.teamcraft_ffi.SavedMeeting

/** The link as the sheet has it: being made, made, or not to be had on this device. */
internal sealed interface LinkState {
  data object Verifying : LinkState

  data class Ready(val link: String) : LinkState

  data object Unverified : LinkState
}

/**
 * Right after scheduling (#1905), as desktop's "Your meeting is ready": the meeting's link to copy
 * or share, and joining it. The link carries the organizer's key fingerprint as this device pinned
 * it (`meetingLink`, e2ee.md §4.6); until the core made it, nothing is handed out.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MeetingCreatedSheet(viewModel: MainViewModel, saved: SavedMeeting, onDismiss: () -> Unit) {
  val context = LocalContext.current
  var link by remember { mutableStateOf<LinkState>(LinkState.Verifying) }
  var isCopied by remember { mutableStateOf(false) }
  LaunchedEffect(saved.meetingId, link == LinkState.Verifying) {
    if (link != LinkState.Verifying) return@LaunchedEffect
    link = when (val answer = viewModel.tryMeetingAction { meetingLink(saved.meetingId) }) {
      is ActionAnswer.Answered -> LinkState.Ready(answer.value)
      is ActionAnswer.Failed -> LinkState.Unverified
    }
  }
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
    MeetingCreatedContent(
      saved = saved,
      link = link,
      isCopied = isCopied,
      onRetry = { link = LinkState.Verifying },
      onCopy = { text ->
        copyText(context, text)
        isCopied = true
      },
      onDismiss = onDismiss,
      onJoin = {
        onDismiss()
        viewModel.joinMeeting(joinable(saved, viewModel.maybeMyUserId.orEmpty()))
      },
    )
  }
}

/** What the sheet shows, for a [link] in each of its states. */
@Composable
internal fun MeetingCreatedContent(
  saved: SavedMeeting,
  link: LinkState,
  isCopied: Boolean,
  onRetry: () -> Unit,
  onCopy: (String) -> Unit,
  onDismiss: () -> Unit,
  onJoin: () -> Unit,
) {
  val context = LocalContext.current
  Column(
    Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = Spacing.l).padding(bottom = Spacing.l),
    verticalArrangement = Arrangement.spacedBy(Spacing.m),
  ) {
    Text(
      stringResource(R.string.meeting_created_title),
      style = MaterialTheme.typography.titleLarge,
      modifier = Modifier.semantics { heading() },
    )
    Column {
      Text(saved.title, style = MaterialTheme.typography.titleMedium)
      Text(
        DateUtils.formatDateRange(context, saved.startsAtMs, saved.endsAtMs, DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    Text(stringResource(R.string.meeting_created_hint))
    when (link) {
      LinkState.Verifying -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        Text(stringResource(R.string.meeting_link_verifying), color = MaterialTheme.colorScheme.onSurfaceVariant)
      }
      LinkState.Unverified -> Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
        Text(
          stringResource(R.string.meeting_link_unverified),
          color = MaterialTheme.colorScheme.error,
          modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
        TextButton(onClick = onRetry) { Text(stringResource(R.string.retry)) }
      }
      is LinkState.Ready -> Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
        SelectionContainer {
          Text(link.link, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
          OutlinedButton(onClick = { onCopy(link.link) }) {
            Text(
              stringResource(if (isCopied) R.string.meeting_link_copied else R.string.meeting_copy_link),
              modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
          }
          OutlinedButton(onClick = {
            val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, link.link)
            context.startActivity(Intent.createChooser(send, null))
          }) {
            Icon(Icons.Filled.Share, contentDescription = null, modifier = Modifier.padding(end = Spacing.xs).size(18.dp))
            Text(stringResource(R.string.meeting_share_link))
          }
        }
        Text(
          stringResource(R.string.meeting_link_hint),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
      }
    }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Spacing.s, Alignment.End)) {
      TextButton(onClick = onDismiss) { Text(stringResource(R.string.close)) }
      Button(onClick = onJoin) { Text(stringResource(R.string.meeting_join)) }
    }
  }
}

/** The saved meeting as the list has it, to join: the user scheduled it, so they organize it. */
internal fun joinable(saved: SavedMeeting, me: String): Meeting = Meeting(
  id = saved.meetingId,
  title = saved.title,
  startsAt = saved.startsAtMs,
  endsAt = saved.endsAtMs,
  status = "scheduled",
  conversationId = saved.conversationId,
  organizerUserId = me,
)
