package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.consumeWindowInsets
import android.text.format.DateUtils
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
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
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.ui.ActionAnswer
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.MeetingCancelScope
import uniffi.teamcraft_ffi.newRequestKey

/**
 * Upcoming and running meetings: join one (its conversation's call), start one now, schedule one,
 * and change or cancel the user's own (#1841). The core saves them and sets up their encrypted
 * chats; this is the form and the list.
 */
@Composable
fun MeetingsScreen(viewModel: MainViewModel, onBack: () -> Unit) {
  val read = rememberRead(viewModel) { meetings() }
  val scope = rememberCoroutineScope()
  var isFormOpen by remember { mutableStateOf(false) }
  var isStarting by remember { mutableStateOf(false) }
  // One key per "Meet now" until it went through: a retry after a lost answer starts no second one.
  var maybeNowKey by remember { mutableStateOf<String?>(null) }
  val meetNow: () -> Unit = meetNow@{
    if (isStarting) return@meetNow
    isStarting = true
    val key = maybeNowKey ?: newRequestKey().also { maybeNowKey = it }
    scope.launch {
      when (val answer = viewModel.tryMeetingAction { startMeetingNow(null, key) }) {
        is ActionAnswer.Answered -> {
          maybeNowKey = null
          read.refresh()
          viewModel.startCall(answer.value.conversationId, answer.value.title)
        }
        is ActionAnswer.Failed -> viewModel.showFailure(answer.text)
      }
      isStarting = false
    }
  }
  Scaffold(
    topBar = {
      ChromeTopBar(
        title = stringResource(R.string.meetings_title),
        maybeOnBack = onBack,
        actions = {
          TextButton(onClick = meetNow, enabled = !isStarting) { Text(stringResource(R.string.meetings_meet_now)) }
          IconButton(onClick = { isFormOpen = true }) {
            Icon(Icons.Filled.Add, contentDescription = stringResource(R.string.meetings_schedule))
          }
        },
      )
    },
    contentWindowInsets = ScreenInsets,
  ) { padding ->
    LazyColumn(Modifier.fillMaxSize().consumeWindowInsets(padding), contentPadding = padding) {
      val list = upcomingMeetings(read.maybeValue.orEmpty())
      placeholderItem(read.phase { upcomingMeetings(it) }, read.retry) { EmptyText(R.string.meetings_empty) }
      // A repeating meeting is listed once per occurrence, all with the series' ID.
      items(list, key = { "${it.id}-${it.startsAt}" }) { meeting ->
        MeetingRow(viewModel, meeting, onChanged = read.refresh)
        HorizontalDivider()
      }
    }
  }
  if (isFormOpen) {
    MeetingFormSheet(viewModel, maybeMeeting = null, onSaved = {
      isFormOpen = false
      read.refresh()
    }, onDismiss = { isFormOpen = false })
  }
}

/** What is still on, soonest first. */
fun upcomingMeetings(meetings: List<Meeting>): List<Meeting> =
  meetings.filter { it.status != "cancelled" && it.status != "ended" }.sortedBy { it.startsAt }

/**
 * A meeting's title and time, with the button to join its call; its organizer also gets "Edit
 * meeting" and "Cancel meeting". [onChanged] runs once one of them went through.
 */
@Composable
fun MeetingRow(viewModel: MainViewModel, meeting: Meeting, onChanged: () -> Unit = {}) {
  val context = LocalContext.current
  val isOrganizer = meeting.organizerUserId.isNotEmpty() && meeting.organizerUserId == viewModel.maybeMyUserId
  var isMenuOpen by remember { mutableStateOf(false) }
  var isEditing by remember { mutableStateOf(false) }
  var isCancelling by remember { mutableStateOf(false) }
  Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
    Column(Modifier.weight(1f)) {
      Text(meeting.title, style = MaterialTheme.typography.titleMedium)
      Text(
        DateUtils.formatDateRange(
          context,
          meeting.startsAt,
          meeting.endsAt,
          DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME,
        ),
        style = MaterialTheme.typography.bodySmall,
      )
    }
    Button(onClick = { viewModel.joinMeeting(meeting) }) {
      Text(stringResource(R.string.meeting_join))
    }
    if (isOrganizer) {
      IconButton(onClick = { isMenuOpen = true }) {
        Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.meeting_actions))
      }
      DropdownMenu(expanded = isMenuOpen, onDismissRequest = { isMenuOpen = false }) {
        DropdownMenuItem(text = { Text(stringResource(R.string.meeting_edit)) }, onClick = {
          isMenuOpen = false
          isEditing = true
        })
        DropdownMenuItem(text = { Text(stringResource(R.string.meeting_cancel)) }, onClick = {
          isMenuOpen = false
          isCancelling = true
        })
      }
    }
  }
  if (isEditing) {
    MeetingFormSheet(viewModel, maybeMeeting = meeting, onSaved = {
      isEditing = false
      onChanged()
    }, onDismiss = { isEditing = false })
  }
  if (isCancelling) {
    CancelMeetingDialog(viewModel, meeting, onDone = { wasCancelled ->
      isCancelling = false
      if (wasCancelled) onChanged()
    })
  }
}

/** Asks before cancelling; of a series, which of its meetings. */
@Composable
private fun CancelMeetingDialog(viewModel: MainViewModel, meeting: Meeting, onDone: (Boolean) -> Unit) {
  val scope = rememberCoroutineScope()
  var isSending by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val cancel: (MeetingCancelScope) -> Unit = cancel@{ cancelScope ->
    if (isSending) return@cancel
    isSending = true
    maybeFailure = null
    scope.launch {
      when (val answer = viewModel.tryMeetingAction { cancelMeeting(meeting.id, cancelScope, meeting.startsAt) }) {
        is ActionAnswer.Answered -> onDone(true)
        is ActionAnswer.Failed -> maybeFailure = answer.text
      }
      isSending = false
    }
  }
  AlertDialog(
    onDismissRequest = { if (!isSending) onDone(false) },
    title = {
      Text(stringResource(if (meeting.isRepeating) R.string.meeting_cancel_title_series else R.string.meeting_cancel_title))
    },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
        Text(stringResource(R.string.meeting_cancel_hint))
        if (meeting.isRepeating) {
          listOf(
            MeetingCancelScope.OCCURRENCE to R.string.meeting_cancel_occurrence,
            MeetingCancelScope.FOLLOWING to R.string.meeting_cancel_following,
            MeetingCancelScope.SERIES to R.string.meeting_cancel_series,
          ).forEach { (cancelScope, label) ->
            TextButton(onClick = { cancel(cancelScope) }, enabled = !isSending) { Text(stringResource(label)) }
          }
        }
        maybeFailure?.let { failure ->
          Text(
            stringResource(failure),
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
          )
        }
      }
    },
    confirmButton = {
      if (!meeting.isRepeating) {
        TextButton(onClick = { cancel(MeetingCancelScope.SERIES) }, enabled = !isSending) {
          Text(stringResource(R.string.meeting_cancel_confirm))
        }
      }
    },
    dismissButton = {
      TextButton(onClick = { onDone(false) }, enabled = !isSending) { Text(stringResource(R.string.meeting_cancel_keep)) }
    },
  )
}
