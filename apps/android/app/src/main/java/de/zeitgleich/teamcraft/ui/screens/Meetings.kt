package de.zeitgleich.teamcraft.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.text.format.DateUtils
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.consumeWindowInsets
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
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
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
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.ui.ActionAnswer
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.MeetingCancelScope
import uniffi.teamcraft_ffi.SavedMeeting
import uniffi.teamcraft_ffi.meetingRowActions
import uniffi.teamcraft_ffi.newRequestKey

/** The meetings screen's views, as desktop's (#956): what is still on, and what ended lately. */
private enum class MeetingsView(@StringRes val label: Int) { UPCOMING(R.string.meetings_tab_upcoming), PAST(R.string.meetings_tab_past) }

/**
 * Upcoming and running meetings, and the past ones: join one (its conversation's call), start one
 * now, schedule one (then its link is handed out), and change, cancel, restore or remove the user's
 * own (#1841, #1905). The core saves them and sets up their encrypted chats; this is the list.
 */
@Composable
fun MeetingsScreen(viewModel: MainViewModel, onBack: () -> Unit) {
  var view by rememberSaveable { mutableStateOf(MeetingsView.UPCOMING) }
  val read = rememberRead(viewModel) { meetings() }
  // The past list is only read once it is looked at.
  val past = rememberRead(viewModel, enabled = view == MeetingsView.PAST) { pastMeetings() }
  val scope = rememberCoroutineScope()
  var isFormOpen by remember { mutableStateOf(false) }
  var maybeCreated by remember { mutableStateOf<SavedMeeting?>(null) }
  var isStarting by remember { mutableStateOf(false) }
  // One key per "Meet now" until it went through: a retry after a lost answer starts no second one.
  var maybeNowKey by remember { mutableStateOf<String?>(null) }
  val meetNow: () -> Unit = meetNow@{
    if (isStarting) return@meetNow
    isStarting = true
    // Measured while the meeting is set up (#1969).
    viewModel.measureLink()
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
    Column(Modifier.fillMaxSize().padding(padding.withoutBottom()).consumeWindowInsets(padding)) {
      PrimaryTabRow(selectedTabIndex = view.ordinal) {
        MeetingsView.entries.forEach { choice ->
          Tab(selected = view == choice, onClick = { view = choice }, text = { Text(stringResource(choice.label)) })
        }
      }
      LazyColumn(Modifier.fillMaxSize(), contentPadding = padding.bottomOnly()) {
        if (view == MeetingsView.UPCOMING) {
          placeholderItem(read.phase { upcomingMeetings(it) }, read.retry) { EmptyText(R.string.meetings_empty) }
          // A repeating meeting is listed once per occurrence, all with the series' ID.
          items(upcomingMeetings(read.maybeValue.orEmpty()), key = { "${it.id}-${it.startsAt}" }) { meeting ->
            MeetingRow(viewModel, meeting, onChanged = read.refresh)
            HorizontalDivider()
          }
        } else {
          placeholderItem(past.phase { it }, past.retry) { EmptyText(R.string.meetings_past_empty) }
          items(past.maybeValue.orEmpty(), key = { "${it.id}-${it.startsAt}" }) { meeting ->
            MeetingRow(viewModel, meeting, onChanged = past.refresh)
            HorizontalDivider()
          }
        }
      }
    }
  }
  if (isFormOpen) {
    MeetingFormSheet(viewModel, maybeMeeting = null, onSaved = { saved ->
      isFormOpen = false
      maybeCreated = saved
      read.refresh()
    }, onDismiss = { isFormOpen = false })
  }
  maybeCreated?.let { saved -> MeetingCreatedSheet(viewModel, saved, onDismiss = { maybeCreated = null }) }
}

/**
 * What is still on, soonest first: also a cancelled meeting until it would have ended, so its
 * organizer can restore it (as desktop lists it). One that was ended early is gone.
 */
fun upcomingMeetings(meetings: List<Meeting>, nowMs: Long = System.currentTimeMillis()): List<Meeting> =
  meetings.filter { it.status != "ended" && it.endsAt > nowMs }.sortedBy { it.startsAt }

/**
 * A meeting's title and time, with the button to join its call while it is on, and its menu: "Copy
 * link", and for its organizer "Edit meeting", "Cancel meeting", "Restore" and "Remove from list",
 * as the core's rules for the row say (`meetingRowActions`). [onChanged] runs once one went through.
 */
@Composable
fun MeetingRow(viewModel: MainViewModel, meeting: Meeting, onChanged: () -> Unit = {}) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val isOrganizer = meeting.organizerUserId.isNotEmpty() && meeting.organizerUserId == viewModel.maybeMyUserId
  val isCancelled = meeting.status == "cancelled"
  val actions = meetingRowActions(
    isOrganizer = isOrganizer,
    isCancelled = isCancelled,
    isOccurrenceCancelled = meeting.isOccurrenceCancelled,
    isEnded = meeting.status == "ended",
    endsAtMs = meeting.endsAt,
    nowMs = System.currentTimeMillis(),
  )
  val hasMenu = actions.canCopyLink || actions.canChange || actions.canRestore || actions.canHide
  var isMenuOpen by remember { mutableStateOf(false) }
  var isEditing by remember { mutableStateOf(false) }
  var isCancelling by remember { mutableStateOf(false) }
  var isBusy by remember { mutableStateOf(false) }
  val act: (suspend ChatClient.() -> Unit) -> Unit = { action ->
    if (!isBusy) {
      isBusy = true
      scope.launch {
        when (val answer = viewModel.tryMeetingAction(action)) {
          is ActionAnswer.Answered -> onChanged()
          is ActionAnswer.Failed -> viewModel.showFailure(answer.text)
        }
        isBusy = false
      }
    }
  }
  Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
    Column(Modifier.weight(1f)) {
      Text(
        meeting.title,
        style = MaterialTheme.typography.titleMedium,
        color = if (actions.canCopyLink) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
      )
      Text(
        DateUtils.formatDateRange(
          context,
          meeting.startsAt,
          meeting.endsAt,
          DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME,
        ),
        style = MaterialTheme.typography.bodySmall,
      )
      if (isCancelled) {
        Text(
          stringResource(if (meeting.isOccurrenceHoliday) R.string.meeting_holiday else R.string.meeting_cancelled),
          style = MaterialTheme.typography.labelMedium,
          color = MaterialTheme.colorScheme.error,
        )
      }
    }
    if (actions.canCopyLink) {
      Button(onClick = { viewModel.joinMeeting(meeting) }) {
        Text(stringResource(R.string.meeting_join))
      }
    }
    if (hasMenu) {
      IconButton(onClick = { isMenuOpen = true }, enabled = !isBusy) {
        Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.meeting_actions))
      }
      DropdownMenu(expanded = isMenuOpen, onDismissRequest = { isMenuOpen = false }) {
        if (actions.canCopyLink) {
          DropdownMenuItem(text = { Text(stringResource(R.string.meeting_copy_link)) }, onClick = {
            isMenuOpen = false
            scope.launch { copyMeetingLink(viewModel, context, meeting.id) }
          })
        }
        if (actions.canChange) {
          DropdownMenuItem(text = { Text(stringResource(R.string.meeting_edit)) }, onClick = {
            isMenuOpen = false
            isEditing = true
          })
          DropdownMenuItem(text = { Text(stringResource(R.string.meeting_cancel)) }, onClick = {
            isMenuOpen = false
            isCancelling = true
          })
        }
        if (actions.canRestore) {
          DropdownMenuItem(
            text = { Text(stringResource(if (meeting.isOccurrenceHoliday) R.string.meeting_restore_holiday else R.string.meeting_restore)) },
            onClick = {
              isMenuOpen = false
              val maybeOccurrence = if (meeting.isOccurrenceCancelled) meeting.startsAt else null
              act { restoreMeeting(meeting.id, maybeOccurrence) }
            },
          )
        }
        if (actions.canHide) {
          DropdownMenuItem(
            text = { Text(stringResource(R.string.meeting_remove), color = MaterialTheme.colorScheme.error) },
            onClick = {
              isMenuOpen = false
              act { hideMeeting(meeting.id) }
            },
          )
        }
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

/**
 * "Copy link": the core makes it with the organizer's key fingerprint as this device pinned it
 * (e2ee.md §4.6), never the server's; without that, nothing is copied and the user is told.
 */
private suspend fun copyMeetingLink(viewModel: MainViewModel, context: Context, meetingId: String) {
  when (val answer = viewModel.tryMeetingAction { meetingLink(meetingId) }) {
    is ActionAnswer.Answered -> copyText(context, answer.value)
    is ActionAnswer.Failed -> viewModel.showFailure(R.string.meeting_link_unverified)
  }
}

/** Puts a meeting link on the clipboard (Android 13+ confirms it itself). */
internal fun copyText(context: Context, text: String) {
  context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(text, text))
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
